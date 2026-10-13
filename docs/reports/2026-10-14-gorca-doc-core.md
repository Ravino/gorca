# gorca-doc web-prep core: arena, treap index, flat text, left-right publication

Date: 2026-10-14. Author: rust-engineer. Scope: `crates/gorca-doc/src/web/` (new), tests, MB benches.
`gorca-core` and `gorca-model` are untouched; the existing `mirror.rs` scaffold is untouched.
Specs: ADR-0003 sections 4.1, 4.4, 4.5, 4.8, 5, 9.6; `docs/reports/2026-10-12-math-web-graph-model.md`
section 1.2; `docs/plan/phase-2.md` section 7.

## What was built

| Module | Content |
|---|---|
| `web::doc` | `WebDoc`: generational arena (`DocNodeId{slot,gen}`, LIFO free list), intrusive tree links, implicit treap over preorder with a per-node summary (size, category OR-mask, 16 ranked-category counts, flat-text scalars and newlines). Priorities are `splitmix64(seed, slot, gen)`, so two copies stay identical (PUB-1). `validate()` checks DM-I1..I3. |
| `web::cats` | `Cat` bit set (ADR section 4.4 bit assignment), `NodeFlags`, `cats_of(role, level, flags)` (DM-4, exhaustive match over `Role`), `NodeSpec`, `SubtreeSpec`, `PropsPatch`. |
| `web::ops` | `DocOp`: `InsertSubtree`, `RemoveSubtree`, `AttachSubtree`, `MoveSubtree`, `ReorderChildren`, `SyncChildren`, `SetText`, `SetProps`, `Reap`. `align_by_key` (IL-3 signature alignment: prefix/suffix trim, exact LCS on the middle, first-fit by key for leftovers). |
| `web::flat` | Reading units over the flat text: char, word, line; `unit_range`, `next_unit`, `prev_unit`, `word_range_in`. |
| `web::publish` | `DocWriter` / `DocRead` double buffer (`channel`, `channel_from`). |
| `web::synth` | Deterministic synthetic article pages and a small RNG (shared by tests and benches). |

### Query API (all O(log n), no IPC)

- Navigation: `next`, `prev`, `first_of`, `last_of`, `next_in` / `prev_in` (subtree scope, SN-2),
  `next_in_landmark` / `prev_in_landmark` (the per-landmark subsequence), `landmark_of`,
  `kth`, `rank`, `count` (ranked categories), `cmp_order`, `position`, `at_position`.
  Level filters are just category unions: `Cat::HEADING_2`, `Cat::headings_up_to(3)`.
- Text: `offset_of(node)`, `at_offset(scalar) -> (node, local)`, `line_start(k)`, `line_of(offset)`,
  `line_range(offset)`, `text_in(a, b)`, `unit_range`, `next_unit`, `prev_unit`.
- Navigation categories with counts ("k of N"): heading and levels 1-6, link, form field, button,
  edit, landmark, region, table, list, list item. Other categories are mask-only (next/prev, no rank).

### Publication (ADR section 4.8)

Left-right semantics without the `left-right` crate (not vendored) and without `unsafe`
(workspace denies it): two `Arc<WebDoc>` copies and an `ArcSwap`.

- Readers: `DocRead::with(|doc| ..)` loads the live copy (wait-free for them). They never wait for the writer.
- Writer: mutates the back copy, logs each op. `publish()` is one `ArcSwap::swap`. The old live copy becomes the back copy; the logged ops are replayed on it before the next write, as soon as its last reader has left (`Arc::get_mut` succeeds).
- If a reader pins the old copy longer than the wait budget (default 2 ms), the writer clones the live copy instead of waiting (`PublishStats::fallback_clones`). A stuck reader costs one O(n) copy, never a stall. Waits over 1 ms are counted (`slow_waits`, PUB-3 watchdog).
- `DocWriter::doc()` is `&mut self` because it first performs that deferred catch-up.

### SPA churn (identity variant C)

