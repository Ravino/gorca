# Web document as a typed graph, and where N-dimensional embeddings help

Author role: math-researcher. Date: 2026-10-12. Section numbers follow the task brief (1 graph model, 3 diffing, 4 embeddings; there is no §2).
Input: `docs/reports/2026-10-12-web-a11y-browsers.md` §4.9, ADR-0001 §3.
Code: `research/web-graph/` (see its README). Machine: Intel Core Ultra 9 285H, Debian 13 RT kernel.
All numbers below were measured unless marked **[estimate]** or **[unverified]**.

---

## 0. Verdict on the user's idea

The idea is "represent the document as a graph with typed edges embedded in an N-dimensional vector space, to make navigation smarter and faster". It has two halves, and they get different verdicts.

1. **Typed graph: yes, build now.** Navigation becomes faster with exact combinatorial structures over the graph, not with geometry. The navigation index answers "next heading after X" in 0.1–0.4 µs p50 and < 2 µs p99 on a 100k-node page in Rust, during a mutation storm. The latency budget is 5 ms, so this is three to four orders of magnitude under it.
2. **N-dim embeddings: useful in one narrow, background role.** They never sit on the keypress path. ANN indexes (HNSW and similar) are unnecessary at our scale once exact duplicates are collapsed.

| Use | Verdict | Why (measured where possible) |
|---|---|---|
| Structural navigation (next/prev/k-th of a type) | **Now**, no embeddings | Augmented treap over document order: next/prev p99 1.7 µs, k-th p99 1.5 µs at 100k nodes |
| Repeated-structure grouping ("collapse 24 product cards", "item 3 of 24") | **Now** (exact part); **soon** (fuzzy part) | Wrapper contraction plus exact skeleton hash: precision 1.00 but recall 0.41. Adding fuzzy WL vectors with brute-force kNN on the *unique* skeletons (~1.7k at 100k nodes) raises pairwise F1 from 0.58 to 0.92. No ANN needed |
| "Jump to next similar item" | **Now**, as a lookup into the group index built above | On semantically marked-up pages, "next node with the same role" already scores 0.97. On div-soup pages: role baseline 0.88, structural clusters 0.90–0.92. A modest gain on synthetic data |
| Identity and position across SPA re-renders | **Now**, with tree matching, not embeddings | GumTree-style matcher: precision 0.95–0.96, recall 0.85–0.92, cursor restored 85–92% (the naive index-path baseline gets 0.6–2%) |
| Same layout across visits | **Later**, keyed by URL plus tree matching | MinHash set fingerprints cannot tell "same page re-rendered" (J 0.34–0.64) from "other page on the same template" (J ≈ 0.39). A negative result |
| Landmark inference on poorly marked-up pages | **Later**, as heuristics or a classifier, not ANN | It is a segmentation/classification problem. It needs labelled real pages, which we don't have |
| Semantic search ("go to the login form") | **Later**, as a plugin | Targets number ≤ a few thousand (headings, landmarks, controls), so brute force over a small text model's vectors suffices. Do lexical/fuzzy search first. No model in the venv, so quality is unmeasured |
| node2vec / DeepWalk / GNN node embeddings | **Never** | Transductive (retrain on every mutation), random-walk cost O(n·walks·len), nondeterministic, and no better than WL features for structural roles |
| ANN index on the keypress path | **Never** | Not needed: the hot path is an ordered-set lookup |
| Embedding every node of a 100k page | **Never** | HNSW build takes 1.4–11 s single-threaded on the duplicate-heavy all-node set, with recall 0.84–0.94; the duplicates make it pathological |

The **one genuinely useful vector space is 2-D**: bounding boxes for visual adjacency ("what is to the right of this"), served by an R-tree. See §1.3.

---

## 1. Graph model

### 1.1 Formulation

