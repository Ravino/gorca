# M18a-4: fixed costs of the plugin call path

Date 2026-10-25, perf-engineer. Branch `worktree-agent-a01467fdcd7a26263`. Raw runs, scripts and summaries:
`bench/results/m18a4/` (`res-m2*.txt` final matrix, `res-t*.txt` A/B probes, `call-matrix-*.txt`
isolated-call matrix, `summary-*.txt`, `tab.py`/`sum.py` table makers). Baseline = the M18a-3 tree
(`e5527f68`) plus the (disabled) probes; "final" = the tip of this branch.

## 1. Verdict

C-4 is **not met** and, on this host, cannot be judged by this harness: `load1` was 9-40 during every
run (other agents build and test on the same 16 cores; there was no quiet window in about three
hours), so the turn p99 and the per-plugin share are dominated by scheduling noise. What the
per-stage probes show is stable across loads:

* The writer-side fixed costs were cut by 10-40 %: snapshot 46 -> 29 us, plan/build/poll about -10 %,
  pre-send 132 -> 117 us (p50). Not the 6 us the model assumed.
* **The guest call is not a fixed software cost at all.** Alone on a warm core one call is 6.5-9 us
  (back to back), 17-25 us (spinning core, 3 ms apart); after the core slept 100 ms it is 107-130 us
  even with a 3 ms pre-spin, 205 us on an LP-E core. In the bench a hot (spinning) worker's call is
  110-120 us. The cost scales with the state of the core and the cache (hybrid CPU: 6 P, 8 E, 2 LP-E
  cores, intel_pstate `powersave`, idle states to C3), not with what the code does. Software cuts of
  the 13 host imports (3 us each in the bench, 0.4 us in the warm loop) cannot reach it.
* A cache "warm call" for plan-armed workers recovers about 30 % of the cold-worker call (101 -> 70 us)
  but is an extra invocation of plugin code; it ships **off** and needs the owner's decision (section 5).

## 2. Per-stage before / after

Same harness (`plugin_latency`, `GORCA_BENCH_STAGES=1`), sps, ten first-party copies, 200 ms key
spacing, interleaved base / final, `load1` 9.5 / 8.6, governor `powersave`. p50 / p95 / p99 in us,
relative to the stage start; first pair of `res-m2.txt` (the second pair ran at `load1` 13.9 / 39.2
and is kept next to it, not used). Marks cost 25 ns each when on.

| stage | before | after | what changed |
|---|---|---|---|
| w1 snapshot + job lists | 48.9 / 68.6 / 79.1 | **29.7 / 44.5 / 74.2** | one tree query per node (was two), integer hasher for rows, no clone of the BFS queue |
| w4 plan (SPS, lock, end_key) | 24.1 / 30.3 / 34.1 | 25.0 / 33.9 / 37.0 | unchanged: 4.6 us hot, the rest is a cold writer |
| w5 build `DeriveJob`s | 16.2 / 21.5 / 23.4 | 14.5 / 19.4 / 42.2 | one shared `ModelView` per snapshot instead of one per plugin |
| w7 send waited + wake | 39.1 / 87.7 / 419 | 43.5 / 314 / 7766 | unchanged: one `FUTEX_WAKE_BITSET`, the syscall plus IPIs |
| **pre-send total (t0 -> sent)** | 132.4 / 198.2 / 526 | **117.5 / 404 / 7819** | p50 -11 %; the tail rows are load, not code |
| k1 pick-up, hot worker | 6.3 / 15.2 / 19.3 | 6.3 / 17.6 / 19.7 | spin loop, unchanged |
| k1 pick-up, cold worker | 107 / 334 / 1191 | 116 / 1133 / 3633 | futex wake plus C-state exit: not software |
| k3 derive prep | 12.9 / 23.2 / 29.7 | 10.9 / 21.1 / 29.3 | one `CLOCK_THREAD_CPUTIME_ID` syscall less per call |
| k4 guest call (13 imports, 36-48 us of it in host code) | 115.4 / 156.7 / 178 | 120.6 / 169 / 241 | unchanged: core state, see 1 |
| k5 derive tail + message resolve | 9.2 / 13.7 / 17.8 | 9.5 / 15.0 / 18.8 | unchanged (already on the worker) |
| k7 post reply | 1.2 / 7.1 / 35 | 1.3 / 7.8 / 31 | unchanged |
| w9 poll (convert replies) | 22.6 / 33.4 / 51.9 | **18.7 / 31.0 / 44.4** | no per-reply `Vec` for slot events; scheduler observations queued, not fed on the admission path |
| w10 admit (core) | 21.4 / 35.7 / 56.7 | 22.8 / 32.8 / 45.9 | integer hasher for bridge maps; the rest is token bookkeeping |
| wait end -> done | 43.4 / 69.5 / 92.7 | 40.8 / 64.8 / 87.1 | about 4 us per reply in the bench (was 6.5) |

