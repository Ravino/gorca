# M18a-3: the statistical plugin scheduler (SPS), implementation and measurements

Date 2026-10-25, rust-engineer. Branch `worktree-agent-a6d25d717362d562b`. Spec: ADR-0026 r6 section
2.4.7 (`docs/adr/0026-wasm-plugin-model.md`), math report `docs/reports/plugin-statistical-scheduler.md`,
reference `tools/research/plugin_sched/sps.py`. Raw runs: `docs/reports/m18a-3-sps-results/`.

## 1. Verdict

SPS is built, tested and wired, **off by default**: `[plugins] scheduler = "labels"` stays the
default, `"sps"` is opt-in. A first measurement showed about 0 % facts at the first utterance; the
diagnosis (section 4) found one implementation flaw (a reserve for replies that then never came,
fixed) and four assumptions of the model that real timings break. After the fix the per-plugin
share at the first utterance is about 0.40-0.48 for ten comparable first-party plugins (0.66-0.78 for
the one first-party plugin among third-party copies), against about 0.93 for LABELS; "all ten in
time" is still about 0. C-4 does not pass on this host. Nothing was installed on the system; DP-1 is
untouched (a documented, default-off hook point exists, section 5).

## 2. What was built

| Piece | Where |
|---|---|
| Pure scheduler: histograms, `on_key_down`, `plan`/`plan_with`, `observe`, token bucket, LABELS fallback, input-log ring and replay | `crates/gorca-plugin-sched/src/{lib,hist,log}.rs` |
| Tests: SPS-1..SPS-9 unit and property tests, adversary harness, bit-equal replay of the 17 594-call `sps.py` log, allocation and cost test | `crates/gorca-plugin-sched/src/tests.rs`, `tests/replay.rs`, `tests/cost.rs`, `tests/data/sps-replay-3000-seed7.log` (made by `tools/research/plugin_sched/dump_replay.py`) |
| Runner: per-job holds in the gate (mask of answered waited jobs), absolute clamp (`Turn::clamped`), one `FUTEX_WAKE_BITSET` for all parked workers, hot/cold start reports, `PrewakeHandle::arm_set` | `crates/gorca-plugin-host/src/{actor,call}.rs`, `crates/gorca-rt/src/wait.rs`; tests `crates/gorca-plugin-host/tests/handoff.rs` |
| Daemon: `Prewake` hub (class per key, one process-wide CPU bucket, DP-1 hook point), `interactive_sps`, observations, adaptive reserve, deferred admission of unwaited replies, `GORCA_SCHED_LOG` | `crates/gorca-daemon/src/{plugins,cli,input,config_task,lib}.rs` |
| Core seam: `ScopeRunner::{poll_waited, turn_done}` (defaults keep the old behaviour) | `crates/gorca-core/src/plugin_port.rs` |
| Config key `[plugins] scheduler = "labels" \| "sps"`, manifest `class = "interactive" \| "refinement"` (`priority` kept as alias, absent = unlabelled) | `crates/gorca-config`, `crates/gorca-plugin-host/src/package.rs`, `plugins/telegram-web/manifest.toml` |
| Bench: cpu, governor, scheduler, key spacing, dummy tier, K knob in the PX-6 line | `crates/gorca-daemon/tests/plugin_latency.rs` |

Deviations from the ADR text are listed in the ADR itself ("SPS as implemented", items 1-9, next to the
r5 gaps) and recorded in `docs/decisions.md`. The main ones: one worker thread per plugin (not a
pool), so a hot place is a pre-armed worker; one shared parking word with a wake bit per worker
instead of a CAS cold queue; scheduler state behind a mutex the key hook only `try_lock`s; the
`sps.py` negative-bucket refund is not copied; the fuse reserve is measured instead of 8 us.

## 3. Obligations (section 2.4.7.8)

