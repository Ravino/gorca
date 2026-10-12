# ADR-0003: Web document mirror — data model, sources, incremental updates, SPA identity, browse mode

- **Status:** Accepted (2026-10-12). Decisions W, G and the variant C identity scheme were approved earlier the same day; all 11 §20.2 items were then approved, item 4 with an owner amendment (no cap on mirrors; one awake mirror, all others dormant; §6.6). Text still marked *proposed* below refers to those approved items; constants marked *heuristic* remain tunable by measurement.
- **Date:** 2026-10-12
- **Deciders:** project owner
- **Extends:** ADR-0001 §9 (R6), §7.2 (`Mode`), §11 (workspace), §14 (metrics), §15 (B-6). Amendments to ADR-0001 are listed in §2.1 and take effect when this ADR is accepted.
- **Related:**
  - `docs/reports/2026-10-12-web-a11y-browsers.md` (WEB): Firefox and Chromium AT-SPI behaviour, NVDA/Orca browse-mode prior art.
  - `docs/reports/2026-10-12-web-source-layer.md` (SRC): tuned AT-SPI vs AGP vs CDP measurements, `DocumentSource` sketch, B-6 spec.
  - `docs/reports/2026-10-12-spa-behavior.md` (SPA): React/Vue/Angular/Svelte event and identity characterisation in Chromium 153 and Firefox 153 ESR.
  - `docs/reports/2026-10-12-math-web-graph-model.md` (GM): typed graph, treap index, re-fetch cover DP, matcher, coalescing bounds.
  - `docs/adr/0002-multimodal-perception-layer.md` (Accepted): feature bindings on mirror nodes, `SpeechCmd::Earcon`.

## 0. Changelog

