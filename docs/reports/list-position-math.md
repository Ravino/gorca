# List position and list size: data structure, virtualised-list rule, key-path cost

Date: 2026-10-26. Author role: math-researcher. Status: research, for the Rust port and the
architect's semantics ADR (in parallel).

Code: `tools/research/list_position/` (`setrank.py` reference, `prop_test.py` oracle property test,
`bench.py` op counts, `virtual_rule.py` Monte Carlo, `vectors.py` -> `vectors.json` test vectors).
Python: `<repo>/.toolchain/venv/bin/python`.

## 0. Answer

- **Structure:** a *signed-weight set rank* folded into the existing document treap
  (`crates/gorca-doc/src/web/doc.rs`): one `i32` weight per node, `w = +1` for a visible list item,
  `w = -c(L)` for a visible list `L` that owns `c(L)` items, 0 otherwise; the treap summary gains
  `sum` and `max_prefix`. Then `count(L) = c(L)` is O(1), `rank(x) = 1 + sum(pos(L)+1 .. pos(x))`
  is O(log n + D), `kth(L, k)` is O(log n) by a max-prefix descent, and every mirror op changes the
  weight of O(1) nodes. D = depth of the owner walk (nearest list ancestor). +12 bytes per node.
- **Virtualised lists:** an idle virtual window is indistinguishable in the a11y tree from a short
  static list (section 3.1), so no tree-only rule is both sound and useful. Recommended rule: author
  `setsize`/`posinset` (validated) > plugin fact > DOM count, with the DOM count and DOM rank
  withheld for any list that ever showed a scroll-attributed churn episode, remembered per list
  identity (per origin + list signature, across sessions). "Stable count" is no evidence at all.
- **Key path:** rank + count p99 = 112 treap/ancestor steps at 100k nodes with a 10k-item list
  (≈ 1 µs in Rust); kth p99 = 171; a patch op adds ≤ 2 owner walks + ≤ 3 O(log n) paths.

## 1. Formulation

Published model: a rooted ordered tree T (the mirror after `FUSE`, plugin patches and ADR-0029
synthetic containers), preorder positions `pos(x)` maintained by the implicit treap. Predicates on
the published `(role, flags)`, i.e. on `cats_of` (`crates/gorca-doc/src/web/cats.rs`):

- `item(x)  := LIST_ITEM ∈ cats(x)` (ListItem, Option; HIDDEN gives no cats, so hidden items are not items);
- `list(x)  := LIST ∈ cats(x)` (List, Listbox; a presentational/hidden list is not a list); the
  document root is a sentinel list (owns items with no list ancestor; never reported).
- `owner(x) := nearest proper ancestor y with list(y)` (task semantics: items of the nearest list
  only; wrappers such as generic divs between list and item are crossed).
- `members(L) := { x : item(x), owner(x) = L }` in preorder; `c(L) = |members(L)|`.

Queries: `count(L)`, `rank(x)` (1-based index of x in `members(owner(x))`), `kth(L, k)`.
Updates (the `DocOp`s of `crates/gorca-doc/src/web/ops.rs`): `InsertSubtree`, `RemoveSubtree`,
`Attach/MoveSubtree`, `SyncChildren` / `ReorderChildren` (= moves, inserts, removes),
`SetProps` (role / flags patch after load, commit 40302be3), synthetic container insert/dissolve
(ADR-0029 §2.1; members keep ids, preorder of members unchanged, SN-3).

The current code (`crates/gorca-core/src/browse/present.rs` `facts_of`) counts direct children only,
O(children) per call, and has no rank path.

## 2. Data structure

### 2.1 Signed-weight set rank (recommended)

Weights: `w(x) = +1` if `item(x)`, `w(L) = -c(L)` if `list(L)` (root included), else 0.
Treap summary per node t: `sum(t)`, `mp(t)` = max over non-empty prefixes of t's sequence.
`pull`: `sum = sum(l) + w + sum(r)`, `mp = max(mp(l), sum(l) + w, sum(l) + w + mp(r))`.

