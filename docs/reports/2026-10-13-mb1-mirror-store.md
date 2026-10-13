# MB-1: mirror store microbenchmark (input to MS-3)

Date: 2026-10-13. Harness: `crates/gorca-bench-mirror` (binary `mb1`). Raw results and the full
per-cell tables: `bench/results/mirror-mb1/run1/` (`steady.csv`, `removal.csv`, `rss.csv`,
`meta.txt`, `summary.md`).

## Recommendation

**Adopt `scc::HashIndex` (`HashIndex<NodeKey, Arc<NodeSnapshot>>`) for `gorca-mirror`.** Confidence:
medium (one host, load 11-23, three interleaved repeats; the ranking at 1M nodes is within noise
for four of the seven stores, the separation comes from the write-storm tails and the 100k removal).

Why, in one line each:

- Lock-free reads through `&self` from any thread: no per-thread handle, which matches
  `MirrorRead: Send + Sync` (ADR-0001 §5.2) and tokio worker migration.
- Best or tied-best reader tail under write storms at every size: 100k at 50k writes/s, p99 560 ns
  (next best 688 ns), and the lowest reader max (1.2 ms vs 2 to 20 ms for papaya and dashmap).
- Visibility at 50k writes/s: p99 5-6 us (left-right 55-82 us).
- 100k-node removal: 6.6 ms, reader max 12 us (no stall); dashmap stalled readers 165 us, papaya
  0.85 ms, sharded ArcSwap 0.5 ms.
- RSS overhead over the payload +30 B/node (papaya +46..67, left-right +46..65).

**Runner-up: `left-right`**, kept for the *document* mirror only (ADR-0003 already chose it for
cross-node-consistent snapshots). For the per-app node store it has three real costs: (1) a
`ReadHandle` is `!Sync`, so `MirrorRead::get(&self)` needs a per-thread, per-mirror handle cache
(a `ReadHandleFactory::handle()` per call allocates and registers an epoch); (2) `publish()` blocks
until every reader leaves its guard, so one descheduled or stalled reader thread stalls the app
actor's writer, which conflicts with "never block"; (3) visibility is bounded by the publish cadence.
Its read numbers are excellent (p50 132-148 ns at <= 100k) and the 100k removal in one publish is the
fastest (3.7 ms), so it is a fine choice if cross-node consistency is ever promoted from MS-2.

**Not recommended:** `papaya` (best p50 at 1M reads-only, 320-336 ns, but reproducible multi-ms reader
stalls under write load: reader max 10-28 ms in all three repeats at 1M with 50k writes/s, visibility
p99 0.8 ms at 100k; +46..67 B/node), sharded `ArcSwap` (reads 2x slower at <= 100k from the shard
`RwLock` cache-line traffic; `ArcSwap::store` is slow under reader load, 0.5-0.7M writes/s flat out),
`dashmap` (baseline: reads 1.5-2x slower than lock-free at <= 100k; 6 ms reader max and 176 us
visibility p999 at 50k writes/s).

## Method

Per cell (store, tree size, write rate): build the tree, 0.4 s warm-up, 2 s measured. Threads: 4
closed-loop readers, 1 visibility prober, 1 writer. All stores use the same integer hasher (`Mix`,
splitmix64) and the same value (`Arc<Node>`, about 110 B payload with name, children, parent,
states), so the comparison is the concurrency scheme only.

- **Reader command** = focus lookup + first child + parent + one sibling through the parent's child
  list (4 lookups, half the focus keys from a 64-node hot set). Latency is per command, in ns,
  from one `Instant` pair (timer overhead 23.7 ns, included). `papaya-pin` holds one pin per command;
  `papaya` pins per lookup, which is the shape of the `MirrorRead::get` API.
- **Writer** (one thread, the app actor): 70 % property change, 10 % insert (node + parent summary),
  10 % remove (node + parent summary), 10 % children reorder. Rates 1k/10k/50k store mutations/s in 1 ms
  batches (publish per batch, so left-right visibility includes the batch), plus `max` (flat out,
  batches of 32). Rates count mutations and an insert or remove counts 2, so the 1k/s cell achieves
  1189/s (one op per tick overshoots) and the 10k and 50k cells are within 2 % of nominal.
- **Visibility**: the writer's last op of every batch is a probe node carrying `t_write`; a dedicated
  spinning thread records `now - t_write` the first time it sees each sequence number.
- **Removal**: 200k-node tree, 100k keys removed in 1024-key chunks (one publish per chunk), readers
  closed-loop and uniform over all 200k keys; latency split into baseline / during / after phases.
  `left-right/oneshot` publishes once for all 100k removals.
- **RSS**: separate child process per (store, size), growth over the build, minus a `Vec<Arc<Node>>`
  floor. glibc malloc, no jemalloc. Sizes <= 10k are dominated by allocator granularity (left-right
  at 1k reads -8 B/node), ignore them.
- **Noise control** (B-2 lesson): 3 repeats interleaved across the whole matrix, store order rotated per
  cell and repeat; tables are medians, with the p99 min-max across repeats shown in `summary.md`.
  Host: Intel Core Ultra 9 285H, 16 CPUs, governor `powersave`, RT kernel 7.1.8, load average 11 at
  start and 18 at end, no pinning. Absolute numbers are inflated by the load (4 readers + prober +
  writer compete with the load for 16 CPUs); rank order and stall events are what to trust.

## Results (medians of 3; full tables in `summary.md`)

Reader command latency under concurrent writes, 50k writes/s (ns; vis = writer-to-reader us; max in ms):

