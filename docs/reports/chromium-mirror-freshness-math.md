# Chromium mirror freshness: staleness detection, bounded re-sync, cursor identity

Date 2026-10-19, math-researcher. Inputs: `docs/reports/chromium-electron-a11y-research.md` (Signal: mirror
built at 4 nodes, document grew to ~900, 312/312 focus misses), ADR-0022, `crates/gorca-doc-source-atspi/src/
{events,source,identity,fetch}.rs`, cost numbers from `docs/reports/2026-10-12-web-source-layer.md` and
`m9-baseline.md`. Prototypes: `tools/research/chromium_mirror_sim.py` (freshness),
`tools/research/chromium_cursor_identity.py` (cursor identity). Both run in the project venv, ~1 min.
The architect's layering (`docs/reports/chromium-engine-base-architecture.md`) was not yet available; the
decision points in section 6 are phrased so they slot into any layering that has a Chromium-engine doc-source
layer under a backend-agnostic core.

## 0. Facts the analysis rests on

| # | Fact | Status |
|---|---|---|
| F1 | AT-SPI object paths from at-spi2-atk are `/org/a11y/atspi/accessible/<n>` with `n` from a monotonic `guint` counter (`refs/at-spi2-core/atk-adaptor/accessible-register.c:156 assign_reference`), so a path is never reused while the counter does not wrap (2^32 allocations). | proven from source |
| F2 | Chromium fires `children-changed:add/remove` only for the root of a created/deleted subtree (`ax_platform_node_auralinux.cc` `OnSubtreeCreated/OnSubtreeWillBeDeleted`); shrink can be silent (35 -> 16 nodes with zero events in the Signal probe). | source + measured |
| F3 | The source drops events whose node is not in `live.locals` (`source.rs::to_atspi`, `key_of(node)?`). | code |
| F4 | `Collection.GetTree` is one D-Bus handler on the browser UI thread; Chromium applies AX tree updates on the same thread, so one call returns a consistent snapshot of its subtree. | **inference**, not verified |
| F5 | Costs: call RTT 0.1-2 ms (P2P p50 85-250 us); `GetTree` ~10 us/node server time (106k nodes 1.1 s); pipelined per-node passes ~25 us/node (window 64); role and state are separate passes. | measured (earlier reports) |
| F6 | `GetMatches` (paths only) server cost assumed 3 us/node. | **assumption, unmeasured** |

## 1. Model and staleness detection

**Model.** Truth is a sequence of trees T_0, T_1, ... produced by mutations (subtree insert, subtree delete,
same-path move, in-place property change). The observation channel maps each mutation to at most one event:
insert -> `add` on the parent with probability p_add, delete -> `remove` with p_rm (small for Chromium),
in-place rename -> `name-changed` with p_name, same-path move -> nothing. Focus events are always delivered and
carry the path. The mirror M is a partial copy keyed by path.