| Id | Status |
|---|---|
| SPS-1 | property test (2 000 random plans, corrupt histograms, random reply times incl. never): hold and `t*` never exceed `D - spent - reserve`; `wait_end_us` clamp property (10^5 cases). **Not done:** fuzz of 10^7 cases, PX-4 A1..A13 on the integrated runtime (the harness is the bake-off one). |
| SPS-2 | property tests (end(W) - end(W without i) <= h_i; third-party hold <= 100 us; busy never waited). A6/A13 adversaries on the integrated runtime not run. |
| SPS-3 | adversary harness, six strategies, N = 10^5 (2 x 10^4 in debug builds), trusted and third-party adversary: offences far under `N/33 + N/256 + 9`; third-party oracle median stall equals honest within 10 us. |
| SPS-4 | dispatched set equals triggered set; `e <= 256`, `g <= 24` asserted over thousands of random plans. PF-6/MQ-2 corpora are untouched. |
| SPS-5 | counting allocator: **0 allocations** in `on_key_down` and `plan`; at m = 16 on this loaded host, release: `plan` p50 4.6 us, **p99 8.1 us**; `on_key_down` p50 0.5 us, p99 0.9 us. |
| SPS-6 | golden vector of `sps.py` reproduced; **17 594 calls replayed, 0 mismatches** (key, refund, dispatch with every assignment field, context and reply calls). The hash `9f3a899cdbc785ac` is a sha256 of Python reprs and is not comparable; the check is output by output. `GORCA_SCHED_LOG` ring writes ids and numbers only. |
| SPS-7 | property tests over random label/tier mixes: refinement never waited, armed or hot; one third-party worker; third parties ordered last; value caps. |
| SPS-8 | the same job content in and out of the wait set (daemon test with two scopes, one refused a wait by the governor). |
| SPS-9 | bucket bound shown by an accounting test (60 s, held key, every worker spinning its whole window). **Not done:** per-pool-thread `CLOCK_THREAD_CPUTIME_ID` over 60 s; the numbers below are whole-process CPU including the bench harness. |

## 4. Diagnosis of the near-zero first-utterance share, and the fix

### 4.1 Per-turn trace (first-party fixture, 200 ms, `GORCA_BENCH_LOG=1`; excerpt `10-trace-excerpt-before-fix-3-turns.txt`)

Times relative to the turn start `t0`, a typical turn:

| step | us |
|---|---|
| snapshot done, plan starts | 36-60 |
| plan and job build done, jobs sent | 62-105 |
| hot worker: pick-up | 0-1 after the send |
| hot worker: guest call (wall) | 80-100 (the model assumed 45) |
| hot worker: reply posted | 130-170 after the send, i.e. **165-250 after `t0`** |
| cold worker (pre-wake did not cover it): pick-up | **100-170 after the send** (C-state exit) |
| cold worker: reply posted | **280-345 after `t0`** |
| clamp of the wait (`t0 + D - reserve`) | **200** (reserve 100 us) |
| the writer's wait ended | 200 |

Answers to the questions asked:
* *hold = 0 (cold estimators, prior)?* No: every job had `hold = deadline` (140-164 us, clipped to the
  window), `waited = true`, and the writer waited.
* *optimal-stopping cap rejecting?* No: `t*` equalled the window; lambda is tiny.
* *pre-wake not reaching the worker?* It worked: 6 of 10 workers started hot (`cold=false`, start
  0-1 us); the other four were cold by the K = 6 cap and started 100-170 us late.
* *reply in time but admitted later?* Not in this version: the replies that were in were admitted.
  (The deferral of unwaited replies is a separate, intended change.)
* ***The reserve ate the budget.*** The reserve was `8 us + unit x (candidates + 1)`, 100 us (its
  cap) for ten candidates, so the wait ended at 200 us; the hot replies arrive at 165-250. But
  only 1-3 replies were then in, so the writer had reserved 100 us for admitting ten replies that
  did not come: a **self-defeating reserve** (implementation flaw). Fixed: the wait now ends at
  `D - fuse - unit x (replies in + 1)`, recomputed as replies arrive (`Turn::per_reply_ns`), so the
  turn stays inside `D` however many answer and the window is not shrunk for replies that never come.
  The plan's own window assumes the best case, one reply.

