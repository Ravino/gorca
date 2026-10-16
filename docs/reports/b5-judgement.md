# B-5 runtime topology: judgement

Date: 2026-10-16. Spec: `docs/bakeoff/b5-runtime-topology/SPEC.md`. Pilot: `docs/reports/b5-pilot.md`.
Judge: `bench/b5/judge.sh` (now also runs K2, K2s, K3, R1, R1s, A1, records host CPU busy %, reader CPU
seconds through `bench/b5/reader-cpu.sh`, and sums `lost` / `superseded` / `late` from the conservation
accounting of `e2e::account`).

## Conditions (read this first)

The host was never quiet. Policy (docs/decisions.md 2026-10-15) is to interleave arms, record the
load and judge on consistent relative ordering. Governor `performance`, C2/C3 off (already set before
the run, not changed by this work). Load average per run: 28 to 79 (other agents building and
testing), host CPU busy 7 to 100 % before the hog started (per run in each `summary.md`). Absolute
numbers are therefore 2-5x worse than on a quiet host and the SPEC section 9 "storm at least 90 %
of target" rule fails for every arm in the heaviest rounds. Full-size runs, arms rotated each round.

| label | what | rounds | notes |
|---|---|---|---|
| `bench/results/b5/auth-sat` | single, lanes, lanes-rt; hog + all 12 scenarios | 5 | load 18-79 |
| `bench/results/b5/auth-idle` | same arms, no hog (the host load is the only load) | 4 | load 29-62 |
| `bench/results/b5/auth-diag` | diagnostic arms, **mostly invalid**: rounds 2-3 and all of `auth-diag-a1` exited 1 because a wip checkpoint commit changed HEAD and the harness refused the stale-rev reader. Kept, not used | 1 valid | |
| `bench/results/b5/auth-diag2` | single, lanes-rt, lanes-rt-nopin, lanes-pin, hog, subset + A1, K3 | 4 | load 28-44 |

Every e2e run of `auth-sat` (15), `auth-idle` (12) and `auth-diag2` (16) exited 0.

## Table: p99 in ms, median over rounds [min..max], `W` = rounds lower than `single` in the same round

`sat` (hog on 16 CPUs), the headline:

| series | single | lanes | lanes-rt |
|---|---|---|---|
| L1 | 13.1 [11.9..22.8] | 14.2 W3/5 | 11.3 [8.5..13.0] W4/5 |
| L2 | 10.5 | 8.4 W3/5 | 7.1 [5.1..8.3] W5/5 |
| K1 | 353 [6.4..724] | 9.9 W3/5 | 3.9 [3.6..14.2] W5/5 |
| L3-50k (L1 under storm) | 197.6 [25..677] | 89.4 W3/5 | **17.3 [14.0..25.1] W5/5** |
| L2-50k | 217.9 [192..743] | 154.8 W3/5 | **13.2 [11.4..13.7] W5/5** |
| K1-50k | 374.5 [7.6..384] | 12.0 W4/5 | 326 [4.0..358] W4/5 (see below) |
| K2 | 10.6 | 6.5 W5/5 | 3.4 [3.0..4.6] W5/5 |
| K2s | 8.9 | 7.5 W4/5 | 4.7 [2.9..7.1] W5/5 |
| K3 | 10.9 | 7.7 W4/5 | 8.1 [7.3..8.5] W4/5 |
| R1 | 9.6 | 6.5 W2/5 | 3.2 [3.0..14.4] W4/5 |
| R1s | 9.5 | 9.4 W3/5 | 5.7 [3.0..14.6] W4/5 |
| A1 | 23.8 [17.4..29.2] | 20.7 W4/5 | **44.6 [43.6..47.3] W0/5** |

`idle` (host load only), p99: L3-50k 135.7 / 45.7 / **21.4** (W4/4 for lanes-rt); L2-50k 265 / 164 / **10.9**;
K1-50k 5.9 / 343 / 2.7; K2 5.8 / 6.3 / 2.8; K2s 7.0 / 8.8 / 1.3; K3 7.9 / 7.9 / 2.6; R1 5.3 / 7.5 / 2.2;
R1s 5.9 / 8.4 / 3.0; L1 11.7 / 10.5 / 8.0; L2 8.7 / 5.0 / 4.6; A1 20.6 / 18.7 / 13.1 (single / lanes /
lanes-rt). lanes-rt is lower than single in 4 of 4 rounds on every series except K3 (3/4) and A1 (3/4).
Arm `lanes` alone is not better than `single` (R1, K2s, K1-50k worse in most rounds): the pilot
conclusion holds.

