# Bake-off B-5: runtime topology (one codebase, three placements)

Status: **harness and arms implemented; pilot done on a loaded host (non-authoritative)**.
Authoritative runs wait for a quiet host (section 7). Decision under test: ADR-0001 §15 row 2
(B-5) and the M-14 budget; `docs/plan/phase-2.md` P0 item 3 and §4.
Pilot: `docs/reports/b5-pilot.md`. Real-time facts: `docs/reports/2026-10-13-p12-realtime-scheduling.md`.

B-5 is a **config A/B over one codebase**, not three implementations. The lane code (key pump,
speech lane, Reader, drain) is the same function in every arm; only *where it runs* and *how the
kernel schedules that thread* differ.

## 1. The switch

`gorca --topology=single|lanes|lanes-rt` (or `GORCA_TOPOLOGY`). Default `single`: the behaviour
before the switch existed, until this bake-off is judged. Startup-only, so it is a flag and an
environment variable, not a hot-reloaded `gorca.toml` key (a key can be added when an arm wins).
`--topology-report=PATH` / `GORCA_TOPOLOGY_REPORT` writes what each lane was granted (`%p` = pid).

Code: `crates/gorca-daemon/src/topology.rs` (arms, placement, watchdog, report),
`rtkit.rs` (the rtkit client), `crates/gorca-rt/src/sched.rs` (policy, affinity, rlimit,
`SIGXCPU`), `Lane` start hook in `gorca-rt/src/lane.rs`, `TaskSet::placed_on` in
`gorca-rt/src/supervisor.rs`.

## 2. Arms

| | (a) `single` | (b) `lanes` | (c) `lanes-rt` |
|---|---|---|---|
| input lane (key source, pump, notices) | tasks on the worker runtime | own thread `gorca-input`, current-thread runtime | as (b) |
| speech lane (failover, lane, notices) | tasks on the worker runtime | own thread `gorca-speech-io`, current-thread runtime | as (b) |
| Reader ("presentation"), drain ("ingress") | own threads (ADR-0001 §3.2), same in every arm | same | same |
| worker runtime | `ncpu` threads | `max(ncpu-4, 2)` | as (b) |
| bulk pool | none | rayon, `max(ncpu-4, 2)` threads, `gorca-bulk-N` | as (b) |
| scheduling | default | default | real-time via rtkit on the four lane threads |
| CPU mask | any | any | lanes pinned to distinct fast cores; everything else kept off those cores |

Diagnostic arms (not part of the decision, used to attribute an effect): `lanes-rt-nopin`
(real-time only) and `lanes-pin` (pinning only).

Facts that shape the arms:

* The Reader and the drain were dedicated threads with their own current-thread runtime before
  B-5 (ADR-0001 §3.2, `Lane::Reader`, `Lane::Drain`). So arm (a) is today's behaviour exactly, and
  what B-5 moves is the input and speech lanes (tasks on the shared runtime until now).
* **rtkit grants `SCHED_RR`, not `SCHED_FIFO`** (measured: `rr:15` in the thread table). The
  direct `RLIMIT_RTPRIO` path would give FIFO; it is tried first and is unavailable on this host
  (RLIMIT_RTPRIO = 0). The report records the class the kernel reports, not the one requested.
* rtkit on this host: `MaxRealtimePriority` 20, `MinNiceLevel` -15, `RTTimeUSecMax` 200000.
  Lane priorities: input 15, Reader 13, drain 12, speech 12 (P-12 §5; all below the threaded IRQs
  at 50).
* The bulk pool has no caller yet (parallel tree builds and say-all chunks arrive with M5/M7);
  it exists so that the lane arms have the shape of ADR-0001 §3.2 and so the judge measures its
  idle cost. Its threads are never real-time.
* Fast cores are the CPUs within 10 % of the highest `cpuinfo_max_freq` (P-cores 0-5 here, never
  the LP-E cores). CPU 0 is left to the background when four other fast cores exist.

## 3. Safety net of an RT lane (arm c)

Never starve the desktop (CLAUDE.md, P-12 §5):

1. only the four lane threads are ever real-time; workers, rayon and blocking-pool threads start
   by returning to `SCHED_OTHER` and the background CPU mask (Linux threads inherit the creator's
   class and mask, so a thread spawned by a lane would otherwise inherit it). The report lists any
   other non-`other` thread (`stray_non_other_threads`, must be empty);
2. bounded priority 12 to 15, clamped to rtkit's `MaxRealtimePriority`;
3. `RLIMIT_RTTIME` soft 100 ms / hard 200 ms (rtkit requires it); a `SIGXCPU` handler demotes the
   offending thread to `SCHED_OTHER` instead of letting the default action kill the reader;
4. a normal-priority watchdog thread (`gorca-rt-watch`, 50 ms tick): a lane at more than 80 % duty
   for 4 ticks in a row is demoted and counted (`watchdog_demotions`);
5. rtkit's canary and the kernel's RT throttling stay on (not touched);
6. graceful fallback, in order: direct FIFO via rlimit, rtkit real-time, rtkit nice -11, plain.
   Any refusal is recorded (`lanes[].error`); the daemon never fails to start because of it.