**Proposition 1 (staleness is absorbing under drop-unknown routing).** With F3, let U = nodes of T not in M. A
node created under a node of U is in U at creation; events whose source is in U are dropped; the only way a
node leaves U is a relist of its lowest known ancestor (LKA), which happens only when an event lands on the LKA
itself. So once one `add` at a fresh subtree root is lost (or arrives before the parent is known), every later
growth below it stays invisible, forever, unless the LKA happens to get another child-list event. Proof: by
induction over mutations; the only M-changing transitions are relists/re-reads of known nodes and a full
rebuild. This is the Signal failure shape (loading shell replaced; everything after is under unknown nodes).
The simulator reproduces it: with the Chromium profile (p_add 0.97, p_rm 0.4, p_name 0.95) strategy S0
(today's routing) got stuck in 1 of 8 runs and averaged 85 % focus hits; with p_add 0.8, 50 % of runs stuck and
46 % hits.

**Corollary.** A detector must not depend on the mirror knowing the event source. The cheapest such signal is
the event itself.

### Detectors, cost and detection power

| Detector | Cost | What it detects | FP sources and mitigation |
|---|---|---|---|
| D1 **orphan event**: any event (children-changed, name, state, text, caret) whose path is not in M but whose app owns a mirrored document | O(1) hash lookup, 0 IPC | any unknown region that emits anything; by F1 "unknown path" is an exact "created after the last fetch of its parent" test | (a) object outside the document (browser UI, menus, other tabs) -> resolved by the Parent walk (D-cost below) reaching no mirror node; cached **exactly** in a negative set because paths are never reused (F1); (b) race with a queued relist (event-to-apply window 5-20 ms) -> check orphans only after the current dirty batch is applied |
| D2 **focus / caret miss** (special case of D1) | 0 IPC | the region the user is in, at the moment it matters | same as D1; 5 % menu focus in the sim was absorbed by the negative cache |
| D3 child-count mismatch (`ChildCount` vs mirror) | 1 call/node, or free from `GetTree` | only changes that alter a count | Chromium replaces more than it grows: of 198 divergent chunks found by a full content compare, **6 (3 %)** changed any child count, and root counts caught ~0 (sim, Chromium profile). Weak; do not build on it |
| D4 path-set / child-list compare (`GetChildren`, or `GetMatches` paths) | 1 call per node or per chunk | every create/delete/move, exactly (F1) | none for structure; blind to in-place renames: 89/198 (45 %) of divergent chunks |
| D5 index-in-parent | 1 call | sibling insert/delete before the node | dominated by D4 on the parent |
| D6 content compare of a `GetTree` chunk (paths + names + ChildCount + attrs) | 1 call + 10 us/node | everything except role/state | none if F4 holds (atomic snapshot); otherwise transient mismatches -> re-check once before acting |
| D7 path signature on focus hit: async `GetParent` of the focused node vs mirror parent | 1 call | same-path moves of the focused node | none |

Detection probability. For a stale region R, D1/D2 fire at the first event or focus inside R: with event+focus
rate lambda(R), P(detected within t) = 1 - exp(-lambda(R) t). Regions that never emit (static content that
appeared silently, silent removals, silent reorders) are invisible to D1/D2 and need a sweep (D4/D6).

**Proposition 2 (sweep bound).** A round-robin sweep over M in document order with node budget rho nodes/s,
chunks of at most B nodes validated by D6, and frontier nodes (subtree > B) validated by their own child list
(D4), detects every divergence within one cycle, T_cycle <= n/rho + F/R_calls + queueing, where F is the number
of frontier nodes. Proof: let x be an unknown node; its LKA a is in M and a's child on the path to x is unknown,
so the child list of a differs from truth; a is covered once per cycle either inside a chunk (D6 compares its
child list) or as a frontier node (D4). A known-but-deleted node is caught at its parent the same way; a
rename at its own chunk. A deleted LKA makes the chunk root defunct, which escalates to its mirror parent.

## 2. Re-sync strategies under IPC latency

Cost model (F5): c0 = call RTT, beta = 10 us/node `GetTree`, gamma = 2 x 25 us/node role+state passes.

| n nodes | full rebuild (GetTree + role/state for all) | **GetTree diff** (role/state only for new paths) | GetMatches paths only (F6) | sweep cycle at rho = 2000 nodes/s |
|---|---|---|---|---|
| 1k | ~60 ms | ~10 ms + new x 50 us | ~3 ms | 0.5 s |
| 10k | ~0.6 s | ~0.1 s | ~30 ms | 5 s |
| 50k | ~3 s | ~0.5 s | ~150 ms | 25 s |

Browser UI thread time is the binding resource (not gorca CPU): rho = 2000 nodes/s costs ~20 ms/s (2 %) of
Chrome's UI thread; one chunk of B = 512 nodes blocks it ~5 ms.

Strategies simulated (Signal-like world: 4-node shell -> replaced -> ~790 nodes, focus 2/s, row recycling,
appends, silent reorders, conversation switches; 8 seeds x 120 s; numbers below for the Chromium profile):

| Strategy | focus hit | stuck runs | focus-miss repaired p50/p95 | silent create seen p50/p95 | silent delete purged p95 | wrong-name frac | UI ms/s |
|---|---|---|---|---|---|---|---|
| S0 today (drop unknown) | 0.849 | 1/8 | never | never (84 %) | 40 s, 3 % never | 0.0068 | 0.7 |
| S1 + orphan -> LKA GetTree diff | 0.998 | 0 | 17 / 18 ms | 0.78 / 2.7 s | 2.7 s | 0.0076 | 0.9 |
| **S2 = S1 + GetTree sweep (rho 2000/s, B 512)** | **1.000** | 0 | - | **0.12 / 1.3 s** | **0.28 s** | **0.00009** | 17.7 |
| S2m = S1 + GetMatches sweep | 1.000 | 0 | - | 0.12 / 1.3 s | 0.28 s | 0.0020 | 6.2 |
| S3 periodic full rebuild every 2 s | 0.999 | 0 | 5 ms | 0.77 / 1.0 s | 1.0 s | 0.0003 | 4.7 (O(n) per period: 3 s per rebuild at 50k, infeasible) |

Scale (S2, lossy profile p_add 0.8): 9.3k nodes: silent create p95 6.4 s, 9.9 ms/s UI; 45k nodes: p95 13.6 s,
25 ms/s UI; focus hit 0.998-1.0 at both sizes (the orphan trigger, not the sweep, carries the user path).

Storm (10k name events/s for 2 s on 50 nodes, Chromium profile): the per-tick dirty set (5 ms) bounded the
op queue at ~49 entries (= distinct dirty nodes); calls rose from 54/s to 127-141/s; no resync storm (1 resync
per run). Rules that make this a guarantee rather than an observation:

1. Events only mark the dirty set (O(1)); ops are per distinct (kind, node) per tick, so queue length <=
   distinct dirty nodes, independent of event rate.
2. Single flight per document for structural resyncs plus one "again" bit: during a storm of length S at most
   1 + S/T_min resyncs per subtree (T_min = 100 ms hysteresis used in the sim).
3. Escalation: if k dirty nodes fall in one subtree of size m and k x 25 us > m x 10 us (k/m > 0.4), replace
   them by one GetTree diff of the subtree; escalate to the document root only when the LKA is the root.
4. Nothing above runs on the key path: browse commands read the mirror as it is; Enter validates the target
   with one call that the activation needs anyway (section 3).

## 3. Identity across re-syncs

Two failure directions, both real on Chromium: (i) **same path, different item**: virtualized lists recycle
row objects and rename their contents in place; (ii) **new path, same item**: unkeyed or keyed React
re-renders recreate objects. F1 makes path equality exact *object* identity, which is the wrong notion for (i).

Tree edit distance (Zhang-Shasha O(n^2 min(depth, leaves)^2), APTED similar) is infeasible at 10k-50k nodes
and unnecessary: the cursor needs one node mapped, and the mirror needs ids only for nodes that survive.

**Proposed matcher M2 (linear expected time):**
- (a) Merkle hash H(v) = h(role, name, tag, H(children)) for old and new trees, O(n). Map isomorphic subtrees
  top-down, largest first (GumTree top-down phase without the O(n^2) bottom-up phase); a hash with multiplicity
  1 in both trees is a certain match; equal-multiplicity groups are paired in document order and **flagged**.
- (b) Path identity is accepted only when the node's own signature **and** its enclosing item hash (nearest
  listitem/article/row ancestor) are unchanged; flagged if that item hash has a twin.