Diagnostic arms (`auth-diag2`, `sat`, 4 rounds, p99): L3-50k single 48.7, lanes-rt 16.4 (W4/4),
nopin 14.0 (W4/4), **pin only 82.0 (W2/4)**; L2-50k 108 / 13.3 / 11.2 / 23.2; K1-50k 166 / 3.9 / 173 / 348;
K2s 5.1 / 3.7 / 3.7 / 6.2; K3 7.2 / 6.4 / 5.3 / 9.4; A1 15.7 / 19.8 / 27.6 / 35.5.
The benefit comes from the real-time class; pinning alone is harmful (worse than single on K1-50k,
K2s, K3, R1s); real-time plus pinning is as good as real-time without pinning, with fewer K1-50k
outliers (3.9 vs 173 median).

### Lost, superseded (conservation accounting)

* **`lost` (silent loss), max over rounds.** lanes-rt: 0 in K2, K2s, K3, R1, R1s (all conditions).
  single: K2s 3 (sat) and 33 (idle). lanes: 0. The `single` arm violated conservation under load.
  `lost` in L1/L2/K1-style probe series (not conservation-based) is non-zero in every arm under
  load 60+ (L1: single 33-94, lanes 15-80, lanes-rt 36-68): probe misses of the host, not a ranking.
* **`superseded` (info), sum over rounds (sat / idle).** K2: single 61 / 60, lanes-rt 0 / 0.
  K3: single 74 / 34, lanes 87 / 84, lanes-rt 0 / 0. K2s: single 9 / 107, lanes-rt 1 / 1. R1: 15 / 5 vs 0.
  Hypothesis C (K2-loss report) **confirmed**: with the speech lane thread real-time the next chord
  rarely overwrites an unsent answer. Plain `lanes` does not help (K3 87). Under the L3-50k storm itself
  superseded is large in every arm (single 1130, lanes-rt 542 in `sat`): those are focus bursts
  coalescing by design.
* `late` (info): K2/K2s 6-16 for lanes-rt vs 0-14 for single: no consistent difference.

### CPU cost (reader user+sys seconds per whole run of 12 scenarios, median [min..max])

`sat`: single 26.3 [20.6..27.7], lanes 31.7, lanes-rt 31.1 [28.5..47.4]; `sat` diag2 (8 scenarios):
single 32.1, lanes-rt 48.1, nopin 47.4, pin 36.8. `idle`: single 31.6, lanes 30.5, lanes-rt 39.0
[31.3..55.6]. Real-time costs roughly +20 to +50 % reader CPU, because the lane threads run instead of
queueing behind the hog and process more work. Spinning was not seen: 0 watchdog demotions, 0
`SIGXCPU` demotions, no stray non-`other` thread in any run (hundreds of reader starts).

### rtkit