### 4.2 The same timings through `sps.py` (`tools/research/plugin_sched/replay_measured.py`, output `11-sps-py-with-measured-timings.txt`)

`sim.py` with its constants replaced by the measured ones (all metrics are the model's per-plugin
share, comparable to the mean of the bench's `per_plugin` list; the bench's "all ten in time" is a
much stricter number the model does not report):

| inputs | LABELS | SPS |
|---|---|---|
| model defaults (snap 6 us, fuse 8 us, exec median 45, cold median 70) | 1.000 | 0.996 |
| ten plugins always triggered, model timings | 1.000 | 0.998 |
| + measured pre-send 75 us, reserve 63 us, exec x2.8, cold median 120 | 0.766 | **0.495** |
| same but reserve 8 us | 1.000 | 0.705 |

So the Python model **also drops to 0.5-0.7 on the measured timings**: it is not an implementation
bug, it is four assumptions that do not hold on this host: (1) snapshot + job build + send is 6 us in
the model, 50-105 us here; (2) the reserve after the wait is 8 us, the real cost of admitting replies
is `unit x (n + 1)` with a unit of 5-7 us, 60-90 us for ten; (3) a call takes 45 us in the model, 80-100
us of wall time here with ten running at once, plus 25-40 us of host work after the guest returns
(`pickup_to_reply - exec` in the trace); (4) the model's parked start has a median of 70 us with a
light tail; the measured one is 100-170 us for a good part of the cold workers. The model's SPS also
loses to its own LABELS once these are in (0.705 against 1.0 at reserve 8): LABELS wakes all ten
workers and spends the CPU, SPS wakes K.

### 4.3 Conditions of the matrix after the fix

Host 16 cores, governor `powersave`, default idle states, no DP-1 lease. `load1` 1.4-7.5 (the run
waited up to 90 s per run for `load1 < 4`; half the runs started above it). Key spacing 24 ms = 41
keys/s, 200 ms = 5 keys/s. "First": nine copies pinned as first-party; "third": sideloaded
third-party copies (one hot place, 100 us hold, never waited for when clipped by that cap). Reserve
before the wait: `fuse` 24 us (it covers `take_ops` and apply in the bench) plus the adaptive part.

### 4.4 Results (`12-FINAL-matrix-adaptive-cut.txt`), medians of two runs, `labels` and `sps` interleaved on one binary

| config | load1 | writer turn p99 (us) | all ten in time | per-plugin share (mean) | process CPU (% core) |
|---|---|---|---|---|---|
| labels, first, 5 keys/s | 4.3 | 594 | 0.68 | 0.93 | 47.9 |
| **sps**, first, 5 keys/s | 3.5 | 420 | 0.00 | **0.40** | **20.1** |
| sps, first, K_max 10, 5 keys/s | 4.6 | 3073 (one run 5678) | 0.01 | **0.48** | 24.4 |
| labels, third, 5 keys/s | 3.1 | 1387 | 0.67 | 0.92 | 47.2 |
| **sps**, third, 5 keys/s | 5.6 | 588 | 0.00 | Telegram alone 0.66 | **13.8** |
| labels, first, 41 keys/s | 4.9 | 1844 | 0.66 | 0.94 | 286.7 |
| sps, first, 41 keys/s | 3.8 | 438 | 0.00 | 0.18 | 36.6 |
| labels, third, 41 keys/s | 4.9 | 515 | 0.70 | 0.94 | 289.4 |
| sps, third, 41 keys/s | 4.1 | 301 | 0.00 | Telegram alone 0.78 | 36.6 |

For comparison, the first version of this slice (before the fix, same fixture, 5 keys/s): per-plugin
mean 0.17, all ten 0.00; and with the constant 8 us reserve of the first runs, 0.2-0.7 per plugin and a
turn p50 of 350 us (the budget was overrun). Raw files `00`..`12`; the `labels` rows measure the
interim policy on this branch (one wake call instead of one per worker, otherwise the code that was
measured in `00`).

