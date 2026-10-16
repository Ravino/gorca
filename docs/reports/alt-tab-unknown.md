# Alt+Tab and Ctrl+Alt+Tab speak "unknown" (2026-10-16)

## Symptom
Live Shell 48, gorca f3c0aaf: Alt+Tab (and Ctrl+Alt+Tab over the top bar) speaks "unknown" when the switcher opens.

## Reproduction
Private headless Shell rig: `tests/shell/run_shell.sh -- <script>` with a mock SSIP server (`tests/shell/ssip_log.py`),
the gorca daemon (debug build, ssip backend, ext-gnome-shell WASM) and `tests/shell/alttab_drive.py` (gtk3 window +
gedit, real Alt+Tab held/quick/Alt+Shift+Tab and Ctrl+Alt+Tab through Mutter RemoteDesktop).
Before the fix every held Alt+Tab and the first Ctrl+Alt+Tab item spoke `unknown`.

## Cause
Not the role table (`Role::Unknown.spoken()` is empty) and not the Shell rules. The only producer of the word is the
no-cache fallback in `present/flow.rs::on_focus_reply`. When a switcher opens the Shell is flooded (hundreds of
`showing` events, a busy main loop); the first fetch of the selected item and the immediate N2 retry, 1-3 ms apart,
are both refused `Busy` (in-flight limit, daemon log: `fetch failed ... connector is busy (in-flight limit reached)`).
Nothing is cached for the brand-new switcher button, so gorca said "unknown". The immediate retry meets the same
full queue, so it never helps.

## Fix
- `Busy` after the immediate retry is retried after 60, 150, 400 ms (`FocusBackoff`, timer driven, cancelled by any
  newer focus/selection through the epoch).
- If it still fails: an unreadable switcher item is silent (it is only passed over); a real focus still says "unknown".
Timeouts are not retried more (they charge the app's health).

## Result (same rig)
Alt+Tab: "gtkwin.py, button" / "gedit, button" then the window title; Ctrl+Alt+Tab: "Top Bar, button", "Windows, button".
Log of the pre-fix control run: Ctrl+Alt+Tab first item `unknown`, then `Windows, button`.

## Tests
`reader::tests::a_busy_switcher_item_is_asked_again_after_a_pause_and_never_says_unknown`,
`busy_for_good_is_silent_for_a_switcher_item_and_unknown_for_a_real_focus`,
`a_newer_focus_cancels_the_delayed_busy_retry` (the first two fail without the fix).

## Open
- The very first item of a held Alt+Tab was once spoken as bare "button": the unnamed switcher button gets its name
  only from a `labelled-by` relation, which can be late. Naming such buttons from their label child is not done.
- The root cause of the refusals (the Shell main loop answering slowly while the switcher animates) is untouched.
