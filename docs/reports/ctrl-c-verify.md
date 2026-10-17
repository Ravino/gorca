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

---

# Round 2: selection mirror rework (head 435d8a13)

## Verdict: MERGE AFTER FIXES (one must-fix, measured)

RemoteDesktop/clipboard code is gone (`clipboard.rs`, `clip_probe`, `browse.copy`, catalogue
strings, `Copy` effect, daemon wiring, extra deps; remaining `RemoteDesktop` mentions are the
pre-existing test rigs and `gorca-input-inject` docs). Ctrl+C is never grabbed. F-1..F-4 and F-6/F-7
are resolved by design or fixed.

## R2-1 (high, must-fix, reproduced) Clearing the page selection moves focus to the first link of the page
`gorca-doc-source-atspi/src/source.rs` `clear_selection` calls `Text.SetSelection(0, 0, 0)` on the
document. Firefox resolves offset 0 to the first text leaf and, in `TextLeafRange::SetSelection`
(`refs/firefox/accessible/base/TextLeafRange.cpp:2430-2465`, `aSetFocus` defaults to true), calls
`TakeFocus()` on that leaf's parent when it is focusable. On any page that starts with a skip link
("Skip to content", very common) every plain browse move after a selection focuses the skip link;
gorca follows the focus.

Measured in a private headless shell (rig copy in
`/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/selprobe`, page = skip
link, h1, two paragraphs, button, paragraph; Firefox; debug gorca at 435d8a13):
* with selection: `Shift+End` on p2 -> "Echo foxtrot golf hotel., selected"; `Down` -> "button, Press
  me" and 160 ms later **"Skip to content, link"** (focus jumped to the top of the page);
* control, same keys with plain `End`: "button, Press me", nothing after.

Fix: clear with `Text.RemoveSelection(i)` for every range, i = n-1..0, on the document
(`HyperTextAccessible::RemoveFromSelection`, `generic/HyperTextAccessible.cpp:667`, removes the DOM
range and never focuses); keep `SetSelection(0,0,0)` out of the clear path. Add a skip link at the
top of `tests/web/selcopy/page.html` and assert no focus change after select + plain move.

## R2-2 (medium) A selection that starts in text directly under the document focuses the document
`web_mirror_selection` exempts `start_container == root` from the focusable guard, but the document
accessible is focusable, so `SetSelection` there calls `DocAccessible::TakeFocus` and blurs whatever
element has focus (page blur/focusout handlers run). Rare (bare text in `<body>`), unmeasured. Fix:
use the snap/first span on a non-focusable inner container, or skip mirroring when focus is on an
element and the start container is the document.

## R2-3 (medium, design) On by default fires page `selectionchange` on every selecting burst
Every debounced selection (40 ms after the last Shift+key) is a real DOM selection: page
`selectionchange`/`mouseup`-less selection handlers run (share-selection popups, translation
bubbles, editors that track selection). No scroll: `AddRangeAndSelectFramesAndNotifyListeners`
does not scroll, only `TakeFocus` would. NVDA ships native selection mode off by default; gorca
turning it on by default is a product call. Keep on (Ctrl+C just works) but document the toggle,
and record the decision in `docs/decisions.md`.

## R2-4 (low) The user's own mouse selection is cleared by a later browse move
`native_sel_node` stays `Some` after the mirror; if the user then selects with the mouse, the next
plain browse move clears it. Acceptable; note it.

## Key path and never-block
* Reader cost per key: one `Option<Instant>` write and an `ArmTimer`; the plain-move clear is one
  `WebEffect::Act` only when a mirror was set (`web_native_clear` takes `native_sel_node`). Nothing
  awaited by the Reader.
* Source: `GetNSelections`, up to 63 `RemoveSelection`, one call per span (spans bounded by tree
  depth), all under the one action deadline; no `unwrap` (`unwrap_or` on parse).
* Mirror runs only in browse mode; a timer that fires after a switch to focus mode returns early,
  so the focus-mode caret is untouched.

## Chromium Ctrl+A before an `<input>` (7/9)
Acceptable as a known issue: Blink keeps one exclusive range and the input boundary cuts it; it fails
safe (less text, never focus moves). Track it in the report's open list.

