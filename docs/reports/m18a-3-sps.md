# M18a-3: the statistical plugin scheduler (SPS), implementation and measurements

Date 2026-10-25, rust-engineer. Branch `worktree-agent-a6d25d717362d562b`. Spec: ADR-0026 r6 section
2.4.7 (`docs/adr/0026-wasm-plugin-model.md`), math report `docs/reports/plugin-statistical-scheduler.md`,
reference `tools/research/plugin_sched/sps.py`. Raw runs: `docs/reports/m18a-3-sps-results/`.

## 1. Verdict

SPS is built, tested and wired, **off by default**: `[plugins] scheduler = "labels"` (the interim policy)
stays the default, `"sps"` is opt-in. On this host the C-4 gate does not pass: SPS cuts the writer's
tail and the CPU, but the facts that make the first utterance fall from about 70 % to about 0 for
ten identical heavy plugins. Nothing was installed on the system; DP-1 is untouched (a documented,
default-off hook point exists, section 5).

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

## 4. Measurements

Bench: `crates/gorca-daemon/tests/plugin_latency.rs` (PX-6): the writer turn of one focus change with
the real Telegram plugin plus nine copies of the same component (all ten match the same node and
all ten trigger on every key), 400 trials, release build, plugins off as the control. "First
utterance" counts a fact only if it is in the plan's output at the end of the bounded wait.

**Conditions.** Host 16 cores, Debian 13 RT kernel, governor `powersave`, default idle states
(C2 127 us, C3 1048 us), no DP-1 lease. **The host was never quiet**: other agents were building;
`load1` was 2.4-11 in the final runs and 13-26 in the "before" runs (a quiet run needs
`load1 < 4`). Every run carries its `load1`. The test's own "quiet" assertion therefore did not
apply and several runs ended with the gate assertion failing (exit 101); those numbers are kept.
Two matrices overlapped by my mistake (a background job kept running); that file is kept and
labelled `01-POLLUTED-...` and is not used.

"Third" = the nine copies are sideloaded third parties (the realistic case: one hot place, 100 us
hold, never waited for when their deadline is clipped by that cap). "First" = the nine copies are
pinned and installed as first-party plugins (the C-4 fixture reading where no tier cap applies).
Key spacing 24 ms is 41 keys/s (stress; the CPU bucket cannot serve ten workers per key at that
rate); 200 ms is 5 keys/s (navigation).

### 4.1 Before (main `bd4a841f`, nothing of this slice), third-party copies, 24 ms

| run | load1 | turn p99 (us) | facts at first utterance: all / weakest plugin | process CPU (% of a core) |
|---|---|---|---|---|
| base1 | 13.2 | 6069 | 0.340 / 0.505 | 179 |
| base2 | 26.3 | 5490 | 0.357 / 0.570 | 186 |
| base3 | 21.3 | 669 | 0.725 / 0.935 | 287 |

