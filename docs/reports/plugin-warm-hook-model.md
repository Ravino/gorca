# Plugin warm hook and startup preload: the quantitative model

Date 2026-10-25, math-researcher. Input to the architect's WIT contract for a plugin **warm hook**
(replacing the replay of the last job, `warm_on_arm`) and a **startup preload** policy. Builds on
ADR-0026 §2.4.7 (SPS), `docs/reports/plugin-statistical-scheduler.md`, `m18a-3-sps.md`,
`m18a-4-plugin-costs.md`.

Code (all self-testing, `. ./env.sh` then `python <file>` in `tools/research/plugin_sched/`):

| File | What |
|---|---|
| `warm_model.py` | cold-call decomposition and decay fit; `--fit FILE...` fits the proposed microbench output |
| `sps.py` | SPS reference, extended with the `warm` action (`keydown_plan`, `observe_warm`, warm-aware `dispatch` and `observe_reply`). The old paths are bit-identical: golden vector unchanged and the 17 594-call `dump_replay.py` corpus byte-identical before/after. New golden vector `_selftest_warm` |
| `warm_sim.py` | policy simulation on the measured host model (scenario grid, `--one` for one scenario) |
| `warm_tab.py` | tables from the grid JSON |
| `preload.py` | startup preload model |

Results: `docs/reports/plugin-warm-hook-results/` (`warm_sim.json`, `warm_sim.txt`, `tables.md`).
Tags: **[M]** measured on this host (M18a-4 matrix, load1 9-16, governor powersave), **[F]** fitted
to measurements, **[P]** proved, **[H]** assumption, **[S]** simulation only.

## 0. Answer in one paragraph

A warm hook is worth having, but it is the **third** lever, not the first. The cold-call cost after
idle (131 µs at 100 ms, 6.5 µs warm) is about half a **step paid whenever the worker sleeps or is
preempted** (F ≈ 58 µs) and half **eviction that grows with idle time** (E ≈ 68 µs, T ≈ 25 ms on the
loaded host). A hook removes both, but if the worker then **parks** for the ~3 ms until the focus
change, the step comes back and the parked start (≈110 µs without the lease) dominates: without the
`cpu_dma_latency` lease, hook-then-park is useless (facts-at-first-utterance 0.00-0.15 in the mix
workload), and a hook only helps workers that also spin. **With the lease**, hook-then-park
(no spinning) reaches the first-utterance share of SPS+spin+hook at **0.3-0.9 ms CPU per key instead of
~90 ms for LABELS** and lifts SPS by +0.15-0.20 on a loaded host (0.61 → 0.81, P cores 0.72 → 0.87),
+0.01-0.02 on a quiet host. Order of impact: **lease > P-core placement > hook > replay**. The hook
beats the replay at equal coverage because the scheduler can aim it at non-armed workers, and it
removes the duplicate-call semantics. Preload everything first-party/trusted that is not
`refinement`, plus third-party by the user's own history: about 9 MiB and 20 ms of background CPU
for 40 installed instances; it removes 72-75 % of first-activation misses.

## 1. Decomposition of the cold call

### 1.1 Data and fit

Isolated call, one guest call with 13 host imports, same input each time
(`bench/results/m18a4/call-matrix-v1.txt`, p50 µs):

| gap / mode | 1 thread | 10 threads |
|---|---|---|
| 0, back to back | 6.5 | 28.6 |
| 3 ms spin | 9.7 | 24.9 |
| 3 ms sleep | 72.1 | 61.5 |
| 20 ms sleep | — | 86.6 |
| 100 ms sleep, 0 / 0.3 / 3 / 20 ms pre-spin | 131 / 131 / 133 / 108 | — |
| 3 ms spin, pinned P / E / LP-E | 16.7 / 20.3 / 39.8 | — |
| 3 ms sleep, pinned P / E / LP-E | 73.8 / 89.3 / 204.9 | — |

Model [F] (`warm_model.py`), for the per-call host overhead `O` after `τ` without a touch of the call
path on that thread:

```
O(τ) = c_w + F·[the thread slept or was preempted] + E·(1 − e^(−τ/T))      (asleep)
O(τ) = c_w + E_s·(1 − e^(−τ/T))                                            (awake, spinning)
c_w = 6.5 µs   F = 58.0 µs   E = 67.8 µs   T = 25.3 ms   E_s = 28.6 µs
```

Three sleep points (the 20 ms point scaled from 10 threads to 1 by 72.1/61.5) fit exactly. Three
parameters on three points is no test of the functional form: this is an **interpolation**, and the
microbench in §1.4 is required to validate it.

### 1.2 Components and who removes them

| Component | Size (P core) | Evidence | Lease | P placement | Spin (pre-wake) | Hook |
|---|---|---|---|---|---|---|
| Thread wake: futex + C-state exit (outside the call) | ~110 µs p50 parked pick-up (k1), 6 µs hot | [M] k1 107-116 vs 6.3 | **yes** (C1 only: 8-18 µs [M m18a-2 probe]) | no | **yes** | only if the worker stays awake after it |
| Frequency / uncore ramp | ≤ 23 µs | [M] 100 ms gap: 0.3 vs 20 ms pre-spin 131 → 108; 3 ms pre-spin no gain | no | partly (P clocks higher) | only spins ≥ ~20 ms | no (a 100 µs hook does not ramp HWP) |
| Deep-idle / preemption step F (L1/L2 flush on power gating, another task's working set) | ~58 µs | [F] | **the C-state fraction φ of it** (φ unknown, §1.4) | no | **yes** | yes, but it **returns on the next sleep** |
| Eviction E(1 − e^(−τ/T)) of code, data, TLB, branch state | 0 → 68 µs over ~25 ms (load-dependent) | [F] | no | less contention (L2 per P core) | slows it (E_s 29 µs) | **yes**, regrows with the lead time |
| Core type | ×1.21 E, ×2.6-2.8 LP-E | [M] pinned | no | **yes** | no | no (a cold hook on LP-E costs ~340 µs) |
| Fresh per-job data (new snapshot rows, `ModelView`) | 5-40 µs (15 assumed) | [H] residual of the replay (101 → 70) | no | no | no | **no**: it does not exist before the focus change |
| wasmtime instance state | ~0 in steady state | [M] no per-call instantiation, 283 faults / 19 000 calls | — | — | — | only first call after instantiation or a fault (lazy guest init): this is the **preload** job (§3) |
| Page-cache eviction of file-backed code (`.cwasm` mmap) after long idle under memory pressure | 0, or ms per major fault | [H] not observed | no | no | no | yes, if the hook runs after the reclaim; better: keep code in anonymous memory (§4 C-10) |

So of the 131 µs cold call at 100 ms idle: 6.5 µs is the floor, ~58 µs is the sleep step, ~66 µs
is eviction. A full-coverage hook brings the next call to 6.5-10 µs **only if the worker stays
awake** until the job; if it parks, the call is back to `c_w + F' + E(1 − e^(−L/T))` (72 µs at
L = 3 ms without the lease, 43 µs with the lease at φ = 0.5, 20 µs at φ = 0.9).

Real-call overhead after a full-coverage hook, by lead time `L` (hook → job), `warm_model.py`:

| L | spin after hook | park, no lease | park, lease φ=0 | φ=0.5 | φ=0.9 |
|---|---|---|---|---|---|
| 0.3 ms | 6.8 | 7.3 (no C2 yet) | 65 | 36 | 13 |
| 1 ms | 7.6 | 67 | 67 | 38 | 15 |
| 3 ms | 9.7 | 72 | 72 | 43 | 20 |
| 10 ms | 15.8 | 87 | 87 | 58 | 34 |
| 30 ms | 26.4 | 112 | 112 | 83 | 59 |
| 100 ms | 34.6 | 131 | 131 | 102 | 79 |

### 1.3 What a hook must do to be effective

* **Run on the plugin's own worker** (same thread, same core as the coming job). A hook on another
  core warms the wrong L1/L2.
* **Touch the code the real call runs**: the guest's derive hot path *and* the host import path.
  In the bench the 13 imports are 36-48 µs of a cold call against 5 µs warm, i.e. 30-40 % of the
  cold excess. A guest-only hook that calls no import leaves that part cold (coverage κ ≈ 0.5-0.6
  [H]). Two ways to cover it: (a) the hook may call read-only imports against a **frozen view**
  (the previous snapshot) supplied by the host; (b) the host runs a native **import self-warm**
  (each import implementation once on the frozen view, no guest involved) right before the hook.
  (b) needs no contract; do both.
* **Touch the data the real call reads** (the plugin's own tables and caches), not more: the cold
  excess of 60-125 µs corresponds to roughly 1-2 k cache lines (64-128 KiB) refilled from L3 at
  low clock [H]. Cap the touched memory at ~256 KiB: more than the derive's footprint only evicts
  other workers' lines (an E-core cluster shares one 4 MiB L2 between four cores).
* **Be short**: a cold hook itself costs about one cold call (≈130 µs on a P core after 100 ms,
  ×2.6 on LP-E). Its guest work should be a fraction γ of a real call (γ = 0.25-1 changes the CPU
  by 0.1 ms per key and the facts by nothing, §2.4).
* **Coverage κ** is the fraction of the cold excess the hook removes. Calibration: the replay in the
  loaded bench gave κ ≈ 0.34 (101 → 70 µs); a quiet-host replay should approach 0.8-0.9 [H]. The
  simulation sweeps κ ∈ {0.34, 0.75}.

### 1.4 Microbench the engineers should run (fits the model, decides φ)

Extend `plugin_call_cost` (`crates/gorca-daemon/tests/`), one CALL line per cell, p50/p95/p99:

1. **Gap sweep**: `GORCA_CALL_GAP_US` ∈ {0, 100, 300, 1 000, 3 000, 10 000, 30 000, 100 000,
   300 000, 1 000 000} × mode {spin, sleep}.
2. **Lease**: hold `/dev/cpu_dma_latency` open at {none, 20, 0} µs during the run (owner approved the
   lease; the bench only holds it for its own lifetime). This measures **φ**: the share of F that is
   C-state, the single most decision-relevant unknown.
3. **Core**: pinned P / E / LP-E (topology from `/sys/devices/system/cpu/cpu*/cpu_capacity` or
   `cpufreq/cpuinfo_max_freq`).
4. **Hook lead**: run a warm call (replay, then the real hook) at lead L ∈ {0.1, 0.3, 1, 3, 10} ms
   before the timed call, then spin or sleep until it. This measures κ and the post-hook decay
   directly.
5. **Background**: idle host (`load1 < 1`), `stress-ng --cache 1` on another core of the same L2
   cluster, and on the same core (affinity), to split F into C-state and preemption.
6. **Counters** per call with `perf_event_open` (group, `paranoid` restored to 3 after): cycles,
   ref-cycles (→ frequency), instructions, L1-icache-load-misses, L2/LLC misses, iTLB and dTLB walk
   cycles, page faults; and per-cell deltas of `/sys/devices/system/cpu/cpuN/cpuidle/state*/usage`
   to know which idle state was entered.

Fit: `python warm_model.py --fit <files>` (sleep-gap CALL lines → F, E, T, rms). Accept the model if
the rms is ≤ 10 % of the 100 ms value on the 10-point sweep; otherwise move to a two-time-constant
eviction term. Run with `load1 < 4` (the rule of m18a-4), and keep any failing run next to the rerun.

## 2. Firing policy

### 2.1 When

The hook must run at least its own duration `h` before the job, and as late as possible after that
(the post-hook decay). Let `L` be key-down → focus change (dispatch). With `L ~ LN(3 ms, 0.7)` [H,
sim.py], the expected delay a real job suffers from colliding with a still-running hook is
`E[(h − L)+]`:

| h | P(L < h) | E[(h − L)+] |
|---|---|---|
| 200 µs | 5.5e-5 | 0.002 µs |
| 300 µs | 5.0e-4 | 0.02 µs |
| 500 µs | 5.2e-3 | 0.5 µs |
| 1 ms | 5.8e-2 | 14 µs |
| 2 ms | 0.28 | 178 µs |

So **key-down is the right trigger** for any hook ≤ 300 µs wall, and a hook longer than ~0.5 ms is
not acceptable on the key path. The `L` distribution is per application and must be measured
(Firefox and VTE differ); the bound is what the contract should state, not the exact numbers.

| Trigger | Verdict | Why |
|---|---|---|
| Key-down of a navigation-class key (Nav, Tab, Enter) | **yes, primary** | lead ≈ 3 ms ≫ h; SPS learns α per key class, typing learns α ≈ 0 and fires nothing |
| Key-down of typing / editor arrows | **no** | α ≈ 0 anyway; also a keystroke-timing side channel to third-party code (C-6) |
| Focus moves to a new scope (app/tab switch, document load) | **yes, once per scope entry, only for that scope's plugins** | the next key is 0.2-2 s away, so L1/L2 decay back, but the hook still restores TLB/L3, page-ins and lazy state after a long absence; cheap (one hook per plugin per scope entry) |
| Periodic idle timer | **no** | keeping E small needs a period ≤ T ≈ 25 ms: 130 µs / 25 ms = 0.5 % of a core per plugin, 5 % for ten, for a 3-30 µs gain the key-down hook gets anyway |
| Auto-repeat | covered by the frequency cap (one hook per plugin per 20 ms) | |

### 2.2 Which plugins: the selection rule (in `sps.py`, `keydown_plan`)

At key-down in context `x`, after K and the armed set are chosen exactly as today, for every
plugin that is wait-eligible and spin-eligible (not `refinement`), not excluded, and not hooked in
the last `warm_gap_ms`:

```
state_i = hot if armed else parked
g_i     = α_i[x] · v_i · max(0, F_i^warm(dw | state_i) − F_i(dw | state_i))     (expected facts gained)
c       = q50 of the pooled hook-CPU histogram (prior 150 µs)
fire, in order of g_i / c descending, while g_i ≥ θ_w (0.01) and the warm bucket ≥ c;
at most one third-party hook per key-down.
```

`F^warm` uses the plugin's warm exec histogram once it has 8 samples, before that the cold
histogram shifted left by the pooled cold-warm difference `warm_delta` (prior 60 µs, integer EWMA
over plugins). Dispatch uses the warm histogram for hooked jobs (deadlines, `t*`, holds). All
integer, deterministic, no clock reads; the Rust port needs two more 16-bucket histograms (one per
plugin, one pooled) and two counters.

*Proved [P]:* with a common hook cost `c`, the greedy order is optimal for the per-key knapsack
(all items have the same weight, so taking the largest gains first maximises the sum under any
budget); with per-plugin costs it is the fractional-knapsack order, within one item of the optimum.
Over time the bucket makes the long-run hook CPU ≤ refill rate + bucket size per window (same
argument as the spin bucket, ADR-0026 §2.4.7.3).

### 2.3 CPU budget

A hook costs ~0.13-0.35 ms of CPU; spinning one worker for the key → focus window costs ~3 ms and,
on keys without a focus change, the full 50 ms window. Measured in the simulation, hook CPU is
**0.1-1.4 ms per key** (0.5-7 ms/s at 5 keys/s, under 1 % of one core). Proposed bucket: refill
30 ms/s (3 % of one core), burst 60 ms, separate from the spin bucket. It never binds in the
simulated workloads; it exists for a held key or a pathological app.

### 2.4 Simulation (`warm_sim.py`, 3 000 keys per cell, same random stream for all policies)

Cells: facts at first utterance (per-plugin share of waitable benign plugins) / writer stall p50 µs /
CPU ms per key. "mix" = the ten-plugin synthetic workload of `sim.py`; pre-send/reserve 60/52 µs
(M18a-4 measured on the model's scale); κ = 0.75; full grid (κ 0.34, quiet host, pre-send 20/24,
the bench workload) in `plugin-warm-hook-results/tables.md`.

**Loaded host (λ_E = 1, the matrix's conditions):**

| lease | place | LABELS | SPS | SPS+REPLAY | SPS+HOOK | HOOK-ONLY (no spin) | LABELS+HOOK |
|---|---|---|---|---|---|---|---|
| off | any | 0.41 / 300 / 90 | 0.01 / 112 / 1.6 | 0.05 / 112 / 5.6 | 0.06 / 112 / 7.1 | 0.00 / 112 / 0.1 | 0.92 / 251 / 92 |
| off | P | 0.62 / 300 / 91 | 0.01 / 112 / 4.0 | 0.20 / 190 / 10.1 | 0.25 / 215 / 11.7 | 0.02 / 112 / 0.2 | 0.96 / 233 / 92 |
| on | any | 0.69 / 300 / 94 | 0.61 / 300 / 0.4 | 0.61 / 300 / 0.4 | **0.81** / 277 / 1.2 | **0.81** / 277 / 0.9 | 0.93 / 242 / 95 |
| on | P | 0.81 / 296 / 90 | 0.72 / 281 / 0.2 | 0.72 / 281 / 0.2 | **0.87** / 259 / 0.9 | **0.87** / 259 / 0.8 | 0.97 / 226 / 91 |

**Quiet host (λ_E = 0.3) [H]:**

| lease | place | LABELS | SPS | SPS+REPLAY | SPS+HOOK | HOOK-ONLY | LABELS+HOOK |
|---|---|---|---|---|---|---|---|
| off | any | 0.79 / 300 / 90 | 0.32 / 260 / 20 | 0.35 / 191 / 21 | 0.45 / 300 / 21 | 0.11 / 300 / 0.2 | 0.94 / 234 / 91 |
| on | any | 0.88 / 269 / 94 | 0.86 / 278 / 0.4 | 0.86 / 278 / 0.4 | 0.87 / 271 / 0.7 | 0.88 / 266 / 0.4 | 0.95 / 226 / 95 |
| on | P | 0.94 / 249 / 90 | 0.92 / 258 / 0.2 | 0.92 / 258 / 0.2 | 0.93 / 250 / 0.3 | 0.93 / 250 / 0.3 | 0.97 / 214 / 91 |

Lease effect φ (mix, P, κ 0.75, quiet): SPS 0.83 / 0.92 / 0.96 and HOOK-ONLY 0.85 / 0.93 / 0.96 at
φ = 0 / 0.5 / 0.9. Sensitivity: fresh-data cost D_f 5 → 40 µs moves HOOK-ONLY 0.95 → 0.86; hook
work γ 0.25 → 1 changes CPU by ≤ 0.1 ms/key and the share by 0.

Readings:

1. **Without the lease nothing parked works.** The parked start (~110 µs) plus the returning step F
   exceed the window; SPS waits for nobody (share ≈ 0, as in the first M18a-3 run). A hook then
   only helps spinning workers (SPS+HOOK 0.25 vs SPS 0.01 on P cores), and LABELS keeps its
   advantage only by spinning every worker at ~90 ms CPU per key.
2. **With the lease the hook replaces spinning.** HOOK-ONLY equals SPS+HOOK within 0.005 at
   0.4-0.9 ms/key, against 90 ms/key for LABELS; on a loaded host it adds +0.15-0.20 to SPS, on a
   quiet host +0.01-0.02 and ~10 µs of writer stall p50.
3. **Hook ≥ replay.** At the same κ the hook wins because the scheduler aims it at parked workers;
   under the lease SPS arms almost nobody, so the replay (armed workers only) does nothing (0.61 =
   0.61). The replay is only useful in the no-lease, spinning regime.
4. **LABELS+HOOK** (spin everything and hook everything) is the share ceiling (0.92-0.99) but costs
   the full spin CPU; the gap to HOOK-ONLY under the lease is 0.10-0.13 on a loaded host, 0.04-0.07
   on a quiet one.
5. **Writer stall p99 is the 300 µs clamp** in almost every cell: whenever one waited plugin is late,
   the writer waits to `D`. p50 is the informative latency column.

**Calibration against the bench** (bench10 workload, loaded, any core, no lease): model SPS 0.46,
REPLAY 0.57 (κ 0.75) / 0.55 (κ 0.34), LABELS 0.79; measured SPS 0.37-0.45, with warm 0.42-0.45,
LABELS 0.92. The model is about right for SPS, **optimistic for the replay** and pessimistic for
LABELS by ~0.13; treat cell differences under 0.05 as noise.

## 3. Startup preload

Cost per instance [M wasm-plugin-cost-spike.md]: deserialize 0.1-0.2 ms, instantiate 3-9 µs,
RSS 14-18 KiB plus touched guest memory; plus its lazy init (the first hook call) [H LN(0.3 ms, σ 1)]
and a worker thread. Benefit: a non-resident instance misses the first utterance of its first
activation (~316 µs cold start + lazy init, m18a-1 review), once per session per instance.

`preload.py` (40 instances, Zipf popularity carried over between sessions with 30 % reshuffle,
labels correlated with popularity, 30 % third-party; per-session averages, 200 sessions):

| policy | misses, 50-activation session | misses, 600-activation session | preloaded RSS | startup CPU |
|---|---|---|---|---|
| none | 19.1 | 39.6 | 0 | 0 |
| first/trusted `interactive` | 16.3 | 33.6 | 1.6 MiB | 4.9 ms |
| + unlabelled first/trusted | 7.7 | 15.8 | 7.8 MiB | 16 ms |
| history top-16 + `interactive` | 9.1 | 21.9 | 5.5 MiB | 13 ms |
| **recommended: first/trusted non-refinement + history top-16 (any tier)** | **4.8** | **11.0** | **9.3 MiB** | **20 ms** |
| all | 0 | 0 | 12.8 MiB | 29 ms |

Policy:

* **Preload at startup**, off the key path (a background task after the core is ready, idle I/O
  priority, never blocking the first keypress): every first-party and trusted plugin that is not
  `refinement`. Preload = deserialize + instantiate + one **init** call of the hook (lazy guest
  init) under a larger, separate budget (C-9).
* **Third-party: never on its own label** (a self-label costs nothing to claim and would let any
  plugin run code at boot). Preload a third-party instance only on evidence from the user's own use:
  top-H by decayed activation count persisted from earlier sessions (half-life 512 activations, as
  in the retention policy), within a third-party budget (proposed 32 MiB, 20 ms).
* **`refinement`**: load on first use; it never affects the first utterance.
* **Scope-driven load**: when a scope's application appears (AT-SPI app registered, Newton
  equivalent), load its plugins then; this covers apps started after gorca. Same rules.
* **Eviction**: RAM is cheap here ("speed beats frugality"), so keep instances resident by default,
  and the dLFU + TinyLFU retention of `plugin-statistical-scheduler.md` §5.6 decides who stays
  under a budget. Evict on **memory pressure** (MemAvailable < 4 GiB or PSI `some avg10` > 10 %):
  `refinement` first, then third-party, then unlabelled, then `interactive`, lowest decayed count
  first within a class. Evict an instance whose scope's app exited after a grace of 5 min. A faulted
  instance is rebuilt off the turn (already the case).

The value of preload is front-loaded (first impression of each app) and small per session
(5-7 % of activations in the long session); the cost is negligible on this host. "All" is also
acceptable for first-party; the third-party rule is about trust, not cost.

## 4. Constraints the contract must give (for the architect)

| Id | Constraint | Number | Why |
|---|---|---|---|
| C-1 | **Max duration.** Hook fuel ≤ the fuel of 50 µs of warm guest work (1/4 of the 200 µs interactive cap); wall time on a P core ≤ 200 µs, cold misses included. Overrun: no fact lost; hooks disabled for that plugin with exponential backoff (2^m keys, cap 256). | 50 µs fuel / 200 µs wall | E[(h − L)+] ≈ 0 for h ≤ 300 µs; 14 µs at 1 ms (§2.1). Fuel bounds instructions, not cache misses, so the wall bound is a host check. |
| C-2 | **No side effects.** No import that emits facts, speech, braille, storage writes, network, timers or `arm`. Allowed: read-only imports on a host-supplied **frozen view** (the last snapshot), at most 16 calls per hook. The host drops anything the hook returns. | ≤ 16 read-only imports | Warms the host import path (30-40 % of the cold excess) without effects. |
| C-3 | **Idempotence / no semantic state.** `derive(x)` must give the same output whether 0, 1 or k hooks ran before it; the hook may fill caches, never change semantic state. Test: property test with hooks interleaved at random. | — | The host may skip, repeat or drop hooks at will (budget, caps, faults). |
| C-4 | **Host-driven only, frequency-capped.** A plugin cannot request a hook. Per plugin ≤ 1 per 20 ms (≤ 50/s); per key-down ≤ 1 third-party hook; never for `refinement`; warm bucket 30 ms/s, burst 60 ms, separate from the spin bucket. | 20 ms, 1, 30 ms/s | Bounds CPU and the timing channel. |
| C-5 | **Optional export in its own interface.** A component without it gets no hook (the host still self-warms its import path). Put it in a new exported interface the host probes for: adding a function to an existing exported interface breaks old guests, and WIT records are not extensible. | — | Compatibility with the pinned first-party plugin and future third parties. |
| C-6 | **No input beyond a coarse hint.** No job data; at most the scope id. Not the key class, and no hook on typing or editor-arrow keys, nor while focus is in a protected (password) field. | — | A key-down hook tells the plugin *when* a key is pressed: a keystroke-timing side channel. |
| C-7 | **Same worker.** The hook runs on the plugin's own worker thread (with P placement: on its P core) and the real job follows on it. | — | Caches warmed elsewhere are useless. |
| C-8 | **Accounting and faults.** Hook CPU counts against the plugin's CPU/storm budget and is reported separately; a trap goes through the same strike machine as a real fault (the security review of `warm_on_arm`), plus it ends hooks for that plugin. | — | One mechanism for misbehaviour. |
| C-9 | **Init mode at preload.** The same export, called once at load with an `init` flag, off the key path, with a larger budget (e.g. 5 ms fuel, 50 ms wall, idle priority) for lazy init. | 5 ms / 50 ms | Preload needs the lazy init done before the first activation. |
| C-10 | **Host side, not contract:** keep compiled code in anonymous memory (deserialize from bytes, not a file mmap) or `mlock` the code of interactive plugins (~150 KiB each), so that a long idle under memory pressure cannot turn the first call into major faults. | — | [H] not observed; cheap insurance. |

## 5. Proven vs heuristic

* **[P]** Greedy-by-ratio selection is optimal per key for equal hook costs and within one item for
  unequal ones; the bucket bounds long-run hook CPU. The `sps.py` extension leaves every existing
  decision bit-identical (golden vector and the 17 594-call replay corpus compared byte for byte).
* **[F]** F, E, T come from three points on a host at load1 9-16: an interpolation, not a validated
  law. φ (the C-state share of F) is unknown; the lease conclusions hold for φ ≥ 0 because the lease
  also removes the ~100 µs parked wake, but their size depends on φ.
* **[H]** κ for a real hook (0.75), fresh-data cost (15 µs), the key → focus lag distribution
  (LN 3 ms, 0.7), core mix without placement (0.55/0.40/0.05), the quiet-host factor λ_E = 0.3,
  and every preload number except the per-instance costs.
* **[S]** All policy comparisons are simulation; the model is within ~0.1 of the measured bench for
  SPS, optimistic for the replay, pessimistic for LABELS.

## 6. Reproduce

```sh
. ./env.sh; cd tools/research/plugin_sched
python warm_model.py                     # decomposition, fit, lead table
python sps.py                            # self-tests incl. the warm golden vector
python dump_replay.py /tmp/x.txt         # must stay byte-identical to the committed SPS-6 corpus
python warm_sim.py --n 3000 --jobs 10 --out ../../../docs/reports/plugin-warm-hook-results/warm_sim.json
python warm_tab.py ../../../docs/reports/plugin-warm-hook-results/warm_sim.json
python warm_sim.py --n 2000 --one workload=bench10,lease=1,place=P   # one scenario
python preload.py
```
