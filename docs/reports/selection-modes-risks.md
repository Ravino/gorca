# Selection modes (gorca+Shift+F10): adversarial risk review

Date: 2026-10-17. Reviewer: reviewer agent. Read-only; no code changed.
Subject: the owner's proposal to make gorca+Shift+F10 switch between
(A) **page selection**: the virtual browse selection is mirrored into the DOM selection with AT-SPI
`Text.SetSelection`, and the browser's own Ctrl+C copies it; and
(B) **virtual selection**: the selection lives only in gorca, and Ctrl+C copies gorca's rendered text
(NVDA browse-mode default).

Code examined: branch `worktree-agent-aa24d6c83ee085224` at `435d8a13` plus the uncommitted round-3
edits in its worktree (the worktree was being edited while this review ran; line numbers are as read).
References: `refs/nvda/source/{cursorManager.py,browseMode.py,virtualBuffers/gecko_ia2.py,api.py}`,
`refs/nvda/user_docs/en/userGuide.md`, `refs/orca/src/orca/{clipboard.py,scripts/web/script.py}`,
`refs/mutter/src/{backends/meta-a11y-manager.c,wayland/meta-wayland-data-device.c}`, installed
`libmutter-16.so.0.0.0` (48.7).

## 0. What the references actually do

* **NVDA has exactly this switch.** `script_toggleNativeAppSelectionMode` (`browseMode.py:2951-2987`,
  NVDA+Shift+F10). It is **per document** (an attribute of the tree interceptor), initialised from the
  global setting `virtualBuffers.nativeSelectionMode` (feature flag, **default disabled**,
  `config/configSpec.py:231`). Turning it on **probes** the document first (`clearAppSelection()` +
  `updateAppSelection()`) and says "Native selection mode unsupported in this document" when either
  fails; it never claims a mode it cannot deliver.
* NVDA Ctrl+C in browse mode (`cursorManager.py:679-688`): native mode re-sends the gesture
  (`gesture.send()`); virtual mode copies `textInfo.clipboardText`, says "No selection" when collapsed,
  and `api.copyToClip` (`api.py:399-424`) **reads the clipboard back and reports success or failure**.
  Ctrl+C is therefore always consumed by NVDA in browse mode (a Windows keyboard hook; no grab lag).
* NVDA mirrors on every selection change only in browse mode (`gecko_ia2.py:47-55`: collapsed ->
  `clearAppSelection`, else `updateAppSelection`), with one multi-object call
  (`IAccessibleTextSelectionContainer::SetSelections`). The user guide warns the native selection
  "may not always be completely accurate" and that NVDA-generated labels are not copied.
* **Orca has no virtual selection at all.** Its web caret navigation moves the real caret, Shift+arrows
  make a real selection, and Ctrl+C is never intercepted: Orca is permanently "mode A". Orca writes the
  clipboard (flat review copy) through GPaste/Klipper D-Bus or a `Gtk.Clipboard` fallback
  (`clipboard.py:106-158, 470-497`); the fallback cannot work for a surface-less client on Wayland
  (see 2.1).

## 1. Mode-switch failure modes for a blind user (ranked)

### S-1 (high) The real danger is a silent stale clipboard, and mode A produces it today
`commands.rs` `web_mirror_selection`: a selection whose first leaf sits in a focusable container that
is not focused (a link, a button label, a contenteditable root, a summary) is **not mirrored**; the
page selection is cleared instead (BM-2 guard), except for select-all. `doc_actor/mod.rs`
`spawn_action`: the `SetSelection` result is discarded (only `Focus` failures are reported), and
over `MAX_ACTIONS = 8` the action is silently dropped. Nothing tells the Reader the mirror failed.
Scenario: the user selects a list of links ("Shift+Down" from a link line), hears "..., selected",
presses Ctrl+C. Firefox has a collapsed selection and copies nothing; the clipboard still holds what
was copied an hour ago; the user pastes the wrong text into a mail. A sighted user sees the
highlight missing; a blind user has no signal.
Fix (needed whatever the switch decision): the mirror result must come back to the Reader
(ADR-0001 R3, request id + `Mirrored(Result)`), and Ctrl+C (observed, see 2.2) must be answered:
"copied" / "nothing selected" / a fallback (S-5). Without this, mode A is not safe as a default.

