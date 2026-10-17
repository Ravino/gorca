# Terminal re-tracking fix (term-degraded / term-baseline)

Date 2026-10-17. Branch worktree-agent-a33489069c15d31be, base 10ab9f6f.

## Reproduction (main, `GORCA_STATE_STRICT=1 GORCA_STATE_CASES=50000`, ~30 s release)
- seed 1: term-degraded (first seed 37737) and term-baseline (first seed 47158).
- seed 777777: term-baseline (812856), seed 2000000: term-degraded (2045517).
Shrunk sequences are the regressions in `crates/gorca-core/src/statemodel/regressions.rs`
(`a_degraded_terminal_is_upgraded_by_the_first_key_after_the_slot_recovers`,
`a_terminal_whose_baseline_read_failed_is_tracked_again_by_the_next_key`,
`a_failed_retry_of_the_baseline_does_not_hold_back_the_next_key`,
`re_reading_a_baseline_less_terminal_while_reads_fail_arms_no_timer_storm`).
All use 1 ms gaps: the failures are rate-limit races, they pass with 700 ms gaps.

## Root causes (`terminal.rs::term_retrack_on_key`)
1. term-degraded: a key that tracked an *untracked* terminal with the `WatchOnly` stand-in (policy slot
   busy) still consumed `RETRACK_INTERVAL` (500 ms). The next key, after the slot recovered, was
   inside the interval, so the stand-in was never upgraded; it stayed until a focus event (Alt+Tab).
   The stand-in claims no keys, so keys were echoed by name. Fix: only a tracking with a real policy
   uses up the interval.
2. term-baseline: a real session whose baseline read(s) failed (busy app) has no snapshot; the
   policy retries 5 times (200 ms..1.6 s) and then waits for a screen event that never comes. The key
   path returned early because "tracked and not degraded". Fix: `Tracked.read_ok/read_failed`;
   a tracked real session with no answered read, a failed last read, no read in flight and no
   snapshot is re-tracked (fresh session, secret hold kept) at the next key. This path is not
   time-limited: it is bounded by one read in flight at a time, each with its own deadline, so
   no busy loop and nothing blocks the key path (the read is an effect).
3. Found by the fix (real, latent runtime bug in ext-vte `flush_vertical`): a history-key request
   (`vertical_open`) whose expectation expired (> EXPECT_MS) while the phase was busy kept a due
   deadline for ever; the tick never consumed it -> timer re-armed in a loop (the model's
   "timer storm"). Fix: drop the stale request. Unit test
   `a_stale_history_request_leaves_no_deadline_behind`.

## Model check (task 3)
No invariant was changed. `term-degraded`/`term-baseline` assert that after a key a settled, healthy
terminal has a snapshot; that is the product requirement (key repairs tracking), and the gaps
are rate limits in the implementation, not in the model. A blank-screen terminal does not trigger
the re-read because `read_ok` is set by any answered read.