**Invariant W (proven).** For every list L: `Σ w over subtree(L) \ {L} = c(L)`; and the total
sum over the document is 0.
Proof: every item y in subtree(L) \ {L} has an owner O that is L or a list strictly inside L.
If O = L, y contributes +1. Otherwise its +1 is matched by O's −c(O), since O's own members are
exactly the c(O) items of that kind. Each nested list's subtree therefore sums to 0 net, by
induction on nesting depth, and the root case gives total 0.

**Corollary R (rank).** For an item x with owner L:
`rank(x) = 1 + Σ w over positions [pos(L)+1, pos(x))`. A nested list N that starts before x
ends before x, because x ∈ subtree(N) would make N (or something below it) x's owner. So N
contributes net 0 and only L's own members before x are counted.

**Corollary K (k-th).** Inside L's interior, the relative prefix sum never goes above its value
on entering a nested list: the list's own weight takes away c(N) first, and its members only
give that back. So L's k-th member (0-based) is the first position p > pos(L) where the prefix
reaches `prefix(pos(L)+1) + k + 1`, and it can be found with an `mp`-guided descent, O(log n).
Guard: `k < c(L)`.

**Updates.** Every op changes the weight of at most 3 nodes (x, old owner, new owner).

| op | weight changes | cost |
|---|---|---|
| insert subtree S under p | S's weights built with S (owners inside S known while building); `exported = #items of S with no list in S above them`; `c(owner_above(p)) += exported` | O(\|S\|) build (already paid) + O(log n + D) |
| remove subtree x | `s = sum(piece)` after the split (items of x owned outside x); `c(owner_above(x)) -= s` | O(log n + D) |
| move / attach / reattach a defunct subtree | as remove + insert, with `s` = the piece's root sum (its interior is unchanged) | O(log n + D) |
| reorder children | none (prefix sums follow the order) | 0 extra |
| `SetProps` item ↔ not | `w(x) = ±1`, `c(owner) ± 1` | O(log n + D) |
| `SetProps` becomes list | `s = Σ w over interior(x)` (exactly the interior items O owned), `c(x)=s`, `w(x)=-s`, `c(O) -= s` | O(log n + D) |
| `SetProps` stops being list | `c(O) += c(x)`, `w(x) = 0` | O(log n + D) |
| hidden toggle | as the role change it implies (cats become NONE) | O(log n + D) |
| synthetic container wrap kids[i..j] | one treap insert at pos(kids[i]) (preorder of the members unchanged), then "becomes list" | O(log n + D) |
| dissolve | "stops being list", then one treap delete | O(log n + D) |

Making a node with 10k item children into a list is O(log n + D), not O(10k): measured 147 steps
(on) / 41 steps (off) at 100k nodes.

Queries: `count(L)` O(1) (stored c); `rank(x)`: owner walk O(D) + `pos(L)`, `pos(x)`, two
prefix descents, O(log n + D); `kth`: O(log n). Treap height at 100k: mean node depth 20.5, max
37 (`bench.py` doc).

Memory: `w`, `sum`, `mp` as i32 = 12 B/node (1.2 MB at 100k); `c` lives in the list's slot, or as `-w`.

Verification: `prop_test.py 300 80`: 300 random documents × 80 random ops (inserts of random
nested subtrees, removes, moves, role patches, hidden toggles, wraps, dissolves; 24k ops), full
oracle comparison of count, every rank, every kth and the out-of-range kth after every op: OK.

### 2.2 Alternatives compared

