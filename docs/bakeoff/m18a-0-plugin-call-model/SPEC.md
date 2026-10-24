# Bake-off M18a-0 (C-3): plugin call model, synchronous in the writer (A) vs plugin actor with a bounded wait (B')

Status: **spec, ready for harness build** (2026-10-24). Owner: architect. Decision under test: ADR-0026 r4
section 5, contested choice C-3 (`docs/adr/0026-wasm-plugin-model.md`). Gate for every later M18a slice.
Judge: reviewer, on the measured numbers only. Nothing here is implemented yet; this is the contract.

Sources: ADR-0026 r4 (sections 2.4.2, 2.4.6, 2.5, 2.6.1, 2.9, 2.10, 5, 6), `docs/reports/adr-0026-review.md`
(r1 H-3, r3 R3-1..R3-5), `docs/reports/plugin-fusion-math.md` (PF-1..PF-8, MQ-2, MQ-3, section 3.3 and 4.3),
`docs/reports/wasm-plugin-cost-spike.md` (call, instantiate and memory costs), ADR-0027 section 7
(CD-W1 `.cwasm` cache key), `wit/gorca-ext-0.10/`, `crates/gorca-present-ir` (PresIr v1, frozen).

## 1. The problem

Plugins (WASM components, wasmtime 49.0.3, Component Model) derive **facts** for nodes: the W lane of
ADR-0026 runs `rules.derive` on a batch of nodes and the plugin asserts facts through `facts`. The node
writer (the app actor for K1, the document actor for K2) must publish the focus chain with those facts
if they are ready, and must never be held up by a guest. Two call models compete:

- **Arm A (fallback, reference).** `derive` runs **synchronously inside the writer's turn**, under a fuel
  grant (MQ-3 progressive filling), wall-clock accounting, size caps and pre-load (ADR-0026 section 2.5).
  The writer's added delay is whatever the guests spend, bounded only by fuel (and by wall accounting after
  the fact).
