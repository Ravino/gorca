# Ctrl+C does not copy while gorca runs (2026-10-17)

## Verdict

gorca does **not** swallow Ctrl+C in ordinary applications. Measured on a real Mutter 48.7 (private
headless gnome-shell), with the daemon owning `org.gnome.Orca.KeyboardMonitor` and the user's own
`gorca.toml` (preset `nvda`, layout `desktop`, modifiers Insert/KP_Insert/Caps_Lock), Ctrl+A, Ctrl+C
in a focused GTK4 entry reached the application and the clipboard held the text (`wl-paste` ==
`hello copy`). Same result with a tapped Insert before, and with Caps Lock on. Baseline without
gorca: identical.

The one place where "Ctrl+C copies nothing" is real and caused by gorca is **Firefox/Chromium
browse mode** (the virtual cursor), by design gap D-3:

* Shift+arrows, Ctrl+Shift+arrows, Ctrl+A are grabbed in browse mode (nvda preset lines 171-183) and
  build a **virtual** selection in gorca's flat text. The page itself selects nothing.
* Ctrl+C is not grabbed, so it reaches the browser, which copies its own (empty or stale) selection.
* Mirroring the virtual range into the page (`gorca+Shift+F10`, "native selection") is off by
  default and works only when both ends lie in one text node outside a focusable element
  (`crates/gorca-core/src/browse/commands.rs`, `web_native_selection`). Ctrl+A on a real page spans
  many nodes and mirrors nothing.
* The real fix is gorca-side clipboard output for the virtual selection (docs/plan/m8-spec.md D-3,
  docs/reports/m8-impl.md M-5 "clipboard output still deferred", docs/plan/phase-3.md spike P-14,
  docs/plan/phase-3-features.md 5.4 K1: `org.gorca.ShellHelper` + St.Clipboard). Mutter 48 has no
  data-control protocol, so the daemon cannot own the selection itself.

## What was checked

| Suspect | Result |
|---|---|
| Ctrl as a grabbed modifier | No. `gorca_modifiers` yields only Insert, KP_Insert, Caps_Lock; Ctrl is only watched (`stop_speech_on_control`). |
| Plain Ctrl+letter chords in the grab set | Presets bind Ctrl only in browse (Ctrl+arrows, Ctrl+Home/End, Ctrl+A, Ctrl+Shift+...) and `gorca+Ctrl+...` (gorca chords are never grabbed as chords, `grabs_where` skips `Modifiers::GORCA`). No Ctrl+C/X/V/Z/Y. |
| `gorca+ctrl+c` (= `settings.save`) | Only with a gorca modifier held; the chord is not in the grab set, so the lane cannot swallow a plain Ctrl+C. |
| Learn mode (`grab_all`, gorca+h / gorca+1) | Swallows everything on purpose, including Ctrl+C. Escape leaves it. |
| Pass-next-key | Grabs nothing, passes. |
| Own clipboard code | None in gorca (no `wl-copy`, no `St.Clipboard`, no data-control). Nothing to conflict with. |
| Stuck grabbed modifier | Mutter 48.7 bug (docs/reports/live-keys-swallowed.md): hold Insert > 0.57 s, then every key is swallowed until Insert is tapped. It affects all keys, not only Ctrl+C; not reproduced here (needs held-modifier autorepeat, which `rd.py` does not emit). If the user's symptom is "all shortcuts dead", that is the cause. |
| Keyboard layout | Sources are `us` + `ru`. The grab check includes the Cyrillic keysyms of the C and V keys. |

Terminals: Ctrl+C is SIGINT in GNOME Terminal; copy is Ctrl+Shift+C. Not related to gorca.

## Fix in this change

Regression test only (no behaviour change needed for the GTK case):
`commands::compile::tests::app_clipboard_shortcuts_are_never_grabbed_and_ctrl_is_not_a_grab_modifier`
asserts, for both presets, both layouts and Focus/Review/Browse, that no grab (plain or exact) is a
Ctrl-only chord on the C, X, V, Z, Y, Insert or Delete keys (US and Cyrillic keysyms, both Shift
levels), and that Control_L/Control_R are never gorca modifiers.

## Recommended follow-ups (not done: outside "minimal")

1. Firefox browse mode: implement clipboard output for the virtual selection (P-14 / K1), with Ctrl+C
   grabbed in browse mode only while a virtual range exists (spec D-3). Until then a user can press
   `gorca+space` to leave browse mode and use the page natively, or `gorca+Shift+F10` for single
   text node selections.
2. Cheap stop-gap: when Ctrl+C arrives in browse mode with a virtual range that was not mirrored,
   speak "selection is virtual, cannot copy" instead of silently copying nothing.
3. Ask the user which application: if it is GTK/Text Editor/Firefox *focus mode*, the stuck-modifier
   bug (Insert held) or learn mode is the likely cause, not the keymap.

## Rig