| design | count | rank | kth | per mutation | role patch makes a 10k-child node a list | memory |
|---|---|---|---|---|---|---|
| **A signed weight on the doc treap** | O(1) | O(log n + D) | O(log n) | +O(log n + D) | O(log n + D) | +12 B/node |
| B member array cached on the list, lazy rebuild | O(1) clean | O(1) clean; **O(subtree(L))** after any change under L | O(1) clean | O(D) dirty marking | O(subtree) on the next query | 8–16 B/item + hash |
| C direct-children walk (current) | O(children) | O(children) | O(children) | 0 | O(children) | 0 |
| per-list order-statistic treap (keyed by doc order) | O(1) | O(log m + D) | O(log m) | locating the slot of a new member needs prev-member-in-same-list = A's machinery anyway; a wrap moves k members between trees, O(k log m) | O(c log c) | ~32 B/item |
| per-depth ranked counts (`LIST_ITEM` at list-depth d) | O(log n) | O(log n) | O(log n) | inserting a list above changes the depth of the whole subtree: O(subtree) or a lazy shift tag | O(subtree) | 4·Dmax B/node |
| Fenwick over doc order | O(log n) | O(log n) | O(log n) | positions shift on insert: rebuild O(n) | – | 4 B/node |

Measured (steps = treap visits + ancestor steps, B/C = node visits), `bench.py synth`, 100 005
nodes, one 10k-item list (every 50th item carries a 5-item nested list), 400 random patches each
followed by a key:

| measure | mean | p50 | p99 | max |
|---|---|---|---|---|
| A key: rank + count | 94 | 95 | 112 | 116 |
| A key: kth (jump to item k) | 147 | 148 | 171 | 179 |
| A patch: insert item subtree | 161 | 161 | 202 | 202 |
| A patch: remove item subtree | 188 | 188 | 234 | 234 |
| A patch: move item subtree | 337 | 341 | 430 | 430 |
| A patch: role toggle | 23 | 0 | 56 | 56 |
| A patch: synthetic wrap (≤256 members) | 259 | 260 | 345 | 345 |
| A patch: dissolve | 146 | 161 | 237 | 237 |
| B key after a patch (rebuild) | 48 032 | 48 527 | 50 256 | 50 261 |
| B key clean | 1 | 1 | 1 | 1 |
| C key (scan 10k children) | 9 570 | 9 668 | 10 011 | 10 012 |

A's patch rows include the treap split/merge the mirror already does for that op; the
feature itself adds only the owner walks and ≤ 3 path pulls (`set_w`). At ≈ 5–10 ns per step in
Rust, A's key path costs about 0.5–1.2 µs, B's rebuild 0.25–0.5 ms, and C 50–100 µs.

## 3. Virtualised lists: when is a total trustworthy

### 3.1 Indistinguishability (proven)

Suppose an undeclared virtual list (N items, window of W rows rendered, idle) and a static list of
W items produce the same accessibility subtree: same roles, names, no posinset/setsize, no
geometry read. Then any rule that is a function of the tree alone says the same thing for both,
and if it states "W items" it is false for one of them. The same holds for any history in which
the window did not move ("the count was stable for T seconds" is satisfied by both).
So a sound total needs one of these: author data, a plugin fact, geometry, or a history that
contains a window move. This is why `R2` below is exactly as bad as `R0` on virtual lists.

Consequences:
- **"Stable count"** is not evidence of completeness. An idle window is perfectly stable.
- **"Scroll extremes reached"** proves completeness only for a list that never churned. For a
  virtual list it would yield an *accumulated* count (distinct identities seen between the two
  extremes). That count goes stale through inserts outside the window, which the tree cannot
  observe: `P(stale after t) = 1 - exp(-λ_off t)`. Example: a new chat added at the top while the
  user is at the bottom. Not recommended, except as an "about N" estimate if the architect wants one.
- **A virtual list with N ≤ W renders everything**, so its DOM count is true. A false total
  requires `m = W ≥ V_min + 2·o_min`. A threshold `m0` below the smallest plausible window
  (≈ 8–10 rows) is safe without evidence, but most real lists are longer.

### 3.2 Recommended rule (RL)