## Gates (round 2)
4 threads. `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -D warnings` clean; `cargo test --workspace` 2182 passed, 0 failed; `xtask strings` ok (318 literals, 102 allowed). Leftover grep for RemoteDesktop/SelectionWrite/EnableClipboard in core, daemon, input-mutter, doc-source-atspi: none.

---

# Round 3 (head ad3d4044)

## Verdict: MERGE AFTER FIXES (one must-fix, measured)

R2-1 (RemoveSelection clear) and R2-2 (focused-container guard) are fixed; rows F1/F2 hold. Results
come back to the Reader with stale requests ignored by `req`; the Reader never awaits; the source
runs the calls in order under one action deadline.

## R3-1 (high, must-fix, reproduced in Firefox) A later span inside a link still moves focus
The focus guard in `web_mirror_selection` checks only `plan.start_container`. On Firefox the
source sends one `SetSelection`/`AddSelection` per span (`source.rs` `select`), and each call
reaches `TextLeafRange::SetSelection(.., aSetFocus = true)`, which focuses the parent of *that*
span's first leaf when it is focusable (`refs/firefox/accessible/base/TextLeafRange.cpp:2458-2465`).
A selection that starts outside a link and grows into it therefore focuses the link.

Measured (private shell, debug gorca at ad3d4044, rig in
`/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/selprobe`, page
`<ul><li><a>First link text here</a> after one</li>...`): from the list item start, Shift+Right x6.
Firefox speaks `'F, selected'` and then, 63 ms later, **`'First link text here, link'`** (a focus
event, the same signature as the round-2 skip-link bug). Chromium, same keys: no focus utterance.
Fix: apply the guard to the first leaf of every span (not only the first), or on Gecko use one
`SetSelection` on the document with `doc_range` (only the start leaf's parent can take focus), and
fall back to "page selection not set" when a span's container is focusable and unfocused. Add a
`selcopy` row: select from plain text into a link, focus must not change.

## Spoken notices: no noise measured
Same probe, both browsers. Plain text (6 x Shift+Right), Shift+End plus Shift+Down x2 across blocks,
selecting into a link, and Ctrl+Shift+Right x4 across inline formatting gave no "page selection not
set" and no "incomplete" utterances. Only "selected" echoes were spoken. (The guard did not refuse in
these rows, so the per-burst rate of the not-set notice was not exercised. ADR-0021 asks for "once
per burst". Today it is once per mirror, so a selection that starts inside an unfocused link says
"page selection not set" on every Shift+arrow press at typing speed. Low; latch it per selection.)

## Other
* Low: `spawn_action` sends each request from its own task. Two selection requests (or a select and a
  clear) less than one scheduling quantum apart could reach the source out of order and leave a
  stale page selection. The 40 ms debounce makes this unlikely. ADR-0021 I-3 (one mirror in flight
  per document) would remove it.
* Chromium `Document.SetTextSelections`: Blink `AXSelection::Select`
  (`refs/chromium/third_party/blink/renderer/modules/accessibility/ax_selection.cc:396-448`) focuses
  only an atomic text field that holds the selection, and sets the selection with `SetSelectionBy::kUser`
  (fires `selectstart`/`selectionchange`). `FrameSelection` is not in refs, so scroll-on-select is
  unverified. Measured: no focus change in the probe or in F1/F2.
* Chromium Ctrl+A before an `<input>`: acceptable as a known issue (fails safe). The implementer
  reports SetTextSelections fixes it.
* `git merge-tree --write-tree main HEAD`: one conflict, `docs/decisions.md` (append-only lines),
  trivial. commands.rs and tests.rs auto-merge. Main now carries ADR-0021 (proposed). The branch
  follows its S1 model except I-3 and the once-per-burst announcement.

## Gates (round 3, ad3d4044, full)
4 threads, memory checked. `cargo fmt --check` clean. `cargo clippy --workspace --all-targets -D
warnings` clean. `cargo test --workspace` 2191 passed, 0 failed. `xtask strings` ok (318 literals,
102 allowed). Incremental cleared afterwards.