`/tmp/scratch/cc/` (drive.sh, entry.py):
`tests/shell/run_shell.sh` world, `tests/web/m7/rd.py` RemoteDesktop key injection (evdev 29+30,
29+46), release gorca binary `<repo>/target/release/gorca --impersonate-orca` in the private world
only.

## Update 2: browse-mode copy, the shipped design (user: Ctrl+C fails only in Firefox)

The user selects with gorca's browse keys in Firefox. The virtual selection was never the
page's, so the browser's Ctrl+C copied nothing. Fixed by mirroring the virtual selection into
the page with AT-SPI `Text.SetSelection` and letting the browser copy: the formatting is kept,
no clipboard of ours exists, and Ctrl+C is **never grabbed** (the regression test above stands
without exceptions). Nothing is spoken on Ctrl+C (like NVDA when the application copies).

### Rejected: a RemoteDesktop clipboard session (first attempt, reverted)

`org.gnome.Mutter.RemoteDesktop` `EnableClipboard` + `Start` + `SelectionTransfer` worked
(pasted text equalled the copied text on Mutter 48.7), but `Start` registers a remote-access
handle and GNOME Shell 48.7 shows the "Stop Screen Sharing" indicator for as long as our text is
the clipboard (reviewer finding F-1, `docs/reports/ctrl-c-verify.md`; clicking it empties the
clipboard). Unacceptable for a blind user. Also rejected: portal clipboard (consent dialog, same
indicator), data-control (absent in Mutter 48). A Shell-extension `St.Clipboard` helper
(`org.gorca.ShellHelper`, P-14) stays a later option for text the browser cannot copy.
Findings F-2..F-5 of the review belonged to that backend and are gone with it; F-6 and F-7 are
fixed below.

### How it works

* `crates/gorca-core/src/browse/selmirror.rs` turns the flat-text range into hypertext spans
  (container node, UTF-16 offsets): the mirror's `StaticText` run leaves are the container's text
  parts, every other child is one U+FFFC. A range inside one container is one exact span. A
  range across containers gets exact legs (tail of the start branch, whole blocks between, head
  of the end branch) and a single rounded-out alternative on the common ancestor; whole blocks at
  both ends collapse to that single range (it is exact, and one range keeps the line breaks).