### S-2 (high) Mode-dependent Ctrl+C semantics are invisible at the moment of use
A copies rich HTML with the page's visible text (links, table structure, no gorca labels, page
`copy` handlers may rewrite it, e.g. "Read more at ..." appenders); B copies plain text exactly as
gorca rendered it. The user cannot hear which happened. Forgetting the mode is guaranteed over weeks
because the toggle is rare. Mitigations, in order of value:
1. Speak the outcome on every browse-mode Ctrl+C ("copied", "copied as page selection" only when it
   differs from the user's expectation, "nothing selected", "copy failed") - NVDA's read-back model.
2. Persist the mode in config (today `BrowseState::native_selection` is a runtime field, reset to
   `true` on every start and not in any settings file: `browse/mod.rs:569,617`).
3. No per-site default in the first version; if profiles (ADR-0016 site triggers) later set it,
   announce the mode on document entry when it differs from the global value.

### S-3 (medium) Default contradicts the recorded decision and its own doc comment
`docs/decisions.md` (2026-10-16, M8 review round 1): "virtual selection by default, native selection
only behind gorca+Shift+F10". `browse/mod.rs:567-569` doc comment: "Off by default". Code:
`native_selection: true` (`mod.rs:617`), help text "it follows by default". NVDA defaults off. The
round-2 R2-3 item (page `selectionchange` on every select burst) is still open. The default must be
decided and recorded, then code, comment and catalogue aligned.

### S-4 (medium) The toggle says "on" without probing support
`web_native_selection_toggle` flips the flag and speaks `WebNativeSelectionOn` for any web document,
including a connector that answers `NotSupported` (Newton has no `SetSelection` path; Chromium older
than the measured version). NVDA probes first. Fix: on "on", run clear+mirror and speak "on" only on
success, else "not supported in this document" and stay off for that document.

### S-5 (design) A two-state user switch is weaker than A-with-B-fallback
Every case where A cannot mirror (S-1 guard, password field truncation, Chromium single-range
rounding, iframe boundaries, connector without selection) is exactly where B works, because B only
needs gorca's own text. A switch forces the user to know which of the two will work on this page.
Preferred product shape: one setting `browse.copy = "page" | "text"`, where "page" means "page
selection when the mirror is confirmed, otherwise gorca text, and say which"; gorca+Shift+F10 toggles
the setting (NVDA's gesture, NVDA's meaning). This needs mode B's clipboard path anyway, so B is not
optional if A is to be safe.

### S-6 (low) Switching with a selection present; focus/browse transitions; tabs
* A->B: `web_native_clear` removes the page ranges, virtual `sel` kept: correct (matches NVDA's
  collapse). B->A: mirrors at once: correct.
* Entering focus mode with an A mirror present leaves the page selection in place; the browser owns
  Ctrl+C there, so the next Ctrl+C in focus mode copies the old browse selection if focus landed
  outside an editor. Acceptable (NVDA syncs on pass-through too) but must be tested, and B must
  never act on Ctrl+C in focus mode.
* `native_sel_node` remembers `(doc, root)`, so a later clear goes to the right tab. The flag is
  global, while NVDA's is per document: with a per-document probe (S-4) the state becomes
  "global preference x per-document capability", which must be spoken on the toggle.
* `b.sel` is flat offsets and is cleared only by `web_set_cursor` and set by selection commands
  (`commands.rs:2150,2256,2316`); nothing invalidates it when the document mutates under it
  (live tickers, chat, SPA re-render). In A the DOM range tracks mutation after the mirror; in B a
  Ctrl+C after a mutation copies shifted text the user never heard. B needs `sel` anchored to
  `(node, local)` endpoints like `WebCursor`, or dropped on a mutation inside the range. Not
  measured; inferred from the code.
* gorca+Ctrl+C is "save settings" (decisions.md 2026-10-13). With Caps as the gorca modifier, a
  stuck or early Caps turns a copy into a settings save. Low, but the confirmation in S-2.1 exposes it.

## 2. Mode B: getting Ctrl+C and writing the clipboard on GNOME 48 Wayland

### 2.1 Clipboard writers (verified facts)
| Mechanism | Verdict | Evidence |
|---|---|---|
| Plain Wayland `wl_data_device.set_selection` (GTK clipboard from gorca) | **Impossible**: Mutter ignores it unless the client holds keyboard focus; gorca has no surface | `meta-wayland-data-device.c:1147` (`client != data_device->focus_client` -> return) |
| wlr/ext data-control | **Absent** in Mutter (none in refs main) | grep of `refs/mutter` |
| `org.gnome.Mutter.Clipboard` | **Absent in 48.7** (main only) | `strings libmutter-16.so.0.0.0`: only `RemoteDesktop`, `RemoteDesktop.Session` |
| Mutter RemoteDesktop session clipboard | **Rejected** (r1 F-1): `Start` creates a remote-access handle and the shell shows "Stop Screen Sharing" for as long as gorca owns the clipboard; no-`Start` variant unmeasured on 48.7 | `ctrl-c-verify.md` F-1 |
| xdg-desktop-portal Clipboard | **Rejected**: needs a RemoteDesktop portal session (consent dialog + same indicator) | `ctrl-c-verify.md` |
| GPaste/Klipper D-Bus (Orca's path) | Only when the user runs one; GNOME ships none | `refs/orca/src/orca/clipboard.py` |
| **`org.gorca.ShellHelper` + `St.Clipboard.set_text`** | **Recommended**: compositor-side memory source, no focus requirement, no indicator, survives gorca exit; the helper already has pidfd+exe+cgroup allow-listing and rate limits | `docs/platform/org.gorca.ShellHelper.md`, `contrib/gnome-shell-extension/gorca-helper@gorca/` |

ShellHelper conditions: write-only method (`SetClipboardText(s)`), no read method on the same allow
list without a separate decision; size cap (e.g. 1 MiB, truncation spoken); deadline on the call;
the client checks that the helper's owner is `org.gnome.Shell`'s owner (existing client rule); the
extension must be installed and enabled, so B (and A's fallback) degrades to "copy unavailable,
helper not running" when it is absent. Unmeasured: `St.Clipboard` behaviour while the session is
locked (refuse, like `CaptureArea`).

### 2.2 Getting Ctrl+C: options ranked by safety
1. **Observe, never grab (recommended).** `WatchKeyboard` delivers every key, grabbed or not
   (`refs/mutter/data/dbus-interfaces/org.freedesktop.a11y.xml` WatchKeyboard; core already sees
   `TextKey::Clipboard` in `text/caret.rs`). In B (or A's fallback) the page selection is collapsed,
   so the browser's own Ctrl+C copies nothing, and gorca writes its text after it. No key is ever
   swallowed, no grab lag exists, the r1 150 ms race (F-4) cannot recur.
   Must be measured before relying on it: (a) Firefox and Chromium leave the clipboard untouched on
   Ctrl+C with a collapsed, non-editable selection; (b) a page `copy`/`keydown` handler that writes
   the clipboard with a collapsed selection (rare; it would race gorca's write - gorca's D-Bus write
   arrives after the key and normally wins, but that is not guaranteed); (c) the page sees Ctrl+C in
   browse mode (NVDA hides it). Order guarantee for (b): delay gorca's write until key release or a
   few ms after the browser's `wl_data_device` traffic cannot be observed, so treat (b) as a known
   limitation, not a blocker.
2. **Grab Ctrl+C for the whole browse mode (NVDA parity).** Deterministic, the page never sees it,
   but any wrong browse/focus classification (an editor gorca thinks is browse) swallows the user's
   copy, which is the bug the user already reported once (decisions.md 2026-10-17). A grabbed key
   cannot be passed on (no injection, decisions.md M8 D-2 note). Only with option 1 as fallback.
3. **Grab only while a virtual selection exists.** Worst of both: grab-set lag on every
   None<->Some transition (r1 F-4: silent wrong clipboard one way, swallowed key the other), plus
   the same misclassification risk. Reject.
Note on KeyboardMonitor: a grab applies whenever Mutter's focus is anywhere, so any grab must be
tied to "browser window focused and browse mode", the same coupling the quick-nav letters already
need; the observe option has no such coupling.

### 2.3 Privacy in B (and in A)
* Password fields: A stops the mirror before a `PasswordEntry` (`selmirror.rs:143`); the AT-SPI
  connector deliberately fetches password text (`gorca-connector-atspi/src/fetch/strategy.rs:283`),
  so B's text renderer must exclude `PasswordEntry` subtrees and form-field values whose
  `PROTECTED`/password state is set, regardless of what the app exposes. Firefox exposes bullets,
  but GTK/other connectors may not. Test with a fake app that exposes the real secret.
* Hidden text: A skips `HIDDEN` leaves; B must use the same filter so B never copies text that is
  in the a11y tree but `aria-hidden`/collapsed. Visually-hidden (`sr-only`) text is copied by both;
  acceptable, it is text the user heard.
* "Text the user did not hear": inherent to both (select-all, Shift+Page Down); NVDA same. Speak
  the copied length or first words in the confirmation so the user can catch an over-copy.
* Over-copy specific to A: on Blink and on Gecko's single-range fallback the `snap` range "rounds
  nested ends out to whole children" (`selmirror.rs` module doc), so A can copy more than the
  virtual selection. B copies exactly the range. Report this in the confirmation or prefer B
  fallback when `snap` was used.
* Clipboard managers (GPaste) record everything in both modes; out of gorca's control.

## 3. Mode A residual risks after the round-2 fixes

| # | Sev | Risk | Status / evidence |
|---|---|---|---|
| A-1 | high | Silent non-mirror -> stale clipboard (S-1) | open, by design of the BM-2 guard |
| A-2 | medium | Mirror result ignored, action dropped over 8 in flight (`doc_actor/mod.rs:541,623-643`) | open |
| A-3 | medium | Gecko engine behaviour encoded in core: the "Firefox focuses the start container" guard lives in `browse/commands.rs`, contradicting "no Gecko rule left in core" (decisions.md M8 r1). The source should own it (refuse with a reason, or expose a `selection_takes_focus` quirk next to `selection_single_exclusive`) | open, extensibility |
| A-4 | medium | Page scripts: every debounced mirror fires `selectionchange` (share/translate popups, editors, analytics); selection-tracking pages may move focus or open overlays the user cannot see (R2-3) | open, design |
| A-5 | medium | Chromium: one exclusive range; Ctrl+A before an `<input>` truncates (known 7/9); `snap` over-copies; NVDA supports Chromium only from 134 | known |
| A-6 | medium | Source busy-time: each mirror is `GetNSelections` + up to 64 `RemoveSelection` + one call per span, serial in the document source's request loop (`source.rs:1263-1290`), so subtree/text fetches for navigation queue behind it on a slow app. Not measured; the 40 ms debounce bounds the rate, not the per-mirror cost | unmeasured |
| A-7 | low | Iframes: a DOM selection cannot cross documents; spans across a frame boundary either fail (falls to `snap` on the common ancestor, which is the outer document) or drop the frame's text. Not tested | unmeasured |
| A-8 | low | contenteditable / editable roots are focusable, so the guard refuses (fails safe, but S-1 silence). Shadow DOM: Firefox's a11y tree is flattened, DOM ranges across shadow roots may clamp; untested | unmeasured |
| A-9 | low | Mouse selection is cleared by the next browse move (R2-4); a focus-mode selection survives into browse mode and is overwritten silently by the next mirror | accepted |
| A-10 | low | Ordering: clear and set are separate spawned tasks racing for the source channel; only a fast double toggle can reorder them. Theoretical | unmeasured |
| A-11 | low | Password field in the middle of a range truncates the copy silently (`selmirror.rs:143-146` breaks) | open, speak it |

Fixed and confirmed in code: R2-1 clear uses `RemoveSelection` only (`source.rs` `clear_selection`);
R2-2 now refuses a focusable unfocused start container except the document root and lets select-all
skip leading focusable text (uncommitted round-3 edit in the worktree; not re-measured by me).

## 4. Core integrity

* **Backend-agnostic core:** both modes fit if (a) the copy text is produced in core from `WebDoc`
  (engine-neutral) and handed to a `ClipboardSink` port implemented outside core (ShellHelper client
  crate), never a ShellHelper or zbus type in core; (b) engine selection quirks stay in the source
  (A-3 must move). The `SelectionSpec`/`SelSpan` model is already neutral.
* **Never block:** B's copy must not build megabytes of text on the Reader thread at Ctrl+C time
  (select-all on a 100k-node page). Snapshot the range from the read handle, build in a task, send
  the result as a message, speak on completion; deadline the ShellHelper call; cap the size. A's
  mirror is already an effect; its result must come back as a message (A-2).
* **Extension boundary:** per-site defaults belong to profiles (ADR-0016), not to the Firefox
  extension; the copy renderer must not call extension hooks synchronously.
* **Key path:** the observe option adds zero grab-set churn; the grab options add a `SetGrabSet`
  push on mode/selection transitions and re-open r1 F-4.

### Tests that must exist before either mode ships
1. **No focus change** (e2e, Firefox and Chromium): page with a leading skip link, a focused button
   and a link-initial paragraph; select from inside a link, across a link, select-all, clear; assert
   no `focus` event and no speech after the selection speech (extend `tests/web/selcopy`).
2. **No Ctrl+C swallow:** unit over `current_grabs()` for every mode x selection state x setting:
   Ctrl+C, Ctrl+Insert, Ctrl+X never in the grab set (existing regression test, extend to the new
   setting); e2e: Ctrl+C in a focus-mode text field copies the field's selection in both settings.
3. **Clipboard content equality:** after each copy, read the clipboard back in the private session
   (wl-paste against the private compositor, or the helper in a test-only read mode) and compare:
   B equals gorca's rendered range exactly; A equals the browser's text for the page range; A's
   fallback equals B. Include a stale-clipboard case: previous content must never survive a copy
   that gorca said succeeded.
4. **Failure is spoken:** helper absent, helper refusing, mirror refused, connector `NotSupported`
   -> catalogue failure string, never "copied".
5. **Privacy:** fake AT-SPI app exposing a real password in a `PasswordEntry` and an `aria-hidden`
   subtree inside the selected range: neither appears in B's clipboard.
6. **Mode persistence:** toggle, save, restart: mode restored; toggle on an unsupported document
   speaks "not supported" and leaves the setting unchanged.
7. **Mutation under selection (B):** select, mutate text before the range, Ctrl+C: copies the
   originally selected text or says the selection was lost.
8. **Focus/browse transitions:** select in browse, enter focus mode, Ctrl+C: gorca does not write
   the clipboard (B) and does not clear the page selection behind the user's back.

## Recommendation
1. Do not ship a bare A/B switch. Ship `browse.copy = "text" | "page"` with gorca+Shift+F10 toggling
   it, **default "text"** (NVDA default, matches the 2026-10-16 decision), where "page" falls back to
   gorca text whenever the mirror is not confirmed, and every browse Ctrl+C speaks its outcome.
2. Clipboard writer: ShellHelper `SetClipboardText` (write-only, capped, allow-listed). Ctrl+C:
   observed through `WatchKeyboard`, never grabbed; measure the collapsed-selection behaviour of
   Firefox and Chromium first (2.2 item 1a).
3. Before either mode: mirror results back to the Reader (A-2), move the Gecko focus rule into the
   source (A-3), anchor `sel` to node endpoints (S-6), persist the setting, add tests 1-8.

## Not verified here
No e2e run, no private-shell run. Unmeasured: `St.Clipboard` from the helper on 48.7 (works in
principle, compositor-internal), Firefox/Chromium collapsed-selection Ctrl+C clipboard behaviour,
RemoteDesktop clipboard without `Start` on 48.7, A-6 source busy time, iframe/shadow DOM mirrors.
