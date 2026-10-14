# Browse mode (M7): design

Status: **Accepted design** (2026-10-14), normative for M7 and the M7a slice. Owner: architect. Amends
ADR-0003 (its §21 lists every change made here) and builds on ADR-0007 (shared text layer).

Inputs: `CLAUDE.md`; `docs/plan/phase-2.md` §M7, §M8, §7; ADR-0001 (§3.4 epoch, §6 actors, §8.1
`may_refine`, §9 SourceLayer); ADR-0003 (all, including the §20.1 amendments); ADR-0005 (commands,
grab sets); ADR-0006 (review, `ReviewFrom::Browse`); ADR-0007 (text layer); `docs/design/modes.md`
(§2 grab sets, §4.4 browse keys); `docs/reports/2026-10-14-web-prep.md` (WP), `docs/reports/2026-10-14-gorca-doc-core.md`
(DC); code on `main`: `crates/gorca-doc/src/web/`, `crates/gorca-doc-source-atspi/`,
`crates/gorca-doc-source-api/`, `crates/gorca-core/src/{doc_actor.rs, mode.rs, app_actor.rs}`.

## 0. Decisions in this document

| # | Decision | Section |
|---|---|---|
| BD-1 | **D1: the source owns the dirty set, settle and refetch planning** (push mode, as built). The doc actor owns delta application, publication, the removal grace (IL-R4), user-driven pull requests (`Revalidate`, `Text`, `Action`) and activity. The doc actor never plans IPC. | §1 |
| BD-2 | **One `DocNodeId`:** the generational `{slot, gen}` handle moves into `gorca-model`; `gorca-doc::web` uses it; the scaffold `gorca_doc::mirror` and `DocNodeId(u32)` are deleted. | §2 |
| BD-3 | Events reach the source through the core (app actor → doc actor → `DocSourceCtx.events`), never through a second bus subscription. Joins travel inside the deltas (`SrcNode.local`). | §3 |
| BD-4 | Firefox text is in the mirror: text tier on, TX-1 run leaves synthesised by the source, block breaks and object markers set by the doc actor's translator. | §4 |
| BD-5 | Cache-domain warm-up runs once per browser application at attach, behind a `DocSourceFactory` hook, and gates document builds for at most 3 s. | §6 |
| BD-6 | `WebMode` (ADR-0003 §11) is the Reader's browse state machine; `Mode::Browse` loses its cursor field (the cursor lives in `WebMode`). | §7 |
| BD-7 | Chromium silent reorders: `Revalidate` on read, only for sources that declare `silent_reorders`, deduplicated and bounded. | §13 |
| BD-8 | M7a = Firefox, static pages, H / 1–6 / K / Tab / Enter / arrows, auto focus mode on editable focus, Escape back. | §16 |

## 1. D1: who owns the dirty set and the plan

**Decision (BD-1):** option (a) of WP §5, made permanent, with a clear split.

