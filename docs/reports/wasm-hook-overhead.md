# wasmtime component-model hook overhead for web quirks (ADR-0008 evidence)

Date: 2026-10-15. Author: architect. Code: `bench/wasm-hook/`. Raw data:
`bench/results/wasm-hook/2026-10-15-{pcore,ecore}.tsv`, conditions in `2026-10-15-conditions.txt`.

## 1. Question

Does running per-engine web quirk hooks as WASM components (wasmtime, component model) cost enough to
threaten the budgets: W1 quick-nav key → speech ≤ 5 ms, focus p99 < 15 ms, X1 hook overhead ≤ 100 µs,
doc-actor ingest < 2 µs per event (ADR-0003 §14), and the 10k events/s storm (CLAUDE.md; L3-50k gate)?

## 2. Setup

- Host: Intel Core Ultra 9 285H (6 P-cores 5.4 GHz, 8 E-cores 4.5 GHz, 2 LP-E), Debian 13 RT kernel
  7.1.8, governor `performance`, load average 1.9–2.0 during the run (live desktop session, nothing else
  heavy). Runs pinned with `taskset -c 3,4` (P) and `taskset -c 8,9` (E); two CPUs so the epoch ticker
  thread does not share the measured core.
- rustc 1.99.0, wasmtime 49.0.3 (Cranelift, `component-model`, synchronous calls), wit-bindgen 0.46,
  guest target `wasm32-wasip2`, release + thin LTO. Unknown (WASI) imports are linked as traps; the guest
  never calls them.
- One logic crate (`bench/wasm-hook/logic`) compiled into both arms. It implements the quirks we have
  today: Gecko UTF-16 end offset recomputed from the payload in code points; Blink caret(0) on a
  non-focused node dropped (scroll-to-anchor), static text via `children-added` rewritten as a text change
  of the parent, silent reorder flagged for parent revalidation; generic index −1 drop and range swap.
- Delta mix per batch: 25 % text-inserted with a 26-scalar Cyrillic + emoji payload (half UTF-16 flagged),
  15 % caret, 20 % children-added (one third static text), 10 % reorder, the rest attrs/state, ~1 %
  index −1.
- Hook shapes:
  - `normalize(engine, list<delta>) -> list<delta>`: the guest returns the rewritten batch (full copy back).
  - `classify(engine, list<delta>) -> list<u8>`: patch form, one verdict byte per delta.
  - `on-key(key, node) -> option<bool>`: per-key predicate (pass-through verdict).
  - `present(node) -> option<list<segment>>`: per-key presentation returning two strings.
  - `walk(start, k)`: the guest calls the host import `get-node` k times (lock-free mirror read stand-in
    returning a record with a 21-byte name).
  - `trap` / `spin`: trap recovery (drop store, new store, instantiate from `InstancePre`, first call) and
    hang interruption with a 2 ms budget (epoch deadline 4 ticks × 0.5 ms).
- Engine modes: `epoch`, `fuel`, `pool+epoch` (pooling allocator, 64 instances, 32 MiB memories).
- Timing: `Instant` around each call, 200k iterations (scaled down for big batches, min 2000), warm-up
  10 %. `Instant` itself costs ~10 ns, visible in the smallest native rows.

## 3. Results (P-core, `epoch` unless stated; ns)

### 3.1 Single calls

| Case | p50 | p99 | p99.9 |
|---|---:|---:|---:|
| native `on_key` | 11 | 12 | 13 |
| wasm `noop` | 35 | 41 | 42 |
| wasm `on-key` (node record with a name in) | 68 | 75 | 81 |
| wasm `present` (two strings out) | 143 | 164 | 181 |
| wasm `classify`, 1 delta | 110 | 128 | 133 |
| wasm `normalize`, 1 delta | 149 | 169 | 174 |

### 3.2 Batches (per-event normalisation)

| N | native in place | native clone + normalize | wasm `classify` | wasm `normalize` |
|---:|---:|---:|---:|---:|
| 8 | 15 | 51 | 295 | 507 |
| 32 | 47 | 193 | 1,115 | 2,220 |
| 128 | 174 | 990 | 4,471 | 9,431 |
| 512 | 621 | 4,865 | 19,218 | 38,369 |
| 2048 | 3,470 | 21,402 | 77,905 | 160,486 |

Marginal cost per delta (slope 512 → 2048): native in place 1.9 ns, native clone 10.8 ns, wasm
`classify` 38 ns, wasm `normalize` 80 ns. The WASM arm is 20–40× the native in-place arm per delta; the
cost is lifting/lowering records and strings across the boundary, not the logic. Returning only verdicts
(`classify`) halves it.

### 3.3 Host imports (per-key walks)

