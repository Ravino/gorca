# Terminal history recall (Up/Down) sometimes silent

2026-10-17, rust-engineer. Symptom (live): in GNOME Terminal + bash, recalling a command with Up/Down is
sometimes not read; Alt+Tab away and back fixes it.

## Reproduction
A pure unit rig in `extensions/ext-vte/src/session/tests.rs` models a readline terminal (rows wrapped at 40
columns, the prompt on the bottom row once the screen scrolls, trailing blank rows, VTE's "every row + newline"
text) and replays one recall in three redraw shapes (one `Replaced`, delete-all + insert-all, minimal
delete/insert) over 10 commands (short, Cyrillic, one char, empty, and three longer than the width) and 4 amounts of
output above the prompt. Before the fix: 576 of 1080 recalls were wrong; every one involved a command that wraps
(either side). Short commands always passed.

## Root causes (all in `ext-vte` `Session`)
1. The recall branch lived inside `line_edit` and needed the change to be a one-row edit starting on the cursor
   row (`first == cl`, `near` window of 2 rows). A wrapped command (or one that scrolls the screen) failed the test
   and fell to the generic diff: the spoken text was a fragment ("-la /usr/share..." without the head, or
   "out 22 / me@box:~$ cmd" including output and the prompt), or, when only a char or two differed, nothing.
2. `expect` was consumed by the first recall. Two quick Up presses whose redraws land in different readings had
   the second one explained by nothing, so only the changed tail was read (or a 1 char tail was held as a late key
   and then read as "text nobody typed").
3. A history key that redraws nothing (the same command twice in a row, the oldest entry) produced no event, so
   there was nothing to read at all. This is the silent case users notice.

Why Alt+Tab "fixes" it: it only resets the reading and re-speaks the cursor line on focus; the state itself does
not persist. The silence is per recall (items 1 and 3), not a stuck state. The key-time focus audit was not
involved: the terminal was tracked throughout.

## Fix
- `recalled()`: with a history key pending, the command on the cursor row is found with the prompt text of the
  previous reading (fallback: the prompt heuristic), wrapped rows are joined, and it is spoken once when it
  differs from the previous command (`command_at`).
- `vertical_keys` counts history keys whose redraws are outstanding; the redraw of a second quick key is still
  recognised.
- `vertical_open` + `VERTICAL_MS` (250 ms): a history key that changed nothing reads the current line.
- No WIT/API change (the WASM component is the same source; `history_read` is reused).

## Tests
Unit: `history_recall_is_spoken_once_in_every_redraw_shape` (1080 recalls), `a_history_key_that_changes_nothing_still_reads_the_line`,
`a_recall_is_not_read_twice_when_its_redraw_comes_late`, `two_quick_history_keys_read_each_recall_once`.
E2E: `vd_recall` inside the VD cycle (real gnome-terminal): short and 200-char wrapped command recalled with
Up/Up/Down/Down, two quick Ups, Up after Ctrl+C; each recall must be heard exactly once.

## Gates
See the end of this file.

## Gate results (host load 10-53, memory PSI up to 20: NOT a quiet host)
- fmt, `clippy --workspace --all-targets -D warnings`, `cargo xtask deps`: clean. `cargo test --workspace` passed on the first
  version of the fix; after the two follow-up fixes below `cargo test -p ext-vte -p gorca-core -p gorca-ext-host -p gorca-bench` passes.
- Real gnome-terminal VD (new recall steps included), WASM and native (`--terminal-policy=native`): 0 failing steps in 5 runs
  (`hr-vd-wasm3/4`, `hr-vd-nat1/2`, `hr-vd-final`); one earlier run had 1 failing step in the tab-switch round ("5 tilde 5 tilde" typed
  garbage, not a recall step; the base cycle with recalls was clean). `hr-vd-wasm` kept: it failed on `no evdev code for 'S'` (my step
  used upper case; fixed).
- Strict K2/K3/R1 (`phase2.sh --only m1`, `--impersonate-orca`, 3 repeats): PASS, 31 gated rows
  (`bench/results/acceptance/phase2-20261017T040807Z`).
- The first m6 acceptance run (kept, same dir) caught a regression of my first version: VH "Down: empty line" read output
  ("git statusOn branch mainme at box"). Cause: the remembered prompt "me@box:~$ " lost its blank in the empty recall, so the
  search for the command's first row climbed to an older prompt line. Fixed (prompt compared without trailing blank; rows above
  the cursor row count as wrapped only when they are full rows), test `recall_to_a_blank_line_below_older_prompts_says_blank_not_the_output`.
  A second regression (`less`, Down: a trailing ":" read after the scroll) is covered by `a_pager_scrolling_under_down_is_not_followed_by_a_line_read`.
- m6 after the fixes (`phase2-20261017T052201Z`, load up to 53): VH, V1, VT, VP, VR, VB, VW pass; V2 key p99 5.0 ms (gate 4), V3 (1 of 3
  repeats), VF (1 of 3), VV (flood p99 106 ms, 1-2 steps in 1 of 3) fail with min 0 / max 1-2: sporadic under that load, same
  family of load failures as documented for VT/V1 on main at load > 40. Not an acceptance run; needs a quiet-host rerun.

## Honest assessment of the live symptom
The recall-specific defects above are real and fixed, but they are triggered by wrapped/long commands, quick double presses and
identical history entries. The live report says "sometimes, repaired only by Alt+Tab, other reading fine", and the user has since found
that numpad flat review leaves the echo state broken (key names such as "space" spoken, `!` not spoken) until Alt+Tab. Up/Down is
silent in exactly that state if the terminal is not claiming keys (an unclaimed Up is not echoed by the generic path). I did not
investigate review mode (out of scope); my fix is therefore not proven to be the whole root cause of the live symptom and may be
partly a symptom patch next to the review/echo-state bug being fixed elsewhere. Re-test Up/Down recall after using flat review once
that fix lands.