| Concern | Owner | Notes |
|---|---|---|
| Event → dirty mark, settle (25 ms quiet, 120 ms cap, 1 s for a burst with ≥ 50 removes), refetch of what changed, resync over 400 dirty nodes | **source** (`AtspiDocSource`, AS-7, AS-8) | It holds the transport, the paths and the cost model (AS-5); it must coalesce *before* it spends IPC |
| The §9.4 DP planner and the 20 ms server-time rounds | **source**, when needed (WM-11) | It would be the source's internal planner; nothing in the doc actor changes |
| Dormant marking (DM-I7, DM-I8) | **source** (`SetActivity(Dormant)`: no IPC, marks only, overflow → `NeedsRebuild`) | |
| `DocDelta` → `DocOp` translation, apply in slices, publication | **doc actor** | §4, §5 |
| Removal grace (IL-R4, D3 of WP: build it) | **doc actor** | removed subtrees stay detached for 1 s (≥ the source's storm settle), then `Reap` |
| `Revalidate`, `Text`, `Action`, `Validate` (user-driven pull) | **doc actor** issues, source answers | IL-R1, IR-5 |
| Urgent marks (UP-2: the cursor's neighbourhood) | doc actor sends `DocRequest::Prioritize { nodes }`, source bypasses its settle for marks on them (M7b) | |
| Wake plan (W1 cursor, W2 dirty flush, W3 drift, rebuild) | doc actor sequences; source executes `Revalidate`, `SetActivity(Awake)` (flushes its marks), `DriftCheck` (M7b), `Rebuild` | §5.2 |

**Settle constants (D4):** the source's 25 / 120 ms (+1 s for move bursts) are the only debounce. The
doc actor adds none: a batch is applied as soon as its `Settled` marker arrives (§5.1), so a one-node
change reaches the mirror in ~29 ms (WP §2), not ~44 ms.

**Why not (b), the doc actor with the DP planner:** two owners of one dirty set (the source must still
coalesce events before IPC) means duplicated bookkeeping and a message round trip per plan round; the
measured push mode already meets every ADR-0003 §14 budget (1 node 29–36 ms, 1,000 nodes 174–283 ms,
route swap 76–117 ms; WP §2). Keeping IPC out of the doc actor makes it a pure CPU actor (apply +
publish), which is easy to test and cannot hang. Confidence: medium-high; revisit only if WM-11 shows
the per-parent refetch is more than 2× the DP plan's server time on the corpus.

Changes to the source contract (ADR-0003 §21, AM-1…AM-4): `Settled { batch }` is emitted at the end of
every pushed batch (it is known exactly in push mode); `DocRequest` gains `SetActivity`, `Rebuild`,
`Prioritize`, `DriftCheck`.

## 2. `DocNodeId`: one type

**Decision (BD-2).**

```rust
// gorca-model/src/doc.rs (replaces `pub struct DocNodeId(pub u32)`)
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DocNodeId { slot: u32, gen: u32 }
impl DocNodeId {
    /// Minted only by `gorca-doc` (arena). Other crates hold and compare ids, never construct them.
    #[doc(hidden)] pub const fn from_parts(slot: u32, gen: u32) -> Self;
    pub const fn slot(self) -> u32; pub const fn generation(self) -> u32;
    pub const fn to_u64(self) -> u64; pub const fn from_u64(v: u64) -> Self;
}
```

- **ID-1** `gorca_doc::web::DocNodeId` becomes `pub use gorca_model::DocNodeId`. The arena keeps its slot
  and generation rules (DM-I3: a reaped slot bumps its generation).
- **ID-2** Lifetime: an id is never reused within one doc actor's life, across navigations too
  (`Reset` detaches and reaps, it does not start a new arena), which is stronger than ADR-0001 §9.3
  "never reused within a `nav_id`". Generation overflow (2³² reuses of one slot) → the doc actor
  restarts with a new `DocId` (ADR-0003 §17 row).
- **ID-3** The scaffold `crates/gorca-doc/src/mirror.rs` (`DocumentMirror`, `DocSnapshot`, `DocNode`,
  `Bookmark`, `DocCursor`, `ApplyOutcome`) is deleted in M7a together with `gorca-core/src/doc_actor.rs`'s
  current body. `DocCursor` is replaced by `WebCursor` (§7.1) in `gorca-core`; `Bookmark` (ADR-0003
  §10.3) moves to `gorca-doc::web::bookmark`.
- **ID-4** An xtask rule forbids `DocNodeId::from_parts` outside `gorca-doc`.

## 3. Event feed, joins and routing

### 3.1 Feed (BD-3)

```rust
// gorca-doc-source-api ctx.rs (amended)
pub struct DocSourceCtx {
    pub doc: DocId,
    pub app: AppId,                                  // new: for transport keys
    pub sink: DeltaSink,
    pub requests: mpsc::Receiver<DocRequest>,
    pub events: mpsc::Receiver<DocEvent>,            // new: the document's normalised events
    pub join: JoinPort,                              // kept for hybrid sources only
    pub shutdown: CancellationToken,
    pub clock: Clock,
}
pub struct DocEvent { pub ev: NodeEvent, pub seq: u64, pub at: Instant }
// gorca-model SrcNode (amended): pub local: Option<LocalId>   // transport key of the same object
```

- **EF-1** The connector's drain and ingress stay the only event path (ADR-0004, PC-2). The app actor
  routes each event of a browser app to the doc actor that owns its node (§3.2), which forwards it to its
  source. `AtspiDocSource` maps `LocalId → PathKey` with its own table (it computes
  `gorca_dbus::drain::local_id(path)` for every path it fetched) and treats the event exactly like its
  current `AtspiEvent` kinds: `ChildrenChanged` / `NodeRemoved` → re-list the parent; name, description,
  value, state → re-read; `TextChanged` → re-read text; defunct → removal. The harness keeps `bus.rs` as
  an adapter that produces `DocEvent`s.
- **EF-2** Bounded: 1,024 events per doc actor and 1,024 per source; overflow sets the overflow flag and
  the source resyncs (UP-5, IL-R5 keeps ids).
- **EF-3** `SrcNode.local` carries the transport key of each fetched object (`None` for synthetic text
  runs). This replaces `JoinPort` for the AT-SPI source: a join can never be dropped separately from its
  node, and IR-2 needs no second channel.

### 3.2 `WebJoin` and routing

```rust
/// One per browser app. Keys are disjoint per document; each doc actor is the single writer of its keys.
pub struct WebJoin(scc::HashIndex<LocalId, (DocId, DocNodeId)>);
```

- **JN-1** The doc actor inserts `(local → (doc, id))` **after** the publish that made `id` visible, and
  removes it when the node is reaped or its `local` changes. A reader that hits the table therefore
  always finds the node in its `DocRead` copy, or a newer one.
- **JN-2 Reader join (IR-2).** `FocusChanged(n)`: `WebJoin.get(n.local)` → `(doc, id)` → `DocRead` check
  → cursor. p99 < 10 µs (gate J1). A miss → IR-3: focus is presented from the app mirror as today, the
  cursor is not moved, `web_join_miss` is counted (M-13).
- **JN-3 App actor routing** (`gorca-core/src/app_actor/doc_route.rs`, new): for an app with at least
  one doc actor, each `NodeEvent` is looked up in `WebJoin` and sent to that doc actor. An unknown node is
  sent to the app's awake doc actor (most new objects arrive as a `ChildrenChanged` of a known parent,
  so this is rare), else dropped and counted `doc_event_unrouted`. Events still invalidate the app mirror
  as today; focus, caret and selection still go to the focus lane (ADR-0007 EV-3).
- **JN-4** Document roots are recognised by role: `Role::DocumentWeb` (new variant in `gorca-model`,
  mapped from AT-SPI `DOCUMENT_WEB`; today it is folded into `Role::Document`). A `DocumentWeb` with no
  `DocumentWeb` ancestor is a top-level document and gets a doc actor; a nested one is a frame subtree
  (§12). Electron apps follow the same rule; `ext-chromium` may change their default mode (§7.3).

## 4. Mirror content: translator and flat text (BD-4)

### 4.1 Text tier and TX-1 (source)

- **FT-1** Browse mode needs the text in the mirror (Firefox exposes no text leaves). The source runs
  with `TextPolicy::Runs`: `Text.GetText(0, -1)` for every node except roles known to carry no text
  (images, separators, scroll bars, documents' own frames), in the first block too. Measured cost of the
  text tier: about +0.9–1.1 s on a 100k-node full build (WP §1.2, AS-10); the 100k full build stays under
  the 5 s budget, the first block grows by ~4 ms per 400 nodes (to be confirmed, W-V3).
- **FT-2 TX-1 runs in the source.** Firefox hypertext is split at U+FFFC into synthetic run leaves
  (`Role::StaticText`, `SrcNode.local = None`), interleaved with the real children in the `Children`
  list: `[run0, child0, run1, child1, …]`. Run ids are deterministic:
  `SrcNodeId(RUN_TAG | xxh3(parent, run_index))`, so an edit of one run is an in-place `Upsert`. Empty runs
  are omitted. Phantom U+FEFF is stripped and text is scalar (TU-2) before it leaves the source. A
  `TextChanged` on a container re-reads its text and re-emits its runs (and `Children` when the run count
  changed). This moves TX-1 from "the connector" (ADR-0003 §3 row) into `gorca-doc-source-atspi`: the
  source is the only component that has both the text and the children (AM-5).
- **FT-3** Chromium (M9) needs no runs: static-text children already carry the text (TX-2).

### 4.2 Translator (`gorca-core/src/doc_actor/translate.rs`)

The translator turns a settled batch of `DocDelta`s into `DocOp`s:

- **TR-1** `by_src: HashMap<SrcNodeId, DocNodeId>` (doc-actor private). `Upsert` of a known id →
  `SetProps` + `SetText` (only fields that changed). `Upsert` of an unknown id → held in `pending` until a
  `Children` list places it.
- **TR-2 Bulk insert.** Inside one batch, new nodes whose parent is also new are assembled into
  `SubtreeSpec` forests; only attachment points (existing parents) get `SyncChildren`. A first block
  becomes one `InsertSubtree` (Cartesian bulk build, DC: 100k nodes in 17 ms).
- **TR-3** `Children { parent, list }` → `SyncChildren { parent, desired }` with `Existing(id)` for known
  ids and `New(spec)` for pending ones. A listed id with no `Upsert` yet (progressive build, WP §1.4) gets a
  placeholder (`Role::Unknown`, no text, `Coverage::Unfetched`) that a later `Upsert` fills.
- **TR-4** `Removed(ids)` → `RemoveSubtree` (detach). IL-R4: a later `Upsert` of a new id under the same
  parent whose signature matches a detached root within the grace (1 s) re-attaches it
  (`find_detached`, `AttachSubtree`) instead of creating a node. After the grace, `Reap`.
- **TR-5** `Reset { root }` → `RemoveSubtree(old root)` + `Reap(None)` + insert of the new root: one
  arena, no new `DocRead` handle; old ids become `Gone` (generation), the Reader restores its cursor from
  the RS-4 per-URL bookmark.
- **TR-6** Node fields. `NodeSpec` (gorca-doc) gains `meta: Arc<NodeMeta>`:

```rust
pub struct NodeMeta {
    pub src: SrcNodeId,
    pub local: Option<LocalId>,
    pub states: StateSet,
    pub description: Option<Arc<str>>,
    pub value: Option<Value>,
    pub attrs: AttrMap,              // tag, posinset, setsize, live, atomic, relevant, busy, grid indices…
}
```

  `NodeView` exposes it; categories stay a pure function of `(role, level, flags)` (DM-4). Flags:

| Flag | Rule |
|---|---|
| `HIDDEN` | `VISIBLE` state absent. **Never** from `SHOWING` (TBD-WEB-1: `showing` is viewport state and flips without events) |
| `VISITED`, `FOCUSABLE` | the states |
| `REGION`, `ARTICLE`, `BLOCKQUOTE` | `xml-roles` / `tag` |
| `LAYOUT` | `layout-guess=true` on a table (Firefox) |
| `LIVE` | the node's **own** `live` attribute is `polite` or `assertive` (region root), or role `alert/status/log/marquee/timer`. Inherited `container-live` does not make a root (AM-8 splits the two keys) |
| `FRAME` (new) | frame / internal frame raw role, or a nested `DocumentWeb` → `Cat::FRAME` |
| `BLOCK` (new) | `display` attribute not inline-level (`block`, `list-item`, `table`, `table-row`, `flex`, `grid`, `flow-root`, …), or, when `display` is absent, role in {paragraph, heading, list, list item, table, row, landmark, separator, document, form, blockquote, figure} |
| `BREAK_BEFORE` (new) | FT-4 |

### 4.3 Flat text rules (`gorca-doc::web`)

- **FT-4 Breaks.** A newline is contributed *before* a node's own text when the node is `BLOCK`, or when its
  previous visible sibling is `BLOCK`. The translator recomputes the flag for a parent's children on every
  `SyncChildren` and on a `display` change (O(children)). This yields one line per block and puts inline
  text that follows a nested block on its own line (`<div><p>A</p>tail</div>` → `A`, `tail`).
- **FT-5 Blank lines.** Lines that contain only whitespace (consecutive block boundaries) are skipped by
  line moves, say-all and quick-nav presentation. Character moves step over them as one `blank`.
- **FT-6 Object markers.** A presentable control or graphic whose subtree contributes no text (image,
  checkbox, radio, empty edit, combo box, slider, spin button, button without text, embedded object)
  gets own text U+FFFC (one scalar), so it has a place in its line. The translator sets it after apply
  for touched nodes, from the subtree's text length (O(1) from the treap summary).
- **FT-7 Soft lines** (ADR-0007 TL-3): hard lines longer than `browse.max_line_length` (default 100,
  NVDA) are split at word boundaries on read. A hard line over 64 Ki scalars is wrapped from checkpoints
  every 4 Ki scalars (bounded cost, approximate wrap there).
- **FT-8** `WebDoc` implements `gorca_text::TextSpace` (ADR-0007 §3): every lookup is `Ready`.

## 5. Doc actor

### 5.1 Pipeline

```
DocEvent (from app actor) ──► source.events
source ──► DeltaSink ──► doc actor: collect batch until Settled ──► translate (TR) ──► apply in ≤1 ms
           slices (CC-1) ──► publish (one ArcSwap swap) ──► WebJoin updates (JN-1) ──► DocNotice to Reader
```

- **DA-P1** Publish points: every `Settled`; during a build every 50 ms or 2,000 nodes (PUB-2); never in
  the middle of a batch.
- **DA-P2** `DocNotice` (background lane, 64, coalescing per kind as ADR-0003 CC-2):
  `FirstUseful { doc }`, `Built { doc }`, `Changed { doc, rev, touched: Interval }`, `CursorRefined`,
  `RouteSwapped` (M8), `Live { .. }` (M7b), `Gone { doc }`.
- **DA-P3** The doc actor is CPU-only; it sends requests to its source and never awaits them inline
  (replies are messages).

### 5.2 Attachment state machine (subset of ADR-0003 §6.6.2 for M7a; full machine in M7b)

| From | Event | To | Effects |
|---|---|---|---|
| Detached | `EnsureDoc` from the registry (focus in the doc, or `load-complete`) | Attaching | wait for the app's warm state (§6) |
| Attaching | `Warm`, `NotNeeded` or `Failed`, or 3 s | Building{Active} | `factory.create` + `attach`; a build started before warm-up finished sets `attrs_suspect` |
| Building | first block applied | Building | publish, `FirstUseful` |
| Building | build complete | Live | publish, `Built` |
| Live | `SetActivity(Dormant)` | Dormant | `SetActivity(Dormant)` to the source (marks only, DM-I7) |
| Dormant | `SetActivity(Awake)` | Waking{Rebuild} (M7a) / Waking{W1Cursor} (M7b) | W0: the snapshot is readable at once (DM-I6); M7a always rebuilds (IL-R5 keeps ids) |
| Waking{Rebuild} | rebuild done | Live | publish, `Changed` |
| Live | warm-up finished while `attrs_suspect` | Live | one `Rebuild` (ids kept) |
| Live | overflow / `Gap` / unknown node in a delta | Live (source resyncs) | — |
| any | document defunct, tab closed, app `Gone` | Gone | WebJoin keys removed; arena dropped on `cpu` (DM-M3) |

### 5.3 Document registry and the active document (`gorca-core/src/doc_registry.rs`)

- **DR-1** One registry per browser app, owned by its app actor. `EnsureDoc { focus: NodeKey }` (a
  `ReaderEffect` sent when the Reader sees focus enter an app with a `DocumentWeb` ancestor or a `WebJoin`
  hit) walks the app mirror ancestry (≤ 32 hops, `Ancestry` fill on a miss, Interactive) to the
  top-level `DocumentWeb`, and creates or wakes its doc actor. `load-complete` on a top-level document also
  ensures it.
- **DR-2** `DocDirectory: DocId → DocHandle { read: DocRead, app, root, url }` is lock-free for the Reader
  (same pattern as `MirrorDirectory`).
- **DR-3 Active document (DA-1, M7a form).** The active document is the one that last held focus. When
  focus moves into another document, the previous one gets `SetActivity(Dormant)` and the new one
  `SetActivity(Awake)` (DM-I5). Focus moving to the browser chrome or another app keeps the active
  document awake. Tab-strip `PreWarm` and background prebuild are M7b.

## 6. Cache-domain warm-up at attach (BD-5)

Facts (WP §1.3, AS-3): Firefox serves AT-SPI from its parent-process cache, which starts with only names,
descriptions and states; the first query of any other domain activates it and every content node is
re-sent (~25 µs per node + ~30 ms). Activation is per application. A build that starts earlier silently
lacks `tag` and other attributes.

```rust
// gorca-doc-source-api (new)
pub trait DocSourceFactory: Send + Sync + 'static {
    fn needs_warm(&self, app: &AppInfo) -> bool;                       // AT-SPI source: toolkit Gecko
    fn warm(&self, app: &AppInfo, doc: NodeKey) -> BoxFuture<'static, Result<WarmReport, SourceError>>;
    fn create(&self, app: &AppInfo, root: NodeKey) -> Result<Box<dyn DocSource>, SourceError>;
}
// gorca-core doc_registry.rs
enum WarmState {
    NotNeeded,
    NoDocument { retry_at: Instant },                 // app Ready, no DocumentWeb found yet
    Warming    { doc: NodeKey, started: Instant },
    Warm       { at: Instant },
    Failed     { at: Instant, attempts: u8, retry_at: Instant },
}
```

| From | Event | To | Effects |
|---|---|---|---|
| (none) | `AppLifecycle::Ready`, browser app (`needs_warm`) | NoDocument | find a document: walk the app root's children down to the first `DocumentWeb` (≤ 6 levels, ≤ 64 calls, Prefetch); any document will do (a background tab, `about:blank`) |
| (none) | `Ready`, `!needs_warm` | NotNeeded | — |
| NoDocument | a `DocumentWeb` seen (walk, `load-complete`, focus) | Warming | `factory.warm(app, doc)` as a task, Background class, budget 5 s |
| NoDocument | `retry_at` (1 s, 2 s, 4 s … 30 s) | NoDocument | walk again |
| Warming | `Ok(report)` with `ready` | Warm | builds waiting in `Attaching` proceed |
| Warming | error, timeout, `!ready` | Failed | builds proceed (`attrs_suspect`); retry after 10 s, at most 3 times |
| Failed | `retry_at` | Warming | — |
| any | app `Gone`, P2P pool reconnect to a new process | (none) | the next `Ready` starts again |

- **WU-1** The warm-up never runs on the Reader lane and never blocks focus speech: until the document
  is built, the Reader presents focus from the app mirror (IR-3) and says "loading" if the user presses a
  browse key (ADR-0003 §8.4).
- **WU-2** gorca already attached to Firefox when a page loads: the page pays nothing (WP: 100k page first
  block 187 ms instead of 2.46 s). gorca started after a large page was loaded: the warm-up pays ~25 µs per
  node of that page once, in the background (2.4 s at 100k).
- **WU-3** Fission: whether a content process created after the warm-up inherits the active domains is
  verified in M7a (W-V1). If it does not, `WarmState` becomes per content process, keyed by the
  document's process (one probe per new process).
- **WU-4** The normalisation stays in the source crate (`warm.rs`), behind the factory: the core never
  names Firefox (ADR-0003 §15).

## 7. Browse and focus mode state machine (BD-6)

### 7.1 Types (`gorca-core/src/browse/mod.rs`)

```rust
pub struct WebCursor { pub node: DocNodeId, pub local: u32 }   // scalar offset inside the node's own text
pub enum Scope { Document, Modal(DocNodeId) }
pub enum FocusCause { Auto, Manual, Application }

pub enum WebMode {
    Inactive,
    Loading { doc: DocId, since: Instant, focus: Option<NodeKey> },
    Browse  { doc: DocId, cur: WebCursor, scope: Scope, sticky: bool },
    Focus   { doc: DocId, node: DocNodeId, cause: FocusCause, resume: WebCursor, scope: Scope },
}
// The Reader also keeps `saved: HashMap<DocId, SavedWeb>` (cursor, mode, scope) for dormant documents,
// removed on `DocNotice::Gone`, no cap (ADR-0003 §11).
```

Mapping to `Mode` (modes.md): `WebMode::Browse` ↔ `Mode::Browse { doc, sticky }`; `WebMode::Focus` and
`Loading` ↔ `Mode::Focus`, with the input context flag `web_focus: Option<FocusCause>` deciding whether
Escape is grabbed (only `Auto`). `Mode::Browse` drops its `cursor` field and `ReviewFrom::Browse` drops
it too (the cursor is `WebMode`'s); a cursor move is not a mode transition and emits no `SetGrabSet`.

### 7.2 Pass-through predicate `P(node, reason)` (`browse/forms.rs`)

From ADR-0003 §11 and NVDA `shouldPassThrough` (`refs/nvda/source/browseMode.py:326-440`):

| Node | `Focus` / `Activate` | `QuickNav` |
|---|---|---|
| editable (`EDITABLE` state) and not unavailable | true | false |
| combo box, list box, tree / tree item, interactive grid cell, slider, spin button, menu / menu item, tab list with focus inside, `application`, embedded object | true | false |
| link, button, check box, radio, toggle, anything else | false | false |

Extensions may override per node (`pass-through-verdict`, ADR-0003 §15). The decision uses the app
mirror's snapshot of the focused object (always present at focus speech time), so it never waits for the
doc mirror.

### 7.3 Transitions

| From | Event | To | Effects |
|---|---|---|---|
| Inactive | focus enters a top-level document, mirror first-useful | `P(n, Focus)` ? Focus{Auto, n} : Browse{cur = n} | mode indication; focus speech as today |
| Inactive | focus enters a document, not first-useful | Loading | `EnsureDoc`; if the focus node is editable go Focus{Auto} at once (no wait) |
| Loading | `FirstUseful` | Browse at the focus node, else the RS-4 bookmark for the URL, else the top | real page load: title, then the first line (M7a); say-all on load per setting (M7b) |
| Loading | 1 s | Browse on the partial mirror | §8.4 of ADR-0003 fallbacks |
| Browse | quick-nav key, cursor key | Browse{cur'} | never changes mode (`P(_, QuickNav)` = false); BM-2 scroll |
| Browse | `browse.activate` on `n` with `P(n, Activate)` | Focus{Auto, n} | `Action::Focus` (IR-5) |
| Browse | `browse.activate` on another `n` | Browse | `DoDefault` on the nearest activatable ancestor (link, button, control), Interactive |
| Browse | app `FocusChanged(m)` in the document (Tab, script, click) | `P(m, Focus)` ? Focus{Auto, m} : Browse{cur = m} | NVDA `autoPassThroughOnFocusChange = true` |
| Browse | `CaretMoved` in the document | Browse | ignored unless it follows our own action (ADR-0007 `OwnMoves`) |
| Browse | `browse.toggle` (gorca+Space) | Focus{Manual, focusable ancestor of cur or the document} | `Action::Focus` when there is a focusable ancestor |
| Focus{Auto} | Escape | Browse{cur = node} | Escape is grabbed only here; if the node is an **expanded** combo box or menu the first Escape is passed through (grab set without Escape while `EXPANDED`) |
| Focus{Auto} | `FocusChanged(m)` in the document | `P(m)` ? Focus{Auto, m} : Browse{cur = m} | |
| Focus{Manual} | `FocusChanged(m)` in the document | Focus{Manual, m} | sticky |
| Focus{Application} | Escape | unchanged | only `browse.toggle` leaves (`role=application`) |
| Focus{*} | `browse.toggle` | Browse{cur = node} | |
| Focus{*} | focused node removed | Browse{restore(resume)} | ADR-0003 §10.3 |
| Browse, Focus | `aria-modal` dialog opened / closed (M7b) | same, `scope = Modal(d)` / `Document` | ADR-0003 §11 |
| any in doc | another document becomes active, or the doc is hidden | Inactive, entry in `saved` | the document goes Dormant |
| Inactive | focus returns to a dormant document | `saved` mode | W0: title and cursor line spoken from the snapshot, `may_refine` |
| any | focus leaves the document (chrome, another app) | Inactive | RS-4 bookmark saved; the document stays awake (DR-3) |
| any | `DocNotice::Gone` | Inactive | — |

- **BM-1** (ADR-0003) every transition advances the epoch and emits `SetGrabSet`.
- **BM-5 Mode indication.** Until earcons exist (ADR-0002 SE-5; no `Earcon` command is implemented
  today), the transition speaks "browse mode" or "focus mode" before the focus utterance, in the same
  utterance (`Prio::Interrupt`). User question Q-W1.
- **BM-6 Quick-nav keys typed into a field.** A letter pressed while the grab set changes from Browse to
  Focus{Auto} is `Ambiguous` (ADR-0005): never executed, "press again" cue (decision 2026-10-13). The
  Focus{Auto} transition is triggered by the focus event, before the next key in practice (FM1 gate).

### 7.4 Grab sets

As `docs/design/modes.md` §2 and §4.4. M7a activates: H, Shift+H, 1–6 ± Shift, K ± Shift, Up, Down,
Left, Right, Ctrl+Left, Ctrl+Right, Home, End, Ctrl+Home, Ctrl+End, Prior, Next, Return, KP_Enter,
space, gorca+space; Escape only in Focus{Auto}. Tab and Shift+Tab are never grabbed.

## 8. Reading over the flat text

### 8.1 Cursor moves (new commands, `browse.cursor_*`)

`WebCursor` resolves to a flat offset with `offset_of(node) + local` (O(log n)) and back with
`at_offset` (O(log n)); it follows its node through edits because `local` is relative to the node.

| Key | Command | Move | Spoken |
|---|---|---|---|
| Down / Up | `browse.cursor_next_line` / `prev_line` | next / previous non-blank soft line (FT-5, FT-7) | the line (§8.2); "bottom" / "top" at the ends (NVDA) |
| Right / Left | `browse.cursor_next_char` / `prev_char` | grapheme (ADR-0007 TL-2), blank lines as one `blank` | the character (verbaliser); a marker speaks its object |
| Ctrl+Right / Ctrl+Left | `browse.cursor_next_word` / `prev_word` | TL-1 word | the word |
| Home / End | `browse.cursor_line_start` / `line_end` | soft-line edge | the character there |
| Ctrl+Home / Ctrl+End | `browse.cursor_top` / `bottom` | document edge | the line |
| Prior / Next | `browse.cursor_page_up` / `page_down` | 25 lines (NVDA `linesPerPage`, `configSpec.py:218`) | the line |
| Ctrl+Up / Ctrl+Down, Alt+Up / Alt+Down (M7b) | paragraph / sentence | hard line / sentence | the unit |

`info.current_line`, `text.say_char/word/line`, spelling and `text.say_all` act on the virtual cursor in
Browse (ADR-0007 §12).

### 8.2 Line presentation (`browse/present.rs`)

- **LP-1** For a line `[a, b)`, walk the nodes from `at_offset(a)` in preorder until `offset_of ≥ b`
  (O(log n + k)). Text runs give text segments. A node *entered* on this line whose role is announced
  inline adds a prefix: link ("link", "visited link"), heading at line start ("heading level 2"), button,
  check box (with state), radio (with state), combo box (with value), edit (with value or "blank"),
  graphic (name), list item marker. A marker (FT-6) speaks role, name, state and value.
- **LP-2 Field diff** (BM-3; M7b): entering and leaving lists ("list with 5 items", "out of list"),
  tables, landmarks, block quotes, frames, dialogs, articles, compared on the container stacks of the old
  and the new cursor.
- **LP-3** The line is built inside one `DocRead::with` closure (PUB-3, ≤ 100 µs; lines are capped at
  2,048 scalars per guard and continue in a second guard).
- **LP-4 Quick nav** speaks the target element's text range (its subtree, capped at 400 scalars) with its
  role prefix: "heading level 2, Products"; "link, Home". "‹k› of ‹N›" (`rank`, `count`) only at
  verbosity `verbose`. No wrap (SN-1): "no next heading", "no previous link".
- **LP-5** Everything spoken from the mirror carries `may_refine` (ADR-0001 §8.1); `CursorRefined`
  (§13) can replace it within the refine window.
- **LP-6 Scroll (BM-2).** After a move, `Action::ScrollIntoView` of the cursor node, Background class,
  debounced 150 ms and coalesced to the latest, so held keys do not flood the browser.

## 9. Say-all in browse (M7b)

The ADR-0007 §11 engine with `SayAllSrc::Doc`: chunks are produced from the flat text with the line
presenter's segments (objects by role and name), zero IPC; `Started` progress moves the virtual cursor;
any key stops and leaves the cursor at the start of the chunk being spoken. Say-all on page load follows
`browse.say_all_on_load` (NVDA `autoSayAllOnPageLoad = true`, `configSpec.py:223`; Q-W2).

## 10. Forms mode auto-switch, in detail

| Situation | Result |
|---|---|
| Tab or Shift+Tab onto a link or button | Browse, cursor on it, focus speech |
| Tab onto an edit, combo box, list box, slider | Focus{Auto}, "focus mode" + focus speech |
| Click (mouse) into an edit | same, through `FocusChanged` |
| Page script focuses an edit on load (`autofocus`) | Loading → Focus{Auto} at once (§7.3) |
| Enter or Space on an edit in browse | Focus{Auto} + `Action::Focus` |
| Escape in Focus{Auto} | Browse, cursor on the edit, "browse mode" + the line |
| Escape with an expanded combo box | passed to the page (closes the popup); a second Escape returns to Browse |
| `role=application` focus | Focus{Application}; Escape passes through; only gorca+Space leaves |
| Focus moves out of the edit by script to a non-editable | Browse at the new focus |
| Typing in focus mode | ADR-0007 typing echo and caret echo on the focused editable (Firefox UTF-16, CR-5) |

## 11. Live regions (M7b)

The ADR-0003 §13 policy, placed as follows: the pure parts (classifier LR-1/LR-2, dedupe ring,
token buckets with an injected clock, LR-4/LR-5 rules) in `gorca-doc::web::live`; extraction at apply time
in `doc_actor/live.rs` (the translator knows old and new run texts, so inserted text comes from the
mirror diff, never from raw event strings); delivery as `DocNotice::Live { doc, region, text, politeness }`
on the background lane; scheduling in `browse/live.rs` (only the active document, TTLs, priorities,
history, `browse.live_regions_toggle`). Live regions are announced in Browse and Focus modes and while focus
is in the browser chrome of the active document's window. Gate LV1: last mutation → speech start ≤ 65 ms
wall. The source's settle (25 ms quiet, 120 ms cap) already merges Chromium's per-update triad into one
batch, so LR-3's separate 50 ms region settle is dropped (AM-9); a region's message is extracted from the
batch that settled it.

## 12. iframes

- **IF-1** One mirror per tab: nested documents are subtrees under their frame node (`FRAME` flag,
  `Cat::FRAME`, quick-nav M). Navigation, quick nav and lines cross frames in document order.
- **IF-2** Build: whether `GetTree` descends into a nested document in Firefox and Chromium is verified in
  M7a (W-V2); if not, the source treats each nested `DocumentWeb` as a frontier subtree and fetches it as
  its own block on the same connections.
- **IF-3** A nested document's `load-complete` or `reload` re-lists its frame's children (no `Reset`); its
  defunct root removes the subtree.
- **IF-4** Field diff announces "frame ‹name›" / "out of frame" (M7b).
- **IF-5** Content a browser does not expose (closed shadow roots, sandboxed or cross-process content
  that is missing from the tree) is a coverage gap, counted (`frame_unexposed`), never filled by
  guessing.

## 13. Chromium silent reorder: `Revalidate` (BD-7)

Facts: Chromium reorders a keyed list without any event, keeps the objects and paths (TBD-WEB-1, WF-7);
`Revalidate { parent }` (one `GetChildren`) repaired 100 % of the measured cases (WP §1.5).

- **RV-1** `SourceCaps.silent_reorders: bool` (new). `AtspiDocSource` sets it for toolkit Chromium
  (Electron included), not for Gecko (Firefox re-creates moved nodes loudly, WF-8).
- **RV-2 Trigger.** After every browse presentation (line, quick-nav target, table move, say-all chunk
  start) the Reader sends `DocMsg::Presented { node, seq }` to the doc actor (fire and forget). With
  `silent_reorders`, the doc actor issues `Revalidate` for the node's parent and, when the node is
  within 3 levels of a list, list box, table, grid or tree, for that container. Without it, `Revalidate`
  runs only at wake (W1), before an action on a heuristic join (IL-R2), and after an unknown-node delta.
- **RV-3 Dedupe and bounds.** Once per parent per 250 ms, plus once after any applied batch that touched
  the parent; at most 2 in flight per document; a newer request replaces a queued one (the latest
  position matters); deadline 50 ms (Validate class); never on the hot path.
- **RV-4 Result.** The source pushes the difference as ordinary deltas plus `Settled`; the doc actor
  applies and publishes, then compares the presented line's node ids and text hash (sent with
  `Presented`) against the new mirror. A difference → `DocNotice::CursorRefined { seq }`; the Reader
  re-speaks if the utterance is still refinable (same epoch, < 150 ms of audio, ADR-0001 §8.1), otherwise
  the next key simply uses the new order. The cursor stays on its logical node.
- **RV-5** `RowReordered` (Chromium grids) is a `ChildrenChanged` of the rows' parent in the source (AS-4).
- **RV-6** Metric WM-7: Chromium keyed reverse under the cursor's list corrected on the next read, ≤ 1
  revalidation per read; correction ≤ 60 ms wall.

## 14. Failure modes and invariants

| Failure | Behaviour | Counter |
|---|---|---|
| Browser hung during build | focus from the app mirror (IR-3); browse keys say "still loading" | `doc_build_stalled` |
| Browser hung while Live | navigation continues on the mirror; "not responding" once | ADR-0003 §17 |
| Warm-up fails | build proceeds, `attrs_suspect`, rebuild after a later successful warm-up | `warm_failed` |
| Join miss on focus | IR-3; cursor unchanged | `web_join_miss` |
| Doc event queue overflow | source resync (IL-R5) | `doc_event_overflow` |
| Translator meets an inconsistent batch (child listed twice, cycle) | batch rejected before mutation (ops validate first, DC); source `Rebuild` | `doc_batch_rejected` |
| Doc actor panic | supervisor restarts it with a rebuild-as-resync; the Reader restores the cursor from its bookmark | `doc_actor_restart` |
| Very long line | FT-7 checkpoints | `vline_checkpointed` |

- **BI-1** No browse key performs IPC on its path to speech (W1, BR1); scrolling and revalidation are
  fire-and-forget afterwards.
- **BI-2** The Reader never waits for the doc actor; it reads `DocRead` wait-free.
- **BI-3** At most one document per process is awake (DM-I5), apart from M7b pre-warm and prebuild.
- **BI-4** `WebMode` and the doc actor's state machines are enums with exhaustive `match` and pure `step`
  functions, unit-tested before wiring (PR-4).

## 15. Latency gates

All gorca-side p99, median of interleaved repeats, under L3-50k with Δp99 ≤ 1 ms, `lost = 0`,
`stale = 0`, unless marked wall.

| Gate | Stimulus → end | Threshold (ceiling / target) | Slice |
|---|---|---|---|
| **W1** | quick-nav key → SPEAK, 10k and 100k mirrors, zero IPC | 5 ms / 2 ms | M7a |
| **BR1** (new) | browse arrow (char, word, line) → SPEAK, zero IPC | 5 ms / 2 ms | M7a |
| **BR2** (new) | held Down, 30 Hz, 3 s | 0 stale; the last line always spoken | M7a |
| **W2** | 10k-node page `load-complete` → first line speakable (domains warm), wall | 150 ms; cold start reported, not gated | M7a |
| **W2b** (new) | focus enters a loaded, never-visited 10k-node document → first line speakable, wall | 150 ms | M7a |
| **W3** | tab switch → title + cursor line from the dormant snapshot | 15 ms | M7a |
| **FM1** (new) | Tab onto an edit → "focus mode" + focus speech | 15 ms / 5 ms | M7a |
| **FM2** (new) | Escape in Focus{Auto} → "browse mode" + line | 5 ms / 2 ms | M7a |
| **J1** (new) | focus join `WebJoin` + `DocRead` check | 10 µs | M7a |
| **WM-2** | W1 and BR1 during a 10k ev/s storm on the same document | Δp99 ≤ 1 ms | M7a |
| **LV1** | live region last mutation → speech start, wall | 65 ms | M7b |
| **SAB** (new) | browse say-all → first chunk SPEAK; chunk gaps as ADR-0007 SA2 | 5 ms / 2 ms | M7b |
| **RV** | Chromium silent reorder corrected (RV-6) | ≤ 60 ms wall | M9 |
| M-13 | focus/caret join hit rate on the corpus | ≥ 99.9 %, 0 wrong nodes | M7a |

## 16. Slices

### 16.1 M7a: Firefox browse mode on static pages (ships first)

User-testable: open a static page in Firefox; gorca says the title and the first line; Up/Down read
lines, Left/Right characters, Ctrl+Left/Right words, Home/End, Ctrl+Home/End, Page Up/Down; H and 1–6
jump to headings, K to links (Shift reverses); Enter follows a link; Tab moves between links and fields;
Tab into a text field says "focus mode" and typing works; Escape says "browse mode"; switching tabs and
back reads the title and the line where the cursor was.

Scope:
1. T0 of ADR-0007 (`gorca-text`).
2. `gorca-doc`: `DocNodeId` in `gorca-model` (BD-2), `NodeMeta`, flags `BLOCK`, `BREAK_BEFORE`, `FRAME`,
   FT-4…FT-8, `TextSpace` impl; scaffold mirror deleted.
3. `gorca-doc-source-atspi`: text tier + TX-1 runs (FT-1, FT-2), `Settled` per batch, `DocSourceCtx.events`
   input, `SrcNode.local`, `DocSourceFactory` with `needs_warm`/`warm`/`create`, `SetActivity`, `Rebuild`,
   `silent_reorders` cap.
4. `gorca-core`: doc registry and `WarmState` (§5.3, §6), doc actor v1 (§5.2) with the translator (§4.2,
   without TR-4's grace), `WebJoin`, `DocDirectory`, `app_actor/doc_route.rs`.
5. `gorca-core/src/browse/`: `WebMode` with Inactive, Loading, Browse, Focus{Auto, Manual}; `P`; grab sets
   of §7.4; cursor commands of §8.1 (no paragraph/sentence); quick nav H, 1–6, K; `browse.activate`;
   `browse.toggle`; line presenter LP-1, LP-3, LP-4, LP-6; BM-5 spoken indication.
6. `gorca-daemon`: `wire_web` (factory, P2P pools).
7. Tests: fake document source in `gorca-testkit` (scripted deltas, no browser) for every state machine;
   the B-6 harness corpus (`large1k/10k/100k`, a synthetic article, a form page) in the private Firefox for
   W1, BR1, BR2, W2, W2b, W3, FM1, FM2, J1, M-13.

Not in M7a: say-all, live regions, modal scope, container announcements, tables, elements list, find,
the other quick-nav letters (same code path; enabled per category as their tests pass), IL-R4 grace,
W1–W3 wake catch-up, pre-warm, background prebuild, SPA route-swap restore beyond ADR-0003 §10.3 rules 1
and 4, Chromium.

Acceptance: the gates of §15 marked M7a; M-13 ≥ 99.9 % with 0 wrong nodes; live checklist with the user
on a Wikipedia article, a GNOME Help page, an MDN page, a login form and a news front page, side by side
with what NVDA would say for the same keys.

### 16.2 M7b and handoff to M8/M9

M7b: say-all (§9), live regions (§11), modal scope, field diff (LP-2), paragraph and sentence moves,
IL-R4 grace (TR-4), full dormancy (W1–W3, pre-warm, background prebuild), iframes verified (§12), route
swap restore (ADR-0003 §10), B-6 bake-off run. M8: tables, elements list, find, remaining quick nav,
selection and copy, NVDA parity checklist. M9: Chromium (§13 in anger, `ext-chromium`).

## 17. Parallel work with M3 and file ownership

Only T0 (ADR-0007 §20) must land first. After it, M3 and M7a run in parallel:

| Track | Agent | Owns (W) | Touches additively (w) | Depends on |
|---|---|---|---|---|
| M3 (ADR-0007) | rust-engineer A, linux-platform-engineer | `gorca-text`, `gorca-core/src/text/`, `present/text.rs`, `present/echo.rs`, `gorca-speech/verbalize.rs`, `gorca-connector-atspi/src/text/`, `gorca-input-api` (`TextKey`) | `app_actor.rs` `route()` arms, `commands/handlers.rs` + `table.rs` (text rows), `gorca-config` (echo/text keys), `gorca-daemon` `wire_text` | T0 |
| W-A mirror | rust-engineer B (with math-researcher review) | `gorca-doc/src/web/**` (meta, flags, FT-4…FT-8, `TextSpace` impl, bookmark), `gorca-model/src/doc.rs` (`DocNodeId`, `SrcNode.local`), deletion of `gorca-doc/src/mirror.rs` | — | T0 |
| W-B source | web-a11y-engineer | `gorca-doc-source-atspi/**`, `gorca-doc-source-api/**` (ctx, factory, requests, caps) | `crates/gorca-bench-web` (adapter for `DocEvent`) | W-A types (`SrcNode.local`) |
| W-C doc actor | rust-engineer B (after W-A) | `gorca-core/src/doc_actor/**`, `doc_registry.rs`, `app_actor/doc_route.rs`, `gorca-core` `WebJoin` / `DocDirectory` | `app_actor.rs` (one call into `doc_route`), `gorca-daemon` `wire_web` | W-A, W-B API |
| W-D browse | rust-engineer C | `gorca-core/src/browse/**`, `mode.rs` (`Mode::Browse` payload, `web_focus` context flag), `gorca-model` `Role::DocumentWeb` | `commands/handlers.rs` + `table.rs` (browse rows, new `browse.cursor_*`), `gorca-config/presets/*.toml` (browse context), `reader.rs` (`BrowseMsg` arm and `ReaderEffect` variants only), `gorca-connector-atspi/src/role_map.rs` (`DocumentWeb`) | T0, W-A read API; runs against the testkit fake source before W-C lands |
| W-E tests | test-engineer, perf-engineer | `gorca-testkit/src/fake_doc_source.rs`, `bench/e2e` W1/BR1/BR2/W2/W2b/W3/FM1/FM2/J1, `tests/web/b6` scenarios | — | W-B, W-C |

Conflict rules:
- **PF-1** `app_actor.rs`: M3 edits only `route()` arms; W-C adds `app_actor/doc_route.rs` and one call in
  the pump loop. No shared lines.
- **PF-2** `commands/table.rs`, `handlers.rs`: additive rows; M3 owns `text.*`, `info.current_line`,
  `info.selection`, `info.formatting`, `echo.*`, `speech.punctuation_cycle`; W-D owns `browse.*`,
  `quicknav.*`, `table.*`. The orchestrator merges M3 first when both are ready.
- **PF-3** `reader.rs`: each track adds only its message arm and effect variants (P0 seam rule).
- **PF-4** `gorca-model`: M3 adds text types and `Utterance.sensitive`; W-A changes `doc.rs`; W-D adds
  `Role::DocumentWeb`. Different files except `lib.rs` re-exports.
- **PF-5** Browse char and word speech use ADR-0007's verbaliser through the same `present/text.rs`
  functions; until M3a merges, W-D calls the current minimal `Verbalizer` (no behaviour switch needed when
  M3 lands).

## 18. Amendments made by this document

- **ADR-0003**: §21 (AM-1…AM-13) and inline markers in §3, §4.1, §4.3, §6.1, §6.2, §9, §11.
- **ADR-0001 §9.3** (`DocNodeId` "doc-local u32") and **§9.2** (`DocSourceCtx`, `SrcNode`): by ADR-0003
  AM-2, AM-3.
- **ADR-0005 §3.2**: new commands `browse.cursor_next/prev_line`, `_char`, `_word`, `line_start`,
  `line_end`, `top`, `bottom`, `page_up`, `page_down` (M7a), `_paragraph`, `_sentence` (M7b), bound in the
  Browse context to the keys of modes.md §4.4.
- **`docs/design/modes.md`** §1: `Browse { doc, sticky }` (no cursor); §2: Escape in the Focus grab set
  only when `web_focus = Auto` and the node is not expanded; §4.4 status column per §16. To be applied
  with the W-D code.
- **ADR-0006 §3**: `ReviewFrom::Browse { doc, sticky }`.

## 19. Verification items

- **W-V1** (web-a11y-engineer) Firefox Fission: does a content process created after the warm-up need its
  own warm-up (tags missing on its first build)?
- **W-V2** (web-a11y-engineer) Does `GetTree` cross into nested documents (same-site and cross-site
  iframes) in Firefox and Chromium?
- **W-V3** (web-a11y-engineer) Text-tier cost per block and the role list that carries no text; first
  block latency with text on (10k, 100k).
- **W-V4** (web-a11y-engineer) Firefox `display` attribute coverage for FT-4 (inline-block, contents,
  flex children) on the corpus; compare the line splits with NVDA's for the same pages (screen layout on).
- **W-V5** (perf-engineer) LP-3 cost of the longest corpus line inside one guard.

## 20. Questions for the user

| # | Question | Options | Recommendation |
|---|---|---|---|
| Q-W1 | How should gorca signal browse ↔ focus mode? | (a) spoken words "browse mode" / "focus mode"; (b) short beeps like NVDA (needs earcon playback in voicego, not built yet); (c) both | **(a) now**, (b) as an option once voicego can play earcons. Medium confidence: NVDA users are used to beeps. |
| Q-W2 | Start reading the page automatically when it loads (NVDA default on)? | on / off | **On** (NVDA parity), toggle `browse.say_all_on_load`; lands in M7b with say-all. |