- Removal does not free: `RemoveSubtree` cuts the subtree out of the tree and the treap (two splits and a merge, nodes untouched) and keeps it as a defunct root with its ids. `find_detached(sig)` finds re-match candidates; `AttachSubtree` / `SyncChildren` bring the same `DocNodeId`s back (IL-R3: remove x N, add x N keeps ids; IL-R4: re-match window). `Reap` frees; ids then go stale and reused slots get a new generation (DM-I3).
- `SyncChildren { parent, desired: [Existing(id) | New(spec)] }` is the keyed diff of section 9.6: the longest already-ordered run of current children stays put, every other existing child is moved (|C| - LIS moves), new subtrees are inserted, undesired children become defunct. Nodes from elsewhere in the tree are hoisted (wrapper removal), deepest first, so removing the wrapper in the same op cannot strand them. All validation happens before any mutation (a rejected op never mutates, tested).
- `align_by_key(old_sigs, new_sigs)` produces the `Option<old index>` alignment for IL-3 from the caller's signatures.

## Tests (all in `make check`)

- `crates/gorca-doc/tests/web_model.rs`: seeded property tests against a naive tree model. 60 seeds x 120 ops plus 6 seeds x 700 ops of random insert/remove/attach/move/reorder/sync (with hoists and defunct re-attach)/set_text/set_props/reap. After every op: `validate()`, preorder, children/parent, categories, `next/prev/next_in/prev_in/landmark_of/next_in_landmark`, `first_of/last_of`, `count/kth/rank`, `cmp_order/position/at_position`, flat text, `offset_of/at_offset/line_start/line_of/line_range/text_in`, word/char/line units and forward/backward walks; and, at random points, a publish with the reader copy checked against the model. The test asserts each op kind ran at least 50 times and that documents reach 60+ nodes. Proptest is not vendored offline; these are seeded loops (failures print seed and step).
- Mutation check: reversing the deepest-first hoist order made the suite fail at seed 6 step 8, as intended.
- Unit tests: categories, LIS, `align_by_key`, word boundaries, publication (unpublished changes invisible, copies converge after replay, stuck reader triggers clone fallback), synthetic pages.
- Targeted tests: id stays with content across detach/re-attach/reap, rejected ops do not mutate, 20,000-deep chain builds without recursion.

## Benchmarks

