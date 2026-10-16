# B-5 runtime topology: reviewer verdict

Date: 2026-10-16. Branch `worktree-agent-ab8dc123ed2f390a2` at `d0a13c0` (default `--topology=auto`).
Inputs: `docs/bakeoff/b5-runtime-topology/SPEC.md`, `docs/reports/b5-judgement.md`,
`bench/results/b5/{auth-sat,auth-idle,auth-diag2}`. Reviewer run: `bench/results/b5/verdict-r1`.

## Verdict

**Adopt `auto` with conditions.** The latency win of `lanes-rt` on the storm and key paths is real
and consistent in every round set, including the reviewer's own like-for-like rerun. It is smaller
than the headline (about 30 % at matched storm rates, not 91 %). Three defects must be fixed before
`auto` ships as the default. One of them is a kernel behaviour, verified here, that turns the second
real-time runaway into a `SIGKILL` of the reader.

### Must fix before merge

| # | Severity | Where | Failure | Fix |
|---|---|---|---|---|
| F-1 | high | `crates/gorca-rt/src/sched.rs:259-269` (`on_xcpu`), `crates/gorca-daemon/src/topology.rs:322-336` | When `RLIMIT_RTTIME` hits its soft limit, the kernel sends `SIGXCPU` **and raises the process soft limit by 1 s** (`check_thread_timers`). After the first demotion the soft limit is 1 100 000 us, above the hard limit of 200 000 us. The next lane that runs 200 ms without blocking (any lane, any time later) gets `SIGKILL` with no `SIGXCPU` first. The duty watchdog cannot win that race: it needs 4 hot ticks of 50 ms, at least 200 ms, and it runs `SCHED_OTHER` on a loaded host. **Verified**: `verdict-r1/probes/rttime_rearm_probe.c` copies gorca's settings (soft = hard/2 = 100 ms, the same handler). Its first spin was demoted at 123 ms and left soft = 1 100 000. Its second spin was `Killed` (exit 137). Scenario: a 100k ev/s burst keeps the drain from sleeping for 100 ms (demoted), then a later Reader hot loop kills the screen reader in silence. | Re-arm `setrlimit(RLIMIT_RTTIME, soft, hard)` after every `SIGXCPU`. Do it from the watchdog thread when `xcpu_demotions()` changes (not in the handler), or use a raw syscall in the handler. Also start the watchdog's streak earlier, or make 4 x 50 ms shorter than the hard limit. Add a test that spins twice under rtkit (skip when rtkit is absent). |
| F-2 | medium | `topology.rs:113-123` (`resolve`), `364-406` (`place_lane`), `410-444` (`request_rt`) | `resolve()` only reads rtkit's limits. It never makes a test grant. When rtkit answers but refuses `MakeThreadRealtime`, the arm stays `LanesRt`. Refusals come from polkit (an inactive or ssh session), the burst limit (a crash-restart loop) or the per-user thread cap. The lanes are pinned **before** the request and the workers are already kept off the hot cores, so the daemon runs as `lanes-pin`, the worst arm measured (diag2 sat p99: L3-50k 82 vs 48.7 for single, K1-50k 348 vs 166, A1 35.5 vs 15.7). The judgement's claim that "rtkit refused, so single" holds only when rtkit cannot be reached. | When any hot lane is refused real-time, undo the pinning: restore every lane's and worker's affinity to `allowed`. Better still, make `auto` start lanes unpinned and pin only after a grant succeeds. Record the outcome in the report. |
| F-3 | medium | `tests/harness/run-isolated.sh:61` (`DBUS_SYSTEM_BUS_ADDRESS` set to a missing socket, environment cleared with `env -i`) | Under `auto`, every normal e2e gate, `make check` and daemon integration test resolves to `single`, so the shipped default is never gated. | In `base_env`, export `GORCA_RTKIT_BUS_ADDRESS=unix:path=/run/dbus/system_bus_socket` when the socket exists, with an opt-out (`GORCA_HARNESS_RTKIT=0`). The e2e runner should read the topology report and print the arm it resolved to. A strict mode (`GORCA_E2E_EXPECT_TOPOLOGY=lanes-rt`) should fail when the arm differs. This touches only rtkit, for the reader's own threads, which `judge.sh` already does. |