| k host calls | native | wasm |
|---:|---:|---:|
| 1 | 11 | 103 |
| 10 | 19 | 701 |
| 100 | 105 | 6,599 (p99 7,396) |
| 1000 | 951 | 65,618 (p99 68,989) |

≈ 66 ns per `get-node` round trip with a string in the record, vs < 1 ns for the inlined native stand-in.

### 3.4 Faults

| Case | p50 | p99 | p99.9 |
|---|---:|---:|---:|
| trap recovery, `epoch` | 12.8 µs | 31.0 µs | 36.8 µs |
| trap recovery, `pool+epoch` | 6.2 µs | 8.9 µs | 13.6 µs |
| trap recovery, `fuel` | 14.7 µs | 34.2 µs | 39.5 µs |
| spin → trap, 2 ms budget, `epoch` | 2.17 ms | 2.81 ms | 3.07 ms |
| spin → trap, 2 ms budget, `pool+epoch` | 2.20 ms | 4.65 ms | 4.79 ms |

Component compile (Cranelift, cold): 42–49 ms, once per extension per process (cacheable with
`Component::serialize`).

### 3.5 Modes and cores

- `fuel` vs `epoch`: within 2 % on every row (these hooks are short and loop-light); fuel's long tail
  shows only on the 1000-call walk (p99.9 223 µs vs 74 µs).
- Pooling: same call cost, trap recovery 3.5× faster.
- E-cores: +10 % to +35 % (`noop` 37 ns, `present` 168 ns, `classify` 512 → 23.1 µs, walk 1000 → 86.6 µs,
  trap recovery p99 39.5 µs).

## 4. Against the budgets

| Budget | WASM cost measured | Share |
|---|---|---:|
| X1 hook overhead ≤ 100 µs p99 | `present` 164 ns | 0.16 % |
| W1 quick nav ≤ 5 ms | one `on-key` or `present` per key: ≤ 0.2 µs | < 0.01 % |
| W1, a per-key walk of 1,000 nodes through `get-node` | 69 µs | 1.4 % |
| W1, the same walk over a 100k-node page (extrapolated, linear) | ≈ 6.6 ms | **132 %, fails** |
| Doc-actor ingest < 2 µs per event | `classify` 38 ns per delta batched; 110 ns as a per-event call | 2 % / 5.5 % |
| Storm 10k ev/s, one call per event (`normalize`, N = 1) | 1.5 ms of CPU per second | 0.15 % of a core |
| Storm 10k ev/s, batches of 32 (`classify`) | 0.35 ms/s | 0.035 % |
| L3-50k, one call per event (`normalize`) | 7.5 ms/s | 0.75 % |
| Trap recovery < 50 ms | 31 µs p99 (8.9 µs pooled) | 0.06 % |
| `on-event` CPU limit 2 ms (ADR-0001 §10) | enforced at 2.2 ms p50 / 2.8 ms p99 | overrun ≤ 0.8 ms |

## 5. Conclusions

1. The component-model **call** is not a latency problem for any hook shape gorca plans: tens to
   hundreds of nanoseconds per call, tens of nanoseconds per delta. Neither the storm target nor X1/W1
   argues for putting browser code in the core.
2. What does not scale is **chatty host access**: ~66 ns per host call means any per-key algorithm that
   walks the document through `get-node` is O(nodes) × 66 ns and breaks W1 on large pages. Structural
   navigation, index queries and the document mirror must stay native and in the core; extensions may
   only contribute at apply time (ADR-0003 §15: `nav-match` at apply time, never per key press).
3. A WASM hook cannot run inside the connector's IPC planning (warm-up, P2P, revalidation calls) without
   giving extensions IPC, which ADR-0001 §10 forbids. That is an interface argument, not a speed one.
4. Hang protection works (epoch trap within ~1 tick of the budget) and trap recovery is three orders of
   magnitude under the 50 ms gate. Native in-process code has neither.
5. Patch-form hooks (verdict bytes) are 2× cheaper than full rewrites and should be the WIT shape for any
   batch hook.

## 6. Caveats (unverified)

- Synchronous calls only. If gorca calls hooks with `call_async` (fibers) to yield on epoch instead of
  trapping, add a fiber switch per call (not measured).
- The native `get-node` stand-in is inlined and allocation-free; the real `scc::HashIndex` mirror read
  (MS-3) is slower, so the WASM/native ratio for walks is overstated, not the absolute WASM cost.
- One guest, one host thread; no contention between stores on different workers.
- The spin tail on `pool+epoch` (4.65 ms p99) is ticker jitter on the shared two-CPU pin; it was not
  re-run with a dedicated ticker core.
- Memory per instance not measured (linear memory of this guest stays at its initial pages).