(The orchestrator's quiet-host figures for M18a-2: p99 434 us, 71 % / 93 %. Base3 matches them.)

### 4.2 After: final matrix (`06-FINAL-matrix-unit-times-n-plus-1.txt`), `labels` and `sps` interleaved on the same binary

Medians of two runs per row; the `labels` rows are this branch's interim policy, the policy that was
measured in 4.1 (same pre-wake, same countdown semantics, one wake call instead of one per worker).

| config | load1 | writer turn p99 (us) | facts all / weakest plugin | process CPU (% core) |
|---|---|---|---|---|
| labels, first-party copies, 5 keys/s | 6.0 | 443 | 0.754 / 0.944 | 47.8 |
| **sps**, first-party copies, 5 keys/s | 3.5 | **273** | 0.002 / 0.011 | **20.1** |
| labels, third-party copies, 5 keys/s | 5.7 | 1671 | 0.693 / 0.894 | 47.0 |
| **sps**, third-party copies, 5 keys/s | 4.9 | **294** | 0.000 / 0.000 (Telegram alone: 0.47 / 0.59) | **13.8** |
| labels, first-party copies, 41 keys/s | 9.5 | 5050 | 0.457 / 0.750 | 243.7 |
| sps, first-party copies, 41 keys/s | 9.9 | 476 | 0.002 / 0.019 | 35.6 |
| labels, third-party copies, 41 keys/s | 9.7 | 4511 | 0.625 / 0.893 | 279.9 |
| sps, third-party copies, 41 keys/s | 9.7 | 1182 | 0.000 / 0.000 (Telegram alone: 0.54 / 0.73) | 32.6 |
| sps K_max = 10, first-party copies, 5 keys/s | 4.4 | 296 | 0.007 / 0.052 | 24.6 |

Every plugin's fact arrives within 15 ms in all runs (`min_eventually_within_15ms` 0.995-1.000):
what SPS gives up is the first utterance, not the fact.

Earlier variants of the same code, kept for the record (`03`..`05`): with the constant 8 us reserve
the SPS turn p50 was 350 us for ten plugins and p99 450-650 us (PF-8 not held: admitting ten replies
costs 60-90 us after the wait); with the reserve proportional to the replies (04, 05) the first
utterance was the same ~0 and the p99 was within the gate only in the calmest runs.

### 4.3 What the numbers say

* **Tail and CPU.** At the navigation rate and `load1 < 8` the SPS turn p99 is 273-296 us against
  443-1671 us for LABELS (gate 310 us). The process uses 14-20 % of a core against 47 %; at the stress
  rate 36 % against 244-280 %. Outliers of milliseconds appear in both policies whenever `load1` is
  above about 8 (the writer thread is descheduled); SPS has fewer because it waits less.
* **Facts at the first utterance fall to about zero** when ten comparable plugins trigger together.
  The causes, from the per-reply log (`GORCA_BENCH_LOG=1`, whose own cost distorts absolute
  numbers): the writer spends 50-60 us before the send (snapshot 20-40, plan and job build 15-25);
  a guest call takes 100-200 us of wall time on this loaded host (the simulation assumed 45 us); admitting the replies
  after the wait costs about `6.5 us x (n + 1)`, which the reserve must leave inside `D`; so the
  window from the send is about 90-150 us, and a hold that long cannot cover a call that long. LABELS
  ignores the reserve (its turn p50 is 350 us, i.e. it breaks the 0.3 ms budget by itself) and wins
  the facts by overrunning. With one first-party plugin and nine third-party copies SPS still
  delivers 47-73 % of the first-party facts in time, against about 97 % for LABELS.
* **Third parties** are capped as designed: one hot place, 100 us hold, never waited for when the
  deadline is clipped by the cap, so their share at the first utterance is 0 by construction (L-2);
  they arrive as refinements.
* **K_max.** Raising K_max from 6 to 10 changes the weakest-plugin share from 0.01 to 0.05; the pre-wake
  budget is not what limits the first utterance here. At 41 keys/s the bucket allows about one worker
  per key.

### 4.4 C-4 gate (section 2.4.7.12)

| Criterion | Result |
|---|---|
| (1) facts at first utterance >= 99 % overall, >= 97 % per plugin, within 0.5 points of LABELS | **fails** (about 0 against 69-75 %) |
| (2) scheduling CPU <= 1/4 of LABELS | passes at 41 keys/s (0.12-0.15), **fails** at 5 keys/s (0.29-0.42, total process CPU) |
| (3) oracle adversary: stall p50 within 10 us of honest | holds in the pure harness (third party); not run on the integrated runtime |
| (4) stall p99 <= 0.31 ms and not more than 60 us above LABELS | below LABELS in every pair; **<= 0.31 ms only at 5 keys/s and `load1 < 8`** (273-296 us) |
| (5) SPS-1..SPS-9 | pass, with the not-done items in section 3 |

By the ADR's rule (1) failing means LABELS stays the shipped policy and the crate stays as a replay
tool; the config key lets the owner run SPS meanwhile. The result needs a rerun on a quiet host
(`load1 < 4`) before it is final.

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
