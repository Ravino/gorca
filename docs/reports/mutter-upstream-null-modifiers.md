# DRAFT upstream Mutter issue (not filed)

Target: https://gitlab.gnome.org/GNOME/mutter/-/issues (component: a11y / `meta-a11y-manager.c`).
Filing is up to the owner. Verified against Debian libmutter-16-0 48.7-0+deb13u1 and `refs/mutter`
main (d82671c, 2026-10-07). Background: `docs/reports/shell-segfault-20261019.md`.

---

**Title:** gnome-shell crashes (NULL deref) in `rebuild_all_grabbed_modifiers()` when a key grabber exists that never called `SetKeyGrabs()`

**Summary**

`org.freedesktop.a11y.KeyboardMonitor` clients are tracked as `MetaA11yKeyGrabber`. Every method
(`WatchKeyboard`, `UnwatchKeyboard`, `GrabKeyboard`, `UngrabKeyboard`, `SetKeyGrabs`) goes through
`ensure_key_grabber()`, which allocates the grabber with `modifiers == NULL` and `keystrokes == NULL`.
Only `handle_set_key_grabs()` allocates the arrays. `rebuild_all_grabbed_modifiers()` walks every
grabber and dereferences `grabber->modifiers->len` without a NULL check, so the compositor is
killed (SIGSEGV, fault address 0x8) as soon as it runs while any grabber is still in the
"watch/grab only" state. Other code in the same file already treats the arrays as optional
(`should_grab_keypress()` checks `grabber->keystrokes`, `key_grabber_bus_name_vanished_callback()`
checks `grabber->modifiers`), so this function is the inconsistent one.

`rebuild_all_grabbed_modifiers()` is called from
- `handle_set_key_grabs()` (from any client), and
- `key_grabber_bus_name_vanished_callback()` when the vanishing grabber had modifiers.

**Stack (from debuginfod)**

```
rebuild_all_grabbed_modifiers+45, ../src/backends/meta-a11y-manager.c:122   (48.7; line 148 on main)
  mov 0x28(%rbp),%rax ; mov 0x8(%rax),%edx     ; grabber->modifiers->len with modifiers == NULL
kernel: gnome-shell[...]: segfault at 8 ip ... error 4 in libmutter-16.so.0.0.0
```

**Who can trigger it**

Only the owner of `org.gnome.Orca.KeyboardMonitor` is authorised, but a legitimate screen reader
that reconnects is enough: the new connection's `WatchKeyboard` creates a NULL-modifier grabber;
a second connection's `SetKeyGrabs`, or the disconnect of a connection that had modifiers, then
walks it. Calls queued from a dead connection can also re-create a NULL grabber for the dead name
before its vanish callback runs. We hit it live after a long shell stall made the client drop and
re-open its connection.

**Reproduction** (private session, no other software needed; `repro.py` below uses GLib/Gio only)

1. Run a nested/headless gnome-shell with `--unsafe-mode` on a private bus, or any session, as
   the owner of `org.gnome.Orca.KeyboardMonitor` (the script's connections must own it in turn;
   the test rig requests it for A and hands it to B, see the notes in `repro.py`).
2. Connection A: `WatchKeyboard()`. Connection B: `SetKeyGrabs([], [])`.

Minimal sequence (mode `setg`): A calls `WatchKeyboard` only; B calls `SetKeyGrabs` with empty
arrays -> `rebuild_all_grabbed_modifiers()` visits A's grabber -> SIGSEGV.
Mode `det`: A `WatchKeyboard`+`SetKeyGrabs`, B `WatchKeyboard`, A disconnects -> vanish callback
rebuilds and visits B's grabber -> SIGSEGV.
Mode `stall2` (what happens in the wild): the shell is stopped for ~1 s while A has many queued
`UngrabKeyboard`/`SetKeyGrabs` calls and exits and B connects with `WatchKeyboard`+`SetKeyGrabs`;
crashed 2/3 runs.

(Attach `tools/platform-probes/shell-segfault/repro.py`. Results in our rig: det 1/1, setg 1/1,
stall2 2/3 crashes.)

**Proposed fix (one line)**

```diff
--- a/src/backends/meta-a11y-manager.c
+++ b/src/backends/meta-a11y-manager.c
@@ rebuild_all_grabbed_modifiers
       if (grabber == ignored_grabber)
         continue;

+      if (!grabber->modifiers)
+        continue;
+
       for (i = 0; i < grabber->modifiers->len; i++)
```

An equivalent alternative is to allocate both arrays in `ensure_key_grabber()` so that no grabber
can have NULL arrays; the existing `if (grabber->modifiers)` / `if (grabber->keystrokes)` checks
then become redundant but harmless.

**Why it matters**

A bug in an authorised accessibility client's call sequence (or just a reconnect after a slow
shell) takes down the whole session. The compositor should not be killable by the order of
D-Bus calls.

**Client-side workaround (for reference)**

Never leave a grabber without modifiers: call `SetKeyGrabs()` (an empty set is fine) before any
`WatchKeyboard()`/`GrabKeyboard()`/`UngrabKeyboard()` on a new connection, do not overlap
connections, and do not re-connect before the compositor answers `Peer.Ping`. gorca implements
this in `crates/gorca-input-mutter`.