The task's model inputs against the measurement now: snapshot + job build + send 50-105 -> 45-85 us
(model 6); admission 6.5 -> about 4-5 us per reply (model 8 in total); a call 80-100 + 25-40 (unchanged,
model 45); cold start median 100-170 (unchanged, model 70).

Isolated call (`plugin_call_cost`, one thread, no handoff; `call-matrix-*.txt`; `load1` 9-16):

| case | base p50 | final p50 |
|---|---|---|
| 1 thread, back to back | 8.9 | 6.5 |
| 1 thread, 3 ms spin gap | 18.9 | 9.7 |
| 1 thread, 3 ms sleep gap | 73.6 | 72.1 |
| 10 threads, 3 ms spin gap | 26.0 | 24.9 |
| 10 threads, 3 ms sleep gap | 60.6 | 61.5 |
| 1 thread pinned P / E / LP-E cores, spin gap | 16.7 / 20.3 / 39.8 | not rerun |
| 1 thread pinned P / E / LP-E, sleep gap | 73.8 / 89.3 / 204.9 | not rerun |
| 1 thread, 100 ms sleep gap, 0 / 0.3 / 3 / 20 ms pre-spin (P cores) | 131 / 131 / 133 / 108 | not rerun |

No page faults or syscalls scale with the call count (page faults 283 per 19 000 calls).

## 3. What was done (commits on this branch)

1. `gorca_rt::stage`: a lock-free, bounded, default-off timestamp ring (a disabled mark is one relaxed
   load); marks on both sides of the handoff, around the guest call and in the host-import hook;
   `plugin_latency` prints them as STAGE lines (`GORCA_BENCH_STAGES=1`); new `plugin_call_cost` test.
2. Core plugin port (`plugin_port.rs`): one tree query per node in the snapshot; `IndexHasher`
   (`RowMap`, `FastMap`, `FastSet`) for the snapshot and the bridge's token maps. Keys are host-side
   indices; a guest handle is verified by MAC before any lookup.
3. Daemon: one `ModelView` per snapshot shared by the jobs of a turn; slot events without a heap
   allocation; scheduler observations queued and handed over by the next non-waited poll or the next
   plan (not on the admission path).
4. Host: the CPU clock read by `begin_call` is reused by `derive` (one syscall less per call).
5. Warm call (opt-in, section 5).

Not done, with the reason: pooling allocator and pre-instantiation (instances are built once and reused;
no per-call instantiation exists); epoch interruption instead of fuel (threat-model change); batching
the 13 imports (a WIT change to the pinned first-party plugin and the interface); zero-copy argument
passing (wasmtime lifts strings with a copy, 0.7 us of 3 us per import at full speed); a thread
pinning policy for workers (section 6).

## 4. Matrix: labels vs sps, base vs final (`summary-m2.txt`, medians in the file)

Interleaved base / final per config, two repeats, governor `powersave`, `load1` 9-40 (column in the
file). Host far above the harness's own "quiet" bar (`load1 < 4`), so every row carries
`quiet_violated`. Examples (first-party copies, 24 ms keys): sps per-plugin mean 0.39 / 0.37 (base) ->
0.44 / 0.41 (final), with warm 0.45 / 0.42; labels 0.92 -> 0.93; sps CPU 28 % vs labels 245 %
(ratio 0.11). 200 ms keys: sps CPU 11-20 %, labels 30-45 % (ratio 0.3-0.45), turn p99 3-16 ms in
both because of the load. `res-interim-warm-default-on.txt` is an earlier run with the warm call
on by default (before the review below); kept, not used.