Let the document be a typed multigraph G = (V, E, τ, μ). V is the set of accessible objects (core `NodeKey`, ADR-0001 §3.1), τ: E → EdgeType, and μ: V → attributes (role, states, name, category mask). A11y graphs have heterogeneous degree and cardinality per edge type, so each type gets the representation that fits it. A single generic adjacency list (petgraph-style) fits none of them well.

| Edge type | Cardinality | Representation | Update cost |
|---|---|---|---|
| parent/child | tree, n−1 edges | intrusive links in the arena node: `parent, first_child, last_child, prev_sib, next_sib` (u32) | O(1) per link |
| reading order (document order) | total order | **implicit** in the order index (§1.2); never stored as edges | O(log n) |
| flows-to / flows-from | sparse overrides of reading order | forward `SmallVec` in node + reverse map | O(deg) |
| labelled-by / label-for, described-by / description-for, controls / controlled-by, details, error-message, member-of | sparse, degree ≤ ~4 | forward list in `NodeSnapshot.relations` (already in ADR-0001) + **reverse index** `HashMap<NodeKey, SmallVec<(Rel, NodeKey)>>` | O(deg) per node update |
| table row/column | grid | per-table grid structure (§5.2), not edges | O(R·C) per table rebuild, small |
| same-landmark | **equivalence classes, not edges** (materialising would be O(k²) cliques) | "landmark of v" = nearest landmark ancestor: O(depth) walk, or a lazily cached id invalidated on subtree moves | O(depth) |
| visual adjacency | geometric, k-NN in 2-D | R-tree over bounding boxes of nodes whose extents are fetched (viewport) | O(log n) per insert; bulk load O(n log n) |
| in-page link targets | sparse | `HashMap<fragment-id, NodeKey>` built from `id` attributes + link URIs | O(1) |
| "same component family" (derived, §4) | equivalence classes | cluster id per node; per-cluster ordered set | O(log n) |

Assumptions: n ≤ 10⁶ nodes per document (u32 indices); category count ≤ 64 (u64 mask; the prototype uses 19); relation degree O(1).

### 1.2 Document-order index (answers 4.9 Q1)

Basic fact (proven): in preorder, every DOM subtree is a **contiguous interval**. Inserting, deleting or moving a subtree is therefore a splice of a contiguous block in one sequence. All "next/prev/k-th node of category c relative to v" queries are order queries over that sequence.

Candidates evaluated, all implemented and differentially tested against a brute-force oracle (`test_navindex.py`: 6 seeds × 300 random mutations; adversarial same-point inserts):

| Structure | next/prev | k-th / rank | insert k nodes | delete k nodes | compare(a,b) | notes |
|---|---|---|---|---|---|---|
| **A. Order-maintenance labels** (Bender et al. 2002, u62 universe, T=1.5) + per-category `BTreeMap<label, node>` | O(log n) | O(k) (no rank in BTreeMap) | O(k log n) + amortised O(log n) relabels/insert | O(k log n) | **O(1)** | relabel spikes |
| **B. Implicit treap over preorder**, augmented with size, OR-mask, per-category counts | O(log n) exp. | **O(log n)** | O(k + log n) (O(k) Cartesian build + split/merge) | O(log n) split/merge (+O(k) free) | O(log n) (two ranks) | no spikes |
| C. Fenwick tree over DFS positions | O(log n) | O(log n) | **O(n)** renumbering | O(n) | O(1) | static only: rejected |
| D. Euler-tour tree (balanced BST over Euler tour) | as B | as B | as B | as B | O(log n) | twice the elements of B for no gain, because we never re-root: B is the special case we need |
| E. Chunked B+tree "sum tree" (rope with monoid summaries) | O(log_B n) | O(log_B n) | O(k + log_B n) | same | O(log_B n) | same algebra as B, better cache behaviour, more code |

Measured, Rust (`research/web-graph/rust-bench`, std only, release + thin LTO). Workload: realistic mutation mix (40% leaf replace, 20% category flip, 25% SPA component re-render of 20–500 nodes, 15% infinite-scroll append of 50–300 nodes), interleaved with queries. Every query's answer was checked against the Python reference, with **0 mismatches** in all runs.