Harness: `cargo bench -p gorca-doc --bench doc_mb -- --out <abs dir>` (`--quick`, `--sizes`). Synthetic article pages (landmarks, headings level 1-4, paragraphs with links, lists, tables, forms). Per-call timer overhead (11-26 ns) is subtracted from per-call samples; "bulk mean" is a timer-free loop. Update batch: about 280 ops touching about 1,018 nodes (12 panel inserts of 61 nodes, previous batch's panels removed, 6 paragraph removals, one keyed `SyncChildren`, 150 text changes, 100 attribute changes, `Reap`).

Results: `bench/results/doc-mb/2026-10-14-run2` .. `run4` (identical code, three consecutive runs). `run1-grow-bug` is kept as recorded: its update phase had a harness bug (inserted 720 nodes per batch and never removed them, so the document grew from 1k to 171k nodes during the measurement); superseded by run2-4, which remove the previous batch's insertions.

Conditions: Intel Core Ultra 9 285H, 16 cores, governor `powersave`, loadavg 7-8 from other agents' work (not a quiet host). Medians of run2-4 shown; run-to-run spread is within about 10% except the 1M tail (run4 p99 batch 1.21 ms vs 0.47-0.61 ms).

| nodes | build | heap/node | next heading p50 / p99 | next link in landmark p50 / p99 | kth heading p50 | at_offset p50 | word at offset p50 | batch apply+publish p50 / p99 | publish() only p50 | batch incl. deferred replay p50 / p99 |
|---|---|---|---|---|---|---|---|---|---|---|
| 1k | 0.1 ms | 223 B | 119 ns / 202 ns | 160 ns / 246 ns | 14 ns | 39 ns | 373 ns | 69 us / 83 us | 0.2 us | 139 us / 165 us |
| 10k | 1.9 ms | 355 B | 196 ns / 303 ns | 316 ns / 447 ns | 50 ns | 78 ns | 660 ns | 93 us / 106 us | 0.5 us | 196 us / 220 us |
| 100k | 17 ms | 287 B | **331 ns / 505 ns** | 557 ns / 817 ns | 210 ns | 170 ns | 927 ns | **213 us / 312 us** | 0.3 us | 488 us / 685 us |
| 1M | 167 ms | 232 B | 725 ns / 1.67 us | 1.18 us / 2.5 us | 1.3 us | 470 ns | 2.2 us | 430-500 us / 0.47-1.2 ms | 0.5 us | 0.95-1.17 ms / 1.0-2.2 ms |

(heap/node is per copy; the double buffer holds two.) Publish latency of a one-op change is 80-220 ns at every size; it does not depend on document size because it is one pointer swap.

Reader under writer load (one reader thread issuing `at_position` + `next heading` per sample, writer running batches continuously for 1.5 s): 100k nodes p50 0.39 us, p99 1.2 us (two queries per sample); 1M p50 2.7 us, p99 5.1 us; zero clone fallbacks, zero waits above 1 ms.

Targets:

- Nav queries <= 1 us at 100k: met. Every category query is 0.29-0.33 us p50, 0.46-0.51 us p99; the scoped and per-landmark variants are 0.56-0.59 us p50, p99 0.82-0.86 us. The B+tree upgrade path (GM option E) is not needed at this size; at 1M the treap p50 is 0.5-0.8 us and p99 1.1-1.8 us.
- 1k-node update batch with publish <= 1 ms: met with margin at every size up to 1M on the writer's critical path (0.07 ms at 1k, 0.21 ms at 100k, 0.43-0.5 ms at 1M). Including the deferred replay on the other copy (which a real writer runs during idle time before the next batch) the steady-state cost is 0.14 / 0.2 / 0.49 / about 1 ms at 1k / 10k / 100k / 1M, i.e. borderline only at 1M.

## Design notes and decisions

- `left-right` crate vs arc-swap: chose arc-swap because the workspace denies `unsafe` and `left-right` is not vendored. The cost is the clone fallback (above) in place of left-right's unbounded wait.
- Treap summary is a concrete struct, not the ADR's `Summary` trait: no second implementation until the B+tree option is taken.
- Ranked-category count array is 16 wide (64 B/node); 287 B/node total at 100k including the arena slot (about 90 B), treap node (48 B), counts (64 B) and strings.
- `DocNodeId` here is `gorca_doc::web::DocNodeId {slot, gen}`. `gorca_model::DocNodeId(u32)` is still what the scaffold `mirror.rs` uses; M7 must reconcile the two (ADR places the type in `gorca-model`).

## Risks and open questions

- Deeply nested trees: `insert_pos` and `detach_core` walk the last-child chain (`last_desc`), O(depth). Fine for realistic DOM depth (tens to low hundreds); a 20k-deep chain works but each insert under it is O(depth). A cached subtree end would fix it if profiling asks for it.
- Treap split/merge recurse to the treap depth (about 2 ln n, max 38 at 100k, GM). Priorities are a hash, so adversarial depth is not a concern; the recursion is not guarded.
- Dropping a very deep `SubtreeSpec` recurses (source side; the 20k-deep test uses a 64 MB thread stack).
- Hidden (`NodeFlags::HIDDEN`) affects the node itself only, not its descendants; the source must hide subtrees explicitly.
- Defunct subtrees live until `Reap`: the doc actor must call it after the re-match window (W_max) or memory grows.
- Word and line queries are O(line length) in the local text, so a single 1 MB line would cost milliseconds. Soft wrapping and sentence/grapheme units (unicode-segmentation) are not implemented.
- Not implemented here (out of scope for this step): tables grid (section 4.6), live-region registry (4.7), `rels` reverse index, `Coverage`/`Fresh` states, dirty set and plan DP (sections 9.2-9.4), burst classification. They sit above these primitives.
- The update benchmark shrinks the 10k document to 4.2k nodes and the 100k one to 91.7k nodes over 300 batches (sync and removal ops drop more than they add); the 1k and 1M documents stay at their size. Timings at 100k are therefore for 92-100k nodes.
- Bench host was loaded and on the `powersave` governor; numbers should be repeated on a quiet host with `performance` before they are used as gates.