### Required follow-ups (not merge blockers)

* **A1 (window/dialog speech) regression under saturation.** Fix it in the next milestone, gated so that `lanes-rt` A1 p99 under `sat` is no higher than `single`'s (details below).
* **The duty watchdog is per thread, not per core.** `core_for` wraps `slot % hot.len()`, and on a machine with 4 or fewer CPUs `background` falls back to every CPU. On a 2-CPU laptop, two lanes on one core can each stay below 80 % duty while together holding that core near the 95 % RT throttle. Only the 16-CPU hybrid host was measured. Either do not resolve `auto` to `lanes-rt` when the fast CPUs (excluding CPU 0) are fewer than `HOT_LANES`, or watchdog the sum per core.
* **Priorities 12 to 15 outrank PREEMPT_RT's FIFO-1 kernel threads.** `ktimers/N`, `rcuc/N`, `irq_work/N` and `rcu_preempt` run at FIFO 1 (`ps -eLo cls,rtprio` on this host). They also outrank Firefox's rtkit-granted `AudioIPC` callbacks at RR 10. P-12 §5 covers only the IRQ threads at 50. This is bounded by `RLIMIT_RTTIME`, the watchdog and RT push/pull, but it belongs in P-12 and the ADR.
* **Quiet-host repeat** under the SPEC §7 conditions before the ADR-0001 §15 row is closed.

## (1) Is the evidence sufficient, and were the invalidations fair?

* The invalidations are fair. `auth-diag` rounds 2-3 and all of `auth-diag-a1` exited 1 because a checkpoint commit moved HEAD and `e2e` refused the stale reader (`check_reader_rev`). They are kept on disk and not used.
* **The SPEC §7 rule 5 (storm at 90 % of 50k/s) was not applied, and the headline is inflated by it.** In `auth-sat`, lanes-rt's L3-50k storm ran at 19-45k ev/s against single's 32-47k, so lanes-rt faced a lighter storm. The reviewer rerun (`verdict-r1`, `sat`, 3 interleaved rounds) has matched rates: lanes-rt 31.8 / 30.2 / 34.4k ev/s, single 30.5 / 41.0 / 33.5k.

  | L3-50k `sat` p99 (ms) | r1 | r2 | r3 |
  |---|---|---|---|
  | lanes-rt | 23.8 | 21.4 | 33.9 |
  | single | 36.4 | 34.9 | 48.4 |

  lanes-rt is lower in 3/3 rounds, by about 30-40 %, far from 198 vs 17. The ordering holds; the magnitude does not.
* L3-10k `sat`, both arms at 10k ev/s: lanes-rt 19.8 / 19.0 / 14.0 ms, single 22.1 / 23.1 / 22.5 ms (3/3). In `idle` the arms are equal (about 6 ms) because the storm alone does not saturate the reader.
* Conditions of `verdict-r1`: load 43-62, memory nearly exhausted (swap full, `MemAvailable` about 2.9 GB). Memory PSI `full avg10` was 41 % at the start of sampling, median 2.8 %, in `verdict-r1/psi-memory.log`. Sampling began at 17:41 UTC, so the first two to three runs were under heavy memory pressure without a record. Their A1 numbers (`idle` 75-131 ms in **both** arms) are invalid. The storm series are interleaved, so both arms carry the same pressure.
* The reader binary was the `d0a13c0` code, stamped `e185a10`: the relink at HEAD was OOM-killed (`signal: 9`), so the run used `--allow-stale-reader`. `--help` confirms the `auto` code. The explicit `--topology` arms are unaffected.
* **Sufficient for "lanes-rt orders ahead of single on storm and key paths". Not sufficient for the magnitude** or for the absolute gates, which no arm meets on this host.

## (2) A1