## 4. Metrics

All from the Phase 1 e2e harness (`bench/e2e/README.md`), reader = this `gorca` binary, backend
voicego mock, private session and a11y bus (`tests/harness/run-isolated.sh`).

| series | scenario | metric (ms) | stimulus |
|---|---|---|---|
| L1 idle | `L1` | `l1_ms` | focus changes 8-16 ms apart |
| L1 under storm | `L3-50k` | `l1_ms` | L1 while another app emits 50k signals/s |
| L2 idle | `L2` | `l2_ms` | newer focus cancels stale speech |
| L2 under storm | `L2-50k` (new) | `l2_ms` | L2 while the 50k storm runs |
| K1 idle | `K1` | `k1_ms` | key to speech |
| K1 under storm | `K1-50k` (new) | `k1_ms` | K1 while the 50k storm runs |

Reported as **p50, p95, p99 per round**, then the median over rounds with min..max and standard
deviation. Also `lost` (max over rounds), storm rate achieved, the real-time grants, demotions,
stray RT threads, and for every run the load average, the governor and the exit status.

## 5. Conditions

* `idle`: no extra load (the host is whatever it is; its load is recorded).
* `sat`: `gorca-hog` (`crates/gorca-bench/src/bin/hog.rs`) runs one busy thread per CPU in the
  default scheduling class, doing decode/sort/hash rounds of a node table: the synthetic form of
  the M-15 parallel builds. Started before the run and stopped by the PID the judge recorded.

Each condition is measured with and without the storm (the `-50k` series), so `sat` + storm is
the headline and `idle` without storm is the baseline.

## 6. Procedure

```
bench/b5/judge.sh <label> [--quick] [--rounds N] [--arms single,lanes,lanes-rt] [--conds idle,sat]
bench/b5/summarize.py bench/results/b5/<label>      # re-run any time; judge.sh calls it at the end
```

For each round, each condition and each arm (arm order rotated every round) one `e2e run` is
executed; the arms are therefore interleaved at run granularity and the e2e harness additionally
interleaves its scenarios. Results go to `bench/results/b5/<label>/raw/<cond>/<arm>/r<N>/`
together with the daemon's topology reports; nothing is overwritten (an existing label is refused)
and failing or noisy runs stay on disk with their load and governor.

## 7. Quiet-host procedure (authoritative run)

1. No other agent builds, tests or benchmarks for the duration (roughly 90 minutes for 7 full
   rounds). Check `uptime`: 1-minute load below 1.5 before the first run.
2. `sudo bench/b1/quiet-host.sh on` (governor `performance`, idle states deeper than C1 off);
   `bench/b1/quiet-host.sh status` is stored in `host.txt`. Restore with `sudo ... off`.
3. rtkit must be reachable: `busctl --system get-property org.freedesktop.RealtimeKit1
   /org/freedesktop/RealtimeKit1 org.freedesktop.RealtimeKit1 MaxRealtimePriority` answers 20.
4. Full size (no `--quick`), `--rounds 7` or more. Keep the pilot label; use a new one.
5. Reject the run (rerun, keep the rejected one) when the `idle` condition shows load above 2
   before the run, or the storm achieved less than 90 % of 50k/s in an arm and not in the others.

## 8. Gates and decision rule

Gates (all arms, all conditions; median over rounds): L1, L3-50k and K1 p99 <= 15 ms, L2 p99 <=
5 ms, `lost` = 0, plus L2-50k <= 5 ms and K1-50k <= 15 ms.

An arm replaces the default `single` when all of these hold:

1. it passes the gates in `idle` and `sat`;
2. under `sat`, the p99 of L3-50k, L2-50k and K1-50k is lower than `single`'s in the same round in
   at least 80 % of rounds (paired; arms are interleaved so a round is one comparable slice of
   host load) and the median p99 improves by at least 20 % or 0.5 ms;
3. in `idle` no p99 is worse than `single`'s median by more than 10 % or 0.2 ms;
4. no watchdog or `SIGXCPU` demotion and no stray RT thread in any run;
5. (c) must also beat (b) by the margin of rule 2, otherwise (b) is preferred: real-time
   privileges are only worth it when they buy measurable latency.

If no arm clears rule 2, `single` stays (and the M-14 occupancy budget, not placement, is what
guards the Reader). A decision gets a line in `docs/decisions.md` and an ADR-0001 §15 update.

## 9. Known limits

* The reader is measured through the mock speech server and the private bus; the cost of the
  real voicego/PipeWire path is outside gorca (CLAUDE.md latency goal is gorca-side).
* Nothing is isolated (`isolcpus`/`nohz_full` unset): pinning only expresses preference; IRQs and
  other tasks can still land on the chosen cores.
* The harness processes (python fake apps, storm feeder, mock server) compete with the reader
  under `sat` equally in every arm; they are not RT.
* The hog is one workload shape. A different bulk workload (memory bandwidth bound) would stress
  the cache and memory system rather than CPU time.