- (c) Unmatched cursor: candidates with the same own signature (hash index), score = item-hash equality (10) +
  matching names of k = 2 leaves on each side; confident only with a unique maximum, item evidence present and
  no twin item in either tree; ties are broken by relative document position and flagged.
Complexity: O(n) hashing and indexing per re-sync (O(n log n) if sorted by size; bucket by height for O(n)),
O(c k) for the cursor, c = candidates with the cursor's signature (<= number of rows).

Results (1000 cursor trials per cell, 40-message list; "WRONG-conf." = an Enter would act on another item):

| content | transform | M0 path: correct / WRONG-conf. | M1 ladder (today, approx.): correct / WRONG-conf. | M2: correct (+flagged) / WRONG-conf. |
|---|---|---|---|---|
| distinct | keyed re-render (+1 insert/delete/edit) | 0 / 0 (98 % lost) | 0.65 / **0.35** | 0.95 / 0 |
| distinct | virtual scroll (rows recycled in place) | 0 / **1.00** | 0 / **0.78** | 0.93 / 0 (rest: message scrolled away, correctly lost) |
| distinct | mixed recycle + recreate | 0 / 0.51 | 0 / **0.88** | 0.92 / 0 |
| duplicates (16 % twins) | keyed re-render | 0 / 0 | 0.64 / 0.36 | 0.81 (+0.14) / 0.002 |
| duplicates | virtual scroll | 0 / 1.00 | 0 / 0.85 | 0.78 (+0.14) / 0.002 |
| duplicates | mixed | 0 / 0.52 | 0 / 0.94 | 0.79 (+0.12) / 0 |

The M1 column approximates `identity.rs` without DOM ids: list items have empty names, so IL-3 degenerates to
positional pairing within a gap, and recycled rows keep their ids by path. Real Signal rows may carry usable
DOM ids (IL-2), which would improve M1 on re-renders but not on recycling; unverified.

