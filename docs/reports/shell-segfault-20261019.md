# Live gnome-shell SIGSEGV 2026-10-19 00:09:51 - root cause and gorca involvement

Author: linux-platform-engineer. Live session was only observed (journal, installed library); every repro ran in a private headless shell (tests/shell/run_shell.sh, unique display/bus/runtime dir, only its own recorded shell PID was stopped/continued).

## 1. Symbolization (verified)
- Installed: libmutter-16-0 48.7-0+deb13u1, BuildID 8a81b520331651808ae5599c03f9f83826a49f0b.
- `gdb -iex "set debuginfod enabled on"` (DEBUGINFOD_URLS=https://debuginfod.debian.net, no sudo, no system change; debuginfo cached under ~/.cache/debuginfod_client) gives:
  `0xbbb2d = rebuild_all_grabbed_modifiers+45, ../src/backends/meta-a11y-manager.c:122`.
- Disassembly: `mov 0x28(%rbp),%rax ; mov 0x8(%rax),%edx` = `grabber->modifiers->len` with `grabber->modifiers == NULL` -> fault address 8 (matches `segfault at 8, error 4`).
- Core went nowhere: `kernel.core_pattern = |/bin/false`, so no coredump exists. Not changed (system setting).

## 2. Mutter bug (refs/mutter/src/backends/meta-a11y-manager.c)
- `ensure_key_grabber()` creates a `MetaA11yKeyGrabber` (modifiers = keystrokes = NULL) for ANY of GrabKeyboard / UngrabKeyboard / WatchKeyboard / UnwatchKeyboard / SetKeyGrabs.
- Only `handle_set_key_grabs()` allocates `modifiers`.
- `rebuild_all_grabbed_modifiers()` walks every grabber in `key_grabbers` and dereferences `grabber->modifiers->len` with no NULL check. (`should_grab_keypress` etc. do check `grabber->keystrokes`; this function is the inconsistent one.)
- Callers of rebuild: `handle_set_key_grabs` (any client) and `key_grabber_bus_name_vanished_callback` (when the vanishing grabber had modifiers).
- So: any grabber that exists but never completed SetKeyGrabs (Watch/Ungrab only) + any later SetKeyGrabs by another connection, or the vanish of a grabber that had modifiers = NULL deref in gnome-shell. Not fixed in refs/mutter main (d82671c, 2026-10-07).
- Access control (owner of org.gnome.Orca.KeyboardMonitor) limits who can trigger it, but a legitimate reader reconnecting is enough.

## 3. gorca's part (crates/gorca-input-mutter/src/source.rs, monitor.rs)
- Every session is a NEW zbus connection (new unique name): RequestName -> WatchKeyboard (creates a NULL-modifier grabber) -> SetKeyGrabs per generation, each followed by Grab/UngrabKeyboard in the same task (`spawn_grab_call`).
- GRAB_DEADLINE = 200 ms, GRAB_RETRIES = 2 (monitor.rs): a Shell main-loop stall of ~0.6 s ends the session ("grab set could not be applied"); the old connection is dropped (its pending SetKeyGrabs/UngrabKeyboard calls may still be queued toward Mutter), `backoff 100ms`, a new connection does WatchKeyboard + SetKeyGrabs while Mutter is still draining the queue. That is exactly the journal sequence (3x deadline at 00:09:50.199/.399/.600, lost, active .793, deadline again .995 = Shell still stalled).
- gorca never sends Unwatch and has no ordering knowledge of when Mutter processed the old connection's vanish.
- Root trigger of the stall was host load (swap ~15 GB, PSI full ~20), not gorca; gorca reacted to a slow Shell as a lost one, which created the churn that hits the Mutter bug. Without the churn the bug is not reachable by gorca (single connection, SetKeyGrabs right after Watch).
- I could not prove the exact interleaving in the live crash (no core); I reproduced the same crash signature with the churn pattern.

## 4. Reproduction (private headless shell), tools/platform-probes/shell-segfault/repro.py
Run: `tests/shell/run_shell.sh -- /usr/bin/python3 tools/platform-probes/shell-segfault/repro.py <mode>`
| mode | scenario | result |
|---|---|---|
| det | A: Watch+SetKeyGrabs; B: Watch only; A disconnects | crash 1/1 |
| setg | A: Watch only; B: SetKeyGrabs | crash 1/1 |
| stall | SIGSTOP private shell; A SetKeyGrabs queued; A closes; B Watch+SetKeyGrabs; SIGCONT | no crash 0/3 |
| stall2 | as stall, A also leaves 40 queued UngrabKeyboard calls (gorca does Ungrab after each SetKeyGrabs) | crash 2/3 |
All crashes: kernel `gnome-shell[...]: segfault at 8 ip ...b2d ... libmutter-16.so.0.0.0[bbb2d,...]`, identical to the live one. The stall2 mechanism: calls from the dead connection processed after its vanish callback re-create a NULL-modifier grabber for the dead name (and a new watcher), and the next SetKeyGrabs/vanish walks it.
Host was loaded (load 10-14) during runs; numbers are for crash/no-crash only.

## 5. Recommended gorca mitigations (not implemented)
1. Slow is not lost: SetKeyGrabs timeout must not tear down the connection. Keep the connection, widen deadline (>=2-5 s for grabs, still never blocking the reader: reader keeps last committed set), retry the same generation on the same connection. Only a NameOwnerChanged of the manager or our name ends a session.
2. Never run two registrations concurrently: when a session must end, close conn1, then wait until Mutter has certainly drained it (>= 1-2 s settle plus a successful round trip, e.g. Peer.Ping to the manager from a throwaway connection, which proves the main loop passed the queued vanish) before conn2 WatchKeyboard.
3. Do not send UngrabKeyboard after every SetKeyGrabs (only when grab_all actually changes): removes the Grab-family calls that create NULL-modifier grabbers.
4. Send SetKeyGrabs immediately after WatchKeyboard and coalesce (already one in flight; keep it), and use exponential backoff starting >= 1 s after a Shell stall.
5. On repeated timeouts, back off globally and do not reconnect while the manager's PSI/stall persists; measure Shell responsiveness (Peer.Ping RTT) before retrying.

## 6. Upstream
Warranted: a one-line NULL check (`if (!grabber->modifiers) continue;`) in rebuild_all_grabbed_modifiers (or allocate arrays in ensure_key_grabber). Any DBus-reachable crash of the compositor by an authorized screen reader is worth reporting; include repro.py and the symbolization. Filing is up to the owner; I did not file anything.
