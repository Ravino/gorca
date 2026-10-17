# Browse-mode Ctrl+C copy: adversarial review (2026-10-17)

Branch `worktree-agent-aa24d6c83ee085224`, head `0c743205` (`git log main..HEAD`: 6b251106, 0c743205).
Reviewer: reviewer agent. No code changes made; one temporary probe test was added, run and deleted.

## Verdict: BLOCK (as a clipboard backend), core feature otherwise sound

The Reader side (command, grab gating, catalogue strings, `ClipText` redaction, regression test) is
good. The clipboard backend has a user-visible side effect that the private-shell measurement could
not see, and the feature speaks "copied" whether or not anything was copied.

## Findings (ranked)

### F-1 (high) Every copy lights the "screen is being shared" indicator for as long as our text is the clipboard
`crates/gorca-input-mutter/src/clipboard.rs:104-106` calls `Start` on a Mutter RemoteDesktop session
and keeps it alive until another application copies (lines 11-13, 239-248).

* `refs/mutter/src/backends/meta-remote-desktop-session.c:296-313`: `meta_remote_desktop_session_start`
  calls `init_remote_access_handle`, which `notify_new_handle`s a `MetaRemoteAccessHandle`
  (`is-recording` defaults to false, `meta-remote-access-controller.c:65,264`).
* `gnome-shell` 48.7 as installed (`gresource extract /usr/lib/gnome-shell/libshell-16.so
  /org/gnome/shell/ui/status/remoteAccess.js`, lines 144, 163-176): `ScreenSharingIndicator` adds
  every non-recording handle and shows the orange "screen shared" button ("Stop Screen Sharing") in
  the top bar while any handle is alive (minimum 5 s).
* Failure scenario: user copies a paragraph in Firefox; the top bar shows screen sharing until they
  copy something else, possibly for hours. A sighted colleague or the user's own privacy audit sees
  a false "your screen is shared" alarm; clicking it calls `handle.stop()` on our session (and on any
  real share) and silently empties the clipboard. The headless private shell used for the probe has
  no visible panel, so `docs/reports/ctrl-c-passthrough.md` ("no portal prompt") is true but misses this.
* No consent prompt, no notification, no headless requirement (the Mutter D-Bus API has no consent;
  the portal adds it). Coexists with gnome-remote-desktop and screencasts (separate sessions;
  selection ownership simply moves between them).
* Possible cheap mitigation, unverified on 48.7: in Mutter main, `EnableClipboard`, `SetSelection`,
  `SelectionWrite`, `SelectionWriteDone` check only `check_permission` (sender), not `started`
  (`meta-remote-desktop-session.c:1090-1200` delegate to `MetaClipboardSession`); `Start` is what
  creates the handle. If 48.7 serves the clipboard without `Start`, skipping `Start` removes the
  indicator. Must be measured on the private 48.7 shell (indicator visibility via the private shell's
  own AT-SPI tree), not assumed. Mutter main also has a dedicated `org.gnome.Mutter.Clipboard`
  interface (`refs/mutter/data/dbus-interfaces/org.gnome.Mutter.Clipboard.xml`), presumably post-48.

### F-2 (high) "copied to clipboard" is spoken before, and regardless of, the copy
`crates/gorca-core/src/browse/commands.rs:2483-2486` says `WebCopied` right after pushing the
effect. Silent-failure paths, all still speaking "copied":
* `runtime.rs` `exec_web` `try_send` fails (queue of 4 full while the owner is inside a transfer, or
  receiver dropped) -> one warn line;
* `wire.rs` `wire_clipboard` returns without spawning when `session_address` is `None`: the receiver
  is dropped, every copy is a warn line plus "copied";
* `clipboard.rs:247` `start` failure (RemoteDesktop absent, denied, 2 s deadline) -> warn only.
Fix: result message back to the Reader (ADR-0001 R3: request id + `ReaderMsg::CopyDone(Result)`),
speak "copied" on success and a catalogue failure string ("copy failed") on error; or keep the
optimistic "copied" only after `start` returned Ok within the deadline. `try_send` failure must also
produce the failure message.

### F-3 (medium, confirms coordinator scan) Timed-out pipe write is not cancelled; blocking thread and fd pinned
`clipboard.rs:144-151` wraps the `SelectionWrite` pipe in `tokio::fs::File`. Its writes run in
`spawn_mandatory_blocking` (`tokio-1.53.2/src/fs/file.rs:407`), the closure owns the std file, so
dropping the `timeout` future neither stops `write(2)` nor closes the fd. Probe
(`crates/gorca-input-mutter/tests/zz_review_pipe_probe.rs`, run then deleted): a 4 MiB write to an undrained pipe under a 200 ms `timeout` timed out as expected, yet a reader started after the timeout still received **2 097 152 bytes** (one 2 MiB `max_buf_size` chunk) before EOF: the blocking `write(2)` kept running and kept the fd open after the future was dropped. Confirmed.
Scenario: a hung pasting client stops reading; Mutter's splice stalls; our write of >64 KiB blocks a
blocking-pool thread until Mutter's own 15 s transfer timeout (main, `meta-selection.c`
`transfer_request_new`; unverified for 48.7) cancels and closes the read end. Repeated pastes pile up
threads; Mutter sees `SelectionWriteDone(false)` while bytes may still arrive. `shutdown_timeout(2 s)`
in `gorca-daemon/src/main.rs:58` bounds exit, so no hang at quit.
Fix: `tokio::net::unix::pipe::Sender::from_owned_fd` (sets O_NONBLOCK) or `AsyncFd`, so dropping the
future closes the fd; at most one in-flight transfer per session (they are serial today, keep it so).