Before (main `bd4a841f`, third-party copies, 24 ms, `load1` 13-26): turn p99 669-6069 us, all ten
0.34-0.73, weakest plugin 0.51-0.94, CPU 179-287 %.

### 4.5 What remains

* The per-plugin share is capped by physics here: the writer spends 50-105 us before the send, a
  call takes 80-100 us of wall time with ten at once and 25-40 us more before its reply is posted,
  and admitting replies costs 5-7 us each (admission is half in `poll`: the conversion of effects,
  slot events, observations; half in the core). A 200 us window from the send fits a hot call only
  just; cold workers (100-170 us start) do not fit at all.
* Cheaper admission is the lever that is left in the code (the reserve is ~25 % of the budget at ten
  plugins); it is a follow-up, not done.
* The turn p99 of SPS (300-590 us at `load1 < 6`) is no longer below LABELS everywhere: the wait now
  uses the whole window, so host jitter lands on the p99. At 41 keys/s on the third-party fixture it
  is 301 us against 515.

### 4.6 C-4 gate (section 2.4.7.12)

| Criterion | Result |
|---|---|
| (1) facts at first utterance >= 99 % overall, >= 97 % per plugin, within 0.5 points of LABELS | **fails** (all-ten 0.00 against 0.66-0.70; per-plugin 0.18-0.48 against 0.93) |
| (2) scheduling CPU <= 1/4 of LABELS | passes at 41 keys/s (0.13); **fails** at 5 keys/s (0.29-0.42 of total process CPU) |
| (3) oracle adversary: stall p50 within 10 us of honest | holds in the pure harness (third party); not run on the integrated runtime |
| (4) stall p99 <= 0.31 ms and not more than 60 us above LABELS | **not met** (300-590 us; above LABELS in two of nine pairs) |
| (5) SPS-1..SPS-9 | pass, with the not-done items in section 3 |

By the ADR's rule LABELS stays the shipped policy. The result needs a rerun on a quiet host
(`load1 < 4` throughout) before it is final.

## 5. DP-1

Not decided, nothing installed (no udev rule, no write to `/dev/cpu_dma_latency` or `pm_qos`).
`gorca_daemon::plugins::IdleStateHook` is a documented hook point: `Prewake::set_idle_hook` installs
it, `Prewake::key_down_class` calls `plugins_active` whenever a key-down arms at least one worker.
Nothing in the product sets it. Cold starts here are 15-40 us at p50 and up to 250 us at p99, so
the lease would help the cold tail; it does not touch the structural cause above.

## 6. Risks and open questions

* Not measured on a quiet host; no PX-4 A1..A13 integrated run; no per-thread CPU (SPS-9) and no
  10^7-case fuzz of `plan`.
* The reserve model (`unit x (n + 1)`) is fitted to two points and the cap is 100 us; the real
  `apply_plugin_ops` and fuse run after `bridge.interactive` and are not in the reserve (the bench
  does not run them). A real session may need a larger reserve, which shortens the wait further.
* `KeyClass` cannot tell an arrow in a text field from navigation (no editable context at the key
  hook); typing is recognised, so typing learns to wake nothing.
* `term_parity` in `gorca-daemon` failed once and passed once on a loaded host (`busy_skips` of the
  extension host stats); it does not touch plugins.
* Disk was 94 % full, so no second build of main for an interleaved "before"; section 4.1 is the
  same harness on the unchanged tree, earlier the same day, under a different load.

## 7. Reproduce

```sh
. ./env.sh
cargo test --release -p gorca-daemon --test plugin_latency --no-run   # copy the binary, then:
GORCA_PLUGIN_SCHEDULER=sps GORCA_BENCH_DUMMY_TIER=third GORCA_BENCH_SPACING_MS=200 \
  GORCA_BENCH_OUT=/var/tmp/px6.txt <binary> --nocapture
```
`GORCA_SPS_KMAX` sets K_max, `GORCA_BENCH_LOG=1` logs plans and replies (distorts timing),
`GORCA_SCHED_LOG=<file>` (daemon) appends the scheduler input log when a scope closes.
