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

`/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/cc/` (drive.sh, entry.py):
`tests/shell/run_shell.sh` world, `tests/web/m7/rd.py` RemoteDesktop key injection (evdev 29+30,
29+46), release gorca binary `<repo>/target/release/gorca --impersonate-orca` in the private world
only.

## Update: browse-mode copy implemented (user clarified: fails only in Firefox)

The user selects with the virtual cursor in Firefox, so the cause is the browse-mode gap above.
Fix, NVDA parity (`script_copyToClipboard`): Ctrl+C in browse mode copies the virtual selection to the
system clipboard and says "copied to clipboard".

* New command `browse.copy`, bound to `ctrl+c` in the browse table of both NVDA presets (Orca presets
  have no browse selection). The chord is grabbed **only while a virtual selection exists**
  (`ModeKeys.copy`, `copy_grabs`, `current_grabs`, synced when the cursor settles like the Tab grab);
  without a selection Ctrl+C reaches the page untouched. Focus mode, review mode and every other
  application never grab it (regression test above, with `browse.copy` as the single exception).
* `ReaderEffect::Web(WebEffect::Copy(ClipText))` -> bounded channel (`Core::take_clipboard_requests`)
  -> daemon task `wire_clipboard` -> `gorca_input_mutter::clipboard::run`. `ClipText` hides its text in
  `Debug`; copied text is never logged. The Reader never waits: speech confirms at once.
* Clipboard backend: a Mutter `org.gnome.Mutter.RemoteDesktop` session with `EnableClipboard`
  (mime types text/plain;charset=utf-8, text/plain, UTF8_STRING) then `Start`; Mutter emits
  `SelectionTransfer(mime, serial)` on paste, we answer `SelectionWrite(serial)` -> pipe fd ->
  `SelectionWriteDone`. No portal prompt (same as key injection). The session lives only while our text
  is the clipboard: a newer copy replaces it, `SelectionOwnerChanged(session-is-owner=false)` or
  `Closed` ends it. Every D-Bus call and the pipe write have a 2 s deadline.
  Interface contract: `refs/mutter/data/dbus-interfaces/org.gnome.Mutter.RemoteDesktop.xml`.
* select-all now stores a half-open range `(0, len)` so the last character is copied.

Verified on real Mutter 48.7 (private shell, `examples/clip_probe` + GTK4 entry + RemoteDesktop Ctrl+V):
pasted text equals the copied text, repeated pastes served by the same session
(`pastedrive.sh` in the scratchpad `cc/`). Occasional lost injected Ctrl presses in that rig are the
rig's, not the owner's (a re-run pastes). Not run: a Firefox end-to-end (m8 rig is heavy and the host
was loaded); `RD.copy.say` in the m8 corpus still reads the clipboard with `gorca+C`, which is not
implemented, so its xfail stands. Unit tests: `browse::tests::ctrl_c_copies_the_virtual_selection_and_is_grabbed_only_while_one_exists`,
`select_all_then_copy_takes_the_whole_text`.

Open: the session is created at copy time; if Mutter's RemoteDesktop service is denied on the user's
setup the copy logs a warning (`clipboard: copy failed`) but still says "copied". Failure feedback to
the Reader is a follow-up.