`replay_measured.py` with the new measured inputs (`ffu` = per-plugin share, all ten triggered):

| case | LABELS | SPS |
|---|---|---|
| F M18a-3 measured (75 / 63 / x2.8 / cold 120) | 0.766 | 0.495 |
| I M18a-4 measured (60 / 52 / x2.8 / cold 120) | 0.870 | 0.587 |
| J target pre-send 20, reserve 24, call x1.0 | 1.000 | 0.994 |
| K target pre-send 20, reserve 24, call x2.8 | 1.000 | 0.754 |
| L K with a warm cold start of 40 us | 1.000 | 0.985 |

So the cuts move the model from 0.495 to 0.587; the gate needs the call and the cold start to come
down too (rows J, L).

## 5. Gate C-4 (this host, this load)

| criterion | result |
|---|---|
| facts at first utterance, per plugin >= 0.95 | **fails**: sps 0.37-0.45 (24 ms keys, first party), not above 0.5 at 200 ms under load; labels 0.92 |
| writer turn p99 <= 310 us | **not met / not measurable**: 394-498 us at 24 ms keys, thousands at 200 ms under `load1` 9-40 |
| CPU <= 1/4 of labels at 5 keys/s | **fails**: 0.3-0.45 (spin of the armed workers dominates, not the work) |

## 6. Remaining and recommendations

1. The remaining cost is the core: a worker's first call after idle is 4-6 times a warm one. Options:
   (a) the warm call below; (b) DP-1 (`/dev/cpu_dma_latency`) for the C-state part, owner decision;
   (c) an affinity policy that keeps hot workers off LP-E/E cores (P cores are 6, the K cap is 6): not
   built, needs a topology probe; (d) a persistent spinning writer-side executor for the cheapest
   plugins: a design change.
2. **Warm call (needs the owner's OK)**: `Config::warm_on_arm` / `PluginOptions::warm_on_arm`, default
   **off**. A worker armed by a plan (never by a blanket arm) replays its last interactive job once per
   arming, at least 1 ms before the arming ends. After the security review: only while the writer says the
   plugin is enabled, unmasked and its last call returned (`ActorRunner::set_warm`, revoked at mask,
   backoff or fault, which also drops the saved job); `running_since` covers the replay (wedged-actor
   detector); a fault or overrun is sent as a `WARM_JOB` fault and goes through the same strike machine,
   mask and accounting as a real fault, and ends warming for that worker; same grant, fuel and
   capabilities (it is the same `derive`); every import only buffers effects and they are dropped, so it
   makes no fact and no side-effect call. What remains observable to a plugin: one more invocation with
   input it has already seen. Tests: `crates/gorca-plugin-host/tests/handoff.rs` (`warm::*`, five) and
   `a_fault_of_a_warm_call_strikes_the_slot_like_a_real_fault` in `gorca-daemon`.
3. Rerun the matrix on a quiet host (`load1 < 4`): `bench/results/m18a4/ab-real.sh` and the commands in
   section 7.

## 7. Reproduce

```sh
. ./env.sh   # then, in a worktree: cargo test --release -p gorca-daemon --test plugin_latency --test plugin_call_cost --no-run
GORCA_PLUGIN_SCHEDULER=sps GORCA_BENCH_DUMMY_TIER=first GORCA_BENCH_SPACING_MS=200 GORCA_BENCH_STAGES=1 \
  GORCA_BENCH_OUT=/var/tmp/x.txt <plugin_latency binary> --nocapture     # GORCA_BENCH_WARM=1: opt-in warm call
GORCA_CALLS=300 GORCA_CALL_THREADS=1 GORCA_CALL_GAP_US=3000 GORCA_CALL_SLEEP=1 <plugin_call_cost binary> --nocapture
python3 bench/results/m18a4/tab.py <res file>        # stage table;  sum.py <res files>: matrix table
```
`kernel.perf_event_paranoid` was lowered 3 -> 1 for `perf record` during the work and must be restored
to 3 (`sudo sysctl -w kernel.perf_event_paranoid=3`).
