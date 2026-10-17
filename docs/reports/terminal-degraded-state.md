# Degraded terminal / page state: key names spoken, output silent (2026-10-17)

## Symptom (user, live session)
Intermittently, in GNOME Terminal (bash) and also in Firefox (Telegram Web: close a popup, Tab through the
chat list, Enter): history, typed text, completion and output are not read, and the generic key echo speaks
"tab", "backspace", "enter". Alt+Tab away and back repairs it.

## What the symptom means in the code
- Terminal: `term_key` (text layer) claims Tab/Backspace/Enter only while a terminal is *tracked*; untracked, the
  key falls to the generic echo and the screen is never read. So the state is "the focused terminal is not tracked".
- Firefox: in browse mode Enter/Tab are grabbed commands. The state is "the web layer is not browsing"
  (`WebMode::Inactive`, or `Loading` that never resolves: IR-3 keeps the grabs off for a node no mirror knows).
- Both are states that only the *next focus event* repairs (`begin_focus` -> `text_track` / `web_on_focus`). Alt+Tab
  produces that event. Nothing re-examines them while keys arrive.

## Second bug (explains the word "enter" with `echo.command_keys = false`)
`commands/keys.rs` spoke every named key (enter, tab, backspace, escape, delete, F n) when
`echo.typed_characters` was on; `echo.command_keys` was only *toggled* (`echo.command_keys_toggle`) and never read
anywhere. The user's config has `typed_characters = true`, `command_keys = false`, so the names were spoken whenever
the text layer did not own the key. Fixed: key names need `echo.command_keys`; characters and "space" stay under
`typed_characters`. (Orca-default behaviour of echoing action keys is now opt-in: product decision, see decisions.md.)

## Reproduction attempts (private sessions, nothing reproduced the live trigger)
- New real-terminal soak `VD` (`crates/gorca-bench/src/e2e/m6.rs`, `GORCA_VD_ROUNDS`, `GORCA_VD_RECOVER`): ordinary bash
  cycle (type, Enter, Up/Down history, Tab completion with several candidates, Tab Tab candidate list, BackSpace,
  Ctrl+U) after each of: idle 20 s / 60 s, second application window + Alt+Tab back, tab switch, notify-send x3, bell,
  Super overview, Alt+Tab storm, search bar + Escape, context menu + Escape. Every step asserts the recalled/completed
  text is spoken and no key NAME is spoken. 10 rounds on the fix: 0 degraded rounds (the one failing step is the tab-switch
  round typing into the other tab's prompt, a harness artifact). 8 rounds on the base before the fix: same, so the
  trigger is not among these. Reader trace (`gorca_terminal` target) shows every focus event re-tracks within 0.1 ms.
  Runs: `bench/results/e2e/vd1`, `vd2`.
- Firefox `WD` (`tests/web/m7/pages/dialog.html`, `web.rs`): popup closed four ways (focus restored, blurred, list
  re-rendered, focus dropped); Enter is grabbed and no names spoken in all (`bench/results/e2e/wd1`).
- The rig has no Shell accessibility (its events: switcher, notifications, popups, overview) and no Telegram. The exact
  live trigger stays **unproven**. Candidate paths (each is now self-healing): (1) focus presentation never finished
  (fetch failed twice, "unknown"), so nothing was tracked; (2) tracking lost without focus change: `text_node_gone`,
  the terminal extension not attached yet / slot busy or disabled at the focus (`term_track` returns untracked; `WatchOnly`
  claims no keys), mode not Focus when the presentation settled; (3) web: `NoDocument`/lookup timeout leaves `Inactive`,
  or `Loading` with `known: false` for good.
- Orphaned focus (focused node removed, `QueryActiveWindowFocus`) is still a no-op in the runtime ("not implemented in
  this slice"): not fixed, needs a connector focus query (open).

## Fix (core, generic; ext-vte untouched)
`commands/keys.rs::focus_audit_on_key`, run for every key passed to the application (press, Focus/PassThrough modes,
focus state `Focused`, not a switcher selection):
1. `Focused { presented: false }` for more than 1.5 s: present the node again (`represent_focus`, at most once per
   2 s; counter `focus_represented_on_key`).
2. The presented node is a focused terminal that is not tracked: track it again from the presented snapshot
   (`term_retrack_on_key`, once per 500 ms per node; counter `TermReads::retracked_on_key`).
3. A web application whose web layer is `Inactive`/`Loading`: resolve the focus again (`web_resync_on_key`: `Inactive`
   re-runs `web_on_focus`, `Loading` re-asks `Ensure` and retries the join; once per 500 ms; counter
   `key_resyncs`). Never a guess: it starts from the focus state the Reader already holds; focus on a different node,
   an `Orphaned` state, or a switcher item are left alone.
Debug tracing (`gorca_terminal` target): "terminal tracked", "terminal untracked" with a `why` reason
(focus event / retrack / node or app gone / text object focused / app not followed / not trackable), the key-time
re-track, re-present and web resync. Never any typed text.

## Tests
Unit (`gorca-core`): `a_key_in_a_focused_terminal_that_lost_its_tracking_tracks_it_again_and_reads_what_follows`,
`a_terminal_that_is_not_the_focus_is_not_tracked_by_a_key`, `the_retracking_on_a_key_is_rate_limited`,
`a_key_after_a_focus_that_never_finished_presents_it_again_once`,
`key_names_are_silent_while_command_keys_are_off_and_spoken_when_on` (terminal/tests.rs);
`a_key_after_a_failed_document_lookup_asks_again_and_the_page_comes_back`,
`a_key_while_the_document_is_still_unknown_asks_for_it_again_at_most_twice_a_second`,
`a_key_in_a_browsing_page_resolves_nothing` (browse/tests.rs). Existing tests that asserted a bare "enter"/"tab"/
"backspace" echo now enable `command_keys`; the daemon e2e (`switcher` Tab) asserts the default is silent.
E2E: `VD` (real terminal soak) and `WD` (Firefox popup) as regression steps.

## Verification
fmt, `clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (all pass), `cargo xtask deps` clean.
Gate results (M6 fake, VV, strict K2/K3/R1, 3 repeats): see the end of this file.
