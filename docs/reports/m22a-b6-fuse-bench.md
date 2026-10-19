# M22a-A4 and M22a-B6: representation micro-bench and B-FUSE bench

Date 2026-10-19. Author: perf-engineer. Branch `worktree-agent-a359050a6954580d6`, engine under test:
`gorca-fuse` `Ivm` at `51ebed51` (M22a-B2 merged).

Code: `crates/gorca-bench/benches/sem_repr.rs` (A4), `crates/gorca-bench/benches/fuse.rs` (B6),
`bench/m22a/run.sh`. Raw data: `bench/m22a/results/*.json` and `bench/m22a/logs/*.log`
(nothing deleted; see "Result sets"). Reproduce: `. ./env.sh && bench/m22a/run.sh 3 2`
(builds into `CARGO_TARGET_DIR=/var/tmp/m22a-b6-target`, pins to CPU 2, 3 runs).
Single scenarios: `M22A_ONLY=storm,focus M22A_SECS=6 M22A_CPU=2 <fuse bench binary>`.

## Conditions (labelled on every result file)

Core Ultra 9 285H, 16 CPUs, governor `performance` (already set; `quiet-host.sh` not applied, no
host tuning was changed). Load average 11-15 throughout (other agents' builds), but the pinned
core was never contended: per-CPU busy 14-27 % over a 3 s sample. Both benches run pinned to CPU 2
(`taskset`/`sched_setaffinity`), single thread, spin-paced (no sleeps), release profile with
debuginfo. Memory pressure `some avg10` 0.05-0.75. This is **not** a quiet host: p99.9 and max
carry OS jitter (single events of 1-7 ms); p50 to p99 were stable across 3 runs (spread under 10 %).
`kernel.perf_event_paranoid` was raised 3 -> 1 with sudo for one `perf` run and set back to 3.

## A4: A (typed `SemEntry`, x32, 128 B) against C (property bag)

Compose = read the fields a presentation rule reads (role, name, check/expand when the role
applies, avail, level, pos, flags, description, live, orientation) and emit tokens. C-sorted is
`Vec<(u16, PropValue)>` with binary search (`PropValue` is 24 B); C-hash is `std::HashMap`.
ns per node, p50 / p99 over 4 runs (spread in brackets), batches of 256 nodes, pinned core:

| Scenario | A | C-sorted | C-hash |
|---|---|---|---|
| compose, plain (link) | 7.2-10.2 / 7.7-10.8 | 10.3-14.6 / 10.4-15.0 | 44-62 / 46-73 |
| compose, form (checkbox) | 8.3-11.3 / 8.5-12.1 | 19.9-26.5 / 20.5-30.4 | 56-71 / 58-82 |
| compose, rich (all groups) | 10.7-11.8 / 11.6-13.1 | 33.6-36.9 / 34.5-41.1 | 78-85 / 83-95 |
| compose, mixed, warm 4k nodes | 8.4-8.7 / 8.6-8.9 | 19.8-19.9 / 20.3-20.8 | 57.8-58.9 / 58.8-63.1 |
| compose, mixed, 100k nodes, shuffled (cache-cold) | 52-58 / 82-85 | 168-219 / 246-272 | 310-366 / 369-793 |
| CoW update of one group + equality (early cutoff) | 18.1-18.6 / 18.2-19.9 | 35-42 / 38-46 | 60-64 / 66-71 |
| build from a neutral spec | 40 / 42-44 | 64-66 / 70-72 | 126-130 / 140-147 |
| heap per node, mixed (chunks, shared strings excluded) | 160 B + 35 B `SemText` (only 20 % of nodes) | 218 B | 346 B |
| 100k nodes, entry carrier only | 19.5 MB | 21.8 MB | 34.6 MB |

Criterion (mean, warm mixed): A 9.2 ns, C-sorted 18.8 ns, C-hash 60.3 ns.

Reading: A composes 2.3x faster than the best bag and 6.8x faster than a hash bag warm, and 3-4x /
6x when cache-cold. All variants are far below M-4 (p99 <= 3 us/node), so compose cost alone does
not decide A against C; the decision rests on exhaustive matching (ADR-0022 section 3) and on
memory: the bag is no smaller than A (218 B against 195 B for sparse nodes, because `Vec` header +
24 B values + allocator rounding). Verdict: A holds on every measured axis (compose, CoW, build,
heap). Caveat: C is the best realistic implementation I could write quickly; a perfect-hash/array
bag would approach A's compose time but gives up the typed invariants.

## B6: B-FUSE gate table

Result set `20261019e` (final bench code), 3 runs; ranges are over the 3 runs. M-11, `Tick`, memory,
sweep, aging, S-3 and churn rows are identical in sets `d` and `e` (the focus scenario changed).

| Gate | Target | Measured | Verdict |
|---|---|---|---|
| `SemEntry` size (A1 const) | 128 B, <= 160 B | 128 B | PASS |
| 100k-node RSS increase, all carriers | <= 25 MB | **+154 MB** whole `Ivm` (entries alone +16.0 MB) | **FAIL** |
| M-11 fusion+publish, per event, 10k/25k/50k ev/s + 5k facts/s | p99 <= 2 us | p50 2.7-4.6 us, **p99 8.3-13.0 us** (clean case below: p50 1.8, p99 5.9) | **FAIL** |
| M-11 same, per 1 ms coalescer batch (per-event average) | p99 <= 2 us | p50 3.0-3.6 us, **p99 4.3-5.6 us** | **FAIL** |
| M-11 no ingress lag (batch apply+run over the 1 ms tick) | 0 | 1 of 6000 batches per rate (p99.9 batch 0.24-0.5 ms; the one outlier is 1.4-3.6 ms host jitter) | FAIL on the strict count, no backlog (`pending` max 0) |
| `Input::Tick` cost independent of stored facts | flat | 0.03 us (0 facts), 1.0 us (1k), 14-26 us (10k), **114-116 us (50k)** | **FAIL** (O(facts)) |
| Focus change, ~100k dirty nodes: synchronous part (`set_focus_chain` + `set_visible`) | p99 <= 0.5 ms | p50 134-137 us, p95 268-281 us, **p99 329-364 us** (visits ~690 nodes, cap 1024) | PASS (27-34 % margin) |
| Focus change, ~100k dirty nodes: whole focus chain fused and published | p99 <= 0.5 ms | p50 346-365 us, p95 1.1 ms, **p99 1.53-1.61 ms**; needs p50 144 / p95 544 / p99 816 fusions for ~6 nameable chain nodes (info: chain + 200-fusion drain p50 0.70-0.74 ms, p99 1.86-1.91 ms) | **FAIL** (chain is not fused first) |
| `bump_global` synchronous part, 100k nodes | p99 <= 0.5 ms | p50 241-250 us, p99 244-375 us | PASS |
| `bump_global` background sweep | no starvation | 100k nodes in ~103 ms (46 runs of 2 ms); focus-chain event published in the next 32-fusion run in 33 us p50 / 45 us p99 (leaf), 45-57 us (root); 0 of 60 missed | PASS (0 misses in all `e` runs; one root miss in `d` run 3) |
| Background aging under a saturating priority storm | no starvation | 98 274 of 100 000 backlog nodes drained in 6000 ticks, steady 24 non-priority nodes per run (= budget/4), no stall; steady-state fresh non-priority event p99 2 ticks | PASS |
| Per-key anchor re-resolution (S-3) | O(touched), not O(all) | p50 0.70 us/key @2k anchors, 1.3-1.6 us/key @20k (ratio 1.8-2.2: sub-linear, cache effect); hot id with 16/64/256 anchors: 168/180/215 ns per key | PASS in 4 of 6 runs of sets d and e (my ratio <= 2 threshold is borderline, up to 2.1; cost is clearly sub-linear in anchor count) |
| Lifecycle churn, 600 x (add + remove 200 nodes) | no leak, consistent | add 2.4 us/node (incl. inputs + fuse), remove 0.19 us/node, cycle p50 0.52 ms; RSS growth 0.4 MB; `verify()` ok | PASS |

Throughput headroom: at 50k ev/s + 5k facts/s the writer thread is 20 % busy (per-event) /
21 % (batched), `pending` never above 0 at 10k-50k, so the engine sustains the rate; it is the
per-event latency gate that fails, not capacity.

## Findings (ordered by impact)

1. **Memory gate fails by 6x; the cause is S0.** `Ivm` stores an `S0Node` per node: **1008 B**
   (18 `S0Slot`s of 56 B) = 100 MB, plus `NodeRegs` 136 B and the `Node` itself. The published
   `SemEntry` is only 16.0 MB (matches math 10.1). To meet 25 MB the S0 must not be kept for every
   node (store it only for nodes read as neighbours or by selectors, or shrink the slot to ~8 B).
2. **Per-event fusion cost is 1.8 us at best and 3-5 us in the mixed storm; p99 is 4-13 us.**
   1.1 fusions per event, 65-74 % early cutoff, 0.3-0.4 publishes per event. Attribution at 25k ev/s
   per-event mode (3 runs, p50 / p99 us): clean 1.8 / 5.9-6.1; + 100 patches 1.9-2.0 / 6.3-6.6;
   + 5k facts/s 2.9-3.2 / 8.6-9.4; both 3.1-3.5 / 9.1-10.5. So the 2 us p99 gate fails even without
   patches or facts; patches add ~0.1-0.2 us, facts ~1.1-1.4 us per event on average. `perf` is
   dominated by the spin loop; of the real work `process`+`s0_with`+`fuse_node_with` are ~40 %,
   `changed_groups` (1008 B S0 compare) 6 %, `mark_dirty`/`BTreeSet` and `apply_one` ~10 % each.
   The gate as written (2 us p99) looks unreachable with 1 KB S0 per fusion; either the S0 cost
   falls (finding 1 fixes both) or the gate should be restated as p50 plus a per-batch bound.
3. **The focus chain is not fused first.** The synchronous part itself is fine (p99 0.33-0.36 ms:
   `set_focus_chain` + `set_visible` visit ~690 nodes and re-key every dirty one in the 100k
   `BTreeSet`). But the priority class is ordered `(depth desc, index)` over chain + visible window
   (200 nodes) + their content-named children, so the focused leaf and its ancestors wait behind
   deeper window nodes: the nameable chain nodes were all published only after p50 144 / p99 816
   fusions, i.e. 0.35 ms p50 / 1.5-1.6 ms p99 (the earlier root-event miss at a 32-fusion budget in
   `d` run 3 is the same cause). Splitting the class (chain, then window, then rest) would make
   this ~10 fusions. Note also `run(Budget::calls(1))` spends its one fusion on the aged backlog
   (aging may take `max(1, calls/4)` per run), so single-fusion runs never reach priority work.
4. **`Input::Tick` scans every register** (`SideTable::expiring`, `side.rs:488`): 115 us at 50k
   facts, every tick. A TTL min-heap or timer wheel makes it O(expired). In the first (v1) bench the
   clock was advanced every 1 ms and this made batched per-event cost 10-13 us; v2 times it apart.
5. **A deadline-bounded `run` publishes only when it returns.** During a global sweep
   `run(until 2 ms)` runs the full 2 ms, so a focus-chain event waits ~2.0 ms (p99 2.003 ms) for its
   `RunReport`; with `Budget::calls(32)` it publishes in 33 us. The reader should do a small
   priority-first run for each input batch and leave the 2 ms runs to background work.
6. **Aging works but is slow for a deep backlog**: a fresh non-priority event queued behind a 100k
   backlog waits p50 1129 / p99 3675 ticks (aged-first FIFO by dirty time, 24 nodes per 100-fusion
   run). Fine for a post-`bump_global` sweep, not for long outages. In steady state (no backlog)
   p99 is 2 ticks.
7. **S-3 and lifecycle are in good shape**: per-key cost 0.17-0.7 us, flat in fan-out 1 to 256;
   no leak, `verify()` consistent after 120 000 add/remove node operations.

## Method notes

* Storm: real-time spin pacing; inputs generated untimed (they come from the connector and the
  secondary); timed region is `apply + run(until 2 ms) + drop(published)`. Mixed events: 45 %
  state (check/expand/avail), 20 % name, 10 % no-op rewrite, 15 % `Vis`, 10 % orientation; 70 %
  of events hit a 2000-node hot set. Secondary: `bus` description facts (Secondary, `inexpressible`
  group) and `ocr` name facts (Inferred, TTL), 20 % Dom anchors, 5k/s, interleaved on the same
  thread (so its cost is included). 100 patches indexed by role, parent-role and unnamed
  selectors, selector reads narrowed to parent role. Focus moves every 100 ms (timed apart).
  Each storm run uses a fresh 100k-node world (the first runs reused it, so facts accumulated).
* Focus gate: all 100k nodes dirtied (apply, no run); 600 focus changes, each first re-dirtying
  the chain nodes with fresh names and the nodes the last run consumed, so ~100k stay dirty.
  Cuts: (a) set only (the gate), (b) + 16-fusion runs until every nameable chain node was
  published, (c) + 200 more fusions (`max_sync_drain`).
* The ingress half of "no ingress lag regression" is not measured here (no ingress in this
  harness): only the writer's own backlog/lag. Needs the e2e harness with `Ivm` wired in (C1b).
* Percentiles are over events (per-event mode) or 1 ms batches (batched mode); batch values are
  per-event averages, which hides in-batch tails, hence both modes.

## Result sets (all kept)

* `20261019-run1-sem_repr.json`, `20261019b-run{1,2,3}-sem_repr.json`: A4 (4 runs).
* `20261019b-run{1,2,3}-fuse.*`: first bench version: reused one world per storm (facts grew
  to 30k+), clock advanced in the timed batch, root event used for the sweep check, repeated
  event names let early cutoff hide publishes (2 flaky "missed" gates). Superseded, kept: they
  show `Tick` inflating batched cost to 10-13 us/event.
* `20261019c-run{1,2,3}-fuse.*`: after the world/tick fix, before the leaf/root sweep split
  (run 2 had 2 stray "missed" from the root-at-32-fusions case, later split out).
* `20261019d-run{1,2,3}-fuse.*`: focus scenario judged only by `set_focus_chain`'s sync part
  plus a chain check that was invalid (pending-count based); other scenarios identical to `e`.
* `20261019e-run{1,2,3}-fuse.*`: final code (the table above). Exploratory terminal runs while
  developing the harness (not saved as files) showed focus sync p99 0.6-3.3 ms on a busier moment;
  the saved sets are the reference.

## Risks and open questions

* Not a quiet host (load 11-15, performance governor, no C-state change); p99.9 and max include
  scheduler jitter. A re-run on a drained host would shrink p99 a little but cannot close 2 us.
* M-11 as written does not say whether "per event" means per input or per fused node, or whether
  the secondary's facts count; I used input events and included facts (stricter).
* The chain-first question (finding 3) needs a decision: ADR-0022 says "focus chain and active
  descendant first"; the implementation orders one priority class deepest-first.
* "Chain fused" awaits only nameable chain nodes (generic and group nodes cannot publish a name
  change in this workload), so it is a lower bound.
* The 25 MB gate needs the S0 storage decision before B6 can pass; ADR-0024 2.12 counted only
  entries and side tables.
