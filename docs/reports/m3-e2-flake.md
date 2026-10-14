# M3 E2 flake (`e2_missing`), investigation 2026-10-14

Base: main 4f4a479. Old reader = 54a6fef (before the 036c22a lane change), new = 4f4a479, both driven by the HEAD harness
(`target/release/e2e`, `--only E2 --reader daemon:<bin>,--impersonate-orca --reader-env GORCA_M3=1`).
Raw data: `bench/results/e2e/m3-flakeB-*`, `m3-flakeS-*`, `m3-flake-evidence/` (README explains labels; `m3-flake-*` and `m3-flake-probe`
are invalid runs without `--impersonate-orca`, kept).

## Answers

1. **Not a regression from the lane changes.**
   - Interleaved, normal load (4.1-11.6, performance governor), 7 runs per arm x 10,000 actions (seed A): old 0/7, new 0/7 missing; every run
     `spoken: 10033`. Last round (m3-rv1-e2/e2b, new code): 2 misses in 7 repeats = 2 per 70k actions. The 14 interleaved runs give 0 per 140k, so the
     rate is host-state dependent (P(0 | rate 2/70k) is about 2%); it was not reproducible at normal load.
   - Under 12 CPU hogs (load 21-25), 2 repeats per run: old missing 7,21 and 0,85 (doubles up to 628: the pre-036c22a key pairing breaks down);
     new 1,2 / 0,2; new+instrumentation 1,2 / 2,4. So the new code is far better than the old under load; the residual miss exists in both.
2. **The gate counts "no SPEAK for the action reached the voicego mock in [key_i, key_i+1)".** Server-side interruption after the SPEAK is therefore not a
   miss (matches the intent). A drop before the server is a miss. ADR-0007 TI-3 says "at least once per key press" with no carve-out for E-2
   (a newer key makes older speech stale), and the harness comment at the pause says the opposite ("right and not a missing echo"). The gate and
   the comment disagree; the ADR needs one sentence.
3. **Root cause of the dominant class (instrumented run m3-flakeS-instr-2, 6 misses, 4 lane-stale):** 4 of 4 `submit_stale` lines are a paste echo submitted
   with epoch N while the epoch became N+1 microseconds later (`crates/gorca-speech/src/lane.rs:184` `is_valid` in `submit`; the epoch is advanced
   by the next key in `crates/gorca-input-api/src/lane.rs:~295` `advance_epoch`, pumped at `crates/gorca-daemon/src/input.rs:46`). The paste echo is derived
   from the app's insert event (+EV-4 hold, read-back) so its latency is about the 20-30 ms key gap the harness uses. 5 of 6 misses in that run, and 4 of 7 in
   the other new-code runs, are pastes (2.4% of actions). By E-2 this is correct newest-wins behaviour: the user typed a second key before the paste echo
   existed. A real typist's gap is 70-200 ms. The remaining 2 misses had no lane drop (submitted, not stale at submit; one "space", one paste), i.e.
   dropped or merged upstream in the Reader or lost to a later race: **unexplained**. The first-key miss (e2b action #0) is unreproduced; hypothesis:
   text tracking of the entry (`text_track` after the focus presentation) not yet armed after the 300 ms settle in `focus_node` (m3.rs:173) under load.
   No product bug confirmed; nothing implemented.

## Recommendation
- Harness: pace after a paste like after an IME commit (e.g. `pause(60, 80)` after a paste action in m3.rs E2), or classify a miss as legitimate when the
  daemon's `stale_dropped` accounts for it (read the exit stats line). Pacing is about 3 lines and removes the race without hiding real drops.
- ADR-0007 TI-3/E2: add "an echo made stale by a newer key before it is sent is not missing".
- Investigate the 2 unexplained non-lane misses and the first-key case with a Reader-side counter (echo built vs submitted) before declaring E2 stable.

## Caveats
- Interleaved normal-load runs used one seed (repeats 1 = seed A); stress runs used A and B. Hogs and the instrumented build perturb load; load is in each
  result header. Instrumentation (eprintln/file log in lane.rs) lived in a scratch worktree, not committed.
- /tmp (tmpfs 16G) hit 100% during the work (other users' data plus my scratch build); runs were not corrupted.
