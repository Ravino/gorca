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