Granted on every lane-RT start (single `auth-sat` 60/60, `auth-idle` 32/32, `auth-diag2` 48/48): all four
lane threads, `SCHED_RR` 15 (input), 13 (Reader), 12 (drain, speech). No refusal, canary or throttling
event observed. Failure modes by design: rtkit absent or `RTTimeUSecMax` 0 gives a startup error that
the daemon swallows and runs the lane threads plain (`lanes`), which these runs show is no better
than `single` (see the default's fallback below).

## Caveats (not hidden)

1. **A1 p99 is worse with real-time**: 44.6 vs 23.8 ms under saturation, 0/5 rounds lower (diag2: 19.8
   vs 15.7, 2/4; nopin and pin-only are worse too, so it is not the pinning). A1 is the cold-mirror
   dialog path whose fetches run on the unpinned `gorca-worker` pool (12 threads, kept off the lane
   cores) competing with the hog. Single fails the 15 ms A1 gate in `sat` as well (23.8) and lanes-rt
   passes it in `idle` (13.1 vs 20.6). Follow-up item, not a blocker: give the worker pool a nice
   boost or let a mirror-fill actor keep some of the lane cores.
2. **Achieved storm rate is lower with lanes-rt under extreme load** (L3-50k median 24.7k ev/s vs 34.3k
   single in `sat` of `auth-sat`; 37.6k vs 46.4k in `auth-diag2`; 25.8k vs 42.7k idle). Everything stays
   above the 10k/s goal, but the lanes-rt storm was lighter, so the latency win is not like for
   like. Two readings: a throttling generator/mock sharing cores 1-4 with RR lanes (harness effect) or
   a real drain throughput cost when pinned RR on one core. The SPEC section 7 rule 5 would reject the run
   for this reason in the heaviest rounds; the relative ordering is nevertheless the same in the
   `auth-diag2` round set where the rates were 75-100 %. Open: measure drain throughput at
   saturation without the harness (`gorca-bench` storm sink) before relying on 50k/s with RT.
3. K1 p99 of about 350 ms in some rounds of every arm (single too) are single-probe stalls at load 60+,
   an artefact outside the topology.

## Verdict against the SPEC section 8 rules

1. Absolute gates in `idle` and `sat`: not met by any arm on this loaded host (single misses by far more); ordering is the discriminator.
2. Paired under `sat`: lanes-rt lower than single in 5/5 (L3-50k, L2-50k) and 4/5 (K1-50k) rounds, median p99
   better by 91 % (L3-50k), 94 % (L2-50k): passes.
3. `idle`: no p99 worse than single's by 10 % or 0.2 ms (A1, K3 also better): passes.
4. No demotion or stray RT thread: passes.
5. lanes-rt beats lanes under the same margin: L3-50k 17.3 vs 89.4, L2-50k 13.2 vs 155, K2/K2s/K3 superseded
   0 vs 87: passes.

Consistent across three independent runs (`auth-sat`, `auth-idle`, `auth-diag2`).

## Decision

* **Default topology: `lanes-rt`**, as `--topology=auto` (the new default). `auto` resolves at startup:
  rtkit reachable with a non-zero real-time budget gives `lanes-rt` (real-time lanes plus pinning),
  otherwise **`single`** (not the plain lanes: they were not better than `single`).
* **rtkit on by default: yes. Pinning on by default: yes, together with real-time** (pinning alone is
  harmful; real-time with pinning equals real-time without it and has fewer outliers).
* Explicit `--topology=single|lanes|lanes-rt[-nopin]|lanes-pin` keep their meaning; `lanes-rt` still
  falls back to plain lanes when rtkit refuses mid-way (a granted probe, then a refused thread).
* The e2e harness points the system bus at nothing, so with no `GORCA_RTKIT_BUS_ADDRESS` the default
  resolves to `single` there (the pre-switch behaviour); a run that needs the new default passes
  `--reader-env GORCA_RTKIT_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket`.
* Open items: A1 under real-time (caveat 1), drain throughput under RT (caveat 2), a quiet-host repeat
  when one exists (the policy is to judge on ordering, which is consistent, but the 50k/s rate rule
  was not met).

## Validation of the new default

`cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask deps` clean;
`cargo test --workspace --no-fail-fast`: 1873 passed, 0 failed (one `gorca-ext-host` crash-reset
timing test, `a_crash_is_contained_and_the_instance_is_reset_under_50_ms`, failed once at load 83 and
passed on the rerun: load flake, unrelated). Strict `K2,K3,K4,R1` with `--impersonate-orca`, 3 repeats,
no `--topology` flag and rtkit reachable (`bench/results/b5/default-strict`, load 13-41): all 12 reader
starts resolved to `lanes-rt` with all lanes real-time, 0 FAIL rows, `lost` 0 in K2/K3/R1, K4 clean
(10000 switches x 3), K2 p99 0.25 ms, K3 1.2 ms, R1 0.41 ms, 0 watchdog demotions.

## Verdict conditions F-1, F-2, F-3 and the A1 follow-up (2026-10-16)

Reviewer verdict: `docs/reports/b5-verdict.md` (adopt `auto` with conditions). Fixes on this branch:

* **F-1 (SIGKILL on the second runaway).** The `SIGXCPU` handler (`crates/gorca-rt/src/sched.rs`) now
  re-arms `RLIMIT_RTTIME` to the configured soft/hard after demoting the thread (a plain `setrlimit`
  syscall, async-signal-safe), counted in `xcpu_rearm_failures` (report field). The watchdog logs a
  WARN when `xcpu_demotions` grows. Regression test
  `topology::tests::a_second_runaway_is_demoted_not_killed` (gorca-daemon): re-executes the test binary,
  gets a real rtkit grant, spins three RT runaways with soft=100 ms / hard=200 ms and checks the limit
  is restored after each. With the re-arm disabled the child fails ("limit not re-armed after runaway 1");
  skips when rtkit is unreachable.
* **F-2 (refused grant left lanes pinned).** `auto` now makes a test grant: `resolve` reads rtkit's
  limits and asks `MakeThreadRealtime` for one throw-away thread, else `single`. Lanes pin only after
  their own grant succeeds, and `lanes-rt` no longer restricts workers/bulk threads to other CPUs, so a
  refusal can never leave the pinned-without-RT shape. Tests with a fake rtkit on a private
  `dbus-daemon` that grants or refuses (`auto_resolves_to_lanes_rt_only_after_a_real_grant`,
  `a_refused_lane_is_not_pinned`). `RLIMIT_RTTIME` is lowered by the probe and cannot be raised again;
  it is inert while no thread is RT.
* **Small machines.** `auto` -> `single` unless the process has more than 4 CPUs and at least 4 fast CPUs
  besides CPU 0 (`lanes_fit`, tested). The per-core duty sum stays a follow-up (ADR-0001 section 15.1).
* **F-3 (harness hid the default).** `tests/harness/run-isolated.sh` exports
  `GORCA_RTKIT_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket` to the daemon when the socket exists
  (`GORCA_HARNESS_RTKIT=0` opts out). `e2e run` forces a topology report per reader, records
  `topology_arms=` in the result header, and `--expect-topology ARM` / `GORCA_E2E_EXPECT_TOPOLOGY` fails
  the run with exit 3 (not softened by `--no-fail`) on a mismatch or when no report exists.
* **A1 under saturation (follow-up gate).** Cause per the verdict: pinning moved the workers off 4 of 6
  P-cores. Fix: workers and the bulk pool are no longer kept off the hot cores in `lanes-rt` (still the
  case in the diagnostic `lanes-pin`). Measured with `bench/b5/judge.sh f3-a1 --arms single,lanes-rt
  --conds sat --only A1 --rounds 5` (interleaved, `bench/results/b5/f3-a1`, all 5 lanes-rt readers
  `lanes-rt`, `rt_granted_all` true; memory PSI `full avg10` at most 3.2 %, log
  `f3-a1/psi-memory.log`): A1 p99 median **lanes-rt 26.2 ms [24.9..32.1], single 29.6 ms [19.4..35.4]**
  (before: 44.6 vs 23.8), p95 14.1 vs 16.2. The ranges overlap and the arm ordering is not stable (W3/5),
  so "no worse than single" holds at the median; it is not proof of a win. p50 equal.

### Gates on the shipped default

* `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo xtask deps`: clean.
  `cargo test --workspace --no-fail-fast -- --test-threads=4`: all result lines ok, 0 failed.
* Strict `K2,K3,K4,R1,A1`, `--impersonate-orca`, no `--topology`, 3 repeats, through the fixed harness,
  `--expect-topology lanes-rt` (the check passed: `topology_arms=lanes-rtx15`):
  * `f3-strict`: round 3 died when the harness temp dir under `/tmp` was removed by something else
    (`bind /tmp/gorca-h.*/run/sp11.sock: No such file`); kept, not used.
  * `f3-strict2` (host load 15-20): K2 p99 1.07 ms, K3 1.42, R1 1.09, lost 0, K4 leaked/doubled/lost 0.
    **Two FAIL rows:** `K4.chord_resent_per_10k` (repeats 0/2/13 against a gate of 5) and `A1.a1_wrong`
    (0/0/10 against 0). One disturbed repeat each.
  * Controls (`f3-ctl1..4`, K4+A1, 3 repeats each, interleaved single / lanes-rt / single / lanes-rt,
    same host): lanes-rt `chord_resent` 0/0, `a1_wrong` 0/0 (A1 p99 median 6.0 and 5.6 ms); single
    `a1_wrong` max 1 in the first, 0 in the second. So `a1_wrong` and `chord_resent` outliers also occur on
    `single` on this loaded host (`f3-strict` itself had `a1_wrong` 1), and lanes-rt reproduced clean in 6 of
    6 control repeats. Not proven to be host noise: the 13 and 10 were not reproduced, nor explained.
    Treat the strict gate as open until a quiet-host repeat.
* Open: per-core duty sum for the watchdog; quiet-host repeat (SPEC section 7); the A1 and K4 outliers above.