1. **Author data.** If every rendered member carries the same `setsize > 0` with `setsize ≥ max
   posinset`, and the `posinset` values are distinct and increase in document order, then use
   them for both the total and the position. A container-level setsize, `setsize ≤ 0` and
   `posinset > setsize` are ignored (ADR-0029 changelog line). `setsize = -1` (unknown) means the
   list is virtual: no total.
2. **Plugin fact** (per-app extension: total and/or "virtual"). The host currently drops
   `Value::Position` (telegram-web-live-diagnosis.md, follow-up 5), so this needs a position/virtual
   fact in the plugin host.
3. **Suspicion.** The list is *suspect* once any of these is observed: the first rendered posinset
   is > 1, setsize > rendered members, a scroll-attributed churn episode (members added or removed
   at an end of the list within ~300 ms of a scroll, ScrollTo or focus move; **one** episode is
   enough, since a false alarm only costs a withheld number), `feed` or `aria-busy`, or a geometry
   gap (if extents become available). Suspicion is remembered **per list identity**: owner origin
   + list signature (ADR-0029 anchor or role/class path). It survives revisits and sessions and is
   cleared only by a source epoch bump with a changed signature.
4. **Suspect list:** say no DOM total and no DOM rank ("list", items without "k of N"). The
   architect may choose a hedged form ("showing m").
5. **Otherwise:** use the DOM count and rank from section 2 (exact).

False-statement probability per statement (`virtual_rule.py 4000`, ASSUMED class mix, window,
revisit and reading-length parameters; p_ev = 0.9). Statements are weighted by visits, and app
lists are revisited 3–16 times per session:

| rule | general web: P(false count) | P(false pos) | correct counts withheld | chat app: P(false count) | P(false pos) | withheld |
|---|---|---|---|---|---|---|
| R0 DOM always (NVDA/Orca behaviour) | 0.152 | 0.259 | 0 | 0.534 | 0.594 | 0 |
| R1 suspicion within the visit | 0.152 | 0.129 | 0 | 0.534 | 0.298 | 0 |
| **R1m = RL (memory per list identity)** | 0.032 | 0.028 | 0 | 0.112 | 0.065 | 0 |
| R2 stable-count gate (T = 2 s) | 0.152 | 0.259 | 0.023 | 0.534 | 0.594 | 0.047 |
| R3 RL + geometry certificate (g = 0.8, f = 0.01) | 0.006 | 0.005 | 0.007 | 0.022 | 0.013 | 0.004 |
| R4 RL + withhold every m ≥ 12 without completeness evidence | 0.0006 | 0.0004 | 0.275 | 0.002 | 0.0008 | 0.157 |

What is left for RL is exactly the first visit to an undeclared virtual list, which section 3.1
says cannot be avoided without author data, a plugin or geometry. Cross-session memory reduces it
to the first visit ever per site and list. A plugin fact (Telegram chat and message lists)
reduces it to 0 for that app. p_ev = 0.5 changes RL by < 1 point (0.119 / 0.072). Withholding
everything (R4) is nearly sound but withholds 16–28 % of correct totals. That is acceptable only
for app-like documents (Electron or known chat apps), not for the general web.

## 4. Bounded key-path cost

Let h be the treap height (≤ c·ln n w.h.p.; measured max 37 at 100k) and D the owner-walk depth
(DOM depth; the Blink parser caps nesting at 512, JS-built DOM is unbounded). Then:

- **per keypress** (say "k of N" or "N items"): owner walk D, `pos(L)`, `pos(x)`, two prefix
  descents: ≤ D + 4h steps. Measured p99 112 at D ≈ 4. The D term at D = 500 is 501 ancestor
  steps + 24 treap steps (≈ 3 µs). `kth` (first/last/item k, next item of the same list) ≤ 2h
  descent + h.
- **per patch op**: the existing treap op, plus ≤ 2 owner walks (2D), ≤ 3 `set_w` path pulls
  (3h), and for a list toggle one range sum (2h). So ≤ 2D + 5h extra.