| Date | Change |
|---|---|
| 2026-10-12 | Draft (Proposed). |
| 2026-10-12 | **Accepted with amendment.** All §20.2 items approved. Item 4 amended: no fixed cap on mirrors (N mirrors = M tabs, iframes inside their tab's mirror); only the active tab's mirror is awake, the others are dormant and do cheap invalidation marking only; wake = cursor revalidation, dirty-set re-fetch, block-wise drift check, or rebuild-as-resync that keeps logical ids when stale; mirrors are evicted only on tab close. The "8 dormant mirrors per browser" LRU limit is removed. New §6.6, `Waking` state, DM-I5…I8, DM-M1…M3, wake budgets (§14), WM-12/13/14/15. |

## 1. Context

ADR-0001 R6 fixed a per-document mirror built viewport-first, fed by a `SourceLayer`, with the source decided by bake-off B-6. Research has since closed most of the open hooks (TBD-S1…S4):

| # | Fact | Evidence | Consequence |
|---|---|---|---|
| WF-1 | Tuned AT-SPI (P2P, pipelined window 16–64, `Collection.GetTree`, ≤ 4 connections) builds a 10k-element Firefox page in 0.32 s and 100k in 3.0 s (Chromium 6.5 s). `GetTree` alone: 0.12 / 1.1 s (Firefox), 0.21 / 2.5 s (Chromium). Server ceiling ≈ 75k calls/s (Chromium) and 95k calls/s (Firefox); more than 4 connections gives nothing. | SRC §2.1–2.2 | AT-SPI is a viable baseline. The browser main thread is the shared bottleneck, so cost is counted in **server time**. |
| WF-2 | `Collection.GetTree(rule, properties)` is undocumented (absent from `xml/Collection.xml`), returns the **whole** subtree in DFS order with no size limit, and does not return role or state. | `refs/at-spi2-core/atk-adaptor/adaptors/collection-adaptor.c:1142-1185` | Feature-detect it. Bound per-call server time by choosing small roots. A second pass fetches role and state. |
| WF-3 | Firefox cache domains are lazy; the first query of a new kind returns empty data and triggers a push of that domain for every accessible. Activation is process-global. | WEB §1.5, `refs/firefox/accessible/ipc/DocAccessibleParent.cpp:87-117` | Warm once per Firefox process, at app attach, off the critical path. |
| WF-4 | Chromium's renderer needs ~35 s to serialise a 100k-element page; CDP is no faster, refuses the default profile since Chrome 136, and gives no live or caret events. | SRC §3 | **No CDP** (decision W). Chromium is AT-SPI only. |
| WF-5 | Privileged in-process Firefox source (AGP): 229k nodes walked in 0.75 s, richer events (REORDER, NAME_CHANGE, per-batch), needs `-remote-allow-system-access` + Marionette or AutoConfig, and depends on Gecko-internal XPCOM. | SRC §4.2 | Opt-in accelerator in hybrid mode; B-6 decides whether it becomes the default (decision W). |
| WF-6 | The four SPA frameworks are indistinguishable at the AT-SPI level. What matters is keyed vs unkeyed rendering and DOM moves vs in-place rewrites. | SPA §3 | One generic matcher, no framework code. |
| WF-7 | **Chromium keeps object identity across DOM moves but emits no event** (a keyed list reverse produces 0 `ChildrenChanged`); grids send one `RowReordered`. Chromium's DOM `id` attribute goes **stale** after an attribute-only change. | SPA §3 | Event-only mirrors drift. The cursor's parent is revalidated on read. DOM ids are validated before use. |
| WF-8 | **Firefox recreates moved nodes** (remove + add, new object paths) and is loud about it (keyed grid sort: 658 nodes gone, 658 new). | SPA §3 | Remove+add bursts under one parent are re-matched to keep logical identity. |
| WF-9 | A `remove` event names only the **top** of the removed subtree (a 20,943-node route swap = 874 removes). Firefox sends index −1 in 13 % and a null child in 1.6 % of `ChildrenChanged`. Firefox may drop hide events whose parent is gone. | SPA §2, WEB §1.4, `refs/firefox/accessible/atk/AccessibleWrap.cpp:1127` | `remove` = drop the whole subtree locally. Any `ChildrenChanged` may degrade to "refetch the parent's child list". Defunct replies are authoritative (ADR-0001 I-1). |
| WF-10 | **No live-region signal** in either browser. Live updates arrive as `ChildrenChanged:add` + `TextChanged:insert` under nodes whose attributes carry `container-live`, `atomic`, `relevant`, `busy`. `notification` exists for `ariaNotify`. | SPA §2, WEB §1.4, §2.4 | Live regions are inferred from the mirror. |
| WF-11 | Burst timing after trusted input: first event p95 ≤ 40 ms; last event p50 19–33 ms, **p95 ≈ 300 ms** (288 Chromium, 357 Firefox). | SPA §2 | Debounce windows and settle times below. |
| WF-12 | A Firefox window that is occluded, on another workspace or behind a lock screen stops updating its a11y tree; events are delayed by seconds or lost. | SPA §7.1, `refs/firefox/accessible/base/NotificationController.cpp:488,689` | Silence from a hidden document is not trusted; the mirror goes `Dormant` and runs a drift check on return. |
| WF-13 | Augmented treap over preorder: next/prev p99 1.7 µs, k-th p99 1.5 µs, delete p99 2.8 µs per mutation at 100k nodes; no relabel spikes (order-maintenance labels spike to 13–174 ms). | GM §1.2 | Decision G. |
| WF-14 | The minimum-cost re-fetch cover of a dirty set is an exact DP in O(\|D\|·depth). A GumTree-style matcher restores the cursor in 85–92 % of re-rendered cases where index paths manage 0.6–2 %. | GM §3.2–3.4 | Update planning and SPA re-render matching below. |

Prior art. NVDA builds its virtual buffer **inside** the browser process and answers every navigation from local memory; it invalidates subtrees with ancestor dominance and a fixed 100 ms timer (`refs/nvda/nvdaHelper/vbufBase/backend.cpp:85-87`) and re-renders whole subtrees. Orca has no mirror: structural navigation runs `Collection.GetMatches` over the whole document on every key press, and lines are rebuilt from geometry with tens of round trips each (WEB §1.6, `refs/orca/src/orca/structural_navigator.py`, `refs/orca/src/orca/scripts/web/script_utilities.py`). Orca keeps live-region messages for 45 s before discarding them (`refs/orca/src/orca/live_region_presenter.py:125`), so stale messages are spoken long after the fact. gorca takes NVDA's model (local answers, virtual caret, auto focus mode) without in-process injection, and adds fine-grained diffing and logical identity, which NVDA lacks.

## 2. Decision (summary)

1. **Mirror per top-level document** (one doc actor per browser tab document, iframes included as subtrees), with **no cap on the number of mirrors**. Only the mirror of the active tab is awake; all others are dormant (invalidation marking only, no IPC) and catch up on wake (§6.6). Mirrors are evicted only when their tab closes. The mirror is a typed graph in a generational arena, with an **augmented treap over preorder** as the single navigation index and flat-text map (decision G). The B+tree "sum tree" is the upgrade path behind the same summary trait.
2. **Publication by double-buffering** (`left-right`): the doc actor writes, the Reader and `PresentJob`s read wait-free. Mirror operations are deterministic so both copies stay identical. *Proposed.*
3. **`DocumentSource` abstraction** (decision W; the trait is ADR-0001's `DocSource`, kept under that name in code). Baseline: `AtspiDocSource`, tuned. Opt-in: `HybridDocSource` = AT-SPI (authority for focus, caret, actions) + `AgpFirefoxSource` (bulk and batches). B-6 decides Firefox's default. **No CDP.** Chromium is AT-SPI only.
4. **Identity: variant C.** `DocNodeId` is the *logical* node. The ladder AT-SPI path → validated DOM id → (role, name, tag) + sibling order maps transport objects to it, and a `GetChildren` revalidation of the cursor's parent on read catches Chromium's silent reorders.
5. **Build**: viewport-first and parallel: focus context, then `GetTree` per block (viewport blocks first, 4 connections), then a role/state pass in priority order. Partial coverage is a normal state with a `GetMatchesFrom` fallback.
6. **Updates**: events → dirty marks (with local subtree drops for `remove`) → trailing debounce (40 ms, max 150 ms; user-relevant marks immediate) → optimal re-fetch cover (GM §3.4) under a server-time budget → keyed child diff / re-matching → sliced apply → one publish per batch.
7. **SPA**: burst classification (route swap, keyed move, unkeyed rewrite, append, virtual scroll), re-matching, and bookmark-based position restore.
8. **Browse mode**: NVDA-style virtual cursor and auto focus mode, as an explicit state machine refining ADR-0001 `Mode`.
9. **Live regions**: inferred from mirror attributes; politeness queue with TTLs, per-region and per-document throttles, deduplication, busy deferral.

### 2.1 Amendments to ADR-0001 (on acceptance)

| ADR-0001 | Change |
|---|---|
| §9.1 diagram, §11 "not scaffolded" | `CdpDocSource` and `WebExtDocSource` are removed. `AgpFirefoxSource` (crate `gorca-doc-source-agp-firefox`, plus `browser/firefox-agp/`) replaces them and is created by B-6 arm (b). |
| §9.2 `SourceCaps`, `DocRequest`, `SourceKind` | Extended as in §6.1 (`reorder_events`, `live_announcements`, `text_leaves_as_nodes`, `trust`; `Revalidate` and `Matches` requests; `SourceKind = Atspi \| AgpFirefox \| Hybrid`). |
| §9.3 `DocNodeId` "doc-local `u32`" | Becomes a generational `(slot: u32, gen: u32)` handle denoting a **logical** node (§5). I-1 still holds: a `DocNodeId` never denotes two logical nodes. |
| §9.3 TBD-S1…S4 | Closed: S1 no CDP; S2 replaced by AGP-Firefox (§6.3); S3 no shared id exists, the AGP join is lockstep DFS alignment (§6.4); S4 split in §6.4. |
| §9.4 debounce "50–100 ms" | 40 ms trailing / 150 ms max-wait, with an immediate class (§9.3). |
| §15 B-6 arms | Replaced by §16 of this ADR. |

## 3. Crate placement (extends ADR-0001 §11)

| Crate | Owns (new in this ADR) | Must not |
|---|---|---|
| `gorca-model` | `DocNodeId` (generational), `DocDelta`, `SrcNode` (+ `tag`, `dom_id`, `level`, `live` fields), `Category` bit assignments, `Politeness`, `BurstClass` | I/O, tokio |
| `gorca-doc-source-api` | `DocSource` (= decision W's `DocumentSource`), `SourceCaps`, `DocRequest` (+ `Revalidate`, `Matches`), `JoinPort`, source conformance traits | depend on any connector |
| `gorca-doc` (pure data; no tokio, no I/O; `rayon`, `left-right`, `smallvec`, `xxhash-rust`, `unicode-segmentation` allowed) | modules: `arena` (nodes, typed edges, reverse relation index), `navindex` (treap, `Summary` trait, categories), `flattext` (text dimensions, line map), `tables` (grids), `dirty` (dirty set, virtual tree, re-fetch cover DP, budgeted planner), `diff` (LIS keyed child diff), `matcher` (identity ladder, GumTree-style matcher, burst classifier), `bookmark` (restore), `live` (classifier, dedupe ring, token buckets with an injected clock), `publish` (left-right wrapper, `DocRead`) | tokio, zbus, atspi |
| `gorca-connector-atspi` | `AtspiDocSource`: per-app P2P pool (4 connections) shared by that app's doc actors, windowed pipelining, `GetTree` detection and fallback, cache-domain warming, event → `DocDelta` normalisation (§9.1), Firefox text-run synthesis (§4.3), caret offset mapping | presentation policy |
| `gorca-doc-source-agp-firefox` (B-6 arm b; not scaffolded before) | AGP client: Unix-socket framing, columnar snapshot decode, sequenced batches, gap detection, lockstep aligner (§6.4) | zbus, atspi (it joins through `JoinPort` only) |
| `browser/firefox-agp/` (JS, B-6 arm b) | bootstrap script, `JSWindowActor` child, `accessible-event` observer with subtree dominance | — |
| `gorca-core` | `DocActor` (task: source attachment SM, build SM, update scheduler, live scheduler), Reader: `WebMode` SM (§11), structural navigation commands (§12), position restore glue | IPC on the Reader lane |
| `extensions/ext-firefox`, `extensions/ext-chromium` | policy only (§15) | IPC, mirror writes |

Dependency rules 1–7 of ADR-0001 apply unchanged. New rule **13**: only `gorca-doc-source-agp-firefox` and `gorca-daemon` may depend on the AGP crate; `gorca-doc` must not depend on `gorca-doc-source-api` (the mirror consumes `gorca-model::DocDelta` only).

## 4. Mirror data model

### 4.1 Arena and node

```rust
// gorca-model
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DocNodeId { slot: u32, gen: u32 }        // logical node; generation bumps on slot reuse (I-1)

// gorca-doc::arena
pub struct DocNode {
    // identity (§5)
    pub atspi: Option<SrcNodeId>,      // interned AT-SPI path; None for synthetic text runs
    pub agp: Option<SrcNodeId>,        // Gecko uniqueID, hybrid only
    pub sig: u64,                      // xxh3(role, name, tag); recomputed when any of them changes
    pub dom_id: Option<Atom>,          // interned DOM id attribute
    pub dom_id_fresh: bool,            // false after a Chromium attribute-only change (WF-7)
    pub tag: Atom,
    pub join: JoinQuality,             // Exact | Ladder(Step) | Heuristic { confirmed: bool }
    // semantics
    pub role: Role,
    pub role_src: RoleSrc,             // Provisional (from tag / xml-roles) | Confirmed (GetRole)
    pub states: StateSet,
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub value: Option<Value>,
    pub attrs: AttrMap,                // interned keys: xml-roles, level, live, container-live, atomic, relevant, busy, ...
    pub cats: CatMask,                 // u64, §4.4; 0 when not presentable (hidden, ignored)
    pub rels: SmallVec<[(Rel, DocNodeId); 2]>,
    // tree links (arena slots)
    pub parent: Slot, pub first_child: Slot, pub last_child: Slot, pub prev: Slot, pub next: Slot,
    pub child_count: u32,
    pub coverage: Coverage,            // Complete | ChildrenUnfetched { count: u32 } | Enriching
    pub fresh: DocFresh,               // Fresh | Stale { mask: PropMask } | Fetching { req } | Defunct
    pub text: Option<TextBuf>,         // own text only (§4.3); UTF-8 + sparse scalar index (ADR-0001 TU-3)
    pub rev: u32,                      // batch revision that last touched the node
    // order index (same slot): treap links + summary (§4.4)
    pub ord: TreapLinks,               // left, right, up: Slot; prio: u32 (deterministic, §4.8)
    pub sum: Summary,
}
```

- **DM-1** The arena is a `Vec<SlotEntry>` with a deterministic free list; `DocNodeId` packs into a `u64`. No hash lookup on the hot path: Reader cursors hold `DocNodeId`s, and `get(id)` is an index plus a generation compare.
- **DM-2** Source-to-logical joins are two hash maps owned by the doc actor: `atspi: SrcNodeId → DocNodeId`, `agp: SrcNodeId → DocNodeId` (IR-2: O(1), no IPC).
- **DM-3** `Role` is `Provisional` when it comes from `tag`/`xml-roles` in `GetTree` attributes, before the role pass (§8.3). Provisional roles are good enough for headings (`h1`–`h6`, `xml-roles=heading` + `level`), landmarks, links and lists, so structural navigation works after the skeleton phase.
- **DM-4** A node's `cats` is a pure function of `(role, states, attrs, presentable)`. It is recomputed on every field change and pushed into the treap summary in O(log n).

### 4.2 Typed edges (GM §1.1)

| Edge | Representation |
|---|---|
| parent/child | intrusive links in the slot |
| document order | implicit in the treap; never stored |
| labelled-by, described-by, controls, details, error-message, flows-to (and reverses) | forward `rels` + reverse index `HashMap<DocNodeId, SmallVec<(Rel, DocNodeId)>>` |
| table row/column | per-table grid (§4.6) |
| same landmark | nearest landmark ancestor, cached per node, invalidated on subtree moves |
| in-page link target | `HashMap<Atom /* fragment id */, DocNodeId>` from `dom_id` |
| visual adjacency | lazy R*-tree (`rstar`) over nodes with known extents, rebuilt on viewport change (Phase 2) |

### 4.3 Canonical text shape (*proposed*)

The two browsers expose text differently: Chromium has text leaves (`static`) as children, Firefox has none and keeps text in the parent's hypertext with one U+FFFC per embedded child (WEB §1.2, §2.2). The mirror has **one** canonical shape: **text leaves as nodes**.

- **TX-1** `AtspiDocSource` for Firefox splits each hypertext container's text at U+FFFC into synthetic **run leaves** (`Role::TextRun`, `atspi: None`, keyed by `(parent, run index)`). The run table (offsets of each U+FFFC) is kept per container to map Firefox `TextChanged` and caret offsets (UTF-16 + phantom U+FEFF, ADR-0001 TU-2) to `(run leaf, scalar offset)`.
- **TX-2** For Chromium, a container's own hypertext is not stored; its static children carry the text. Caret offsets on a container are mapped by walking its children: a static child contributes its length, any other child one U+FFFC. The U+FFFC-to-child invariant does not hold in Chromium (72 of 300 nodes, WEB §2.2), so the mapping never counts U+FFFC in the parent string.
- **TX-3** AGP-Firefox delivers real Gecko text leaves, which match TX-1's runs one to one; this makes lockstep alignment (§6.4) direct.
- **TX-4** Every node in preorder is an element of the treap; the flat text is the concatenation of leaf texts with block separators. Offsets are scalars (ADR-0001 TU-1).

### 4.4 Navigation index (decision G)

The augmented implicit treap over preorder (GM §1.2, port of `research/web-graph/rust-bench/src/main.rs`):

```rust
// gorca-doc::navindex
pub trait Summary: Copy + Default {
    fn of(node: &DocNode) -> Self;
    fn combine(a: &Self, b: &Self) -> Self;      // monoid
}
#[derive(Copy, Clone, Default)]
pub struct DocSummary {
    pub size: u32,
    pub mask: CatMask,                  // OR of cats in the subtree
    pub counts: [u32; RANKED_CATS],     // per-category counts, only for categories that need "k of N"
    pub scalars: u32,                   // flat-text length
    pub newlines: u32,                  // hard line breaks (block boundaries, <br>, \n)
}
pub trait NavIndex {
    fn next(&self, from: DocNodeId, c: CatMask) -> Option<DocNodeId>;   // O(log n)
    fn prev(&self, from: DocNodeId, c: CatMask) -> Option<DocNodeId>;
    fn kth(&self, c: Cat, k: u32) -> Option<DocNodeId>;
    fn rank(&self, n: DocNodeId, c: Cat) -> Option<u32>;                // "3 of 24"
    fn count(&self, c: Cat) -> u32;
    fn cmp_order(&self, a: DocNodeId, b: DocNodeId) -> Ordering;        // two ranks
    fn offset_of(&self, n: DocNodeId) -> u32;                           // flat-text start
    fn at_offset(&self, scalar: u32) -> (DocNodeId, u32);
    fn line_start(&self, k: u32) -> (DocNodeId, u32);
}
```

Categories (`CatMask` bits; *proposed* assignment; bits 48–63 reserved for extension navigation units, §15):

| Bits | Categories |
|---|---|
| 0–6 | `HEADING`, `HEADING_1` … `HEADING_6` |
| 7–9 | `LINK`, `LINK_VISITED`, `LINK_UNVISITED` |
| 10–17 | `FORM_FIELD`, `EDIT`, `BUTTON`, `COMBOBOX`, `CHECKBOX`, `RADIO`, `SLIDER_SPIN`, `LISTBOX` |
| 18–21 | `LANDMARK`, `REGION`, `ARTICLE`, `DIALOG` |
| 22–26 | `TABLE`, `LAYOUT_TABLE`, `LIST`, `LIST_ITEM`, `TERM_DEF` |
| 27–32 | `GRAPHIC`, `BLOCKQUOTE`, `SEPARATOR`, `FRAME`, `EMBEDDED`, `MATH` |
| 33–37 | `LIVE_REGION`, `ALERT`, `CLICKABLE`, `FOCUSABLE`, `ANNOTATION` |
| 38–40 | `TEXT_BLOCK` (paragraph-like block with non-link text, for "skip links"), `LINE_HOST`, `TEXT_RUN` |
| 41–47 | spare |

`RANKED_CATS` holds counts for about 20 categories (headings, links, form fields, landmarks, tables, lists, list items, graphics, articles), about 80 B per node; GM measured 108 B per node with 19 counts.

### 4.5 Flat text and lines

The same treap carries the text dimensions (GM §5.1). Lines are hard breaks (block boundaries and newlines) refined by extents only when they are known; soft wrapping uses a "virtual line length" (NVDA `maxLineLength = 100`, `refs/nvda/source/config/configSpec.py:217`) when extents are absent. Word, sentence and grapheme boundaries are computed locally in a leaf with `unicode-segmentation`.

### 4.6 Tables

Per table: a dense row-major `Vec<Option<DocNodeId>>` of R×C slots with spanning cells filling every covered slot, plus row and column header vectors. Spans are fetched per cell (Mozilla bug 1862437, `refs/orca/src/orca/ax_table.py:714-720`). Virtualised grids (`aria-rowcount` ≫ rendered rows) use a sparse `BTreeMap<(row, col), DocNodeId>` keyed by `aria-rowindex`/`aria-colindex`. A row insert or delete rebuilds that table's grid (≈ 10 µs at 10⁴ slots, GM §5.2).

### 4.7 Live-region registry

`LiveRegions: HashMap<DocNodeId /* region root */, LiveProps>` where `LiveProps = { politeness, atomic, relevant, busy, created_rev, ring: DedupeRing, bucket: TokenBucket }`. A region root is a node whose own `live` attribute (or ARIA role default: `alert`, `status`, `log`, `marquee`, `timer`) is not `off`. Each node's nearest region root is cached and invalidated on subtree moves. Policy in §13.

### 4.8 Publication (*proposed*)

```rust
// gorca-doc::publish
pub struct DocWriter { w: left_right::WriteHandle<DocState, MirrorOp> }   // owned by the doc actor
pub struct DocRead  { r: left_right::ReadHandle<DocState> }               // one per reading thread
impl DocRead {
    /// Wait-free. The closure must finish within GUARD_MAX (100 µs); bulk readers copy out in chunks.
    pub fn with<R>(&self, f: impl FnOnce(&DocState) -> R) -> Option<R>;  // None if the doc is gone
}
```

- **PUB-1 Determinism.** Every mutation is a `MirrorOp` applied to both copies (left-right's operation log). Treap priorities are `xxh3(DocNodeId, doc_seed)`, never an RNG, and the free list is deterministic, so both copies have the same shape and the same slots.
- **PUB-2 Publish points.** At each `Settled` batch end and each update-round end; during a build, every 50 ms or 2,000 nodes (ADR-0001 §9.4). Readers never see half a batch.
- **PUB-3 Bounded guards.** Reader-side closures hold the guard ≤ 100 µs (hot-path queries take 1–5 µs). Say-all and the elements list copy out ≤ 256 nodes per guard. `publish()` waits for readers of the old copy, which is therefore bounded by `GUARD_MAX`; the watchdog counts any wait over 1 ms.
- **PUB-4 Cost.** Memory is about 2× the mirror (≈ 2 × 40 MB at 100k nodes [estimate]; the awake mirror counts inside ADR-0001 M-8's 600 MB with ADR-0002's 96 MiB, dormant mirrors are exempt, DM-M1); write CPU about 2×, off the hot path (PC-6).

Why not the alternatives: messaging every query to the doc actor puts a cross-thread round trip and the actor's slice latency on every key press; persistent maps (im/rpds) cost 2–5× per update and copy on every write (GM §1.3); cloning a `HashMap` snapshot per publish (the current scaffold, `crates/gorca-doc/src/mirror.rs`) is O(n) per batch. The B+tree upgrade path (GM §1.2 option E) with `Arc`'d chunks would allow O(log_B n) path-copy snapshots and could replace double-buffering; microbenchmark **MB-4** compares them (§19).

### 4.9 Invariants

- **DM-I1** Every slot reachable from the root is in the treap exactly once, in preorder; every subtree is a contiguous treap interval (GM §1.2).
- **DM-I2** `summary(root).counts[c]` equals the number of presentable nodes with category `c` (checked by a property test against a brute-force oracle replaying `research/web-graph/traces/testvec-nav2k.trace`).
- **DM-I3** A `Defunct` node is not in the treap. Its slot is freed only after the next publish, so readers holding an old `DocNodeId` see `Gone`, not a different node.
- **DM-I4** No node has `atspi` and `agp` keys that disagree on parent (hybrid); a disagreement marks the subtree unreconciled (§6.4).

## 5. Identity: variant C

`DocNodeId` is the logical node; transport objects map to it. When a browser recreates an object (Firefox move) or reuses one for different content (Chromium unkeyed rewrite), the mapping changes and the `DocNodeId` stays with the content. Cursors, bookmarks, ADR-0002 feature bindings and live-region state follow it.

### 5.1 The ladder

| Step | Key | When used | Validation |
|---|---|---|---|
| IL-1 | AT-SPI object path (`atspi` map) | whenever the object exists | **signature check**: if `sig` changed together with ≥ 1 sibling's in the same burst, the burst is an unkeyed rewrite (§10.1) and IL-3 re-assigns logical ids among the siblings |
| IL-2 | DOM `id` | re-matching removed and added objects under the same parent within one burst | `role` and `tag` must match, and the id must be fresh on both sides (`dom_id_fresh`): read in the same burst, or after the node's last name or children refresh. Chromium ids are never trusted after an attribute-only change (WF-7) |
| IL-3 | `(role, name, tag)` + sibling order | leftovers after IL-2 | LCS alignment of leftover children on the full key, then on `(role, tag)` (GM §3.2 step 3). Ties break by sibling order |
| IL-4 | GumTree-style subtree matcher (GM §3.2) | route swaps and wholesale re-renders larger than one parent | Results are `Heuristic { confirmed: false }` and are used for cursor restore and announcement suppression only |

- **IL-R1 Revalidation on read.** When the Reader presents a node in browse mode (navigation step, say-all chunk start, table move), the doc actor issues `Revalidate { parent }` (one `GetChildren`, Prefetch class, deadline 50 ms) for the cursor's parent and, when navigating a list, table or grid, for that container. Dedupe: once per parent per 250 ms, plus once after every burst that touched the parent. A mismatch yields a `Children` delta → keyed LIS diff (GM §3.1) → if the user's logical item moved, a `may_refine` correction (ADR-0001 §8.1). The hot path never waits for it.
- **IL-R2 Heuristic joins are never acted on blindly.** An action on a node whose join is `Heuristic { confirmed: false }` first sends `Validate` (role and name equal) within the Interactive deadline (ADR-0001 IR-4).
- **IL-R3 Firefox move re-matching.** `remove` × N and `add` × N under the same parent in one burst, with equal multisets of `(role, name)`, is classified `KeyedMove`; IL-2/IL-3 move the existing `DocNodeId`s onto the new objects and the subtree is not re-announced.
- **IL-R4 Remove semantics.** `ChildrenChanged:remove(child)` drops the child's whole mirror subtree at once (WF-9). Logical ids of dropped nodes stay re-matchable for one burst (they are `Defunct` with a re-match window of `W_max`), then their slots are freed.

## 6. Document sources (decision W)

### 6.1 Boundary (extends ADR-0001 §9.2)

```rust
pub enum SourceKind { Atspi, AgpFirefox, Hybrid }
pub struct SourceCaps {
    pub bulk_snapshot: bool,          // AT-SPI: true when GetTree exists
    pub push_deltas: bool,
    pub settled_markers: bool,        // AGP: true; AT-SPI: false (the doc actor infers settle, §9.3)
    pub reorder_events: bool,         // AGP: true; AT-SPI: false (WF-7)
    pub live_announcements: bool,     // AGP: politeness + text; AT-SPI: only `notification`
    pub text_leaves_as_nodes: bool,   // after TX-1 normalisation: always true at the boundary
    pub text_unit: TextUnit,
    pub actions: ActionCaps,
    pub join_keys: JoinKeyCaps,
    pub trust: Trust,                 // Authoritative | Advisory, per field group (§6.4)
}
pub enum DocRequest {                 // ADR-0001 variants, plus:
    Revalidate { parent: SrcNodeId, deadline: Instant, reply: Reply<SmallVec<[SrcNodeId; 16]>> },
    Matches    { from: SrcNodeId, rule: MatchRule, dir: Dir, limit: u32, deadline: Instant,
                 reply: Reply<SmallVec<[SrcNodeId; 4]>> },   // Collection.GetMatchesFrom/To fallback (§8.4)
}
```

SL-1…SL-5 of ADR-0001 apply. In particular SL-5: focus and caret always come from the AT-SPI connector, whatever the source.

### 6.2 `AtspiDocSource` (baseline, mandatory)

- **AS-1 Connections.** One P2P pool per browser app (`Application.GetApplicationBusAddress`), 4 connections, shared by that app's doc actors with weighted fair queueing (focused document first). In-flight window 16–64 per connection, adapted by AIMD on reply latency. The client must not link libatspi, which would make the browser return an empty address (SRC §2.1). Fallback: the bus call connection (ADR-0001 D-7).
- **AS-2 `GetTree` detection.** At attach, call `GetTree` on the document root with an empty rule and `[ChildCount]` under a Startup deadline. `UnknownMethod` or a signature error disables `bulk_snapshot` for that app, and the build uses pipelined `GetChildren` walks (≈ 25 µs/node at window 64, SRC §2.1).
- **AS-3 Cache-domain warming (Firefox).** On app attach (not per document; activation is process-global, WF-3), issue one throwaway query per needed domain on any document node: `GetAttributes` (DOMNodeIDAndClass, ARIA), `GetText` (Text), `GetRangeExtents` (TextBounds, Bounds), `Table.GetNRows` (Table), `GetRelationSet` (Relations). Treat "empty text on a node with children" as `Stale` (ADR-0001 §5.6) and re-query after the cache-update burst.
- **AS-4 Normalisation** (the full event mapping is §9.1): `/system` suffix stripped; index −1 or null child → `Children` refetch of the parent; Chromium `RowReordered` → `Children` refetch of the table rows; `text-caret-moved(0)` on a Chromium document → `ScrollToAnchor` (ADR-0001 §4.4); TX-1/TX-2 text shape.
- **AS-5 Per-call server-time bound.** The source tracks a per-app EWMA of server time per node (`β`) and per request (`α`), measured as reply latency minus queueing, and exports them to the planner (§9.4).

### 6.3 `AgpFirefoxSource` (opt-in accelerator; B-6 decides the default)

- Bootstrap: `--marionette -remote-allow-system-access` or AutoConfig; a `JSWindowActor` in the content process walks `nsIAccessibilityService` and streams over a Unix socket created with mode 0600 in `$XDG_RUNTIME_DIR/gorca/` (SRC §4.2). gorca checks `SO_PEERCRED` (same uid, pid in the Firefox process tree). Marionette's unauthenticated loopback listener is the weak point; closing it after handoff is TBD-B6-6.
- Data: columnar snapshots, sequenced batches with `Settled` markers, REORDER and NAME_CHANGE with the real accessible, subtree dominance applied in JS (1,752 HIDE → a few roots, SRC §4.2), `Announce` with politeness.
- Trust: page-controlled data, untrusted (SL-3). A sequence gap emits `Gap`; three gaps in 60 s or a crash loop detach it and fall back to AT-SPI (ADR-0001 IR-7).
- Enabled only by explicit user setting `web.firefox.source = "hybrid"`, with documented security implications. Default until B-6: `"atspi"` (**TBD-B6-1**).

### 6.4 `HybridDocSource` split (closes TBD-S4; confirmed or changed by B-6, **TBD-B6-3**)

| Field group | Authority in hybrid | Notes |
|---|---|---|
| Structure (children, order), names, roles, states, attributes, static text | AGP (sequenced) | AT-SPI `ChildrenChanged` events are used only as gap detectors and wake-ups |
| Focus, caret, selection, editable text, actions, bounds | AT-SPI | SL-5 |
| Live announcements | AGP `Announce`, else mirror inference (§13) | policy stays in the mirror |
| Route hints | AGP `RouteChanged`, plus §10.2 inference | |

Join (closes TBD-S3): there is no shared id (neither browser implements `AccessibleId`, SRC §5). AGP and `GetTree` snapshots of the same subtree are **zipped in DFS order** after TX-3 folding; each pair is verified on `(role, name, tag/id)`. A mismatch marks that subtree *unreconciled* and it is served AT-SPI-only until the next aligned snapshot. The mismatch rate is a B-6 metric (**TBD-B6-7**).

### 6.5 Source attachment state machine (doc actor)

```rust
enum SourceState {
    Detached,
    Attaching   { kind: SourceKind, deadline: Instant },
    Building    { phase: BuildPhase },                       // §8
    Live        { since: Instant },
    Resyncing   { scope: ResyncScope, reason: ResyncReason }, // Subtree(root) | Document
    Dormant     (Dormancy),                                   // not the active tab, or hidden (WF-12); §6.6
    Waking      { phase: WakePhase, started: Instant, plan: WakePlan }, // §6.6.3
    Degraded    { from: SourceKind, until_retry: Instant },   // hybrid fell back to AT-SPI
    Gone,
}
```

| From | Event | To | Effects |
|---|---|---|---|
| Detached | doc appears (showing, or focus enters it) | Attaching | open source; Startup deadline 5 s |
| Attaching | caps ok | Building{Attach→FocusContext} | §8 |
| Attaching | deadline / error | Degraded (hybrid) or Detached + retry backoff (AT-SPI) | AT-SPI failure: doc presented through the app mirror only (ADR-0001 IR-3) |
| Building | build complete | Live | publish; `Complete` coverage |
| Live | dirty set over the overflow bound (§9.5) or `Gap` | Resyncing{Document} | rebuild-as-resync: logical ids preserved by diff |
| Live | IL-R1 mismatch, unknown node in delta | Resyncing{Subtree} | targeted `GetTree` |
| Resyncing | done | Live | publish |
| Live, Resyncing | `SetActivity(Dormant)` (another tab became current) or the active document loses `SHOWING` | Dormant | full table in §6.6.2 |
| Dormant | `SetActivity(Awake)` or `SHOWING` back on the active document | Waking → Live | wake catch-up, §6.6.3 |
| any (hybrid) | AGP detach / 3 gaps in 60 s | Degraded | AT-SPI resync of unreconciled parts; cursor kept (IR-7) |
| Degraded | retry timer (backoff 30 s → 10 min) | Attaching{Hybrid} | — |
| any | `DocGone`, app `Gone` | Gone | tombstone; Reader notified |

Mirrors are never dropped for memory or by LRU; §6.6 defines dormancy and the only eviction rule (tab close).

### 6.6 Tab activity and dormancy (owner amendment, 2026-10-12)

**Owner decision.** There is no fixed cap on mirrors: **N mirrors = M tabs**, one mirror per top-level tab document, iframes included as subtrees (§2 item 1). Only the mirror of the tab the user is currently in is *awake*; every other mirror is *dormant*. Memory is explicitly not a constraint, so a dormant mirror is never evicted for memory, age or count. It is dropped only when its tab closes (§6.6.6).

#### 6.6.1 Who is awake

- **DA-1 Active document.** The Reader owns `active_doc: Option<DocId>`: the document of the selected tab in the **most recently focused browser window**, across all browser apps. It changes when (a) focus enters a different web document, (b) the selected `page tab` of the focused browser window changes (`STATE_SELECTED` on the tab, or `SHOWING` set on another document of that window), or (c) the document of the active tab is replaced by navigation (§6.6.6). When focus moves to a non-browser app, `active_doc` is **kept**: the user is still "in" that tab when they come back, and keeping one mirror live costs one document's events.
- **DA-2 Single awake mirror.** The Reader sends `SetActivity(Awake)` to the new active doc actor and `SetActivity(Dormant)` to the previous one, in that order, with the same epoch. Invariant **DM-I5**: at most one doc actor in the whole process is outside `Dormant`/`Detached`/`Gone`, apart from the bounded background prebuild of §6.6.5 and the single pre-warm of DA-4.
- **DA-3 Hidden awake document.** If the active document's window is minimised, on another workspace or occluded (`SHOWING` lost), Firefox stops updating its tree (WF-12). The doc actor goes `Dormant { cause: Hidden }` while it stays `active_doc`; it wakes when `SHOWING` returns. The wake path is the same as for a tab switch.
- **DA-4 Pre-warm (optional, default on).** When focus lands on a `page tab` in the browser chrome (the user is walking the tab strip) or on a tab in the tab-search list, the Reader sends `PreWarm` to that tab's doc actor. It runs wake phases W1–W2 (below) at `Prefetch` class without becoming awake, and is cancelled by the next `PreWarm` or by focus leaving the tab strip. At most one pre-warm runs at a time.

#### 6.6.2 State machine

`SourceState::Dormant` and the old "Dormant → Resyncing{Document}" row of §6.5 are replaced by:

```rust
enum SourceState {
    Detached,
    Attaching   { kind: SourceKind, deadline: Instant },
    Building    { phase: BuildPhase, priority: BuildPriority },   // Active | Background (§6.6.5)
    Live        { since: Instant },
    Resyncing   { scope: ResyncScope, reason: ResyncReason },
    Dormant     (Dormancy),
    Waking      { phase: WakePhase, started: Instant, plan: WakePlan },
    Degraded    { from: SourceKind, until_retry: Instant },
    Gone,
}

struct Dormancy {
    since: Instant,
    cause: DormantCause,          // BackgroundTab | Hidden
    marks: DormantMarks,
    resume: Bookmark,             // the Reader's saved cursor (also kept by the Reader, §11)
    rev_at_sleep: u64,            // mirror revision when it fell asleep
}
enum DormantMarks {
    Tracking { set: DirtySet, events: u32 },   // §9.2 dirty set, filled without any fetch
    Overflowed { at: Instant },                // UP-5 bound crossed while dormant: marks dropped
    NeedsRebuild { reason: RebuildReason },    // reload / load-complete / document swap seen while dormant
}
enum WakePhase {
    W1Cursor,      // revalidate the cursor's parent and container (IL-R1), urgent class
    W2Dirty,       // re-fetch the dormant dirty set, viewport first (§9.4 DP, same budget rounds)
    W3Drift,       // block-wise drift check of the rest of the document (§6.6.3)
    Rebuild,       // rebuild-as-resync (§9.5 UP-5): diff keeps DocNodeIds, cursor, live state
}
enum WakePlan { CatchUp, Rebuild }
enum DormantCause { BackgroundTab, Hidden }
```

| From | Event | To | Effects |
|---|---|---|---|
| Live, Resyncing | `SetActivity(Dormant)` or `SHOWING` lost on the active doc (DA-3) | Dormant{Tracking} | cancel queued fetches (generation bump, CC-3); in-flight replies are applied when they arrive (already paid for); live-region queue of this doc flushed; final publish |
| Building{Active} | `SetActivity(Dormant)` | Building{Background} | the build continues at `Bulk` class under §6.6.5 limits, then Dormant; the viewport block already fetched keeps the doc readable |
| Dormant{Tracking} | doc event | Dormant{Tracking} | fold into the dirty set (§6.6.4); no IPC |
| Dormant{Tracking} | marks > `max(50_000, 20 % of nodes)` | Dormant{Overflowed} | drop the dirty set (bounded memory) |
| Dormant{*} | `load-complete`, `reload`, document name+URL change, `Gap` | Dormant{NeedsRebuild} | drop the dirty set |
| Dormant | `SetActivity(Awake)` or `SHOWING` back on the active doc | Waking{W1Cursor, plan} | `plan = Rebuild` if marks are `Overflowed`/`NeedsRebuild` or dormancy > `T_stale`; else `CatchUp`. Publish nothing new yet: the snapshot is readable at once (DM-I6) |
| Dormant | `PreWarm` | Dormant (pre-warming) | run W1 and W2 at Prefetch class; results applied and published; marks cleared for fetched nodes; still Dormant |
| Waking{W1Cursor} | W1 done or 50 ms deadline | Waking{W2Dirty} (CatchUp) or Waking{Rebuild} | mismatches become `may_refine` corrections (IL-R1) |
| Waking{W2Dirty} | dirty set empty | Waking{W3Drift} | publish per round (PUB-2) |
| Waking{W3Drift} | drift ratio of the checked blocks > 25 % | Waking{Rebuild} | the rest of the document is rebuilt, not checked |
| Waking{W3Drift} | all blocks checked | Live | publish |
| Waking{Rebuild} | rebuild done | Live | diffed against the mirror: logical ids, cursor and live state kept (UP-5) |
| Waking{*} | events arrive | same | ordinary Live handling (§9): marks, settle, plan; wake rounds and event rounds share the §9.4 budget, urgent first |
| Waking{*} | `SetActivity(Dormant)` (fast tab cycling) | Dormant{Tracking} | queued wake fetches cancelled; what was applied stays; unfinished phases leave their marks in the dirty set; W3 progress is discarded and the next wake checks again |
| any except Gone | `DocGone`, tab closed, app `Gone` | Gone | evict (§6.6.6) |

`T_stale = 30 min` (*heuristic*, tuned by WM-13). A dormant mirror older than that is rebuilt rather than checked because a long-hidden Firefox document can have lost events (WF-12) in any part of the tree, and a block-wise check of everything costs about as much as a rebuild.

#### 6.6.3 Wake catch-up

The order follows what the user hears first.

1. **W0 (instant, no IPC).** **DM-I6:** a `Dormant` or `Waking` mirror is always readable through `DocRead` (wait-free, §4.8); it is the snapshot published when it fell asleep, plus whatever wake rounds have published since. The Reader restores `WebMode` and the cursor from its saved state (§11) and speaks the title and the cursor's line within the key → speech budget. Anything spoken from a waking mirror is marked `may_refine` (ADR-0001 §8.1).
2. **W1 cursor.** `Revalidate` of the cursor's parent and, if it is in a list, table or grid, of that container (IL-R1), urgent class, deadline 50 ms.
3. **W2 dirty set.** The marks gathered while dormant are planned with the §9.4 DP, viewport first, then ranked categories, in 20 ms server-time rounds. Each round publishes.
4. **W3 drift check.** Needed because neither browser's event stream can be trusted for a non-showing document: Firefox stops updating hidden documents (WF-12), and Chromium reorders silently (WF-7). The document is walked in the §8.1 `BlockPlan` order (viewport blocks first) with `GetTree(Name, ChildCount)`; a keyed diff against the mirror turns differences into ordinary dirty marks, which are fetched in the same rounds. BU-1 block splitting applies.
5. **Rebuild** (when chosen at wake or escalated from W3) is §9.5 UP-5 rebuild-as-resync, viewport block first. Because the result is diffed by `atspi` key and then by the §10 matcher, `DocNodeId`s, bookmarks and live-region state survive.

#### 6.6.4 What a dormant mirror still listens to

**Decision: cheap invalidation marking, not deafness.** The browser's events reach gorca regardless: AT-SPI match rules are per application, not per document, so the app actor receives and decodes them whether the doc actor listens or not (ADR-0001 §6). Folding an event into a dirty mark costs < 2 µs (§14) and makes the wake catch-up proportional to what changed, not to document size. Going deaf would save only those 2 µs per event and would force every wake to run a full drift check.

| Event (routed by the app actor) | Dormant handling |
|---|---|
| `children-changed`, `property-change` (name, description, role), `state-changed` (other than below), `text-changed`, attribute changes | dirty mark only (§9.2 bits); `remove` marks the parent, no local subtree drop (no `MirrorOp`, no publish) |
| `state-changed:showing` on the document, tab `selected` | activity signal to the Reader (DA-1, DA-3) |
| `focus` / `state-changed:focused` inside the document | activity signal (the tab became current) |
| `document:load-complete`, `reload`, `load-stopped`, document frame name/URL change | `NeedsRebuild` (a real navigation is handled as `Reset` on wake, §8) |
| document `defunct`, `children-changed:remove` of the document from its tab, app `Gone` | `Gone` (§6.6.6) |
| live-region changes | dirty marks only. Nothing is classified or queued: background tabs are muted (§13.2), and messages would exceed their TTL before a wake anyway. On wake, content that arrived while dormant is treated as present at region creation (not announced) |
| caret, selection, `active-descendant-changed`, `bounds-changed`, `visible-data-changed` | dropped (re-read from the mirror and W1 on wake) |

- **DM-I7** A `Dormant` doc actor issues no IPC call, holds no fetch in a queue and runs no timer except `T_stale` bookkeeping, outside an explicit `PreWarm`.
- **DM-I8** Dormant work per event is O(1) amortised and allocation-free except dirty-set growth, which is bounded by the `Overflowed` transition.
- When the app actor is `Flooded` (ADR-0001 R-3), the dormant doc actor is not even woken per event: the app actor sets one `Overflowed` bit for the document and the mirror is rebuilt on wake.

#### 6.6.5 Memory and background tabs

- **DM-M1 No memory eviction.** Dormant mirrors keep both `left-right` copies, the treap, the flat text and the live-region registry. Keeping both copies lets W0 read immediately with no re-copy on wake. Dormant mirrors are **exempt from ADR-0001 M-8** (owner decision); their size is measured and reported (WM-15), never enforced.
- **DM-M2 Background prebuild.** A tab that has never been active (opened in the background, restored session) gets its mirror built in the background so that its first visit is a wake, not a cold build: `Building{Background}` at `Bulk` class, **one document per browser app at a time**, only while the active document is `Live` with an empty dirty set, and paused whenever the active document has work. Tabs the browser has not loaded (Firefox lazy session restore, Chromium discarded tabs) expose no document and get no mirror until loaded. Setting: `web.prebuild_background_tabs = true`.
- **DM-M3 Dropping is off the actor.** Freeing a 100k-node mirror takes milliseconds; arenas are moved to a drop task on the `cpu` pool so that closing a window with 100 tabs never stalls an actor.

#### 6.6.6 Eviction: only on tab close

A mirror is evicted when its document leaves the browser: the tab closes (document `defunct`, or removed from its tab), the browser discards the tab's document (it comes back as a new load), or the app goes `Gone`. Navigation inside a tab replaces the document: the old mirror is dropped and the new document is built (`Reset`), with the cursor restored from the RS-4 per-URL bookmark. Nothing else evicts a mirror: not memory, not age, not mirror count.

## 7. Concurrency and backpressure

- The doc actor runs on `workers`; decode, `GetTree` reply parsing, matcher runs over 2k nodes and grid rebuilds go to `cpu` (ADR-0001 §3.2). The Reader reads through `DocRead` (wait-free).
- **CC-1** Apply work runs in slices of ≤ 1 ms with a `yield` between slices; `Revalidate`, `Validate`, `Matches` replies and Interactive requests are served between slices (biased select). Publish happens only at PUB-2 points.
- **CC-2** Queues: app actor → doc actor event batches (64, overflow merges into the dirty set, ADR-0001 §6.2); doc actor → source requests (per-app in-flight window, AS-1); doc actor → Reader: `DocNotice` on the background lane (64; drop-oldest except `RouteSwapped`, `CursorRefined`, `Announcement` with assertive politeness, which coalesce per kind).
- **CC-3** Cancellation: every outstanding fetch carries the dirty-set generation it was planned for. A reply whose target was dropped (remove event) or re-planned is discarded on arrival; superseded `Revalidate`s are dropped by request id. Build phases are cancelled by `Reset` (real navigation).

## 8. Build: viewport-first, parallel

### 8.1 Build state machine

```rust
enum BuildPhase {
    Attach,                                               // resolve showing document, caps (AS-2)
    FocusContext { inflight: u16 },                       // ancestors + siblings of focus/caret; their block
    Skeleton     { blocks: BlockPlan, done: BitVec },     // GetTree(Name, ChildCount, Attributes) per block
    Enrich       { queue: PriorityQueue<Slot> },          // role + state pass (pipelined)
    Complete,
}
```

1. **Attach.** Pick the `document web` with `STATE_SHOWING` (background tabs are skipped, WEB §1.2). Triggers: `document:load-complete`, first focus inside the document, or 500 ms after the document appears with `busy` set (Chromium's busy flag can outlive load-complete, WEB §2.4).
2. **FocusContext.** Ancestors of the focused node (or caret holder), its siblings, and the `GetTree` of its enclosing block. Publish **first-useful**: the focused line is speakable.
3. **Skeleton.** `BlockPlan`: the document's children (descend while there are fewer than 8 blocks, at most 3 levels), each with `GetExtents` pipelined (≤ 50 calls). Blocks intersecting the viewport go first, then by distance from the viewport. `GetTree(Name, ChildCount, Attributes)` per block across the 4 connections. **BU-1 block size:** a block whose previous `GetTree` took > 50 ms of server time is split one level down for the next request, so one call never janks the browser for long (`GetTree` has no limit, WF-2). Publish every 50 ms or 2,000 nodes.
4. **Enrich.** `GetRole` + `GetState` pipelined over nodes in priority order: cursor neighbourhood, viewport, nodes with provisional categories (headings, links, form fields), then the rest. Provisional roles are replaced; `cats` are recomputed (DM-3, DM-4).
5. **Complete.** Text bounds and extents remain lazy (only for line layout and visual adjacency).

### 8.2 Events during the build

- **BU-2 Snapshot plus replay.** Each block request records the event sequence number at send time. Events on a node inside a block whose `GetTree` is in flight are kept as dirty marks and replayed against the reply (marks newer than the request survive). Events on blocks not yet requested are dropped (the later `GetTree` sees the new state). Events on finished blocks go to the normal update path (§9).
- **BU-3 Chromium tree arrival.** On a 100k page the browser-side tree completes ~37 s after load (SRC §2.2). The build completes against what exists; later `ChildrenChanged:add` events fill the rest through §9. Coverage is reported honestly (`ChildrenUnfetched`).

### 8.3 Parallelism

Up to 4 `GetTree`s in flight per app (WF-1: saturation beyond 4), reply decoding on `cpu`, and index construction as an O(k) Cartesian-tree bulk build per block (GM §1.2) merged into the treap. Separate documents of different browsers build fully in parallel.

### 8.4 Partial coverage

`next(c, v)` that crosses a `ChildrenUnfetched` region, or returns nothing while coverage is incomplete, issues `Matches { from: v, rule: c→MatchRule, dir, limit: 1 }` (`Collection.GetMatchesFrom`/`GetMatchesTo`, `refs/at-spi2-core/xml/Collection.xml:106-160`) with a 50 ms deadline, and speaks the result through the app mirror. On timeout the user hears the `loading` earcon and "still loading"; the key is not retried silently.

## 9. Incremental update pipeline

```
drain ─► app ingress (ADR-0001 §6.3, coalescing) ─► app actor ─► doc actor
   └─ focus/caret (immediate) ─► Reader                   │
                                                          ▼
              event → DirtySet marks / local ops (§9.1) ─► settle (§9.3) ─► plan (§9.4) ─► fetch
                                                                                            │
                                 publish ◄─ sliced apply ◄─ diff / re-match (§10) ◄──────────┘
```

### 9.1 Event → dirty marks and local operations

| Event (normalised) | Local op (no IPC) | Dirty mark |
|---|---|---|
| `ChildrenChanged:remove(parent, child)` | drop the child's whole subtree (WF-9; IL-R4) | Chromium: none (index exact). Firefox: `CHILDREN(parent)` if the index was −1 or the child null |
| `ChildrenChanged:add(parent, idx, child)` | insert a placeholder (`Coverage::ChildrenUnfetched`) at `idx` when `idx` is valid | `SUBTREE(child)`; index −1 or null child → `CHILDREN(parent)` |
| `PropertyChange:accessible-name` | mark `name` stale (keep the old value for presentation) | `NAME(n)`; fetched eagerly only if `n` is in viewport, holds the cursor or has a ranked category; else lazily on read (SPA §8.4) |
| `PropertyChange:accessible-description / role / parent` | — | `PROPS(n, mask)` |
| `StateChanged:x(detail)` | apply the bit from the event payload | none |
| `TextChanged:insert/delete` (Firefox, any text; Chromium, editable and live only) | apply the splice to the run leaf (TX-1/TX-2) | `TEXT(n)` verify, idle priority |
| `RowReordered` (Chromium) | — | `CHILDREN(table rows)` |
| `PropertyChange:accessible-name` on the document | update title | route-swap signal (§10.2) |
| `Focus`, `focused`, caret, selection | none here | none; they go to the Reader immediately and to the doc actor for IL-R1 |
| `busy` true/false on a live region | §13 | — |
| `defunct` / `UnknownObject` reply | drop subtree (ADR-0001 I-1) | `CHILDREN(parent)` |

### 9.2 Dirty set

```rust
pub struct DirtySet {
    marks: HashMap<DocNodeId, DirtyMark>,     // bitflags: CHILDREN | SUBTREE | NAME | PROPS(mask) | TEXT
    gen: u64,                                  // bumped per plan; replies carry it (CC-3)
    first_at: Option<Instant>, last_at: Option<Instant>,
    urgent: SmallVec<[DocNodeId; 8]>,          // marks touching the cursor, its parent, or focus
}
```

Insert is O(1); memory is bounded by distinct dirty nodes, not by event rate (GM §3.5).

### 9.3 Settle (debounce)

- **UP-1** Trailing debounce `W = 40 ms` after the last mark, max-wait `W_max = 150 ms` from the first (WF-11: first event p95 ≤ 40 ms; a p95 burst of ~300 ms is processed in 2–3 rounds, each cheap).
- **UP-2** Immediate class (no debounce): marks in `urgent`, `Revalidate` mismatches, and route-swap rounds once the swap is detected (§10.2).
- **UP-3** `busy` subtrees (`aria-busy`) are deferred until `busy` clears or 2 s pass.
- **UP-4** AGP batches carry `Settled`; in hybrid mode the debounce is replaced by the marker plus a 10 ms grace.

Constants are heuristics (GM §7) and are tuned with perf-engineer's storm generator (WM-5).

### 9.4 Plan: optimal dirty-subtree selection

1. Drop marks under `Defunct` ancestors. Sort the rest by preorder rank (`cmp_order`, O(log n) each) and build the **virtual tree** of D (stack construction, LCAs by depth walk), O(|D|·depth).
2. Bottom-up DP (GM §3.4, exact):
   `best(v) = min( α_sub + β·size(v),  [v ∈ D]·a_v + Σ_{c ∈ vchildren(v)} best(c) )`
   where `size(v)` is the current mirror subtree size (an estimate for unfetched subtrees: the parent's average), `α_sub` and `β` are AS-5's calibrated per-app server-time costs (initial values from WF-1: `α ≈ 12 µs` server / 0.1–0.3 ms latency per request, `β ≈ 30 µs` per node for `GetTree` + role/state), and `a_v` is the sum of the mark costs (`CHILDREN` = 1 `GetChildren` + `GetTree` of new children; `NAME`, `PROPS`, `TEXT` = 1 call each).
3. **Budgeted round.** The plan is executed in rounds of at most `B = 20 ms` of estimated server time (browser-jank guard). Order inside a round is greedy (GM §3.4): urgent marks, then viewport, then nodes with ranked categories, then by `best(v)` ascending cost per resolved mark. Leftovers carry over to the next round.
4. NVDA's rule (always re-render the dominant ancestor, `refs/nvda/nvdaHelper/vbufBase/backend.cpp`) is the special case "every dirty node re-fetched whole"; WM-11 measures the saving over it.

### 9.5 Overload

- **UP-5** If marks exceed `max(50_000, 20 % of nodes)` or the doc actor's event queue overflows, the document goes `Resyncing{Document}`: a rebuild via §8 whose result is **diffed** against the mirror (keyed by `atspi`), so logical ids, cursor and live state survive. Work is capped at one rebuild (GM §3.5).
- **UP-6** The app actor's `Flooded` state (ADR-0001 R-3) turns doc events into dirty bits only; the doc actor resyncs lazily: cursor neighbourhood first.

### 9.6 Apply

Fetched subtrees become `MirrorOp`s: keyed child diff per parent (LIS, minimal moves |C| − LIS, GM §3.1) when identity holds; the matcher (§10) when a remove+add burst or a rewrite is detected. Text: common prefix/suffix diff (Chromium's own approach, `ComputeHypertextRemovedAndInserted`, WEB §2.4). Ops are applied in slices (CC-1), the treap and grids are updated incrementally, live-region diffs are extracted (§13), and one publish ends the batch.

## 10. SPA re-render matching and position restore

### 10.1 Burst classification

Computed by the doc actor over one settle window, from local information only:

| Class | Signature | Handling |
|---|---|---|
| `RouteSwap` | §10.2 | IL-4 matcher on the swapped region; restore (§10.3); announce title |
| `KeyedMove` (Firefox) | under one parent: N removes + N adds with equal `(role, name)` multisets | IL-2/IL-3; no announcement |
| `UnkeyedRewrite` | ≥ 3 name changes on sibling paths under one parent in one burst (SPA: 50 per prepend) | sibling re-match by `sig` (IL-1); logical ids follow content |
| `Append` | adds at the end of a container, no removes (infinite scroll; `busy` toggles) | plain insert; cursor untouched |
| `VirtualScroll` | adds at one end + removes at the other of a container with `aria-setsize`/`posinset` or a stable sliding pattern | sliding window; positions from `posinset` when present (TBD-WEB-2) |
| `LocalEdit` | anything else | keyed diff |

### 10.2 Route-swap detection

Any of: (a) a document name change plus, in the same burst, removal of subtrees totalling ≥ 50 nodes; (b) removals in one burst covering ≥ 30 % of the document or ≥ 80 % of the `main` landmark; (c) `DocURL` change (fetched with `Document.GetAttributeValue("DocURL")` after a title change, Background class); (d) AGP `RouteChanged`. SPA routes produce no document-level event (WEB §1.4; SPA §4).

### 10.3 Position restore

The Reader's position is a bookmark:

```rust
pub struct Bookmark {
    pub node: DocNodeId, pub offset: u32,
    pub ancestor_sig: u64,        // hash of the (role, tag) path from the root
    pub text_hash: u64,           // hash of the node's text
    pub ordinal: u32,             // flat-text offset at save time
}
```

Restore (GM §3.3), in order: (1) the node still exists (directly or through re-matching): same node, offset clamped; (2) nearest surviving or matched ancestor, then descend by `(child index, role)`; (3) nearest preceding matched node in document order; (4) `ordinal` clamped (ADR-0001 IR-7 text anchor).

- **RS-1 Route swap** (*proposed*): if a focus event arrived in the burst (focus management, e.g. `h1[tabindex=-1]`), the cursor goes to the focus target. Otherwise rule (2) puts it at the **start of the new content** (the first node of the swapped region under the surviving ancestor), rather than the document top, and the new title is spoken. No automatic say-all on SPA routes (setting).
- **RS-2** If restore lands anywhere other than the bookmarked node, the next utterance is prefixed by a short `moved` earcon (ADR-0002 SE-5); the user is never moved silently.
- **RS-3** Matched nodes are not re-announced, and matched live-region content is not treated as new.
- **RS-4** Per-URL bookmarks are kept in RAM for the session (NVDA remembers the caret per document, WEB §3.1), so a reload or back navigation returns to the last position. Never persisted (ADR-0002 PR-4).

## 11. Browse-mode state machine (refines ADR-0001 §7.2)

`WebMode` is owned by the Reader, one instance for the document that has focus; it maps onto ADR-0001 `Mode`: `Browse` ↔ `Mode::Browse`, `Focus` ↔ `Mode::Focus` with a document context.

```rust
enum WebMode {
    Inactive,                                                           // focus not in a web document
    Loading { doc: DocId, entered: Instant, focus: Option<DocNodeId> }, // before first-useful
    Browse  { doc: DocId, cur: Bookmark, scope: Scope },                // Scope: Document | Modal(DocNodeId)
    Focus   { doc: DocId, node: DocNodeId, cause: FocusCause, resume: Bookmark, scope: Scope },
    Dormant { doc: DocId, saved: Box<WebMode> },                        // not the active tab, or hidden (§6.6)
}
enum FocusCause { Auto, Manual, Application }
```

Pass-through predicate `P(n, reason)` (from NVDA `shouldPassThrough`, `refs/nvda/source/browseMode.py:366-440`): reason `QuickNav` → false; editable and not unavailable → true; focused `combobox`, `listbox`, `tree`/`treeitem`, cells of an interactive `grid`/`treegrid`, `slider`, `spinbutton`, `menu`/`menuitem`, `application`, embedded objects → true; links, buttons, checkboxes, radios, toggles → false. Extensions may override per node (§15).

| From | Event | To | Effects |
|---|---|---|---|
| Inactive | focus enters doc, mirror first-useful | Browse{cur = focus node} or Focus{Auto} if `P(n, Focus)` | earcon, grab set |
| Inactive | focus enters doc, not yet first-useful | Loading | `loading` earcon after 300 ms |
| Loading | first-useful | Browse at the focus node, else the RS-4 bookmark, else the top | real page load: announce title and counts; say-all on load per setting (NVDA `autoSayAllOnPageLoad = true`, `configSpec.py:223`) |
| Loading | 1 s timeout | Browse on the partial mirror | §8.4 fallbacks |
| Browse | quick-nav key, arrows, Home/End | Browse{cur'} | never switches mode (`P(_, QuickNav)` = false); speech per §12 |
| Browse | Enter/Space on `n` with `P(n, Activate)` | Focus{Auto, n} | `Action::Focus` via IR-5; earcon `mode.focus` |
| Browse | Enter/Space on other `n` | Browse | `DoDefault` (click) via IR-5 |
| Browse | app `FocusChanged(n)` in doc (Tab, script) | `P(n, Focus)` ? Focus{Auto, n} : Browse{cur = n} | NVDA `autoPassThroughOnFocusChange = true` (`configSpec.py:220`) |
| Browse | `CaretMoved` | Browse | ignored unless it follows our own action (correlated by the Reader) |
| Browse | toggle (gorca+Space) | Focus{Manual} | focus the cursor's focusable ancestor if any |
| Focus{Auto} | Escape | Browse{cur = node} | Escape swallowed; but if `n` is an expanded popup (combobox, menu) the first Escape goes to the app |
| Focus{Auto} | `FocusChanged(m)` in doc | `P(m)` ? Focus{Auto, m} : Browse{cur = m} | — |
| Focus{Manual} | `FocusChanged(m)` in doc | Focus{Manual, m} | sticky |
| Focus{Application} | Escape | unchanged | only the toggle leaves (`role=application`; NVDA treats `APPLICATION_ROLES` specially, `browseMode.py:302, 1989-1999`) |
| Focus{*} | toggle | Browse{cur = node} | earcon `mode.browse` |
| Focus{*} | focused node removed | Browse{restore(resume)} | §10.3 |
| Browse, Focus | `aria-modal` dialog opened | same, `scope = Modal(d)` | cursor confined to `d`; quick nav searches inside `d` only. This differs from NVDA, which treats `dialog` as an application role (focus mode, `browseMode.py:302`); gorca keeps browse mode so long dialogs stay readable, and Tab/focus changes still apply `P` |
| Browse, Focus | modal closed | same, `scope = Document` | cursor back at the opener's bookmark |
| any in doc | `RouteSwapped` | same mode | §10.3 |
| any in doc | another tab becomes active, or the doc is hidden (DA-1, DA-3) | Dormant{saved} | the Reader keeps `saved` per `DocId` (`HashMap<DocId, WebMode>`, entry removed on `DocGone`), with no cap |
| Dormant | doc becomes active or showing again | `*saved` | W0: title and cursor line spoken from the snapshot with `may_refine`; wake catch-up runs (§6.6.3) |
| any | focus leaves doc | Inactive | bookmark saved (RS-4) |
| any | `DocGone` | Inactive | — |

- **BM-1** Every transition advances the epoch (ADR-0001 §3.4), emits `SetGrabSet` (Browse: quick-nav letters, arrows, Home/End, PgUp/PgDn, Enter, Space; Focus{Auto}: gorca commands and Escape; Focus{Manual, Application}: gorca commands only), and plays the mode earcon (`SpeechCmd::Earcon`, ADR-0002 SE-5).
- **BM-2 Browse cursor does not move system focus** while moving (NVDA sets focus on caret moves only for pass-through targets, `browseMode.py:1976-1985`; `autoPassThroughOnCaretMove = false`, `configSpec.py:221`). It scrolls the target into view with `ScrollTo` (Background class), debounced 150 ms and coalesced to the latest, so held arrows do not flood the browser.
- **BM-3 Field-diff speech.** On every move, the presenter diffs the ancestor container stack (list, table, landmark, form, dialog) of the old and new position and speaks entered/left containers before the content (NVDA `speakTextInfo`, WEB §3.1).
- **BM-4** The full key map lives in `docs/design/modes.md` (still open since ADR-0001).

## 12. Structural navigation commands and index queries

All answers come from `DocRead` with no IPC (WEB §4.1). Keys are provisional (`docs/design/modes.md`); Shift reverses direction.

| Command (default key) | Query | Utterance |
|---|---|---|
| Next heading (H), heading level 1–6 (1–6) | `next(cur, HEADING \| HEADING_k)` | "‹name›, heading level k" (+ "k of N" by verbosity: `rank`, `count`) |
| Link (K), unvisited (U), visited (V) | `next(cur, LINK…)` | name, link, visited state |
| Form field (F), edit (E), button (B), combobox (C), checkbox (X), radio (R) | `next(cur, …)` | role, name, state, value |
| Landmark (D) | `next(cur, LANDMARK \| REGION)` | landmark type, name |
| Table (T), list (L), list item (I) | `next(cur, …)` | "table with R rows and C columns", "list of N items" |
| Graphic (G), blockquote (Q), separator (S), frame (M), embedded (O), article (A) | `next(cur, …)` | role, name |
| Live region (W) | `next(cur, LIVE_REGION)` | region name, politeness |
| Skip link block (N) | `next(cur, TEXT_BLOCK)` | text |
| Next similar item (Phase 2) | `next(cur, cluster(cur))` (GM §4.1) | as the item |
| Table cell (Ctrl+Alt+arrows) | grid lookup O(1) | cell, entering header changes |
| Line, word, character (arrows) | flat-text offsets (§4.5) | text |
| Elements list (gorca+F7) | in-order iteration over a category, chunked (PUB-3) | dialog |
| Where am I | `rank`, `count`, ancestor stack | position summary |

- **SN-1** No wrap: at the end, the `edge` earcon plus "no next heading". Wrap is a setting.
- **SN-2** The `scope` filter (modal) is an interval check: `rank(result)` inside the scope node's preorder interval.
- **SN-3** The target is presented from the mirror immediately. If its `name` is `Stale`, the cached value is spoken with `may_refine`, and IL-R1 runs.
- **SN-4** During partial coverage, §8.4 applies.
- **SN-5** Index query cost p99 < 2 µs at 100k nodes (WF-13); the whole command p99 < 5 ms gorca-side.

## 13. Live-region policy

### 13.1 Classification (no IPC on the event path)

- **LR-1** An insert or text change belongs to the nearest region root in the registry (§4.7). Attributes are in the mirror from `GetTree`; if a region root's attributes are not yet known, they are fetched once (Prefetch) and the message is held ≤ 100 ms.
- **LR-2** Messages come from the **mirror diff**, not from raw event strings: inserted text for `relevant` ⊇ `additions text` (the default), removed text only when `relevant` includes `removals` or `all`, the whole region text when `atomic=true`.
- **LR-3** Per-region settle of 50 ms trailing (200 ms max) merges Chromium's per-update triad (name change, text delete, text insert; SPA §5).
- **LR-4** Content that is present when its region appears is not announced, except `role=alert` (an added `alert` node is announced assertively). Changes after the region exists are.
- **LR-5** `busy=true` defers the region until `busy` clears (or 3 s), then the final diff is presented once (Orca does the same, `refs/orca/src/orca/live_region_presenter.py:247-284`).
- **LR-6** `notification(text, politeness)` (`ariaNotify`, WEB §1.4) and AGP `Announce` are direct messages under the same throttle.

### 13.2 Scheduling and throttling (*proposed defaults*)

| Rule | Value |
|---|---|
| Politeness → speech priority (ADR-0001 §7.4) | assertive → `Message`; polite → `Notification`; off → history only |
| Assertive vs user speech | assertive interrupts current speech, except speech the user caused within the last 1.0 s in the current epoch (then it is queued next). It flushes queued polite messages of the same document |
| Polite | never interrupts; plays when the speech lane is idle |
| TTL | polite 5 s, assertive 10 s (Orca keeps 45 s, `live_region_presenter.py:125`) |
| Per-region rate | token bucket: polite 1 msg/s, burst 3; assertive 2 msg/s, burst 3 |
| Per-document rate | 4 msg/s total |
| Over the limit | messages are counted, not spoken; at most once per 5 s a summary "‹N› updates in ‹region name›" |
| Atomic regions | last-writer-wins slot: a ticker speaks only its latest value |
| Dedupe (GM §5.3) | normalise (truncate 512, digits → `#`, collapse whitespace, case-fold), 64-bit hash ring of the last 8 per region, suppress within 10 s; near duplicates by 64-bit SimHash over 3-grams, Hamming ≤ 3 |
| Scope | only the focused document's regions; background tabs muted (Orca has the same switch, `live_region_presenter.py:582-600`) |
| History | last 20 messages per document, RAM only, navigable by command; dropped with the document |
| User controls | mute a region, mute all on the page, per-page politeness override, live regions on/off |

- **LR-7 Bounded work.** O(L + m) per message (L ≤ 512, m = 8); the classifier never blocks or allocates on the steady path. A 10k inserts/s flood into one region costs dirty marks and bucket decrements, and the rest of the pipeline is unaffected (WM-8).

## 14. Latency budgets (gorca-side unless stated)

| Operation | Budget (p99) | Notes |
|---|---|---|
| Key → speech for quick nav, line, table move | < 5 ms of the 15 ms (CLAUDE.md) | index query < 2 µs, presentation < 1 ms, zero IPC |
| Focus or caret event in doc → speech | < 15 ms | join O(1) < 10 µs (IR-2) |
| Mode switch → earcon + speech | < 15 ms | grab set update per ADR-0001 §7.3 |
| Doc-actor event ingest (mark insert) | < 2 µs | per event, after app ingress |
| Settle | W 40 ms, W_max 150 ms; urgent 0 | §9.3 |
| Small refresh (≤ 200 nodes) after settle | < 20 ms wall; ≤ 2 ms doc-actor CPU | §9.4 |
| Apply slice / publish | ≤ 1 ms / < 100 µs + reader drain | CC-1, PUB-3 |
| Route swap, 1k nodes / 20k nodes | mirror consistent < 150 ms / < 1 s after the last event (p95) | WM-4 |
| Matcher (cursor restore) | ≤ 5 ms for ≤ 2k swapped nodes on the doc actor; larger on `cpu`, ≤ 40 ms at 100k [estimate, GM §3.2] | §10 |
| Parent revalidation | Validate deadline 50 ms; correction only within `may_refine` (150 ms audio) | IL-R1 |
| Wake W0: tab switch → title and cursor line speech start (from the dormant snapshot) | < 15 ms | no IPC; `may_refine` (§6.6.3) |
| Wake W1: cursor parent/container revalidated | < 50 ms (Validate deadline) | IL-R1 |
| Wake W2: dormant dirty set re-fetched, ≤ 1,000 marks | < 150 ms wall p95; viewport marks within the first 20 ms round | §9.4 rounds |
| Wake W3: drift check complete | 10k nodes < 1 s; 100k < 5 s in the background, viewport blocks < 150 ms | §6.6.3; WM-12 |
| Wake rebuild-as-resync | viewport consistent < 300 ms; full per WM-1 | §9.5 |
| Dormant event cost | < 2 µs per event, 0 IPC calls | DM-I7, DM-I8 |
| Live region: last mutation → speech start | ≤ 50 ms settle + 15 ms | under throttle caps |
| First-useful (10k nodes) | < 150 ms after load-complete (ADR-0001 M-9) | §8 |
| First-useful (100k nodes) | < 300 ms after the browser has the tree | viewport block first |
| Full mirror | 10k < 2 s (M-9); 100k Firefox < 5 s, Chromium bounded by tree arrival (WF-4) | background |
| Coverage fallback (`Matches`) | 50 ms deadline | §8.4 |
| IPC deadlines | Interactive 120 ms total per action; Validate/Revalidate 50 ms; Prefetch 300 ms; `GetTree` block Background 2 s with a 50 ms server-time target | ADR-0001 §6.4 |

## 15. Core vs extensions

| Concern | Where | Why |
|---|---|---|
| Mirror, treap, flat text, grids, diff, matcher, dirty planner, publication | `gorca-doc` | generic, hot |
| Browser quirk normalisation: offsets, text-leaf shape (TX-1/TX-2), caret overload, cache warming, remove semantics, index −1/null child, `/system`, `RowReordered`, stale-id flag | `gorca-connector-atspi` (`AtspiDocSource`) | normalisation, keyed by toolkit (ADR-0001 §5.6, §9.4) |
| Browse/focus SM, default pass-through predicate, structural navigation, live policy engine, route-swap inference, position restore | `gorca-core` / `gorca-doc` | same behaviour in every browser |
| AGP protocol, bootstrap, lockstep aligner | `gorca-doc-source-agp-firefox`, `browser/firefox-agp/` | B-6 arm |
| Firefox UI noise filters (autocomplete noise, page-switch stale state: Mozilla 1867044, `refs/orca/src/orca/scripts/web/script.py:1460-1464`), dialogs owned by an unknown app (Mozilla 1882794), bogus `SECTION` in frames (Mozilla 1879750), PDF.js viewer mode rules | `ext-firefox` | browser-specific policy |
| Electron detection (toolkit `Chromium` without browser chrome, `refs/orca/src/orca/document_presenter.py:170-190`), omnibox popup quirks | `ext-chromium` | browser-specific policy |
| Site rules (per-site politeness overrides, `role=application` web apps, auto focus-mode exceptions) | data files read by both extensions | site hacks stay out of the core |

WIT additions (*proposed*, for wasm-engineer, `gorca:ext@0.2.0`): `pass-through-verdict(node, reason) -> option<bool>`; `live-politeness(region) -> option<politeness>`; extension navigation units get a reserved category bit (48–63) computed by `nav-match` **at apply time** in the doc actor (2 ms limit per batch), never per key press. A workaround needed by both extensions moves into `AtspiDocSource` (ADR-0001 §10 rule).

## 16. Bake-off B-6 (spec reference and TBD hooks)

The spec is SRC §8 (pages, metrics 1–9, decision rule). This ADR changes the arms and adds criteria:

| Arm | Description |
|---|---|
| (a) `ATSPI-TUNED` | `AtspiDocSource` as in §6.2, no flags |
| (b) `HYBRID-AGP-FF` | `HybridDocSource` = AT-SPI authority + `AgpFirefoxSource` (§6.3–6.4) |
| (c) optional `AGP-FF-WEBEXT` | DOM-based WebExtension, as a cost reference for "no privileged flags" only |

Firefox only; Chromium stays AT-SPI (WF-4). Both arms implement `DocSource` against the same `gorca-doc` mirror, so the arms differ only below the boundary. Added judging inputs: WM-1…WM-5 and WM-9 of this ADR, plus ADR-0001 M-9, M-13, M-15. Decision rule (SRC §8): (b) becomes Firefox's default only if it wins metrics 1–3 by ≥ 5× and passes robustness (7), coverage (8) and operational cost (9); otherwise (a) stays default and (b) ships opt-in.

TBD hooks filled by B-6:

| Hook | Plugs into |
|---|---|
| **TBD-B6-1** Firefox default source | `web.firefox.source` default (§6.3) |
| **TBD-B6-2** AGP wire format (columnar snapshot, batch framing) and socket authentication | `gorca-doc-source-agp-firefox` |
| **TBD-B6-3** Hybrid field split confirmed or changed | §6.4 table |
| **TBD-B6-4** Subtree dominance in JS vs in the mirror for AGP | UP-4, §6.3 |
| **TBD-B6-5** Per-source cost constants (α, β, AGP subtree dump) | §9.4 DP defaults |
| **TBD-B6-6** Closing Marionette after handoff, or AutoConfig bootstrap | §6.3 security |
| **TBD-B6-7** Lockstep alignment mismatch rate | §6.4; IR-4 |

## 17. Failure modes

| Failure | Detection | Behaviour |
|---|---|---|
| Browser hung (main thread blocked) | ADR-0001 `AppHealth::Hung` | doc actor stops planning; navigation continues on the mirror (stale but present); "not responding" once |
| Browser floods events | app `Flooded` | dirty bits only; lazy resync (UP-6) |
| `GetTree` missing or erroring | AS-2 | pipelined walk |
| `GetTree` reply huge / slow | BU-1 server-time EWMA | smaller blocks |
| Firefox cold cache domain | empty text on a node with children | `Stale`; retry after the push (AS-3) |
| Hidden or occluded Firefox document | `SHOWING` lost, window inactive | `Dormant{Hidden}`; drift check on wake (§6.6) |
| Events lost for a dormant document (WF-12, WF-7) | not detectable while dormant | W3 drift check on every wake; rebuild if `T_stale` passed or drift > 25 % |
| Fast tab cycling (Ctrl+Tab held) | `SetActivity` churn | each wake cancelled at the next switch; only W0 runs per visited tab; no backlog of fetches (§6.6.2) |
| Wake while the browser is hung | `AppHealth::Hung` | W0 serves the snapshot (stale but present); "not responding" once; catch-up resumes on recovery |
| Window with 100 tabs closed | 100 `DocGone` | 100 evictions; arenas freed on the `cpu` pool (DM-M3) |
| Many open tabs (hundreds) | — | by design: one mirror each, memory grows with tabs (DM-M1); reported by WM-15 |
| Chromium silent reorder | IL-R1 mismatch | `Children` delta; `may_refine` |
| Wrong re-match | `Validate` failure before an action, user report | action refused and re-resolved; counted (WM-6) |
| AGP gap, crash, version drift | sequence gap, EOF, alignment mismatches | `Degraded` → AT-SPI, cursor kept (IR-7) |
| Doc actor panic | supervisor | rebuild-as-resync; cursor from the Reader's bookmark (ADR-0001 §6.5) |
| `DocNodeId` generation overflow | arena | rebuild under a new `nav_id` |
| Reader guard held too long | PUB-3 watchdog counter | logged; offending reader fixed (bug) |
| Live-region flood | token buckets | counted, summarised; nothing else affected |

## 18. Alternatives considered

| Alternative | Why not |
|---|---|
| CDP source for Chromium | No faster (renderer-bound), no live/caret events, refused on the default profile since Chrome 136 (SRC §3). Decision W. |
| DOM-recomputing WebExtension as the tree | A second accessibility implementation to keep bug-compatible; 16 s for 100k elements on the page's main thread (SRC §4.3). Kept only as B-6 arm (c) reference. |
| No mirror, query live (Orca) | Tens of round trips per line, whole-document `GetMatches` per key press (WEB §1.6). |
| Order-maintenance labels | O(1) compare, but relabel spikes of 13 ms (100k) and 174 ms (1M) (GM §1.2). |
| Persistent collections for snapshots | 2–5× update cost (GM §1.3); double-buffering is cheaper at our write rate. |
| Queries by message to the doc actor | Adds a cross-thread hop and slice latency to every key press; breaks ADR-0001 PC-1. |
| Event-only incremental mirror | Chromium silent reorders (WF-7) make it drift forever. |
| Periodic full reconcile | Costs a full `GetTree` (1–6 s at 100k) on a timer; IL-R1 targets only what the user reads. |
| Identity by AT-SPI path alone | Unkeyed rewrites keep paths with new meaning; Firefox moves change paths (SPA §3). |
| Embeddings for identity | GM §0: tree matching is exact enough and cheaper; embeddings stay for grouping (Phase 2). |
| Separate mirror per iframe | Navigation crosses frames in document order; one mirror per tab keeps one order index. |

## 19. Consequences and metrics

**Positive.** Every navigation is local and microsecond-scale; the browser can hang or flood without stalling the reader; SPA re-renders keep the user's place; Chromium's silent reorders are caught where they matter; live regions cannot flood speech; the Firefox accelerator can be added or removed without touching the mirror.

**Negative / costs.** Two copies of each mirror (PUB-4). An undocumented bulk method (`GetTree`) on the critical path, with a fallback that is ~3× slower. Heuristic identity can be wrong (WM-6 bounds it; actions validate). Synthetic Firefox text runs add a translation layer for offsets. The AGP route, if enabled, depends on Gecko internals and an unauthenticated bootstrap.

| ID | Metric | Target | How measured |
|---|---|---|---|
| WM-1 | time to first-useful / full mirror | 10k: < 150 ms / < 2 s (M-9); 100k: < 300 ms / < 5 s Firefox | `tests/web/pages/gen_large.py`, private browser, B-6 harness |
| WM-2 | browse-mode key → speech | p99 < 5 ms gorca-side during a 10k ev/s storm on the same document | perf harness, M-1 spans |
| WM-3 | index query | next/prev/k-th p99 < 2 µs at 100k nodes under the GM mutation mix | `gorca-bench` |
| WM-4 | mutation → mirror consistent | 1 / 100 / 1,000 changed nodes and route swap (1k, 20k): p95 within §14 budgets | B-6 harness, `tests/web/spa/` |
| WM-5 | storm | 10k AT-SPI ev/s on one document for 60 s: no unbounded growth, no `Gap` on the AT-SPI path, Reader lag < 20 ms (M-3) | storm generator + `tests/web/spa/` floods |
| WM-6 | identity and restore | on the SPA corpus (both browsers, 4 frameworks): cursor restored to the same logical item ≥ 0.95 for keyed moves and unkeyed rewrites, ≥ 0.85 for route swaps (GM §3.2); 0 actions on a wrong node (M-13) | `tests/web/spa/` replay + scripted cursors |
| WM-7 | silent reorder | Chromium keyed reverse under the cursor's list: corrected on the next read, ≤ 1 revalidation per read | `tests/web/spa/` |
| WM-8 | live regions | 100 msgs / 2 s chat and 10k inserts/s flood: spoken rate within §13.2 caps, M-1 Δp99 < 1 ms, correct politeness on 100 % of the corpus, 0 messages older than TTL spoken | `tests/web/spa/` live scenarios |
| WM-9 | browser jank | no page frame > 50 ms attributable to gorca during build and storms (rAF jitter probe, SRC §8 metric 4) | B-6 harness |
| WM-10 | memory | ≤ 1 KB per node for both copies plus indexes at 100k nodes [target to validate] (within M-8) | `smem`, heap profiler |
| WM-11 | re-fetch efficiency | DP plan vs NVDA-style ancestor re-render on the SPA corpus: server time and calls per burst; DP must not be worse on any scenario | planner replay |
| WM-12 | dormant drift | background tab and hidden Firefox window mutated for 60 s: mirror matches the browser < 1 s after wake (10k nodes), with `DocNodeId`s of unchanged nodes preserved | private headless gnome-shell (SPA §7.2) |
| WM-13 | wake latency | Ctrl+Tab between 2, 20 and 200 open tabs (10k nodes each, 0 / 100 / 5,000 changes while dormant, dormant 1 min / 1 h): W0 < 15 ms p99, W1 < 50 ms, W2/W3/rebuild within §14; cursor on the same logical node ≥ 0.95; tunes `T_stale` and the 25 % drift threshold | B-6 harness + `tests/web/spa/` |
| WM-14 | dormant cost | 50 dormant tabs each receiving 1k ev/s: 0 IPC calls from dormant doc actors, < 2 µs per event, active document's M-1 Δp99 < 1 ms | storm generator |
| WM-15 | mirror memory | RSS per dormant mirror and total vs open tabs (10, 100, 300 tabs); reported, not gated (DM-M1) | `smem`, heap profiler |
| MB-4 | publication | `left-right` double buffer vs `ArcSwap` of a chunked persistent B+tree: read p99, publish cost, memory at 100k | microbench (perf-engineer) |

## 20. Open questions and sign-off

### 20.1 Open (owner)

| ID | Owner | Question |
|---|---|---|
| TBD-WEB-1 | web-a11y-engineer | Which Chromium `showing`/`visible` states flip on scroll or reorder, and with which events (SPA §10)? Affects `cats` (hidden filter) freshness. |
| TBD-WEB-2 | web-a11y-engineer | Are `aria-posinset`/`aria-setsize`/`aria-rowindex` exposed as AT-SPI attributes in both browsers (virtual scroll positions)? |
| TBD-WEB-3 | web-a11y-engineer | DOM-recycling virtualisers (TanStack Virtual, CDK): identity retention under scroll. |
| TBD-WEB-4 | linux-platform-engineer | P2P socket reachability for Flatpak/Snap browsers; `GetTree` availability on older at-spi2-core. |
| TBD-WEB-5 | web-a11y-engineer | `chrome.automation` as a Chromium accelerator (outside B-6; only if AT-SPI events prove inadequate). |
| TBD-WEB-6 | math-researcher | Re-run the matcher and the grouping on the 46 real trees in `research/web-graph/real-trees/` (GM §9). |
| TBD-WEB-7 | wasm-engineer | WIT 0.2 hooks of §15. |

### 20.2 Sign-off (all items approved 2026-10-12; item 4 amended)

1. **Canonical text shape (§4.3):** text leaves as nodes; Firefox text runs are synthesised by `AtspiDocSource`.
2. **Publication by `left-right` double-buffering (§4.8)** with deterministic apply, validated by MB-4 against a persistent B+tree.
3. **`DocNodeId` as a generational logical identity (§4.1, §5)** that survives re-matching; amends ADR-0001 §9.3.
4. **One mirror per top-level tab document**, iframes included. *Approved with amendment:* no fixed cap (N mirrors = M tabs); only the active tab's mirror is awake, all others dormant; fast wake catch-up; eviction only on tab close (§6.6).
5. **Update constants and overload rule (§9.3–9.5):** W = 40 ms, W_max = 150 ms, urgent class immediate, 20 ms server-time budget per round, rebuild-as-resync on overflow.
6. **Browse/focus defaults (§11):** NVDA parity: auto focus mode on focus change on, on caret move off, browse cursor does not move system focus, Escape leaves auto focus mode, `role=application` sticky, modal dialogs confine the cursor.
7. **Route-swap restore (RS-1):** cursor to the focus target, else the start of the new content; title spoken; no automatic say-all on SPA routes.
8. **Live-region defaults (§13.2):** TTLs 5 s / 10 s, the rate caps, background tabs muted, no announcement of content present at region creation, assertive interrupts except user speech younger than 1 s.
9. **Session-only per-URL bookmarks (RS-4)**, never persisted.
10. **B-6 arms revised (§16)**: no CDP arm; the WebExtension arm only as a cost reference; Chromium out of B-6.
11. **WIT 0.2 additions (§15)**: pass-through verdict and live politeness hooks; extension navigation units as category bits computed at apply time.