| store | 100k p50 | p99 | p999 | max ms | vis p99 us | 1M p50 | p99 | p999 | max ms | vis p99 us |
|---|---|---|---|---|---|---|---|---|---|---|
| scc-index | 168 | 560 | 768 | 1.2 | 5.1 | 408 | 1472 | 1792 | 0.7 | 5.9 |
| left-right | 184 | 704 | 944 | 0.8 | 82 | 448 | 1568 | 2048 | 0.6 | 55 |
| scc-map | 256 | 688 | 912 | 0.9 | 72 | 408 | 1408 | 2048 | 0.7 | 3.6 |
| papaya-pin | 204 | 864 | 1152 | 2.0 | 9.0 | 472 | 1728 | 2432 | 12.7 | 21 |
| papaya | 344 | 1216 | 1760 | 20.3 | 786 | 392 | 1504 | 2240 | 18.5 | 24 |
| dashmap | 256 | 864 | 1248 | 6.0 | 16 | 504 | 1568 | 2304 | 2.9 | 13 |
| arcswap-sharded | 304 | 848 | 1248 | 4.3 | 2.0 | 488 | 1472 | 2176 | 0.5 | 1.9 |

Reads only (0 writes/s), p50 / p99 ns: 100k: scc-index 140/344, left-right 132/352, papaya 132/416,
papaya-pin 132/456, scc-map 204/496, dashmap 212/512, arcswap 244/576; 1M: papaya-pin 320/1280, papaya 336/1344,
left-right 344/1376, scc-map 400/1408, dashmap 416/1696, scc-index 416/1536, arcswap 432/1600.
At 1k nodes: left-right 136/244, scc-index 140/244, papaya-pin 148/280, papaya 164/288, dashmap
264/448, scc-map 212/424, arcswap 264/512. Reader CPU is flat across stores (4 reader cores plus
the writer); writer CPU at 50k/s is 5-14 % of a core (arcswap highest at 14 %).

Flat-out write throughput with 4 readers (M mutations/s, includes building the node):

| store | 1k nodes | 10k | 100k | 1M |
|---|---|---|---|---|
| left-right | 4.7 | 5.1 | 1.8 | 2.4 |
| dashmap | 4.5 | 4.6 | 4.1 | 2.1 |
| scc-index | 4.5 | 4.2 | 3.6 | 1.8 |
| scc-map | 4.9 | 3.4 | 1.9 | 2.2 |
| papaya / papaya-pin | 2.3 / 2.1 | 2.4 / 2.4 | 2.2 / 2.0 | 1.4 / 1.5 |
| arcswap-sharded | 0.58 | 0.55 | 0.52 | 0.74 |

All but arcswap-sharded exceed the 50k/s storm target by 25x or more; the headroom is not a
differentiator. Flat out, left-right at 100k drove reader max to 14.8 ms (publish waits on readers);
papaya at 1M drove p99 to 2.8 ms and papaya-pin to 10.5 ms.

100k-node subtree removal (ns; max in us):

| store | removal ms | during p99 | during p999 | during max us | reader cmds/s during vs base |
|---|---|---|---|---|---|
| scc-index | 6.6 | 560 | 720 | 12 | 20.97 vs 18.16 M |
| scc-map | 7.6 | 672 | 864 | 11 | 16.5 vs 14.9 M |
| left-right chunk1024 | 16.4 | 672 | 896 | 18 | 20.2 vs 19.6 M |
| left-right oneshot | 3.7 | 736 | 1008 | 17 | 15.8 vs 18.5 M |
| dashmap | 12.8 | 960 | 1248 | 165 | 9.8 vs 10.9 M |
| papaya-pin | 23.0 | 1216 | 5632 | 132 | 17.4 vs 18.4 M |
| papaya | 25.4 | 1152 | 6144 | 852 | 12.6 vs 15.1 M |
| arcswap-sharded | 88.2 | 1216 | 1920 | 536 | 11.4 vs 12.8 M |

Survivors after removal: 0 for every store. No store has a removal tail that threatens 15 ms; the
differences are in the 10 us to 1 ms range.

RSS growth per node, 1M nodes (B/node, payload floor 130 B): arcswap-sharded 158, dashmap 158, scc-index
160, scc-map 160, papaya 176, left-right 195. Left-right's second table copy costs only the table
because values are shared `Arc`s (+65 B/node over the payload), not a full 2x of the mirror.

## Risks and open questions

- One host, load 11-23, 3 repeats, no CPU pinning, `powersave` governor. Re-run the shortlist
  (scc-index, left-right, papaya-pin) on a quiet host with `performance` and pinned cores before
  treating the 5-20 % gaps as final. The stall events (papaya, dashmap) are large enough to survive that.
- Papaya's multi-ms reader stalls under write load are reproducible but their cause (resize or
  reclamation) was not profiled.
- `HashIndex` updates insert a new entry and mark the old one removed; long-run bucket and EBR garbage
  growth under a 1 h storm is not measured (needs the M2 RSS gate). A permanently pinned reader would
  delay reclamation, not block anything.
- The benchmark uses 4 reader threads and one mirror per run. Many app mirrors (about 100) with
  tokio-migrated readers were not modelled, which is the main argument against left-right's per-thread
  handles, not a measured number.
- Store API shape differs: `HashIndex` gives `get` by closure (`peek_with`), so the returned
  `Arc<NodeSnapshot>` is cloned once per hit (one atomic increment, included in these numbers).
- Property change here replaces the whole `Arc<Node>`; field-granular updates were not modelled.

## Reproduce

```text
cargo build --release -p gorca-bench-mirror
target/release/mb1 run --out bench/results/mirror-mb1/run2 --reps 3 --secs 2   # about 15 min
target/release/mb1 summary bench/results/mirror-mb1/run2                       # rebuilds summary.md
```

Options: `--sizes`, `--rates` (`0` = read-only, `max` = flat out), `--stores`, `--readers`,
`--skip-steady|--skip-removal|--skip-rss`.