* **What runs A1's read.** The Reader (an RT lane) emits `ReaderEffect::Fetch`. `fetch_dispatcher` and then `do_fetch` run as tasks on the `gorca-worker` pool (`crates/gorca-core/src/runtime.rs:839-900`). The reply goes back to the Reader. So A1 crosses into `SCHED_OTHER` workers at least twice per cold focus fetch.
* **The cause is not RT lanes starving workers on the same cores.** In the pinned arm, workers never share a core with a lane. It is placement:
  * Pinning moves the workers off 4 of the 6 P-cores. They are left with CPU 0 and 5 (P), 6-13 (E) and 14-15 (LP-E) under the hog. `lanes-pin` alone gives A1 p99 35.5 vs 15.7.
  * Unpinned RT lanes preempt workers wherever they land. `nopin` gives 27.6.
  * Plain `lanes` does not regress A1 (20.7 vs 23.8).
  * The lanes-rt tail clusters at 42-47 ms across round sets (`auth-sat` p99 44.4 / 47.3 / 45.2 / 44.6 / 43.6). That fits a few fetches queuing behind CFS slices on the slower cores.
* **Acceptability.** p99 roughly doubles under `sat` (5/5 rounds). `single` fails the 15 ms gate too (23.8), and `idle` A1 is better with lanes-rt (13.1 vs 20.6, W3/4). A1 is one of the four primary paths and the regression shows up in exactly the situation the user creates: a parallel build. It is **not acceptable to leave it**, but it should not hold back a default that wins on keys, focus and storms. Fix options:
  * run the focus `fetch_dispatcher`/`do_fetch` on the Reader lane's own runtime;
  * give the workers `nice -5` through rtkit `MakeThreadHighPriority`;
  * let the workers use one of the P-cores that hold a lane.

## (3) Storm throughput

* There is **no evidence of a drain-throughput cost that breaks the 10k ev/s goal.** At 10k, lanes-rt achieved 9 998-10 000 ev/s under `sat` in 3/3 rounds. The generator lagged at most 132 ms, `lost` was 0 and probe p99 was at most 20 ms, so the drain kept up with no backlog.
* The probes travel through the same socket as the storm, in FIFO order. A probe p99 of 21-34 ms during the 30k ev/s storm therefore also rules out backlog growth at the rate achieved.
* The lower rate in `auth-sat` came from the generator: `storm_max_lag_ms` is in the seconds for **every** arm. The feeder thread and the private `dbus-daemon` are `SCHED_OTHER`, competing with the hog. At matched host conditions the rerun's rates are equal between the arms.
* Still open: the 50k/s rate under RT on a quiet host, and a harness-free drain-saturation measurement.

## (4) Safety

* What works:
  * only the 4 lanes are RT, as RR at 12-15 (verified in the reports, 0 stray threads);
  * workers and the bulk pool demote themselves at start;
  * `RLIMIT_RTTIME` is set before the first request;
  * rtkit's canary and the kernel's 95 % RT throttle are left alone;
  * a missing rtkit resolves to `single`.
* Defects: F-1 (the second runaway gets `SIGKILL`), F-2 (a refusal leaves the lanes pinned), and the per-thread duty watchdog with the small-machine case (follow-up).
* **A busy-loop bug inside a lane:**
  * A loop that never blocks is demoted by `SIGXCPU` at 100 ms the first time. The second time, the reader is killed (F-1).
  * A loop that blocks briefly (a 1 ms timer) resets `RLIMIT_RTTIME` on every sleep. Only the duty watchdog (more than 80 % for at least 200 ms) catches it. At 79 % it holds a hot core indefinitely. The desktop keeps the other 12 CPUs here; on small machines it would not (follow-up).
* `resolve()` and `enable_rt()` connect to rtkit twice at startup, each with a 1.5 s deadline. That is bounded, so acceptable.

## (5) Harness gap

This is F-3, required. Until it is fixed, every e2e result produced at `d0a13c0` describes `single`, not the shipped default.

## Commands

* `bench/results/b5/verdict-r1/probes/vjudge.sh verdict-r1 3 single,lanes-rt idle,sat L3-10k,L3-50k,A1`: 12 runs, all exit 0, interleaved and rotated.
* `gcc -O1 rttime_rearm_probe.c && ./rttime`: own process, pinned to CPU 3, spinning at most 200 ms at RR 10.
* Code reading: `topology.rs`, `sched.rs`, `runtime.rs` (fetch path), `run-isolated.sh`, `e2e.rs` (`check_reader_rev`).