- **per batch** of B ops: ≤ B·(2D + 5h) extra. There is no rebuild and no dependence on list
  length.
- Fixture replay (`bench.py fixtures`: recorded Telegram A trees, source roles plain, plugin
  re-role facts as a later batch, then 300 virtual-scroll steps of 1–4 rows dropped at the top +
  1–4 cloned at the bottom + 30 % "chat moves to top" + re-role batch):
  - `telegram-a-chat-list.json` (51 nodes, 4 facts, list of 3): load batch 54 steps
    (13.5 per fact); churn batch p99 636; key p99 42.
  - `telegram-a-open-chat.json` (208 nodes, 15 facts, message list 13 + empty chat list): load
    batch 264 (17.6 per fact); churn batch p99 826; key p99 52.
  - `telegram-a-live.json`: no node matched the plugin's list classes (different capture shape),
    so there was nothing to measure.
  - After every churn run, `check()` against the oracle passed.

Proven vs heuristic: Invariant W and corollaries R/K are proven, and confirmed by the property
test. The treap bounds are w.h.p. (randomised priorities), as for the existing index. The
section 3 probabilities are modelled; only the indistinguishability argument is proven.

## 5. Porting notes (Rust, `gorca-doc` `WebDoc`)

- Add `w: i32, wsum: i32, wmax: i32` to `TNode` and fold them into the existing pull
  (`pull_path` / rebuild of `cnt`). `own` already holds cats, so the item/list kind is derived from
  `own & (LIST_ITEM | LIST)`. Hidden and presentational filtering then comes for free (cats NONE).
- Hook the kind delta wherever `own` changes (the `SetProps` / patch path near the `set cats` code
  at doc.rs ~990 and `Translator::patch_sem`). The four cases are in `setrank.py set_props`.
- Insert: compute weights while building the subtree's treap (bottom-up `c` per list in the spec),
  then one `bump(owner_above(parent), exported)`. Remove / detach: `s = wsum(piece root)` after the
  two splits, before the merge. Reattach of a defunct subtree: the piece's `wsum` is still valid.
- ADR-0029 wrap is currently `InsertSubtree` + `MoveSubtree` per member: still correct, at k·(log
  n + D). Optionally add a `WrapRun` op (one treap insert + relink, the preorder is unchanged) to
  make it O(log n + D).
- Queries: `list_count(L) -> Option<u32>`, `list_rank(x) -> Option<(DocNodeId, u32)>`,
  `list_kth(L, k)`. They are read-only (no splits) and deterministic (PUB-1 holds: no RNG beyond
  the existing priorities).
- `validate()`: check `wsum(root) == 0` and `c(L) == Σ w(interior L)` for each list.
- Other set families (menu/menuitem, tablist/tab, radiogroup/radio, tree/treeitem) each need their
  own triple (12 B). A shared triple with "nearest container of any family" forces 0-weights for
  items of the wrong family, which makes container toggles O(members).
- Test vectors: `tools/research/list_position/vectors.json`, 5 scenarios, 148 ops. Ids are
  sequential, subtree ids in preorder, and every op lists the expected `{list: [members]}`.
  The "hand" scenario covers a nested list, late re-roles, a synthetic list, a hidden item, an
  item that stops being one, and a nested list that stops being one (its items move up to the
  outer list).

## 6. Open questions

- Semantics are the architect's: "nearest list" (here) vs direct children only. Direct-children
  works with the same weights, but unowned items get weight 0 and a list toggle costs
  O(children).
- Whether to suspect on one churn episode is a UX choice, informed by the asymmetric loss above.
- Geometry certificate (R3): needs a measurement of Component extents availability and cost on
  Gecko and Chromium for list containers vs members.
- The parameters of section 3 are ASSUMED (class mix, revisit counts, window sizes). Measure the
  undeclared-virtual share in the target apps (Telegram, Slack, Signal) before tuning m0 or R4.