**Ambiguity.** With item content key k drawn from distribution p, the collision probability q = sum p(k)^2 and
P(an item has a twin among M) = 1 - (1-q)^(M-1). Distinct content (5 authors, 60 minutes, 10 % short replies):
q = 1.7e-6, P = 0.2 % at M = 1000. A chat of two people trading "ok"/"thanks" in the same minutes: q = 4.5e-3,
P = 16 % at M = 40, 59 % at 200, 99 % at 1000. Twins are indistinguishable by content in principle; only the
flag is sound, never a guess.

**Sound basis for the displaced-cursor rule.** Classify each relocation as Exact (path, own signature and item
hash unchanged, no twin), Confident (unique isomorphic or unique scored match, no twin), Flagged (tie or twin),
Lost. First Enter acts immediately only on Exact/Confident; Flagged/Lost and any "path same, item hash changed"
case announce only. A cheaper and stronger complementary check: keep the 64-bit hash of what was last spoken
for the cursor (role, name, item hash); on Enter re-read the node (one call, needed for the action anyway) and
act only if the hash matches: "act only on what the user heard".

## 4. Recommended algorithm (summary)

1. Route events on unknown paths in a mirrored app to an **orphan trigger** instead of dropping them (D1/D2).
2. Orphan -> after the current dirty batch -> Parent walk to the LKA (h calls, ~h x 0.25 ms; exact negative
   cache for paths outside the document) -> **GetTree diff of the LKA subtree** in chunks of <= 512 nodes:
   diff paths/names/child lists against the mirror, fetch role/state only for new paths, re-key with M2.
   Single flight per document, 100 ms hysteresis per LKA, escalate per rule 3.
3. A **budgeted background sweep** (rho ~2000 nodes/s, B 512, frontier child-list checks, order: cursor
   neighbourhood and quick-nav targets first, then round robin) bounds silent staleness by Proposition 2.
4. Document-ready: treat the first tree as provisional while `busy` or before `load-complete`, and on the first
   `add` after a tiny build (n < 32) do a root GetTree diff (cheap at that size).
5. Identity: add M2(a) as a rung before IL-3 (exact isomorphic subtree), demote IL-1 path identity when the
   item hash changed, and feed the Exact/Confident/Flagged/Lost class to the displaced-cursor rule.

Bounds: per event O(1); per resync O(m) client CPU and m x 10 us UI time for an LKA subtree of m nodes plus
new x 50 us; per sweep cycle n/rho seconds at rho x 10 us/s UI time; queue length <= distinct dirty nodes.

## 5. Proven vs heuristic

Proven: F1 consequences (exact newness test, exact negative cache), Proposition 1, Proposition 2, the storm
queue bound (rules 1-2), the twin probability formula. Heuristic or assumed: F4 atomicity, F6 GetMatches cost,
event-loss probabilities (only "root-only add" and "silent shrink" are sourced), the synthetic Signal tree, the
M1 approximation of the real ladder, chunk/budget constants (rho, B, 100 ms, 0.4).

## 6. Decision points for the architect / reviewer

- **DP-1** Unknown-source events: drop (today) vs orphan trigger. Recommend the trigger; it is what turns
  Proposition 1's absorbing state into a 17 ms repair.
- **DP-2** Resync primitive: full rebuild vs LKA GetTree diff. Recommend the diff (6x cheaper; role/state only
  for new paths); full rebuild only when the LKA is the root.
- **DP-3** Sweep: on for Chromium-engine documents (connector quirk row, not core code), rho and B as config;
  GetTree (catches renames) over GetMatches unless F6 is measured far cheaper and names are reliably evented.
  Firefox: sweep optional at a lower rho.
- **DP-4** Identity: extend `identity.rs` with the subtree-hash rung and the item-hash demotion of path
  identity, or keep the ladder and apply M2 only to the cursor. The table says the ladder alone is unsafe on
  recycled rows (78-94 % wrong-confident in the sim).
- **DP-5** Displaced-cursor rule keyed on the relocation class plus the spoken-hash check on Enter.

## 7. Open questions / next measurements

- Verify F4 (GetTree atomicity) and measure GetMatches cost (F6) on Chrome 155 / Electron 44.
- Count real Chromium event loss: diff a recorded event stream against GetTree snapshots on Signal (read-only).
- Do Signal rows carry DOM ids (`id` attribute) usable by IL-2? Do recycled rows keep paths (F1 says paths
  only change if objects are recreated)?
- Rust constants for M2 hashing at 50k nodes (expected < 5 ms, unmeasured).