## Results
- Strict 50k, after the fix: seeds 1 and 100000 clean. Seed 777777 and 2000000 still report
  `term-silent-after-review (823517) and `review-drift` (2021185). Both were verified
  to reproduce identically on base 10ab9f6f (term code reverted), so they are separate
  pre-existing review bugs, not terminal tracking; not fixed here.
- Gates: fmt, clippy --workspace --all-targets -D warnings, cargo test --workspace (all ok), xtask strings ok.
- e2e (real VTE rig, --quick): VS pass (vs_stale/drift/silent 0). VT: all functional gates pass,
  `vt_ms.p99` 3.291 vs gate 3 ms FAIL (--quick, host shared with other agents; not repeated, no
  baseline comparison run). VD: killed by the 290 s / 590 s sandbox time limit, no verdict.

## Follow-up: remaining statemodel ids, VT/VD reruns
- `term-silent-after-review` (real, user-facing): leaving review by a key re-tracked the terminal in
  `text_mode_changed`, which restarted the session after the key had already reached it, losing the
  history-recall expectation and speaking the stale line plus the diff ("me at box: $ cmb", "d2" instead of
  "cmd2"). Fix: `Reader::term_resume` keeps a live tracked session and has it re-read the screen
  (`term_events_lost`); only an untracked/stand-in/no-baseline terminal is tracked afresh. Regression
  `history_recall_after_review_is_spoken`. Removed from `KNOWN`; `review-kept` was already fixed and is removed
  too (KNOWN is empty).
- `review-drift`: the invariant was too loose, not the anchor code. Background output (`TermAsync`) replaced the
  whole screen, the line under the cursor no longer existed, and the cursor went to the new prompt line. The
  invariant said "line on screen" but tested the *new* line. It now also requires the old line to be on the
  screen (`review_screen_has_line`). With the old line present the cursor must still stay (covered by the
  existing scroll/shift regressions). Regression `review_by_character_stays_on_its_line_while_the_terminal_answers_late`
  documents the case.
- Strict 50k x 40: seeds 1, 100000, 777777, 2000000, 3000000 all clean. One 20k x 150 run (seed 5000000)
  finds `not-tracked` (seed 5014211: OpenDialog, reads fail, CloseDialog refocus, OpenDialog, reads work, type ','
  in the dialog entry). It reproduces unchanged on base 10ab9f6f; it is an entry/dialog tracking race, not
  terminal; not fixed.
- VT without --quick on the real rig (3 repeats, `bench/results/e2e/retrack-VT-full`): vt_ms.p99 median
  0.485 ms (max of repeats 1.68), PASS (gate 3 ms). main's latest VT results (m6-gates-r3, m6fix-fake):
  0.514 / 0.485. The earlier 3.29 ms was the single-sample --quick run at load average 20.
- VD: `GORCA_VD_ROUNDS` is read by the e2e host, not the reader, so `--reader-env` does not shorten it (the
  4 "chunk" runs are each a full 8-round run, ~4 min, under the limit). All four, and a run with the main
  binaries (`retrack-VD-main`), show the same single failing step in round 2 (tab switch): the typed
  `echo 5` arrives garbled ("5 5 5 echo") at the shell, a rig key-ordering artifact that is identical on
  main; every other round and the baseline are clean.

## Review fixes (terminal-retrack-verify.md)
- M1 timer storm (seed 9100776, regression `re_tracking_on_every_key_while_reads_fail_starts_no_timer_chains`):
  (a) the statemodel keeps timers as a set like `runtime.rs` (N1); (b) in the real code a tick now belongs to
  a session only if one of the deadlines THAT tracking armed is due (`Tracked.due`, drained in `term_timer`),
  so the deadlines of replaced sessions can no longer tick, and re-arm, their successor. The regression passes
  with (b) alone (checked with the sim dedup switched off).
- M2 double re-track: `term_resume` now leaves any live non-stand-in session alone (re-reads only when it has
  a baseline), so a key leaving review on a cold terminal starts exactly one tracking
  (`a_key_leaving_review_on_a_cold_terminal_starts_one_tracking`; fails with 2 trackings when reverted).
- L1 review-drift oracle: the old line's presence and its row are read from the WORLD buffer (previous and
  current rows), not `review.cur`. If the line is gone the cursor must be on the row it had (rewrite in place)
  or stay held; otherwise drift is reported. The 2021185 regression still passes under it.
- Harness bookkeeping: `OpenDialog` and a refocusing `CloseDialog` are focus events, so they now reset
  `failed_reads` and the fresh-focus grace (`last_focus`) like `Op::Focus`; this removed the `not-tracked`
  hits (seeds 5014211, 6002957) that were an oracle gap (a key 1 ms after a dialog focus whose presentation
  was still being retried), verified with realistic gaps (the key re-presents the focus).
- Known pre-existing issue (not fixed, from the review, RP4): events lost + the reviewed line replaced by a
  full screen re-read -> the stale char is spoken first, no correction is spoken, and the next move jumps
  silently to the prompt line. Needs an ADR-0018 follow-up and a statemodel check ("a speak-first line
  replaced by the re-read is corrected or announced").
- Gates: strict 50k x 40 seeds 1, 4000000 clean; strict 20k x 150 seeds 9100000, 5000000, 6000000 clean.