- **Arm B' (design target; owner decision 2026-10-24).** Each plugin is its **own actor** (own mailbox,
  own `Store`, run on a dedicated plugin thread pool, never on tokio workers). On a focus change the writer
  (0) reads the clock: `t0` = start of the writer turn and `D = t0 + 0.3 ms`; (1) recomputes S0 and bumps
  `T`; (2) builds one immutable `FrozenRows` per plugin batch (eager only); (3) sends `Derive{interactive}`
  to every plugin actor of the focus chain at once; (4) waits for the replies until `D`; (5) fuses and
  publishes with the facts that arrived; (6) admits late `Done` messages in later turns under the token
  check (LB-4 refine). Hard rules of the wait:
  - the wait is bounded **0.3 ms measured from `t0`** (snapshot, send, wait and fuse are all inside it);
  - **timer slack is 1 ns** (`prctl(PR_SET_TIMERSLACK, 1)`; 0 means "reset to default") on every thread that
    can run or wake a bounded wait: writer worker threads and plugin pool threads;
  - the wait is a **futex** (`futex_wait`, absolute `CLOCK_MONOTONIC` deadline, about 10 us of spinning
    first, atomic countdown reaching zero ends it early) **or an async high-resolution timer** (`select`
    over countdown notify and a `timerfd` through `AsyncFd`), **never a blocking receive on a tokio
    channel** (`blocking_recv` panics inside a runtime and a park stalls the worker's local queue) and
    never `tokio::time` for `D` (about 1 ms granularity);
  - **at most one bounded wait per keypress path** (the writer that owns the utterance waits; the other
    sends and publishes without waiting; a flapping-focus writer takes an interactive wait at most once
    per `N` ms, default 20, tuned here);
  - backpressure: one running job and one coalesced pending dirty set per plugin actor; a busy actor is
    not waited for (`pa_busy`, fail-open); an interactive job pre-empts a running background job of the
    same actor (`Cancel`, re-queue); a reply with a stale `slot_epoch` is dropped and counted.

B' is the design target (owner principle: actors, maximum parallelism, no synchronous calls). The bake-off
**confirms B' against budgets**; it does not pick between equals (ADR-0026 section 5, confirmation rule).
A is measured so that the fallback and the cost of the actor hop are known.

Questions answered here (ADR-0026 section 6): WQ-1 (64-node `derive` call cost), WQ-2 (fuel calibration,
the kappa_int / kappa_bg table per core class), WQ-3 (cost of import pre-charging, the 64th-import
watchdog, `Instant::now` pairs, handle MAC), WQ-6 (wake latency, futex vs `timerfd`, spin vs park, pool
size, late wakeups with and without slack, `FrozenRows` cost, parallel grant rule MQ-4), the lazy-parameter
bindgen check (section 2.5.3), the numbers that freeze B-1..B-9.

## 2. The common interface both candidates implement

Everything below that is not the runner is **shared code** (crate `m18a0-common`, section 6), so the arms
differ in the placement of the guest call and nothing else. Candidates must not fork the shared code.

### 2.1 Crates and layout

```text
prototypes/m18a-0/                       its own [workspace] table (like tools/spikes/wasm-plugin-cost/),
  common/        m18a0-common            outside the main workspace and its gates; wasmtime =49.0.3
  arm-a/         m18a0-arm-a             InlineRunner + A-specific wait-free glue          (candidate A)
  arm-bprime/    m18a0-arm-bprime        ActorRunner, plugin pool, futex wait, timerfd wait (candidate B')
  bin/           m18a0-run               one binary, --arm a|bprime, scenario files, JSON report
```

Prototypes live outside `crates/` and the main presenter; they import `gorca-present-ir` (feature `host`)
for the stub presenter and nothing else from the workspace. They are throwaway: only the numbers, the
report and the code the winner shares with production (`m18a0-common` types) survive into M18a-2.

### 2.2 Host crate (`m18a0-common`)

Loads 1..N components built against a **subset of `wit/gorca-ext-0.10`**: worlds `bakeoff-rules`, importing
`types`, `model`, `facts`, `diag` and exporting `rules` (`attach`, `derive`, `removed`). Subset rule: a
checked-in script proves the subset WIT is a strict subset of the 0.10 files (same record and function
signatures), so no signature is invented. Required host behaviour, all per ADR-0026:

- Engine: fuel only, **no epoch** (section 2.5.2); pooling allocator (`total_component_instances = 512`,
  `total_core_instances = 2048`, `max_memory_size` 32 MiB, memory cap 16 MiB per instance).
- Pre-load (section 2.4): read the `.cwasm` into a buffer, `Component::deserialize` **from the buffer**
  (never `deserialize_file`), build `InstancePre`; cache key = `package sha256 + engine
  .precompile_compatibility_hash()` (ADR-0027 CD-W1); a failed deserialize is a cache miss, not a strike
  (CD-W2). No file read, deserialize or compile on a writer turn.
- Fuel: grant = `time x kappa` (section 2.5.1); per-plugin adaptive `kappa_p = min(kappa_lane, p10 of the
  last 64 calls)`; host imports pre-charge fuel for estimated work (B-8); every 64th import checks the wall
  clock (traps at 4x the grant time).
- Wall clock around every guest call (`Instant::now` pair on the executing thread); wall overrun =
  `exec > max(2 x grant_time, grant_time + 50 us)`; effects of a call that returned normally are still
  admitted; the plugin is demoted to background-only for 60 s; 3 overruns in 60 s = one fault.
- Strike and slot machine exactly as ADR-0026 section 2.5.4 (`Unloaded`, `Cold`, `Ready`, `Backoff`
  30 s / 5 min, `Disabled`), with `slot_epoch` and `busy: Option<JobId>` from section 2.4.6.
- **MQ-3 progressive-filling budget** (section 2.4.2): reserve `n x c_max` (c_max = 2 us), pool
  `P = kappa_int x (B - n x c_max)`, order by predicted fuel ascending (EWMA of the p95 of interactive
  calls), grant `a_k = min(cap, R_k / (n - k + 1))`, unused fuel returns, **rotating tie-break** (focus
  counter mod n), no carry-over; B = 0.3 ms, cap = 0.2 ms per call. Strike rules of section 2.4.2 (strike
  only at the per-call cap, or p95 above `P/n` in 3 consecutive focus changes). The serial-equivalent grant
  applies to both arms; the parallel rule (MQ-4) is an optional B' variant, labelled as such.
- **MQ-2 staleness token** (section 2.6.1): writer clock `T`, `stamp[n][c]` for the six read classes
  (`struct`, `core`, `names`, `text`, `attrs`, `geom`), host-minted token `(t, R, M, g)` with `R` capped at
  256 (beyond: scope-wide stamp), stale iff `exists n in R, c in M: stamp[n][c] > t` or
  `BindGen(target, now) != g`; `g` is taken from the token, never at admission (PF-7). `sem_rev` and
  `vis_rev` are not in the token.
- Size caps before copy (section 2.5.3, lazy `WasmStr` / `WasmList` parameters, or hand-written
  `func_wrap`; which one bindgen allows is a recorded result), effect cap B-6 (facts <= 256, atomic reject).
- Trigger rule (section 2.4.1): `derive` only on attach, S0 class-stamp change of a node in a live token's
  read set, a `[[watch]]` match, TTL, epoch bump; never on admission or on the plugin's own facts.

### 2.3 Facts into a stub fusion writer

The stub **writer** stands for the app actor. It owns: clock `T`, S0 registers (a table of nodes with
the six class stamps), the `Slot` per plugin, the admission path, and a trivial `FUSE` (pure function of
the admitted set, CR-2 rank by tier, specificity, id, anchor key, so arrival order cannot change the
winner, PF-6). It exposes:

```rust
/// The one boundary where the arms differ. Admission code below it is the same function in both arms.
trait WLaneRunner: Send {
    /// Interactive handoff for one focus change. Returns when the arm's wait policy says so
    /// (A: after all calls ran inline; B': at the zero countdown or at `deadline`).
    /// Replies that did not make it are delivered later through `writer_mailbox`.
    fn interactive(&mut self, turn: &TurnCtx /* t0, deadline = t0 + 0.3 ms, T */,
                   batches: &[PluginBatch], rows: &FrozenRowsFactory,
                   writer_mailbox: &WriterMailbox) -> HandoffOutcome;
    /// Background slice: never waits; replies arrive through the mailbox (A: synchronous, same turn).
    fn background(&mut self, batches: &[PluginBatch], rows: &FrozenRowsFactory,
                  writer_mailbox: &WriterMailbox);
    fn cancel(&mut self, plugin: PluginId, job: JobId);
    fn shutdown(&mut self);
}
enum PluginReply { Done{..}, Fault{..}, Cancelled{..} }   // as ADR-0026 section 2.4.6
struct HandoffOutcome { arrived: u32, asked: u32, waited: Duration, woke_late: Duration,
                        busy_skips: u32 }                // woke_late = actual wake minus deadline
```

Stub **presenter and speech sink** (so the keypress path is end to end gorca-side): a key event enters at
the stub input; the writer turn runs; the stub presenter composes a `PresIr` from the fused entry through
the `gorca-present-ir` constructors (types are `#[non_exhaustive]`), renders it to a short string, and the
stub speech sink stamps the instant it would hand the text to the speech client. That stamp is "speech
start". No D-Bus, no real speech server, no real AT-SPI. The harness may swap the stub writer for the
real `FUSE` later; the bake-off does not need it.

## 3. Guest plugins

All guests are Rust, `wasm32-wasip2`, wit-bindgen 0.46, release `opt-level=s`, LTO, `panic=abort` (as the
spike), built once, hashed, precompiled per candidate engine, results cached by the CD-W1 key. Both arms
run the **same** `.wasm` bytes.

**Benign set (the fixture of ADR-0026 section 5: P-2 + 9 dummies, "Signal private session").**
- `signal-p2`: derives author, text and time facts for a message row from the node and its two
  children (the P-2 shape: 3 `model` imports, 3 facts per node).
- `dummy-1..9`: nine plugins that match the same focus chain through their `[[watch]]` selectors and do
  cheap, different amounts of work: trivial; 64-node batch name copy (WQ-1); string-heavy (formatting a
  64-byte name); import-heavy (20 `model` calls); mixed fan-out; each between 2 us and 60 us of guest time on
  the fast core. The mix is fixed by a seed and published in the report. Together they exercise MQ-3 with
  n = 10.

**Adversarial set (PX-4, ADR-0026 section 2.10), one guest per row, run alone and in combinations:**

| Id | Guest behaviour (the case the task lists in brackets) |
|---|---|
| A1 | fresh-page stride: touches a new 4 KiB page per iteration over the memory cap [page-touching loop]; fuel ignores page faults |
| A2 | pointer chase over 16 MiB [cache-miss loop]; fuel ignores misses |
| A3 | returns or passes a 16 MiB string to `diag.log`, `facts.assert` text, `settings`-like and `attr`-key positions [16 MiB string]; cap must be checked before copy |
| A4 | returns a 100k-element list of effects [huge return]; B-6 and B-9 must reject atomically |
| A5 | cold `.cwasm`: cache file evicted with `posix_fadvise(POSIX_FADV_DONTNEED)` in the private session [cold load from disk]; **never** a global `drop_caches`; proves no file-backed fault reaches a writer turn |
| A6 | N = 10 plugins each spending its full interactive grant [10 plugins at full budget] |
| A13 | a loop of `subtree(max)`-class imports (the subset's heaviest import) to exercise B-8 pre-charging |
| A7, A8, A12 | correctness rows run on both arms: patches matching each other's selectors (no oscillation); a `refresh`-like request on every `derive` (<= 1 per real change); a plugin asserting on its own target every call (still one call per real change) |

A5 is run in a **private session** (own mount or own file under the run directory; `posix_fadvise` on that
file only). A1..A6 and A13 are added **one at a time and then all together** on top of the benign fixture.
A9..A11 (keymap, forged handles, timers) are not in scope: they test the R lane and the handle MAC (WQ-3
measures the MAC cost with a valid and a forged handle only).

## 4. Metrics: exact method and pass threshold

Terms. **Focus change** = one interactive handoff on the stub writer. **Stall** = for one focus change,
`t_publish - t0` where `t0` is the first instruction of the writer turn (`clock_gettime(CLOCK_MONOTONIC)`
read before S0 recompute) and `t_publish` is the instant the fused entry is handed to the stub presenter;
the interval therefore contains S0 recompute, snapshot, send, wait and fuse. The stub's own non-plugin work
is measured in the plugins-off run and reported next to it (budget: <= 5 us; stall is **not** reduced by it,
that is the conservative side). Both readings use the same clock on the same thread (no cross-clock skew);
for B' the plugin-side timestamps are on `CLOCK_MONOTONIC` too (the same clock across threads).
Percentiles come from the **raw samples**, not from a histogram, and an HDR histogram (3 significant
digits, 1 ns resolution) is stored beside them for the report.

| # | Metric | Measurement method | Pass threshold (B' confirmed) | Applies to |
|---|---|---|---|---|
| M1 | **Writer stall, max** | max of `t_publish - t0` over all focus changes of a scenario, benign and every adversarial combination, with demotion active, slack 1 ns, quiet host. Loaded-host max is reported separately and is not a gate (dominated by preemption); for every loaded run the number of involuntary context switches of the writer thread (`/proc/<pid>/task/<tid>/status`, `nonvoluntary_ctxt_switches` delta) is recorded so the max can be attributed | **<= 0.35 ms** on the quiet host in every run | B' (A reported) |
| M2 | **Writer stall, p99** | same samples | **<= 0.31 ms** per scenario | B' (A reported) |
| M3 | **Facts at first utterance** | per focus change and per (plugin, node) pair watched on the focus chain and not in `Backoff`/`Disabled`: 1 iff the plugin's fact for that node is in the entry published by this turn, else 0 (the denominator counts `pa_busy` skips, late replies, OutOfFuel and faults). Overall = sum / count; per plugin = the same per plugin id. Benign fixture quiet host: **overall >= 99 % and every plugin >= 97 %**. Adversarial mix: overall >= 97 %, every benign plugin >= 90 %. Reason breakdown reported (late reply, `pa_busy`, OutOfFuel, snapshot exhausted the 0.3 ms) | as stated | both arms (A must reach 99 % overall for rule (ii)) |
| M4 | **Keypress to speech Δp99, gorca-side** | stub input stamps `t_key` at injection; stub speech sink stamps `t_speech`; latency = `t_speech - t_key`. Interleaved runs plugins-on / plugins-off (ABABAB, 60 s each, 5 pairs), same focus-change stream (seeded). Δp99 = p99(on) - p99(off), taken as the median over the 5 pairs | **<= 0.1 ms** benign (this spec's gate); the ADR confirmation rule says <= 0.3 ms benign (section 5, "benign Δp99 <= 0.3 ms") and that is the one the **decision rule** uses; see section 7 on the difference | both arms |
| M5 | **One wait per keypress path** | scenario with a K1+K2 pair (two stub writers, one keypress, shared deadline): count of bounded waits per `t_key`; the second writer must not wait; combined plugin stall of the keypress <= 0.3 ms + one fuse. Flapping focus at 3.3k turns/s: waits per writer <= 1 per `N` ms | exactly <= 1 wait per keypress path; flapping writer stall p99 within M2 | B' |
| M6 | **Throughput, no backlog (PX-7)** | stub ingress feeds 10,000 events/s for 120 s (also 50,000 events/s for 60 s, reported), 3 plugins watching 50 % of the nodes, one writer, plugins on and off; sample every 100 ms: writer mailbox depth, plugin mailbox depth, ingress lag (event stamp to writer-turn start), events dropped (counted) | ingress lag p99 with plugins on within 5 % (or 10 us, the larger) of plugins off; mailbox depth slope over the last 60 s <= 0 (no monotone growth, linear fit slope within noise, reported with its CI); queue drained within 10 ms of the generator stopping; writer never blocked (no wait on the background path) | both arms |
| M7 | **Memory per plugin** | RSS accounting from `/proc/self/smaps_rollup` (Pss, Anon, Rss) and `/proc/self/status` (VmRSS, VmHWM) after 1, 10 and 50 loaded plugins: (a) distinct components, (b) 50 instances of one component; idle, then after 1000 calls; plus the pooling allocator's reserved virtual size, per-thread stack and per-job `FrozenRows` size at 16 nodes and 64 nodes. Per plugin = (RSS(N) - RSS(0)) / N, linear fit over the three points | reported; reference gates from ADR-0026 PX-3: idle instance pair <= 64 KiB + guest state, 50 plugins x 2 lanes <= 120 MB RSS worst case | both arms; B' also reports the pool's threads |
| M8 | **Fault containment** | per row below, plus A1..A6, A13 alone and combined | all assertions hold, see below | both arms |
| M9 | **B' mechanics (WQ-6)** | wake latency of the writer -> plugin actor -> reply round trip p50/p99/max, futex vs `timerfd`/`AsyncFd`, spin 0 / 10 / 20 us, pool T = 2, 4, `cores - 2`, P and E cores, quiet and loaded; late wakeups (actual wake minus `D`) with `PR_SET_TIMERSLACK` 1 ns vs default 50 us; `FrozenRows` build cost at 16 nodes / 64 nodes; `pa_busy` rate; reply delay distribution (`queued` and `exec` apart) | reported; the shipped configuration is the cheapest that passes M1..M3; the `timerfd` variant replaces the futex if it meets M1..M3 | B' |
| M10 | **Correctness under delay** | (a) MQ-2 replay (the corpus of `plugin-fusion-math.md` section 3.3 regenerated with its seed) with randomized delivery delay of replies, 12,000 runs; (b) arrival-order permutation of 3 plugins' replies (PF-6) gives one fused entry; (c) A and B' differential: bit-equal facts on the same view (PF-1) | **0 false accepts**; 0 order dependence; 0 differences | both arms |
| M11 | **Cost and complexity** | LOC of the arm-specific crates (`tokei`, excluding tests and common), number of `unsafe` blocks, number of `Ordering` uses outside `common`, compile time of the arm crate | reported (judge criterion 5); no gate | both arms |

**M8 fault-containment assertions** (every one is an automated test with a fixed exit code):
1. Core writer: no stall above the M1 bound caused by a faulting plugin, on the quiet host, in any row; the
   **sum** of grants of one focus change never exceeds `B + n x c_max` at rho about 1 (A6).
2. A faulting or runaway plugin: fail-open (no facts, the rest of the chain published), the call's effects
   are dropped, a strike is recorded with the right `FaultKind`; **after the first wall overrun the plugin is
   demoted to background-only for 60 s**; 3 overruns in 60 s are one fault; the strike window, `Backoff`
   (30 s, 5 min) and `Disabled` follow the ADR-0026 section 2.5.4 table (the table is driven by the same
   exhaustive test in both arms, with the stale-reply and busy cells added).
3. Isolation: the other nine plugins' M3 coverage during the adversary's run stays within 1 percentage point
   of their benign value (under A the adversary is allowed to cost the others their grants, and that is
   **reported** as the A-vs-B' difference).
4. A3/A4: the oversize value is rejected **before** any copy or UTF-8 validation (an allocation counter
   proves no allocation of the guest size on the host side); one malformed-effect fault per call.
5. A5: zero page faults of file-backed code pages on writer-turn threads (`perf trace -F` or
   `getrusage` major/minor fault deltas on the writer thread, per turn), zero deserialize and zero file
   reads on writer turns (a `strace -f -e trace=openat,read,mmap` filter on the writer tid).
6. B' only: a panic in the plugin actor, a closed reply channel and a wedged plugin thread (no heartbeat)
   produce `ActorLost`, a replacement actor and a detached thread, never a writer stall above M1; a stale
   `slot_epoch` reply is dropped and counted and never revives a `Backoff` or `Disabled` slot.
7. No panic or abort of the process in any row (the harness runs each adversarial scenario in the same
   process as the writer and checks the process survives; a guest trap is an `Err`).

### 4.1 Host conditions (every measurement)

The bake-off is judged on **quiet-host** numbers. Every report row carries the condition label.

| Condition | Definition |
|---|---|
| `quiet` | `sudo bench/b1/quiet-host.sh on` applied (governor `performance`, idle states deeper than C1 off; sudo is granted for host tuning, CLAUDE.md), reverted with `off` at the end and the revert reported; `/proc/loadavg` 1-minute below 1.0 and `/proc/pressure/cpu` `some avg10` below 1.0 at the start and at the end of each run (a run that starts or ends above is **kept and labelled** `quiet-violated`, rerun beside it, never overwritten); no other cargo build, no benchmark of another agent; the live desktop's own load is recorded (`top -bn1` snapshot) and not touched. |
| `loaded` | the same host with the generator `harness/load/spin` running 8 busy threads at nice 0 plus 2 threads of 64 MiB memory-walk (check `free -g` and `/proc/pressure/memory` first; the host is shared with the live desktop, so total memory use of the load stays below 2 GiB). Reported separately; not a gate. |

Fixed settings for both conditions: kernel and wasmtime version in every report header
(`uname -r`, `wasmtime =49.0.3`), CPU model and core-class map (P/E from `lscpu -e`), governor
(`scaling_governor` of every CPU, recorded before and after), `/proc/self/timerslack_ns` of the writer
thread (must read 1 for B', both values reported for the WQ-6 slack comparison), tokio runtime thread
count, pool size T, the build (`--release`, LTO, `codegen-units=1`, `RUSTFLAGS` recorded), the seed. The
writer thread and the pool threads are **not pinned** in the gating runs (production does not pin them);
a pinned variant (writer on a fast core, pool on other fast cores) is run as a diagnostic and labelled.
Real-time scheduling (rtkit) is **not** used in the gating runs (B-5 decides the topology separately); a
labelled variant with the writer under SCHED_FIFO is reported as a diagnostic.

Run counts and statistics:
- **Warm-up**: 5,000 focus changes discarded (JIT-free but caches, pool and adaptive kappa_p settle); the
  discarded count is stored.
- **Samples**: >= 200,000 focus changes per gating scenario per run (so p99 has 2,000 tail samples and a max
  is meaningful); keypress scenario M4: 60 s per leg.
- **Runs**: >= 5 independent process runs per (arm, scenario, condition) on `quiet`; >= 3 on `loaded`.
  A p99 gate must hold in **every** quiet run; a max gate in **every** quiet run (no median hiding a bad
  run); M4 uses the median Δ over 5 interleaved pairs with its range stated. Counters (M3, M6) use all runs
  pooled and each run alone.
- A run that crashes, hangs or is killed by the harness's own 10-minute watchdog is **kept** and counted as
  a failed run, never retried silently.
- Order of runs is randomized per scenario (arm A and B' interleaved) so thermal and background drift hits
  both.

### 4.2 Raw data and report format

Results go to `docs/bakeoff/m18a-0-plugin-call-model/results/<YYYYMMDDTHHMMSSZ>-<arm>-<condition>-<scenario>/`
and are **never deleted or overwritten**; a rerun creates a new directory beside the old one.

- `meta.json`: host facts, condition label, loadavg and PSI before and after, governor before and after,
  versions, build flags, seed, scenario file hash, guest `.wasm` sha256 list, `.cwasm` compatibility hash,
  config (T, spin, wait mechanism, slack, N ms, kappa table).
- `stall.u32.zst`, `latency.u32.zst`, `lateness.u32.zst`: raw samples in nanoseconds (u32 little-endian, one
  per focus change / keypress / handoff), plus `events.jsonl` with one record per focus change (turn id,
  pairs asked/arrived, per-plugin outcome code, `pa_busy`, `exec` and `queued` ns, grant, fuel used).
- `summary.json`: p50, p90, p99, p99.9, max for each series, M1..M11 values, pass/fail per gate with the
  threshold text, counters (faults by kind, overruns, strikes, dropped replies, rejected effects).
- `report.md` per candidate (generated by `harness/report`): a gate table with the numbers and the raw
  data paths, then the figures (stall CDF and tail, facts-at-first-utterance per plugin, late wakeups), then
  every deviation from this spec. Gate verdicts are produced by a script from `summary.json`, not typed by
  hand.
- One cross-arm `docs/bakeoff/m18a-0-plugin-call-model/RESULT.md` written by the reviewer: the table of
  section 5, the verdict, and the fallback step if any gate failed.

## 5. Decision rule (copied from ADR-0026 r4, section 5, contested choice C-3)

> - **C-3 plugin placement: dedicated plugin actor with a bounded wait (B') vs synchronous `derive` in the
>   owner actor (A). Decided by the owner on 2026-10-24: B' is the design target; A is the fallback.**
>   The bake-off (slice **M18a-0**, the gate for every later slice) still runs both arms, but its job is
>   now to **confirm that B' meets the budgets**, not to choose between equals.
>   - **Arm B' (target)**: section 2.4.6. The writer hands the interactive batch to the plugin actors with a
>     frozen view and waits on the shared deadline `D` = 0.3 ms wall; on timeout it publishes without the
>     facts and admits the late facts under the PF-7 token check (refine via LB-4). Cost: one cross-thread
>     wake (about 5-20 us, to be measured) and the snapshot.
>   - **Arm A (fallback, kept as a measured reference)**: `derive` inside the writer turn, with all of
>     section 2.5 (progressive filling, kappa_int, wall accounting, size caps, pre-load).
>   - Fixture: Signal private session, P-2 + 9 benign dummies, then the PX-4 adversarial set A1..A6 and
>     A13 added one at a time and all together. Host conditions per `bench/b1/quiet-host.sh`, labelled.
>   - Criteria (reviewer judges on the measured numbers): (1) writer-turn stall per focus change, p99
>     and max, benign and adversarial; (2) facts at first utterance, overall and per plugin; (3) Δp99
>     keypress -> speech vs no plugins; (4) PX-7 background throughput; (5) writer-integration code size
>     and `unsafe` count; (6) **B' only**: reply delay distribution (queued, exec), `pa_busy` rate,
>     pool-size sensitivity (T = 2, 4, cores - 2), and the arrival-order test of PF-6.
>   - **Confirmation rule (r4).** B' is **confirmed** iff: stall, **measured from the start of the writer
>     turn**, max <= 0.35 ms (quiet-host gate; loaded-host max is reported separately, it is dominated by
>     preemption) and p99 <= 0.31 ms per focus change (benign and adversarial, with demotion active, timer
>     slack 1 ns), at most one wait per keypress path, benign Δp99 <= 0.3 ms, facts at first utterance on
>     the **benign** fixture (P-2 + dummies, quiet host) >= 99 % overall and >= 97 % for every plugin (on the
>     adversarial mix >= 97 % overall and >= 90 % per plugin), PX-7 ingress lag unchanged, and 0 false
>     accepts in the randomized-delay MQ-2 replay. A's numbers are reported for the record and **do not by
>     themselves overturn B'** (A being faster on a gate B' also passes is not a reason to switch).
>   - **If B' is not confirmed** the reviewer reports which gate failed and why; the fallback needs a new
>     owner decision row. Candidates, in order: (i) tune B' (pool size, spin time, grant rule MQ-4) and
>     rerun the failed gate; (ii) arm A if it passes its former rule (adversarial stall p99 <= 0.45 ms and
>     max <= 1 ms with demotion, benign Δp99 <= 0.3 ms, facts at first utterance >= 99 %); (iii) arm C: A
>     for T2 (first-party) and B' for T3.
>   - Budget numbers (B-1..B-9, kappa table) are frozen in the M18a-0 exit report, after WQ-1..WQ-3.

Mapping of the spec's metrics to the rule: M1, M2 = stall; M3 = facts at first utterance; M4 = benign
Δp99; M5 = one wait per path; M6 = PX-7; M10 = MQ-2 replay; M8, M9, M11 = judge criteria and the (i)
tuning loop. Sections 4 and 7 add one stricter spec gate (M4 <= 0.1 ms) which is reported against the rule,
not substituted for it.

## 6. Budget per candidate

Candidates are throwaway prototypes, built by `rust-engineer` (arm-a) and `rust-engineer` or
`wasm-engineer` (arm-bprime) in **separate git worktrees**, each in its own crate under
`prototypes/m18a-0/` (section 2.1), against the **same** `m18a0-common`, guests and harness.

| Item | Budget (per candidate) |
|---|---|
| Time | **<= 1.5 working days** from the day the harness (below) is merged; arm A is expected in about 0.5 day, B' in 1.5 days. A candidate that is not measured by the deadline is reported as is, with what is missing |
| Arm-specific code | arm A <= 600 LOC; B' <= 2,500 LOC (runner, pool, futex wait, `timerfd` variant, supervisor, `slot_epoch` rules), excluding tests and `m18a0-common` |
| `unsafe` | allowed only for `futex` / `prctl` / `timerfd` syscalls (or `rustix` with no `unsafe` at the call site); every block counted and justified in the report |
| Dependencies | wasmtime `=49.0.3` (cranelift, pooling-allocator, component-model; **no epoch interruption**), wit-bindgen 0.46, `rustix` or `libc`, `tokio`, `hdrhistogram`; nothing else without a note |
| Scope exclusions | no D-Bus, no real speech, no real AT-SPI, no production `gorca-ext-host` changes, no R lane, no `present.edit` |
| Allowed tuning | pool size T, spin duration, wait mechanism (futex or `timerfd`), thread affinity preferences, `N` ms flapping cap, the MQ-4 parallel grant (labelled variant). Not allowed: changing any threshold, the shared code, the guests or the metrics |
| Gates | `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test` in the prototype workspace; no `unwrap()` in non-test code |
| Reporting | `docs/bakeoff/m18a-0-plugin-call-model/<arm>/NOTES.md` (what was built, what was cut, deviations) and the generated `report.md` |

If the budget is blown on one arm, the judge compares only completed metrics and lists the rest as
`NOT MEASURED` (a missing gate is a failed gate for the confirmation rule).

## 7. What the shared harness must provide

Built **first** (before the candidates start) by `perf-engineer` (timing capture, load, reports, run
scripts) and `test-engineer` (guests, scenarios, correctness corpora, fault tests), under
`docs/bakeoff/m18a-0-plugin-call-model/harness/`. The crate `m18a0-common` of section 2.2 is part of it
(wasm-engineer reviews the wasmtime configuration). Budget: 1 working day; the candidates start when the
harness `smoke` scenario passes on a no-op runner.

```text
harness/
  common/            m18a0-common (section 2.2): engine, pre-load, fuel, wall, strikes, slot machine,
                     MQ-3 budget, MQ-2 token, stub writer, stub presenter and sink, WLaneRunner trait,
                     NoopRunner (smoke) and the exhaustive SlotState test driver
  wit/               the checked subset of wit/gorca-ext-0.10 + check-subset.sh
  guests/            benign/{signal-p2, dummy-1..9}, adversarial/{a1-page-stride, a2-pointer-chase,
                     a3-huge-string, a4-huge-return, a5-cold-load (a benign guest, cache evicted by the
                     driver), a6-full-grant, a13-subtree-loop, a7-patch-match, a8-refresh-each, a12-self},
                     build.sh (reproducible, writes sha256 list)
  scenarios/         TOML files: benign, adv-<id>, adv-all, adv-each-added, keypress-onoff, k1k2-path,
                     flap-focus, storm-10k, storm-50k, memory-1-10-50, wq1, wq2, wq3, wq6-*, mq2-replay
  load/              spin (busy threads), memwalk (bounded), storm generator (events/s, seeded,
                     open-loop with scheduled send times to avoid coordinated omission)
  timing/            clock and pinning helpers, raw sample writer (u32 ns, zstd), HDR histogram,
                     per-thread context-switch and fault counters, lateness probe for waits
  host/              prepare.sh (calls bench/b1/quiet-host.sh on/off, checks loadavg and PSI, records
                     meta.json, refuses to start a `quiet` run when the checks fail), evict-cache.sh
                     (posix_fadvise on the private cache file only)
  corpora/           MQ-2 replay generator (seeded, 12,000 runs), PF-6 permutation test, PF-1
                     differential test
  report/            summary.json builder, gate evaluator (thresholds from one TOML file that mirrors
                     section 4), report.md generator, cross-arm comparison table
  run-all.sh         runs (arm x scenario x condition x run index) in randomized order, writes results/
```

Harness requirements that are easy to get wrong:
1. **Open-loop load.** Events and keypresses are generated at scheduled instants; latency is measured from
   the scheduled instant, so a stalled writer cannot hide its stall (no coordinated omission).
2. **One clock.** All stamps are `CLOCK_MONOTONIC` ns; the stamp reads cost <= 40 ns and are subtracted from
   nothing (reported as overhead).
3. **No instrumentation in the wait path** beyond two clock reads and a plain store to a pre-allocated ring
   (no allocation, no formatting, no logging inside a handoff).
4. **Determinism.** Every scenario takes a seed; the focus stream, the dummy mix and the replay corpus are
   functions of it. A report names the seeds.
5. **The harness self-tests.** `smoke` with `NoopRunner` shows stall p99 under 10 us; a deliberately slow
   runner (sleep 1 ms) must fail M1; a runner that ignores the deadline must fail M1; a stale-epoch reply
   injector must show the drop counter. A harness that cannot fail a bad runner is not accepted.
6. **Timer slack check.** The harness asserts `/proc/self/timerslack_ns == 1` on writer and pool threads
   in every B' gating run and refuses to evaluate M1 otherwise (the default-slack comparison is its own
   labelled scenario).
7. **Private session only.** No access to the live session or the a11y bus; `drop_caches` is never used.

## 8. Choices this spec makes that the ADR did not fix

1. **M4 threshold.** The task brief asks for keypress to speech Δp99 <= 0.1 ms; ADR-0026 r4 (section 2.10
   PX-6 and section 5) says <= 0.3 ms. The spec keeps both: <= 0.1 ms as a spec gate (the benign waits
   return at the countdown, far below 0.3 ms) and <= 0.3 ms as the decision-rule gate. A result between
   the two confirms B' under the ADR and is flagged to the owner as a miss of the tighter target.
2. Sample counts, run counts, warm-up and the all-runs-must-pass rule (section 4.1).
3. Stall includes the stub's own non-plugin work (<= 5 us) and is not corrected for it.
4. The shared `m18a0-common` crate so both arms differ only in the runner (section 2.2).
5. A5 uses a private cache file with `posix_fadvise(DONTNEED)`, as the ADR says, and a strace/fault-count
   proof; the pass is "no writer-turn fault", not a latency number.
6. Denominator of M3 excludes pairs whose slot is `Backoff` or `Disabled` (a disabled plugin is judged by
   M8, not by coverage).
7. `loaded` definition (8 spin + 2 memwalk threads, memory below 2 GiB) is a diagnostic, not a gate.
8. The default flapping cap `N` = 20 ms is tuned in the allowed-tuning list, and the reported value is
   frozen in the exit report.
