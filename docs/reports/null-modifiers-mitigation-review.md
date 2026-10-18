# Review: Mutter 48.7 NULL-modifiers mitigation (branch worktree-agent-ae22ddd0ef2b2e214, f90ca235)

Reviewer, 2026-10-19. Verdict: **MERGE-WITH-FIXES** (F-1, F-2 required; F-3..F-6 recommended).

## Gates (measured on f90ca235, main toolchain, own target dir in this worktree)

| gate | result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace` | pass (0 failed) |
| `cargo xtask strings` | pass (332 literals, 113 allowed) |
| `regress.py --mode stall2 --runs 6` (private shell, load 12-13) | 6/6 shell alive, B active |
| e2e K1,K2,K2s,K3,K4,K4r,K4h `--quick` (load 16-18) | 1 gate FAILED: **K4.restore_failed** (restore_ms 2002) |
| e2e K4 `--repeats 3` | restore_ms 1997 / 2012 / 2017, restore_failed 2/3 (main k-final: 408 ms) |

KM is not on this branch (it lives on worktree-agent-aa129b307e9aa1778); not run.
Results: `bench/results/e2e/nullmod-review-k/20261018T222039Z.*`, `bench/results/e2e/nullmod-review-k4/`.

## (1) The invariant, checked against refs/mutter (src/backends/meta-a11y-manager.c, main d82671c)

- Every KeyboardMonitor method goes through `ensure_key_grabber()`, which creates the grabber with
  `modifiers == NULL`. `handle_set_key_grabs()` allocates both arrays **before** calling
  `rebuild_all_grabbed_modifiers()`, so a grabber created by SetKeyGrabs is never NULL at any rebuild.
- gorca's first manager-interface call per connection is SetKeyGrabs; nothing else is sent until it
  is acknowledged (`initial_grabs`, source.rs:1041). Retries on timeout are SetKeyGrabs too. If the
  name is lost meanwhile, `check_access` answers AccessDenied before the handler, so no grabber.
- A timed-out SetKeyGrabs that Mutter handles later is still the first handled call of that
  connection: safe. Dead-connection tails: messages of one sender reach Mutter before the bus's
  NameOwnerChanged for it, and GDBus dispatches method idles and signal idles in arrival order, so a
  late call cannot re-create a grabber after its vanish. Sound by reading; not proven by a test.
- `Peer.Ping` does not touch the skeleton (no grabber). **But see F-3: it proves nothing about
  Mutter's main loop.**
- Out of gorca's control: libatspi (Orca 48+, `refs/at-spi2-core/atspi/atspi-device-a11y-manager.c:567`)
  calls `WatchKeyboard` at init and `SetKeyGrabs` later from a timeout, so Orca itself opens the
  NULL window; Mutter's `--debug-control` "without access control" lets any client in. Also the
  gorca bench `stub_reader` (crates/gorca-bench/src/e2e/stub.rs:299) calls WatchKeyboard only: a
  NULL grabber if ever pointed at a real shell.
- The invariant is pinned by no automated test (only the manual private-shell rig).

## Findings (ranked)

### F-1 HIGH: a late GrabKeyboard leaves the whole keyboard grabbed after learn mode ends
source.rs:989-1005 (`send_grab_set`), 904 (`applied_all`). Grab/Ungrab is now sent only when
`applied_all != set.grab_all`, and `applied_all` is updated only on success. A Grab/UngrabKeyboard
that hit the 3 s deadline is still queued in Mutter and will be applied later (calls are not
cancelled). Scenario: learn mode on (gen N: SetKeyGrabs ok, GrabKeyboard stalls > 3 s) and then off
(gen N+1, queued in GrabWriter). At the timeout GrabWriter sends N+1: `applied_all` is still false
== `grab_all` false, so **no UngrabKeyboard is sent**; Mutter then applies the stale GrabKeyboard.
`grab_all` stays TRUE for the life of the connection and every key is swallowed (the user cannot
type in any application). The LM-3 HelpStall safety exit (daemon input.rs `handle_alert`) is exactly
this path. Symmetric case: a late UngrabKeyboard while re-entering learn mode leaves learn mode
without a grab (keys leak to apps).
Reproduced: in-order fake manager (GrabKeyboard blocks the main loop 4 s, like a Shell stall),
real `key_session`: `chord 1`, `all 1`, +0.5 s `all 0` -> fake log: GrabKeyboard applied at 7.36 s,
then SetKeyGrabs, final state `grab_all: true`; gorca logged one "SetKeyGrabs timed out" (it was the
GrabKeyboard) and nothing else. Main always sent Grab or Ungrab with every set, so the next set
healed it (and the old 2-retry rule reconnected, dropping the grabber).
Fix: make the applied state tri-state (`Unknown` after any Grab/Ungrab error or timeout, and
after the SetKeyGrabs half of a set whose Grab/Ungrab was not reached) and send the explicit call
whenever it is not known-equal; or simply send Grab/Ungrab with every set as main did (idempotent
booleans in Mutter, grabber already has modifiers). Add a unit test with a fake that applies late.
Also make the warning name the call that timed out.

### F-2 MEDIUM (gate failure): manager restart now takes >= 2 s to restore the keyboard
source.rs:39-40, 206-210. BACKOFF_MIN 1 s (main 100 ms), MAX 4 s (main 1 s), and the backoff
doubles unless the previous session lived > 30 s. In K4 the name-flap phase ends several sessions,
so the restart after it waits 2 s: K4.restore_ms 2002/1997/2012/2017 vs main 408, gate <= 2000
failed 3 of 4 runs. A real Shell restart after a few flaps leaves commands dead up to 4 s+.
The 1 s floor buys no safety on `ManagerGone`: the new Mutter process has no tail of the old
connection, and with SetKeyGrabs-first every gorca grabber is non-NULL anyway.
Fix: on `ManagerGone` (new owner seen) reconnect at once or after <= 100 ms; keep the 1 s floor only
for same-manager reconnects; reset the backoff when a session reached `watching`, not only after
30 s. Rerun K4 x3.

### F-3 MEDIUM (claim/rig validity): Peer.Ping is answered by the GDBus worker thread
source.rs:500-502, 1009-1034 and the KeyboardMonitor.md bullet claim the ping proves "its main loop
runs and has drained whatever a previous connection left queued". Measured with a GLib/GDBus
service whose main loop is blocked 4 s: `Peer.Ping` answered in 23 ms, a method call in 3478 ms.
GDBus handles org.freedesktop.DBus.Peer in the worker thread. Consequences: the ping (and its 20 s
budget) only gates when the whole process is stopped; the rig's stall modes use SIGSTOP (all
threads), so they model a stopped Shell, not the realistic main-thread stall, and partly pass
because of the ping. The mitigation does not depend on the ping (the SetKeyGrabs-first order does),
so this is a documentation and rig gap, not a crash path. Fix: correct the comment/doc; add a rig
mode that stalls only the main thread (or accept SIGSTOP with a note); optionally drop the ping.

### F-4 MEDIUM: the invariant has no automated guard
`tests/harness/fake_mutter.py` explicitly does not emulate the NULL dereference. Fix: make the fake
abort (or flag `null_walk` in `stats`) when a rebuild visits a grabber without modifiers, i.e. on
SetKeyGrabs or a modifier-holder vanish while another grabber has `modifiers is None`, and assert it
is 0 in K4/K4r. Note `set_latency` applies grab calls out of order, unlike Mutter (in order per
connection); F-1 only shows with in-order late application.

### F-5 LOW: a panicked grab task can leave Pending for ever (pre-existing)
source.rs:708-721. On a JoinError the writer drops `queued` and the machine gets
`GrabsTimeout{inflight}`; if a newer generation was queued, that timeout is stale, nothing is resent,
and the machine waits for a reply that will never come. Fix: resend the machine's current wanted
set (`ApplyModeGrabs`) instead of synthesising a timeout for the dead generation.

### F-6 LOW: initial-path inconsistencies
- The initial `GrabKeyboard` (source.rs:531-544) ends the session on a timeout ("slow is lost"
  there), unlike every other grab call. Harmless (the grabber dies with the connection) but
  inconsistent with the stated policy.
- `MonitorEffect::ArmRetry{at}` ignores `at` and `ArmSettle{after}` ignores `after` (pre-existing).

## (2) Latency / never-block
- All calls have deadlines (Ping 1 s, Startup 5 s, grabs 3 s); method_timeout 10 s is a backstop,
  compile-time asserted > GRAB_DEADLINE. Grab calls run in spawned tasks; the session loop keeps
  pumping keys (biased select) and the lane only reads `GrabView`. No blocking found.
- Keys in flight during a long Pending: chords that differ between committed and wanted are
  Ambiguous ("again") for as long as the Shell is slow; acceptable (a stalled Shell delivers no keys).
- Manager absent at startup: GetNameOwner fails, the session ends, backoff 1-4 s; speech and the
  reader are separate tasks and keep working. PING_BUDGET 20 s / INITIAL_GRABS_BUDGET 30 s hold
  only the key-source task.
- Unbounded same-generation retries: one SetKeyGrabs per 3 s per stall; bounded rate, fine.

## (3) State machine
Exhaustive matches, no unwrap outside tests, stale generations ignored for Timeout/Failed/Ok/Settle,
latest-wins coalescing via GrabWriter is correct (one in flight, newest queued; checked by trace:
GrabsTimeout{old} with a queued newer set sends the newer set, stale timeout resends nothing).
Shutdown: the session future is dropped on cancel (no graceful_shutdown then; harmless).

## (4) Interaction with worktree-agent-aa129b307e9aa1778 and main
- `git merge-tree`: the only conflict is `docs/decisions.md` (append at the same spot), both
  against main (27f155e5 already added a null-modifiers decision line: reconcile wording) and
  against aa129. aa129 does not touch gorca-input-mutter; its larger/partner grab sets just mean
  more SetKeyGrabs, which this branch handles.
- The stuck-modifier hint (daemon input.rs `stale_modifier`) reads only the lane; unaffected. Its
  LM-3 HelpStall sibling is the F-1 trigger.

## Probes (kept in the reviewer scratchpad; recipe)
In-order fake: Gio service owning org.freedesktop.a11y.Manager, GrabKeyboard handler `time.sleep(4)`
once; private `dbus-daemon --session --nofork --print-address=3`; drive
`target/debug/examples/key_session <addr>` with `chord 1`, `all 1`, `all 0` 0.5 s later; read the
fake's final `grab_all`. Ping probe: same fake, `gdbus call ... org.freedesktop.DBus.Peer.Ping`
during the stall.