* The Reader schedules the mirror 40 ms after the last selection change (one update for a held
  Shift+arrow) and sends one `DocAction::SetSelection` (never awaited; the doc actor rewrites the
  ids to the source's). A plain cursor move sends `ClearSelection`, so the page never keeps a
  stale selection. `gorca+Shift+F10` still switches the mirror off (it is on by default now).
* `AtspiDocSource` (`select`) drops stale extra ranges through the **document's** hypertext
  (`GetNSelections` counts the ranges that intersect the object it is asked on, so asking a
  paragraph undercounts: observed as ranges of an earlier selection surviving), then applies the
  ranges, all under the action deadline. Gecko gets the exact spans (adding ranges); Blink gets the
  single range with exclusive ends (`EngineQuirks::selection_single_exclusive`). A refused
  `AddSelection` falls back to the single range. Engine reading of the spans lives in
  `gecko_range` (connector side), not in core.
* A selection whose first leaf sits in a focusable container (a link) is not mirrored: Firefox
  focuses that container (`TextLeafRange::SetSelection`, `aSetFocus`), which would move system
  focus (BM-2). A password field ends the range before it (F-7).
* F-6: a selecting move that ends on the last character of a line (Shift+End) or of the text
  (Ctrl+A) stores the active end one past the cursor, and the next selecting move starts from
  that end, so Shift+End includes the last character and Shift+Left takes back exactly one.

### Measured (private shell, real Mutter 48.7, Firefox 153 ESR and Chromium 154, ssip log + clipboard read)

Firefox `Text.SetSelection` on hypertexts, then Ctrl+C (`/tmp` scratch rig, summarised):

| Request | Result |
|---|---|
| inside one paragraph `(2,10)` | exact |
| document `(2,4)` over children p2..p4 | p2, p3, p4 whole, `\n\n` between: an end offset that is an embedded child's character **includes the child**; `(1,1)` selects child 1 |
| paragraph `(6,12)` with a link starting at 12 | `bravo link inside`: the link comes along (the end offset is its character); `(6,13)` the same |
| end = `len` of a container ending with a leaf | exact; of one ending with an embedded child: invalid, use `len-1` |
| `AddSelection` second range | works; the copy joins ranges with **no** separator (`delta.Echo`) |
| `SetSelection(0,..)` | replaces range 0 only; extras need `RemoveSelection` |

`tests/web/selcopy/run.sh` (new) drives gorca (browse mode) + the browser + RemoteDesktop key
injection and reads the clipboard back through a RemoteDesktop session without a window
(`wl-paste` steals focus and makes gorca re-enter the document):

Firefox: 9 of 9 rows pass: Shift+End on a heading (`Heading one`), on a paragraph with a link,
two words before a link (`Alpha bravo`), one and two paragraphs, Ctrl+A (stops before the
password field, the page text with its line breaks), paste into the page's text field equals the
copy. Chromium 7 of 9: E3/E5 differ only by a trailing space or blank line (Blink keeps exact,
exclusive ends); **Ctrl+A fails** when the range ends right before a text field (doc `(0,5)` with
an `<input>` as the next sibling is ignored by Blink; `(0,6)` and `(0,4)` work). Not mitigated.

### Known limits

* Firefox: a range ending exactly before a link at a letter (no space) takes the link along.
  A range with different containers at its ends and a partial block at one end is several ranges
  and the copy glues them without a separator (`...delta.Echo`) unless both ends are whole
  blocks; the single-range alternative is used where an engine keeps one range (Blink).
* Selecting inside a link does not mirror (BM-2); copying link text needs focus mode or the
  link's own commands.
* Chromium: the Ctrl+A-before-a-field case above.
* The mirror needs the page to be the browser's active document; background tabs are not
  selected.

## Update 3: review round 2 and S0 (2026-10-17)

* R2-1 fixed: `ClearSelection` removes every range with `Text.RemoveSelection` (never
  `SetSelection(0,0,0)`, which made Firefox focus a leading skip link). `tests/web/selcopy` has a
  skip link and row F1 (focused element unchanged after select + plain move); with the old clear F1
  fails (`('link', 'Skip to content')`), with the fix it passes in Firefox and Chromium.
* R2-2 fixed: a selection whose first leaf sits in a focusable container (a link, or the document
  for text directly in `<body>`) is mirrored only when that container already has focus. Row F2
  (bare body text selected while a link has focus) fails without the rule and passes with it.
  Ctrl+A skips leading text in focusable containers (a skip link), so on such a page the copy starts
  at the next text; every other selection starting there is not mirrored.
* S-1: `DocCtl::Select` returns `DocNotice::SelectionDone { req, outcome }` (Done, NotSupported,
  Failed); an action over the in-flight limit, an unaddressable node or a full control queue answer
  Failed instead of vanishing (a dropped Focus now says `FocusFailed`). A failed or skipped mirror
  says "page selection not set" (queued behind the selection speech, not cutting it).
* S-4: switching to the page selection sends a probe (`ClearSelection` with a request id) and says
  "page selection" only on `Done`; `NotSupported` or `Failed` says "page selection not available here"
  and stays virtual.
* S0: `SelectionModel::{Native, Virtual}` + `NativeSync::{Idle, Pending, Probing}` replace the bool;
  gorca+Shift+F10 announces "page selection" / "virtual selection" (en, ru). Virtual means no
  mirroring; no clipboard writer exists. Rows S1-S4 of `selcopy` cover the announcements, that
  Virtual copies nothing new, and that Native copies again after the probe.
* Not done: S1 (Chromium `Document.SetTextSelections`, would fix Ctrl+A before an `<input>`; Chromium
  rows stay 7/9 with E6 an XFAIL) and moving the Firefox focus rule out of core into the source.

## Update 4: mapping fixes from the math review (`docs/reports/selection-mapping-math.md`)

* S-1 units: spans carry both `range` (UTF-16, Gecko) and `range_cp` (scalars, Blink); Blink uses
  `range_cp` (vector V8).
* S-2 Chromium: the exact atom interval goes through `org.a11y.atspi.Document.SetTextSelections`
  (`a((so)i(so)ib)`, one call, offsets in scalars); measured on Chromium 154: exact across blocks,
  the bus name inside an object reference is ignored (the path counts), a reversed range is
  refused (start first, `start_is_active` true). The single rounded-out range stays as the
  fallback (answer false or error). With it every `selcopy` row passes on Chromium, Ctrl+A before an
  `<input>` included (it was 7/9).
* S-3: `trim_ws` survives only on the last leg of a plan; where two legs meet the space is real text
  (V2: "See li" stays "See li" in the plan; Gecko then takes the whole link, which is reported).
* S-4: nested lists plan the item tail and the nested head only (V3), flagged lossy for Gecko.
* S-5/S-6: images are atoms. `end_on_object` ends after an image with no `-1` (Gecko), a range of
  only an image is planned (V6, V7); an image at the very end of its container is flagged lossy
  (the engine may refuse it, unverified).
* S-7: a password field is checked on the ancestors of every atom, so a range that starts inside it
  is not planned, and a range spanning it stops before it.
* S-10 was a stale reading of the working tree: `clear_selection` already used `RemoveSelection` only.
* Exactness: the plan carries `gecko_lossy` (a leg ends in front of a hypertext child it continues
  into, a letter-end before a link, several ranges joined with no separator, an image at the end of
  its container). The source answers `Approximate` when it applied such a plan or had to fall back;
  the Reader receives `SelOutcome::Approximate`, says "page selection incomplete" (queued) and keeps
  the fact for a later Auto mode. No clipboard writer was added.
* Not done: lifting through intermediate levels (S-8/S-9: fewer legs), the container
  character-count cross-check (S-11), the secret-category count in the treap (S-7 perf), the
  echo state machine (nothing listens to `selection-changed` today).