| n = 100,184 | OM + BTreeMap | Treap (augmented) |
|---|---|---|
| build | 7.0–7.7 ms | 7.1–8.6 ms |
| next/prev p50 / p99 | 0.13–0.16 µs / 0.43–0.47 µs | 0.41–0.42 µs / 1.66–1.72 µs |
| k-th p50 / p99 | 1.3 µs / **44 µs** (O(k)) | 0.32 µs / 1.5 µs |
| insert, per node amortised | 0.082–0.086 µs | 0.060–0.061 µs |
| insert p99 per mutation | 29–31 µs | 22 µs |
| delete, per node amortised | 0.080–0.084 µs | 0.029 µs |
| delete p99 per mutation | 27–29 µs | 2.6–2.8 µs |
| category flip p50 | 0.13–0.19 µs | 0.36–0.37 µs |
| adversarial: 100k inserts at one point | mean 3.7 µs, **max 13 ms** single relabel, 11 relabels/insert | no spikes (depth-bounded) |
| adversarial: 1M inserts at one point | **max 174 ms** relabel spike, 13.8 relabels/insert | — |
| memory per node **[estimate]** | ~40 B × (1 + #categories) in B-tree nodes | 108 B (`TNode` with 19 u32 counts) |

At 10k nodes, all operations are 1.5–3× cheaper (see `research/web-graph/traces/rust-nav.txt`). Treap depth at 100k nodes: max 38, mean 20.5, which matches the 2 ln n − O(1) expectation. The Python reference numbers are in §8.

**Recommendation (medium-high confidence): B, the augmented implicit treap over preorder, in a generational arena.** Its advantages:
- O(log n) rank and k-th, which serve "heading 5 of 23" and "item 3 of 24".
- No relabel spikes. An amortised structure that occasionally stalls 13–174 ms is the wrong shape for a latency-bound actor.
- The cheapest deletes (subtree delete is two splits and one merge).
- One structure for every category.
- The same monoid-summary machinery also solves the flat-text offset map (§5.1). One tree, several "dimensions": node count, per-category counts, UTF-8 bytes, UTF-16 units, chars, newlines.

The 3× slower next/prev compared with A is irrelevant: 1.7 µs p99 against a 5 ms budget.

Upgrade path: if profiling shows cache misses dominate, move to E (a chunked B+tree with the same summary trait, in the style of Zed's `sum_tree`; that crate's licensing and crates.io availability are **[unverified]**). Per-node counts then move into internal nodes, cutting memory about 10×.

OM labels remain useful as a **cache**. A per-node `u64` label refreshed lazily gives O(1) compare for sorting dirty sets. It is optional.

Hot-path query mapping:
- next/prev of category c after v: walk up from v, then down via OR-masks; O(depth) = O(log n) expected.
- k-th of c: descend by counts.
- rank of v within c: walk up summing left counts.
- **nearest of type c in reading order**: compare rank distance of next and prev.
- **next in the same landmark**: next(c, v), then check rank(result) < rank(landmark) + size(landmark).
- **next similar item**: cluster ids get their own per-cluster ordered set, or a category bit for the top 64 clusters.

### 1.3 Arena vs persistent structure

- **Arena (recommended).** Use a `slotmap::SlotMap` with generational keys, which matches ADR-0001 invariant I-1 (no key reuse). The document actor owns the mirror and the indexes. Mutations are applied in **budgeted slices** (≤ 1–2 ms of work each). Navigation queries are served between slices with priority over mutation work. The worst-case wait for a query is then one slice plus the query, whatever the storm size. This is the non-preemptive priority-queue bound: a high-priority job waits at most the residual of the low-priority job in service.
- **Persistent (im-rs / rpds HAMT/RRB).** Cheap snapshots for concurrent readers, but each update costs 2–5× more allocation **[estimate]**. The actor model has a single owner, so the snapshot capability buys nothing. If another thread ever needs a consistent view (for example, braille rendering), publish an immutable *presentation summary* (`Arc<[Line]>`) rather than the whole tree. Rejected for now.
- **petgraph.** Not for the mirror. Its generic edges and lack of an ordered-children concept add overhead and indirection. It is fine for offline algorithms in tooling.
- **Visual adjacency.** Use an `rstar` R*-tree over the rectangles of nodes whose extents are known (viewport and its neighbourhood). Bulk-load at viewport change; queries are directional kNN. Extents are the most expensive AT-SPI data (browser report §4.2), so this index is lazy and partial by design. Cost was not measured **[estimate: bulk load of 10k rectangles ≈ 1–3 ms]**.

### 1.4 Cost analysis, 10k–100k nodes, 1k–10k mutations/s

Treap cost per structural mutation of k nodes ≈ 0.06 µs·k (insert) + 0.03 µs·k (delete) + ~1 µs. Measured mix: mean inserted subtree of 74 nodes.

| Load | Index maintenance CPU **[derived from measured per-node costs]** |
|---|---|
| 1k mutations/s, 100k nodes | ≈ 1k × (74 × 0.09 + 1) µs ≈ 8 ms/s ≈ 0.8% of one core |
| 10k mutations/s, 100k nodes | ≈ 77 ms/s ≈ 8% of one core |
| 10k leaf-only events/s | ≈ 10–20 ms/s |

The data structures are not the bottleneck. **IPC is**: the browser report's target is ≤ 0.3 ms/node of fetch. 10k mutations/s × 74 nodes would need 222 s of fetch per second. The engineering effort therefore belongs in coalescing and the re-fetch policy (§3.4, §3.5), not in index constants.

---

## 3. Tree diffing and incremental reconciliation (answers 4.9 Q2–Q3)

### 3.1 Keyed child-list diff: optimal and cheap

When identity is preserved (AT-SPI object references), diffing a parent's old and new child-key lists is a sequence problem. Let C = keys present in both, and let s be the new positions of C taken in old order.

**Theorem (proven; verified exhaustively for m ≤ 6 and on 2000 random cases).** The minimum number of single-element moves is |C| − LIS(s). *Proof.* Elements that are not moved keep their relative order, so they form an increasing subsequence of s; hence #unmoved ≤ LIS(s). Moving exactly the complement of one LIS achieves the bound. ∎

Cost: O(m log m) per parent (patience sorting). The recursive forest diff descends only into surviving children marked dirty, so its total cost is Σ over dirty parents of mᵢ log mᵢ.

### 3.2 Unkeyed matching (identity lost)

Identity is lost when Chromium rebuilds the tree on navigation or when an SPA recreates accessibles. General tree edit distance is O(n³) (Zhang–Shasha, Demaine et al.) and unusable at 100k nodes. A GumTree-style matcher (Falleri et al. 2014) is enough (`diff.match_trees`):
1. Bottom-up hashes: skeleton (roles) and full (roles + names). Match subtrees whose full hash is **unique on both sides**, highest first.
2. Bottom-up: pair an unmatched container with the same-role candidate sharing the most matched descendants, requiring Dice ≥ 0.5. Candidates come from ancestors of matched descendants, so this step is O(n·depth).
3. Top-down inside matched parent pairs: LCS alignment of leftover children on full hash, then on (role, name), then on role.

Complexity: O(n log n + n·depth) time, O(n) memory. Not optimal in edit-distance terms; this is a heuristic with no approximation guarantee.

Measured on simulated SPA re-renders (`test_diff.py`). Ground truth is the copy map; the cursor is restored on 2000 random surviving leaves.

| Scenario | n (old → new) | Python time | precision | recall | cursor restored | index-path baseline |
|---|---|---|---|---|---|---|
| identity loss only | 9,939 → 9,939 | 43 ms | 1.000 | 1.000 | 1.000 | 1.000 |
| default (5% delete, 5% insert, 10% text, 5% swap, 3% wrapper) | 9,939 → 6,870 | 58 ms | 0.963 | 0.920 | 0.916 | 0.020 |
| default | 99,896 → 81,114 | 747 ms | 0.949 | 0.845 | 0.850 | 0.006 |
| heavy (15/15/30/15/10%) | 9,939 → 5,710 | 48 ms | 0.828 | 0.586 | 0.623 | 0.003 |

Error analysis: most precision loss comes from delete + insert of same-template components. A new card gets matched to the deleted card in the same slot. For cursor restoration this is arguably the desired behaviour. The Rust cost **[estimate]** is 20–50× below Python, which puts 100k nodes at ~15–40 ms in the background.

### 3.3 Position preservation

`restore_cursor` maps (node, offset) in three steps:
1. Matched node: same node, offset clamped.
2. Otherwise: nearest matched ancestor, then descend by (child index, role).
3. Otherwise: nearest preceding matched node in document order.

This is NVDA's bookmark idea made structural. Store **(NodeKey, offset, ancestor-path signature, text hash of the node)** as the reading position, so that the same machinery works across reloads (§4.2).

### 3.4 Optimal re-fetch cover: exact, linear-time

Model: dirty set D. Each dirty node v can be patched alone at cost a_v. Alternatively, an ancestor u can be re-fetched whole at cost α + β·size(u), which resolves every dirty node below it. Here α is the per-request overhead (round trip, measured 0.1–0.3 ms p50) and β is the per-node fetch cost.

best(v) = min(α + β·size(v), [v ∈ D]·a_v + Σ_{children c with dirty below} best(c))

**Exact (proven by optimal substructure: subtrees are disjoint and costs additive; verified against brute force on 300 random trees).** The DP is not a knapsack. It runs on the virtual tree of D in O(|D|·depth).

NVDA's "ancestor dominance" is the special case where every dirty node is re-fetched whole.

With a **per-frame work budget** B, choosing which dirty roots to process now *is* a knapsack variant. Use the greedy rule instead (focused/viewport first, then by cost ratio); it is not provably optimal and does not need to be.

### 3.5 Coalescing under storms (Q3): bounds

Policy: per-key (target, kind) dirty map, ancestor dominance, trailing debounce W with max-wait W_max, and an immediate class (focus, caret) that is never debounced. Consequences (all proven, elementary):
- **Memory** is bounded by the number of distinct dirty keys (≤ n), not by the arrival rate λ. Ingest is O(1) per event (hash insert).
- **Refresh rate** per key ≤ 1/W. Total refresh jobs/s ≤ min(λ, K_active/W), where K_active is bounded by the antichain of dirty roots after dominance.
- **Staleness** of any key ≤ W_max + one slice + queueing in the refresh worker. If the worker's utilisation is ρ = rate·E[S] < 1, the M/G/1 mean wait is ρ·E[S²]/(2(1−ρ)·E[S]) (Pollaczek–Khinchine). Under overload (ρ ≥ 1), the bounded-queue rule "overflow → mark document dirty → rebuild" caps work at one rebuild.
- **Hot-path latency** ≤ slice length + query cost, independent of λ (§1.3).

Suggested start values: W = 50 ms, W_max = 150 ms, slice = 1 ms. **[Heuristic: tune with perf-engineer's storm generator.]**

Text diff: common prefix and suffix (O(L)), as Chromium does. Use Myers O((N+M)D) only for re-rendered live regions.

---

## 4. N-dimensional embeddings: where they help and where they don't

### 4.1 End-to-end use case: repeated structures and "next similar item"

Chosen because it is the most valuable. It powers collapse-to-group, "item k of N" and "jump to next card", and it is the use most likely to beat a trivial baseline.

Pipeline (background, after build and after each settled mutation batch):
1. **Canonicalise**: contract presentational wrappers (a `generic` node with one child, or the sole child of a semantic node). O(n). 100k → 95k nodes.
2. **Exact skeleton hash** (a Merkle hash over roles) for every node. 0.17 s in Python **[estimate in Rust: ~5 ms]**.
3. **Dedupe**: group the pool (internal nodes with ≥ 3 descendants, 16k at 100k nodes) by skeleton hash. This leaves **1.7k unique groups**.
4. **WL features** per group: role, (depth, role) for depth ≤ 3, parent→child edges, WL-1/WL-2 labels, and a coarse text-length bucket. Feature-hashed to **D = 128**, log-scaled, L2-normalised. 0.8–1.0 s for the pool in Python. Incremental: only re-embed ancestors within depth 3 of a mutation.
5. **Brute-force kNN** (k=16) among group means, threshold τ, union-find into clusters. 15–27 ms in Python. A single query costs 15 µs at this size.
6. Per-cluster **ordered set** in the document-order index. "Next similar" = next(cluster(v), v), O(log n), on the hot path.

Quality at 100k nodes (`research/web-graph/traces/embed100k.txt`). memP/memR are pairwise precision/recall among ground-truth component members. "next-sim" is the fraction of jumps from a member that land on the true next member of its family.

| method | semantic markup, canon: F1 / next-sim | div-soup, canon: F1 / next-sim |
|---|---|---|
| baseline: same role | 0.64 / **0.973** | 0.43 / 0.878 |
| baseline: same role + parent role | 0.75 / 0.910 | 0.45 / 0.888 |
| exact skeleton hash | 0.58 / 0.566 | 0.58 / 0.566 |
| kNN on all pool vectors, τ=0.9 (no dedupe) | 0.44 / 0.355 | 0.44 / 0.351 |
| **dedupe + kNN16, τ=0.85** | **0.92** / 0.915 | 0.82 / **0.915** |
| **dedupe + kNN16, τ=0.90** | 0.83 / 0.804 | **0.92** / 0.902 |
| SimHash 4×16 banded LSH | 0.31 / 0.630 | 0.31 / 0.652 |

Without canonicalisation, every structural method is 0.2–0.3 worse on next-sim (see the file).

Findings:
- **Canonicalisation and dedupe matter more than the embedding.** kNN without dedupe collapses (F1 0.44): thousands of identical items fill every k-NN list, so template variants never link. This is the classic hubness/duplicate pathology.
- On pages with good semantics, the trivial "next same role" wins at jumping. Embeddings add value for **grouping purity** (F1 0.92 vs 0.64), for **counts**, and for **div-soup pages**, where they give a +2–4 point next-sim gain over the role baseline.
- τ was chosen on the same synthetic data (0.85–0.90 is best at both sizes), so it is **not validated on real pages**.

### 4.2 ANN index choice (measured, single thread, D=128, k=10)

`research/web-graph/traces/ann100k.txt`:

| dataset | index | build | query p50 / p99 | recall@10 | add / remove |
|---|---|---|---|---|---|
| unique groups, n=1,679 | numpy brute | 0 | 15 µs / 19 µs | 1.000 | trivial |
| | faiss Flat | 0.2 ms | 13 / 17 µs | 1.000 | |
| | hnswlib M16 | 35 ms | 14 / 20 µs | 1.000 | 25 µs / 0.18 µs |
| pool, n=16,365 | numpy brute | 0 | 210 / 247 µs | 1.000 | |
| | faiss HNSW M16 | 398 ms | 16 / 25 µs | 0.966 | 52 µs |
| | hnswlib M16 | 477 ms | 12 / 22 µs | 0.908 | 31 µs / 0.24 µs |
| | usearch M16 f32 / i8 | 760 / 362 ms | 21 / 16 µs | 0.837 / 0.610 | 53 / 26 µs |
| all nodes, n=95,177 | faiss Flat | 24 ms | 2.0 / 2.9 ms | 1.000 | |
| | faiss HNSW M16 | 1.4 s | 8 / 65 µs | 0.839 | 183 µs |
| | hnswlib M16 | 4.5 s | 6 / 56 µs | 0.930 | 56 µs / 0.31 µs |
| | usearch M16 f32 / i8 | 11.0 / 3.4 s | 15 / 10 µs | 0.937 / 0.900 | 131 / 35 µs |

Conclusion: at our real problem size (≤ a few thousand unique structures per page), **brute force wins**. It is exact, needs no build, and updates are free. HNSW only pays off past ~10⁴ distinct vectors, and duplicate-heavy data hurts its recall. If an ANN index is ever needed (cross-page or cross-site memory), use hnswlib or usearch (f32); the bindings are `usearch` for Rust and `hnsw_rs` / `instant-distance` in pure Rust **[crate APIs unverified]**. Avoid i8 quantisation for these sparse-ish hashed vectors.

### 4.3 Other candidate uses

| Use | Method | Dim | Index | Build / query at 100k | Incremental | Path | Verdict |
|---|---|---|---|---|---|---|---|
| Find similar / collapse groups | §4.1 | 128 | brute force on unique groups | ~1–2 s Python, **[est. 30–80 ms Rust]** / 15 µs | re-embed ancestors within depth 3 of a mutation | background; hot path is an ordered-set lookup | **Now** |
| Same layout across visits | MinHash-128 over skeleton or local shingles | 128 hashes | per-site table keyed by URL | 5–7 ms per page (Python) | recompute | background | **Later**, keyed by URL + `match_trees` against a stored compact skeleton. Fingerprints alone don't discriminate: local-shingle J is 0.64 / 0.48 / 0.34 for light/default/heavy re-renders of the *same* page vs 0.39 for a *different* page of the same template (`layout_fp.py`) |
| Landmark inference | features (role, tag, link density, text density, depth, position, size, cluster id) → classifier or rules (Boilerpipe/VIPS-style) | ~30 hand-made features | none | O(n) | per-subtree | background | **Later**; needs labelled real pages |
| Semantic search | small static or sentence text model over names of headings, landmarks, controls and buttons (≤ ~5k items) | 256–384 | brute force (≤ 5k × 384 ≈ 2M FLOP, about 0.2 ms) | model-dependent **[unverified, no model in venv]** | embed new names only | background; query on explicit command | **Later**, as a WASM/extension plugin, after lexical BM25 + trigram fuzzy search |
| Graph embeddings (node2vec, GNN) | random walks + skip-gram | 64–128 | — | seconds to minutes | retrain | — | **Never** |

---

## 5. Remaining questions from the browser report

### 5.1 Flat-offset text map (Q4)
Use the same augmented sequence tree (§1.2) with a text summary per node: (utf8, utf16, chars, newlines).
- offset ↔ (node, local offset) in any unit: O(log n) descent.
- Line k: descend by newline count. Word/sentence/grapheme boundaries are computed **locally** within the node with `unicode-segmentation`. Line boundaries come from `\n` and block boundaries when extents are absent.
- A mid-document edit updates one leaf's summary and its O(log n) ancestors.
- U+FFFC: each embedded child is its own element in the sequence, so hypertext offsets map structurally.

### 5.2 Table index (Q5)
- Per table, a dense row-major `Vec<Option<CellIdx>>` of R×C slots, where a spanning cell fills all its slots, plus per-column and per-row header vectors. (row, col) → cell is O(1).
- Row insert/delete rebuilds that table's grid in O(R·C). That is ≤ 10⁴ slots in practice, ≈ 10 µs **[estimate]**.
- Interval or 2-D range trees are unnecessary.
- Virtualised grids (aria-rowcount ≫ rendered rows) use a sparse `BTreeMap<(row, col), CellIdx>` instead.

### 5.3 Live-region dedupe (Q6)
- Normalise the message: truncate to L = 512 chars, mask digits to `#`, collapse whitespace, case-fold.
- Use a 64-bit hash plus a per-region ring of the last m = 8 (hash, time) pairs. Suppress if the same hash appeared within T seconds.
- Near-duplicates: 64-bit SimHash over character 3-grams, Hamming distance ≤ 3.
- Work per event is O(L + m), a hard bound.

### 5.4 ANN on the flat text (Q7)
Out of scope for milestone 1, as the browser report says. See the semantic-search row in §4.3.

---

## 6. Rust porting notes

- **Arena:** `slotmap::SlotMap<NodeKey, Node>` (generational, satisfies I-1). Intrusive tree links as u32 indices; the order-index node lives in the same slot as the mirror node.
- **Order index:** port `TreapIndex` from `research/web-graph/rust-bench/src/main.rs` (≈ 300 lines, std only):
  - recursive split/merge (depth ≤ ~50, no stack risk);
  - xorshift priorities;
  - O(k) Cartesian-tree build for bulk inserts;
  - parent pointers for rank and upward search.
  Make the summary a trait (`size`, `mask`, `cnt[..]`, text lengths) so §5.1 reuses it. Gate a B+tree rewrite on profiling.
- **No `unwrap`:** queries return `Option`. Index invariants are checked in `debug_assert!` and in a property test that replays `traces/testvec-nav2k.trace`, whose expected answers are produced by the Python reference.
- **Diff:** LIS via patience sorting. `match_trees` uses `HashMap<u64, u32>` with blake2/xxh3 64-bit hashes (`xxhash-rust`) and a `u32` matched-pair bitmap. Cap LCS at 250k cells with greedy fallback, as the reference does.
- **Embeddings:** feature hashing into `[f32; 128]`, brute-force dot products (auto-vectorised; `asm-simd-engineer` only if profiling says so), union-find. No ANN crate is needed. If ever needed: `usearch` (C++ via cxx) or pure-Rust `hnsw_rs`.
- **Geometry:** the `rstar` R*-tree for visual adjacency.
- **Avoid:** petgraph for the mirror; persistent collections (im/rpds) on the actor's hot state.

---

## 7. Proven vs heuristic

- **Proven:** subtree contiguity in preorder; minimal moves = |C| − LIS; exactness of the re-fetch cover DP; coalescing memory and rate bounds; the hot-path bound under budgeted slices; the expected O(log n) treap depth (standard result; measured max 38 and mean 20.5 at 100k).
- **Cited, not re-proven:** amortised O(log n) relabels for Bender et al. OM (measured 11–14 relabels/insert under the adversarial pattern).
- **Heuristic:** the GumTree-style matcher (no approximation guarantee); the debounce constants; τ for clustering; the greedy budgeted refresh.

## 8. Python reference numbers (for comparison only)

From `bench_navindex.py run`, 100k nodes:
- OM next p50 0.8 µs, build 71 ms.
- Treap next p50 2.7 µs, build 202 ms.

The Python suite (`test_navindex.py`, `test_diff.py`) runs in ≈ 13 s.

## 9. Risks and open questions

- All quality numbers come from a synthetic generator. Real pages (React div soup, shadow DOM, virtualised lists) may differ. Next step: dump 20–50 real AT-SPI trees from Firefox and Chromium (web-a11y-engineer) and re-run `embed.py` and `test_diff.py` on them.
- Rust costs of the matcher, the embeddings and the R-tree are estimates.
- Treap memory (108 B/node with 19 counts) grows with the category count. Keep the counts only for categories that need rank, or move to the B+tree.
- The OM relabel spike (13 ms at 100k, 174 ms at 1M adversarial inserts) is why it was rejected. A de-amortised OM would fix that but adds complexity.
- Whether Firefox and Chromium expose DOM `id` for in-page anchors over AT-SPI is **[unverified]**.
