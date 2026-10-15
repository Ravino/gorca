# B-5 runtime topology: pilot (NON-AUTHORITATIVE)

Date: 2026-10-15. Spec: `docs/bakeoff/b5-runtime-topology/SPEC.md`. Raw results and the generated
summary: `bench/results/b5/pilot-1/` (`summary.md`, `summary.json`, `host.txt`, `raw/`).

**Read this first.** The pilot ran while other agents were building and testing: the 1-minute load
average was **15 to 30** during every run, before the hog even started (`summary.md`, "Load per
run"), governor `performance`, deep idle states off (C2/C3 disabled already), `--quick` sizes
(150 probes per series, 40 L2 trials), **3 rounds**. The `idle` condition is not idle. Treat the
numbers as a smoke test of the harness and a sign of direction, not as a verdict. The authoritative
run needs the quiet-host procedure (SPEC section 7).

## What the pilot proves

* The switch works end to end: `--topology=single|lanes|lanes-rt`, 18 reader starts per cell, all
  exit 0, no run failed. Daemon tests with the speech lane on its own runtime pass.
* **Real-time was granted by rtkit on every start of arm (c): 18 of 18 in `idle`, 18 of 18 in
  `sat`, all four lane threads each time**, with no refusal and no fallback to nice. rtkit limits
  on this host: `MaxRealtimePriority` 20, `MinNiceLevel` -15, `RTTimeUSecMax` 200000.
* **rtkit grants `SCHED_RR`, not `SCHED_FIFO`.** The thread table shows `rr:15` (input), `rr:13`
  (Reader), `rr:12` (drain, speech). The direct FIFO path needs `RLIMIT_RTPRIO` and is unavailable
  here (it is 0). The topology report records the class the kernel reports.
* Pinning took: input on CPU 1, Reader 2, speech 3, drain 4; background threads on the other
  twelve CPUs (including 0 and the LP-E cores) so they stay off the lane cores.
* Safety net quiet: 0 watchdog demotions, 0 `SIGXCPU` demotions, no stray real-time thread (the
  workers, rayon `gorca-bulk-N` and blocking-pool threads are all `other`).
* The hog does saturate: load average with the hog was 20 to 33 (it was 15 to 30 without).
* Storm: 46.5k to 50k signals/s achieved in every cell (lowest: `lanes-rt` L3-50k `sat` median
  46.5k, 93 % of target).

## Numbers (ms, median over 3 rounds, [min..max]; W = rounds lower than `single` in the same round)

Headline condition `sat` (hog on all 16 CPUs) with the L3-50k storm:

| series, p99 | single | lanes | lanes-rt |
|---|---|---|---|
| L1 under storm (L3-50k) | 31.9 [15.8..59.9] | 52.8 [10.7..97.6] W1/3 | **13.5 [9.8..18.2] W3/3** |
| L2 under storm (L2-50k) | 87.3 [11.1..98.5] | 11.4 [9.9..178.8] W1/3 | **6.8 [4.0..7.2] W3/3** |
| K1 under storm (K1-50k) | 4.5 [4.3..6.1] | 4.8 [4.2..6.3] W2/3 | **1.4 [1.1..31.6] W2/3** |

Same series at p50 / p95 (`sat`, storm):

| series | single p50 / p95 | lanes p50 / p95 | lanes-rt p50 / p95 |
|---|---|---|---|
| L3-50k l1 | 6.2 / 26.8 | 4.0 / 29.4 | 4.8 / 10.3 |
| L2-50k l2 | 2.3 / 8.8 | 1.9 / 6.9 | 1.0 / 3.7 |
| K1-50k k1 | 0.38 / 3.5 | 0.38 / 3.1 | 0.25 / 0.62 |

`sat` without a storm (p99): L1 7.9 / 6.5 / 6.5; L2 5.1 / 4.0 / 4.9; K1 3.2 / 4.4 / 2.9
(single / lanes / lanes-rt).

`idle` (host load 15 to 30, no hog), p99: L1 1.11 / 1.00 / 1.03; L2 0.33 / 0.51 / 0.32;
K1 0.48 / 0.39 / 0.33; **L3-50k 1.12 / 1.28 / 1.96**; L2-50k 0.81 / 0.28 / 0.29; K1-50k
0.43 / 0.43 / 0.35.

## Reading it (direction only)

* Under saturation plus storm the **real-time arm is the only one near the budget**: its L1 p99
  under the storm is 13.5 ms against 31.9 ms for the single runtime, and its spread across rounds
  is narrow (9.8 to 18.2 ms against 15.8 to 59.9 ms). It wins all three rounds on L3-50k and
  L2-50k. L2 under storm is still 6.8 ms against the 5 ms gate.
* **Arm (b) alone is not a clear improvement**: dedicated threads without a scheduling class
  compete with the hog on equal terms; its L3-50k p99 is worse than `single` in two of three
  rounds (52.8 ms median, one round at 97.6 ms). Moving the lanes onto threads only helps once
  they are also favoured by the scheduler.
* **A possible cost of (c) when idle**: L3-50k p99 1.96 ms against 1.12 ms for `single`, lower in
  0 of 3 rounds. The gate is nowhere near, but it is the one series where (c) is worse in all
  rounds. Candidates (not tested yet): a Reader pinned to one core while the storm's wakeups and
  the bus traffic arrive on others; RR time slicing with equal priorities. The diagnostic arms
  `lanes-rt-nopin` and `lanes-pin` exist to split this; run them in the authoritative round.
* Two probes were lost under `sat` in total (`lanes-rt` L1: 1 in one round; `lanes` K1-50k: 1).
  `single` lost none. With 3 rounds this cannot be told from host noise; the authoritative run
  gates on `lost` = 0 as max over rounds.
* The single-runtime p99 under the storm failing the 15 ms gate (median 31.9 ms) is not a
  finding about the product: the host was at load 20 to 30 plus a 16-thread hog, which is also what
  "all cores saturated" is meant to model. It does show that the headroom for a topology choice is
  there.

## Not yet known (why the numbers are not authoritative)

* Run-to-run noise of this host at load 15 to 30: the min..max ranges are as wide as the effects.
* Whether the `sat` effect survives the full-size series (1200 probes, 400 L2 trials): the p99 of
  150 probes is the 2nd largest sample.
* How much of (c)'s gain comes from the scheduling class and how much from pinning
  (`lanes-rt-nopin`, `lanes-pin`).
* Effect on the rest of the desktop of four RR threads: the RT throttling default (95 %) and the
  watchdog bound the damage; nothing was observed, but nothing was measured either (the desktop
  is another agent's concern during the pilot). Add a probe of a non-RT canary's latency to the
  authoritative round.
* The bulk (rayon) pool has no caller yet; its idle cost is invisible in these numbers.

## Next

1. Quiet host (SPEC section 7), `bench/b5/judge.sh b5-auth --rounds 7` with `--arms
   single,lanes,lanes-rt,lanes-rt-nopin,lanes-pin`.
2. Apply the SPEC section 8 decision rule; record it in `docs/decisions.md` and ADR-0001 §15.