### F-4 (medium) Grab lags the selection by up to 150 ms in both directions
`commands.rs:2452-2461` pushes a new grab set only on settle (`SCROLL_DEBOUNCE`) or focus jump.
* Select-then-copy fast (Shift+End then Ctrl+C within 150 ms, or releasing an auto-repeating
  Shift+Down and pressing Ctrl+C): Ctrl+C is not grabbed yet, Firefox copies its own (empty or stale)
  selection, gorca says nothing. Silent wrong clipboard.
* Move-then-copy fast: Ctrl+C still grabbed after the selection dropped; gorca says "nothing
  selected" and the page never sees the key. Benign but swallows a user key.
Fix: push `SetGrabSet` immediately on a `sel` None<->Some transition (rare event, one push), keep
the settle sync for Tab. Grab never sticks across mode switch or reader restart: Focus/Review sets do
not contain `quick_nav`, and Mutter drops KeyboardMonitor grabs when the owner leaves the bus.

### F-5 (low) Signal match rule has no sender
`clipboard.rs:85-90`: any session-bus peer can emit `Closed`/`SelectionOwnerChanged` on our path and
end the session (clipboard DoS), or a fake `SelectionTransfer` (Mutter rejects the serial; we spend
up to 4 s of calls). Add `.sender(RD_NAME)` (zbus resolves the unique name) or compare
`header.sender()` with Mutter's unique name.

### F-6 (low) Select-all range no longer matches the cursor
`commands.rs:2324` stores `(0, last+1)` while the cursor sits at `last`; the next Shift+Left keeps
anchor 0 and sets end = cursor-1, dropping two characters; `web_native_selection(doc, 0, last)`
(line 2333) and the native toggle (`(a, e)` = `(0, last+1)`, line 2526) now disagree by one.
Selections built by Shift+arrows are `(anchor, cursor)`, which excludes the character under the
cursor, so "half open like every other range" holds by accident there. Pick one convention and test
Shift+Left after Ctrl+A.

### F-7 (low) Password text
The AT-SPI fetch exposes password entry text on purpose (`gorca-connector-atspi/src/fetch/strategy.rs:283`).
Firefox exposes masked bullets, so copy yields bullets, but nothing in `web_copy` excludes
`Role::PasswordEntry` nodes. Defence in depth: skip their text in the copied range.

## Checked and fine
* No `unwrap`/`expect` in non-test code; `clip_probe` example uses `unwrap_or`. All Mutter calls via
  `call_method` with 2 s deadlines; connect with the Startup budget. The Reader never awaits.
* Copied text is never logged: `ClipText` `Debug` prints the byte count; clipboard.rs logs errors and
  the session path only; `ClipError::Pipe` carries the io error, not the text.
* Strings in the catalogue (en, ru) for the command help and both messages.
* Session is stopped on newer copy, on `SelectionOwnerChanged(false)`, on `Closed`, on shutdown; on
  reader crash Mutter closes it when the peer vanishes. It is persistent while we own the clipboard
  (inherent to clipboard ownership), not short-lived as the brief described.
* Regression test for Ctrl+C/X/V/Z/Y/Insert/Delete never grabbed and Ctrl never a gorca modifier is
  meaningful (both presets, layouts, modes, Cyrillic keysyms).

## Better paths (recommendation)
1. **Preferred: mirror the virtual selection into Firefox and let the page copy.** Firefox
   `HyperTextAccessibleBase::SetSelectionBoundsAt` (`refs/firefox/accessible/basetypes/HyperTextAccessibleBase.cpp:845`)
   builds a `TextLeafRange` from two hypertext offsets that may lie in different descendants, so
   one `Text.SetSelection` on the document (or nearest common hypertext ancestor) selects across
   nodes; then Ctrl+C is never grabbed and Firefox copies text and HTML itself. gorca's current native
   selection fails only because it calls per text node. Needs an offset map from flat offsets to the
   ancestor's hypertext offsets (embedded-object characters). Chromium parity to be checked.
2. `org.gorca.ShellHelper` + `St.Clipboard.set_text` (planned K1): no indicator, no session, needs
   the extension.
3. RemoteDesktop without `Start` if 48.7 permits (F-1) as a stop-gap.
4. Rejected: xdg-desktop-portal Clipboard (needs a RemoteDesktop portal session: consent dialog and
   the same indicator); wlr/ext data-control (absent in Mutter 48).

## Gates
CARGO_BUILD_JOBS=4, RUST_TEST_THREADS=4, branch worktree target.
* `cargo fmt --all -- --check`: clean.
* `cargo clippy --workspace --all-targets -- -D warnings`: clean.
* `cargo test --workspace`: all `test result: ok`, 0 failed.
* `cargo run -p xtask -- strings`: ok (318 literals, 102 allowed).
* Not run: Firefox end-to-end, private 48.7 shell indicator check (F-1), no-`Start` variant.
