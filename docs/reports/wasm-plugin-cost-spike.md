# WASM plugin cost spike (M18a)

Spike crate: `tools/spikes/wasm-plugin-cost/` (own `[workspace]` tables, outside the gates; `run.sh` builds and
runs everything). wasmtime `=49.0.3` (the workspace pin), Component Model, guest = Rust `wasm32-wasip2` with
wit-bindgen 0.46, release `opt-level=s`, LTO, `panic=abort`; component 49,989 bytes (includes Rust std; the
guest's own code is a few KB). Engine features: cranelift, pooling-allocator, async, parallel-compilation.

## Conditions

- Host: 16 CPUs, governor `performance` (already set; `quiet-host.sh` not used: it needs sudo and changes
  sysfs system-wide, not required for these numbers). The host was **not quiet**: loadavg 9 to 14 (the user's
  desktop plus other agents' builds) during all runs. Tails (p99, max) are therefore pessimistic; p50 is stable
  across runs.
- Pooling allocator: 512 instances, 32 MiB max memory, as in `gorca-ext-host`. Fuel and epoch are both enabled
  for the call benchmarks (the production configuration), per-call fuel set once to a huge value.
- WASI imports are stubbed as traps (`define_unknown_imports_as_traps`), no wasmtime-wasi. Consequence: a guest
  `panic!` ends in a host-trapped WASI stderr write, not an `unreachable`; either way the call returns `Err`.
- 3 runs each (3 separate process invocations for load/memory; 3 passes in-process for calls/safety/async).

## 1. Cold load

| Run | compile (Cranelift) | cwasm size | deserialize | instantiate_pre | instantiate, on-demand p50 / p99 | instantiate, pooled p50 / p99 |
|---|---|---|---|---|---|---|
| 1 | 30.8 ms (cold) | 153,720 B | 0.2 ms | 15 us | 9 / 14 us | 4 / 6 us |
| 2 | 8.0 ms | 153,720 B | 0.1 ms | 9 us | 6 / 57 us | 3 / 6 us |
| 3 | 7.5 ms | 153,720 B | 0.1 ms | 9 us | 5 / 21 us | 3 / 6 us |

Compile is ~8 ms warm (31 ms for the first compile in a process; parallel-compilation on). With a precompiled
`.cwasm` the per-plugin cost at startup is ~0.1-0.2 ms. Instantiating from an `InstancePre` is **3-9 us**.
(Deserialize measured from an in-memory buffer; mmap of a file adds a page-in cost not measured.)

## 2. Call overhead (host to guest), fuel + epoch on, pooled, instance reused

| Run | noop() p50 / p99 | ten(record, 10 strings, 200 B) p50 / p99 | list20() (20 records) p50 / p99 | guest->host import call (per call) |
|---|---|---|---|---|
| 1 | 50 / 51 ns | 467 / 591 ns | 1,831 / 2,131 ns | ~26 ns |
| 2 | 46 / 50 ns | 456 / 565 ns | 1,823 / 2,127 ns | ~26 ns |
| 3 | 46 / 57 ns | 459 / 570 ns | 1,826 / 2,127 ns | ~26 ns |

Max over 100-200k calls was 2-34 us (scheduler noise on a loaded host). The import figure is
(call_host(1000) - call_host(0)) / 1000 on a trivial `ping() -> u32`.

## 3. Memory (RSS delta over N live instances, 100 each, measured in a fresh process)

| Allocator | Guest state | Runs (KiB/instance) | RSS retained after drop |
|---|---|---|---|
| on-demand | minimal | 14.0 / 14.1 / 14.0 | ~0.6 MiB total (malloc residue) |
| on-demand | 1 MB written | 1046.1 / 1046.2 / 1046.1 | ~0.6 MiB |
| pooling | minimal | 17.4 / 17.6 / 17.5 | ~0.55 MiB |
| pooling | 1 MB written | 1049.8 / 1049.6 / 1049.6 | ~0.56 MiB |

Resident cost is linear in touched guest memory plus ~14-18 KiB fixed. Pooling costs ~3.5 KiB more RSS per
instance than on-demand but the memory is decommitted on drop (no growth after drop in either mode). Pooling
create+first-call 16-22 us (minimal), on-demand 15-20 us; with 1 MB written ~0.35-0.5 ms for either (page
faults for 256 pages dominate). Pool virtual reservation (not RSS): 512 x 32 MiB = 16 GiB of address space.
Note: a Rust guest uses >= 3 core instances per component, so a 512-core-instance pool caps at ~170 plugins
unless `total_core_instances` is raised (we hit this with 200 instances: "maximum concurrent limit of 512 for core
instances reached").

## 4. Safety

Epoch ticker thread at 1 ms, `set_epoch_deadline(N)`, infinite loop in the guest (200 trials per row):

| Run | deadline = 1 tick: time-to-trap p50 / p99 | deadline = 5 ticks: p50 / p99 | fuel 3M units (no epoch): p50 / p99 |
|---|---|---|---|
| 1 | 1,107 / 1,220 us | 5,650 / 5,949 us | 138 / 145 us |
| 2 | 1,072 / 1,248 us | 5,263 / 6,067 us | 138 / 150 us |
| 3 | 1,058 / 1,149 us | 5,331 / 6,155 us | 138 / 142 us |

Overshoot beyond the deadline is <= ~1 ms (tick granularity); the loop is interrupted promptly (`Trap::Interrupt`).
Fuel runs out deterministically after ~3M units in ~138 us (`Trap::OutOfFuel`), i.e. about 22 units per us on the
hot path measured here (not the calibration used by `gorca-ext-host`).

Hot-loop slowdown (200M-iteration multiply-add loop in the guest, best of 5, per run):

| Run | none | fuel | epoch | fuel + epoch |
|---|---|---|---|---|
| 1 | 150.0 ms | +4.8% | +5.3% | +34.9% |
| 2 | 148.9 ms | +5.6% | +9.8% | +35.6% |
| 3 | 150.1 ms | +2.4% | +4.8% | +33.6% |

Fuel alone ~2-6%, epoch alone ~5-10%, **both together ~34-36%** on a tight loop (worst case; a call-heavy
guest pays far less). Choosing one of the two is a real saving; the ADR should decide whether the epoch backstop
is worth the extra ~30% on pure compute loops (existing ext-host design uses both).

Trap behaviour (3 runs, consistent): `unreachable` -> `Trap::UnreachableCodeReached` returned as `Err` in
15-35 us; a guest `panic!` returned `Err` in 13-25 us (via the stubbed WASI write trap). The host process is
unaffected. Re-entering the trapped store/instance is refused (error), so the instance must be rebuilt: new Store
+ instantiate + first call took **9-22 us** from an `InstancePre`.

## 5. Async (tokio multi-thread, 8 workers, pooled, no fuel/epoch)

| Run | sync call, inline | `call_async`, in task | sync via `spawn_blocking` round trip (p50 / p99) |
|---|---|---|---|
| 1 | 45 ns | 122 ns | 4.7 us / 13.5 us |
| 2 | 49 ns | 148 ns | 6.6 us / 137 us |
| 3 | 49 ns | 148 ns | 13.8 us / 186 us |

`call_async` adds ~75-100 ns per call over a sync call (fiber switch), p99 within 10 ns of p50. A per-call hop
through the blocking pool costs 5-14 us at p50 and up to 0.14-0.19 ms at p99 on this loaded host: use it only
for long calls. Throughput of 64 independent stores x 20k noop calls: `call_async` 14.6 / 17.7 / 17.7 M calls/s;
`spawn_blocking` with the loop inside the blocking task (no per-call hop, not comparable per call) 96 / 284 /
259 M calls/s (the first run overlapped other load).

## Interpretation against gorca's budgets

- **Keypress to speech p99 < 15 ms.** A hook costs 0.05 us (trivial) to ~2 us (rich structured result) of call
  overhead, i.e. 0.01% of the budget; even 100 hook calls on one keypress path stay under 0.25 ms of
  marshalling. The dominant risk is guest compute, not the boundary: it is bounded by fuel (3M units ~ 0.14 ms
  here) or the epoch deadline (>= 1 ms granularity, +/-1 ms), so a runaway hook on the Reader lane costs at most
  one tick above its limit. Use `call_async` or inline sync calls on the latency path, never `spawn_blocking`
  per hook (5-14 us p50, 0.14-0.19 ms p99 under load).
- **10k+ events/s.** At 10k events/s with ~2 us of marshalling and a small-record call each, the boundary
  consumes ~2% of one core; the measured async path sustains >14M trivial calls/s across 8 workers. Guest
  work, not marshalling, is the thing to budget (event coalescing before the hook call remains the main lever).
- **Lifecycle.** A crashed plugin is replaceable in ~10-20 us; a cold start from `.cwasm` costs ~0.2 ms, from
  source ~8-30 ms (do it off the hot path; cache `.cwasm`). Per-plugin RSS ~15-18 KiB + guest state, so even
  50 plugins with 1 MB state is ~50 MB, acceptable under the performance philosophy.
- **Config choices to feed the ADR.** Raise `total_core_instances` to 3-4x the plugin count; decide
  fuel-only vs fuel+epoch (the 34% compute penalty vs the wall-clock backstop); rebuild instance after any trap.

## Not measured / caveats

- Host was loaded (loadavg 9-14); no CPU pinning; quiet-host tuning not applied. Tails are inflated.
- mmap-backed `.cwasm` load, wasmtime-wasi cost, and `epoch_deadline_async_yield` were not measured.
- Fuel unit calibration here is for one synthetic loop only.
- Raw output of the full run was not kept in the repo; it is reproducible with `tools/spikes/wasm-plugin-cost/run.sh`.
