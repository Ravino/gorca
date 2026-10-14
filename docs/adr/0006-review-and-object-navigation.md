# ADR-0006: Review cursor, object navigation and flat review

Status: **Accepted** (2026-10-14). Owner: architect. Milestone: M4 (`docs/plan/phase-2.md` §M4), first
slice **M4a** (§15).

Inputs: `CLAUDE.md`; ADR-0001 §3.4 (epoch), §4.2 (snapshot), §4.4 (freshness), §5.1 (connector
contract), §6.4 (deadlines, health), §7.1–7.2 (Focus, Mode), §8.1 (present-or-fetch, RV-1);
ADR-0005 (commands, `Availability`, K-1, K-6a, K-8, KC-5, CR-4 multi-press); `docs/design/modes.md`
(modes, §4.3 review keymap, §5 Orca preset); B-1 SPEC §4 (mirror API) and §5 (invalidation);
`crates/gorca-mirror`, `crates/gorca-core/src/{mode.rs,review/,commands/table.rs}`,
`crates/gorca-config/presets/*.toml`.

Numbering note: `docs/plan/phase-2.md` reserved ADR-0006 for "text, caret and review presentation".
Review is split out here because the user asked for it first; the M3 text/caret ADR takes the next
free number (ADR-0007). This ADR defines the seams M3 plugs into (§10).

## 0. Changelog

- 2026-10-14: first version.
- 2026-10-14: M4a implementation amendments, from the platform checks
  (`docs/reports/2026-10-14-adr0006-platform-checks.md`) and the build: F-10 corrected, SF-1 narrowed,
  KM-3 rewritten (V-1 refuted), V-2..V-5 recorded, VD-3 outranks VD-2, replies matched by request id,
  the `Children` fetch primitive, text lines asked of text roles when interfaces are unknown (§20).

## 1. Context

The user ran gorca live and asked for flat review and object navigation. Without them a blind user
cannot read anything that is not focusable: dialog body text, labels next to switches in Settings,
the status bar, Shell panels. Focus tracking (Phase 1), the command layer (M1) and the native mirror
(M2 prework on `main`) are in place, so review can be built on the mirror with no per-key IPC.

### 1.1 Platform facts (verified in `refs/`)

| # | Fact | Source |
|---|---|---|
| F-1 | GTK4 `Component.GetExtents` with `SCREEN` returns origin `(0,0)`: GTK4 has no global coordinates. `WINDOW` and `PARENT` work. | `refs/gtk/gtk/a11y/gtkatspiutils.c:437-442`, `gtkatspicomponent.c:140-170` |
| F-2 | GTK4 `Text.GetRangeExtents` / `GetCharacterExtents` accept only `WINDOW` and `PARENT` (`NotSupported` otherwise). | `refs/gtk/gtk/a11y/gtkatspitext.c:286-300, 440-500` |
| F-3 | GTK4 editables (`GtkEntry`, `GtkSpinButton`, …) answer `GetRangeExtents`, `GetCharacterExtents` and `GetBoundedRanges` with `NotSupported`. | `gtkatspitext.c:904-915` (`editable_handle_method`) |
| F-4 | GTK4 `Component.GrabFocus` and `ScrollTo*` return `NotSupported`. | `gtkatspicomponent.c:189-192, 214-221` |
| F-5 | St/Clutter (`gnome-shell`) ignores the coordinate type and returns stage coordinates. Shell is the compositor, so they are global. | `refs/mutter/clutter/clutter/clutter-actor-accessible.c:449-482` |
| F-6 | St text implements `get_character_extents` and `get_offset_at_point`; range extents fall back to ATK's per-character union (one D-Bus call, O(len) inside Shell). | `refs/mutter/clutter/clutter/pango/clutter-text-accessible.c:1050, 1418` |
| F-7 | The mirror never holds `bounds` today: the decoder sets `bounds: None`, `PropMask::PRESENT` excludes `BOUNDS`, and B-1 §5.1 ignores `BOUNDS` events. | `crates/gorca-connector-atspi/src/decode.rs:158`, `crates/gorca-model/src/node.rs:85-99`, B-1 SPEC §5.1 |
| F-8 | The Reader can read any app's mirror lock-free through `Directory` (`AppId → MirrorHandle`); `MirrorRead::view` never blocks or allocates on a hit; `revision()` is O(1). | `crates/gorca-core/src/directory.rs`, `crates/gorca-mirror/src/store/mod.rs:499-566` |
| F-9 | GTK4 lazy mirrors hold realised nodes only; `ChildList` may be `Count(n)` or `Unknown` (GetItems carries the count only). | B-1 SPEC §2.2 (P-9) |
| F-10 | **Corrected 2026-10-14 (V-2).** A GTK4 popover is parent-window-relative in `WINDOW` coordinates, not relative to its own surface, and `AdwAlertDialog` is an in-window child. Only a real second toplevel (a GTK3 menu window, a dialog window) has its own origin. | `docs/reports/2026-10-14-adr0006-platform-checks.md` |

### 1.2 Prior art and its failures

| System | What it does | What we take / avoid |
|---|---|---|
| NVDA object navigation (`refs/nvda/source/globalCommands.py:1738-1860, 4616-4680`; `NVDAObjects/__init__.py:930-1050`) | Navigator object separate from focus; `simpleReviewMode` skips layout objects (`presentationType`: layout roles, and nameless panels/groupings/windows); next-in-flow is a pre-order walk; "No next", "No containing object". | **Take** the semantics and messages. Its `simpleNext` walk has no bound (it recurses through any layout depth, with a COM call per step); ours is bounded (OB-2). |
| NVDA review modes (`globalCommands.py:1455-1500`) | object / document / screen review, cycled by NVDA+numpad 7/1; screen review uses the Windows display model. | **Take** the kinds and the cycle. There is no display model on Linux; "screen" is built from accessibility geometry (Orca's approach). |
| NVDA status bar (`refs/nvda/source/api.py:438-475`) | app module override, then the object at the bottom-left of the foreground window, then the bottom line of the display. | **Take** the cascade, replacing the point hit-test (no global coordinates) with a role search. |
| Orca flat review (`refs/orca/src/orca/flat_review.py:636-960`) | On every flat-review start: walks the window synchronously (`get_on_screen_objects`), fetches `GetExtents(WINDOW)` and visible text lines per object, clusters zones into lines by vertical overlap and midpoint distance (5 px), restricts to a menu when inside one. | **Take** the zone model, `WINDOW` coordinates and clustering rule. **Avoid**: synchronous IPC on the UI thread (hundreds of blocking calls; a slow app freezes Orca); rebuilt from scratch each time; greedy clustering against the last zone of a cluster drifts on staggered layouts; word and character extents are faked (`flat_review.py:292-296`). |
| Orca object navigator (`refs/orca/src/orca/object_navigator.py:90-160`) | "Simplify" excludes layout-only objects, includes paragraphs; children of excluded objects are spliced into the parent. | Same idea as NVDA; we use one rule (OB-1) for both presets. |
| Orca status bar (`ax_utilities.py:475-484`, `ax_utilities_collection.py:854-860`) | `Collection` match on `STATUS_BAR` + showing/visible, else a descendant walk. | **Take** the `Collection` call as the miss path. |
| Odilia | No review or object navigation implemented. | Nothing to take. |

## 2. Decision (summary)

1. **One review cursor, two positions.** `ReviewCursor` holds the **navigator** (a `NodeKey`) and a
   **position** whose kind is `Object` or `Screen` (`Document` joins in M7). Object commands move
   the navigator; line/word/character commands move the position. Each one follows the other (§3).
2. **Object navigation reads the mirror only.** On a miss it issues at most one bounded Interactive
   fetch per suspension, and a step suspends at most **3** times (OB-3). Simple review (NVDA
   `presentationType`) is the default.
3. **Flat review is a cached, per-surface line model**, built from mirror snapshots plus `bounds`
   (`Component.GetExtents(WINDOW)`) and visible text lines (`TextQuery::VisibleLines`). It is built
   in the app actor and clustered on the `cpu` pool, then handed to the Reader as an immutable
   `Arc<LineModel>`. A step on a valid model is an index move: **zero IPC, R1 ≤ 3 ms**.
4. **The model is checked on every step without messages**: an O(1) mirror revision fast path, then
   an O(zones) content-hash check. A changed zone triggers an incremental rebuild of the dirty
   nodes only; geometry that changes without events (scroll, resize) is caught by **RR-1**, a
   two-call revalidation after any step on a model older than 2 s.
5. **Coordinates never leave a surface.** One model per surface root (window, dialog, popover,
   menu). Coordinates are compared only inside one model. `SCREEN` is never requested (F-1).
6. **Missing extents degrade to tree order**, never to a missing object (§6.7).
7. **Review never blocks the Reader** (R3). Pending fetches are a state with a bounded follow-up
   queue; the epoch cancels stale results (§4.3).
8. **M3 plugs in through two seams**: `TextUnits` (word/char segmentation and spelling) and
   `TextQuery::AtOffset` (object review over a whole text, caret follow). **M6** (terminal) and
   **M7** (document) plug in as additional `LineSource`s behind the same cursor (§10).
9. The keymap of `docs/design/modes.md` §4.3 and §5 is kept as is. Two Orca commands are added
   (`review.above`, `review.below`). Activation of each binding follows the M4a/M4b split (§11).

## 3. Types (interface sketch, `gorca-core/src/mode.rs` and `review/`)

```rust
// mode.rs — Mode keeps its shape; the Review payload becomes concrete. All types are Copy.
pub enum Mode {
    Focus,
    Browse { doc: DocId, cursor: DocCursor, sticky: bool },
    Review { origin: ReviewOrigin, cursor: ReviewCursor },
    PassThrough { scope: PassScope, resume: BaseMode },
    InputHelp { resume: BaseMode },
}
pub struct ReviewOrigin {
    pub node: NodeKey,           // focus node (or browse cursor node) when review started
    pub from: ReviewFrom,        // where ExitReview returns (modes.md §3 "*new*")
}
pub enum ReviewFrom { Focus, Browse { doc: DocId, cursor: DocCursor, sticky: bool } }

pub struct ReviewCursor {
    /// NVDA's navigator object. Always set, may read `Gone` (§13).
    pub nav: NodeKey,
    pub pos: ReviewPos,
}
pub enum ReviewKind { Object, Screen /* Document: M7 */ }
pub enum ReviewPos {
    /// Object review. M4a: the navigator's presentation text (NVDA NVDAObjectTextInfo).
    /// M4b: the whole `Text` of the navigator when it has one (needs M3, §10).
    Object { offset: u32, text_line: Option<u32> },
    /// Screen (flat) review over a line model. `None` = not yet anchored (model pending,
    /// or the navigator is not in the model); the next screen command anchors it.
    Screen(Option<FlatPos>),
}
pub struct FlatPos {
    pub model: ModelId,          // u64, unique per build (never reused in a process)
    pub line: u32,
    pub zone: u16,
    pub offset: u32,             // Unicode scalar offset inside the zone text (TU-1)
    /// Re-anchoring key used when a newer model replaces `model` (§6.6).
    pub anchor: ZoneAnchor,
}
pub struct ZoneAnchor { pub node: NodeKey, pub src_start: u32, pub y_mid: i32 }
```

```rust
// review/model.rs — immutable once published; owned by the Reader through Arc.
pub struct LineModel {
    pub id: ModelId,
    pub app: AppId,
    pub surface: NodeKey,                 // the surface root (§6.2)
    pub source: LineSource,               // Geometry (M4) | Terminal (M6) | Document (M7)
    pub lines: Box<[Line]>,               // top to bottom
    pub zones: Box<[Zone]>,               // lines index into this, left to right
    pub by_node: HashMap<NodeKey, SmallVec<[u32; 2]>>,  // node -> zone indices
    pub built_rev: u64,                   // MirrorRead::revision() at build start
    pub built_at: Instant,
    pub validated_at: Instant,            // last RR-1 or rebuild
    pub completeness: Completeness,       // Complete | Truncated { why } | GeometryFallback
}
pub struct Line { pub zones: Range<u32>, pub top: i32, pub bottom: i32 }
pub struct Zone {
    pub node: NodeKey,
    pub kind: ZoneKind,                   // Text { src: Range<u32> } | Object | Value | State
    pub text: Arc<str>,                   // what is spoken for this zone
    pub rect: Option<Rect>,               // surface-relative; None = synthetic placement (§6.7)
    pub content_hash: u64,                // over the snapshot fields the zone used (§6.5)
}
pub enum LineSource { Geometry, Terminal, Document }
```

```rust
// review/mod.rs — Reader message arm (seam already exists, P0).
pub enum ReviewMsg {
    /// App actor -> Reader (reply lane): a model build or incremental refresh finished.
    Built { build: BuildId, epoch: Epoch, result: Result<Arc<LineModel>, BuildError> },
    /// App actor -> Reader: a navigation fetch was applied to the mirror (or failed).
    NavFetched { req: NavReqId, epoch: Epoch, result: Result<(), NavFetchError> },
    /// App actor -> Reader: RR-1 revalidation result for `model`.
    Revalidated { model: ModelId, verdict: RrVerdict },  // Same | Moved | ContentChanged{lines}
    /// Status-bar `Collection` reply.
    StatusBarFound { req: NavReqId, epoch: Epoch, node: Option<NodeKey> },
}

// reader.rs — new ReaderEffect variants (routed to the node's app actor; never awaited).
pub enum ReaderEffect {
    // ... existing ...
    ReviewBuild { app: AppId, surface: NodeKey, anchor: Option<NodeKey>, build: BuildId,
                  epoch: Epoch, mode: BuildMode /* Full | Refresh { dirty: Arc<[NodeKey]> } */ },
    ReviewNav { app: AppId, req: NavReqId, epoch: Epoch, op: NavOp },
    ReviewRevalidate { app: AppId, model: ModelId, probe: SmallVec<[NodeKey; 8]> },
}
pub enum NavOp {
    Children { node: NodeKey },              // -> FillOp::ChildSnapshots { want: NAV_MASK }
    Ancestry { node: NodeKey },              // -> FillOp::Ancestry { max_depth: 16, want: NAV_MASK }
    Node { node: NodeKey },                  // -> FillOp::Fetch { want: NAV_MASK | PARENT }
    FindRole { root: NodeKey, role: Role },  // -> BackendOp::Collection (status bar, §8)
}
/// What object navigation needs to classify and speak a node.
pub const NAV_MASK: PropMask = PRESENT | CHILDREN | PARENT;
```

```rust
// gorca-model text.rs — one new query (connector-side, M4a). M3 adds AtOffset (§10).
pub enum TextQuery {
    GetText { node: NodeKey, range: Range<u32> },
    CaretOffset { node: NodeKey },
    Attributes { node: NodeKey, range: Range<u32> },
    /// Lines of `node` that intersect `clip` (surface coordinates), top to bottom.
    VisibleLines { node: NodeKey, clip: Rect, max_lines: u16, max_bytes: u32 },
}
pub struct TextLine { pub range: Range<u32> /* scalar offsets */, pub text: Arc<str>, pub rect: Option<Rect> }
```

## 4. Mode and review state machine

### 4.1 Mode transitions (adds to `docs/design/modes.md` §3; names as in `ModeMsg`)

| From | Message | To | Effects |
|---|---|---|---|
| Focus | `EnterReview{origin, cursor}`: any `review.*` or `object.*` command (NVDA), or `review.toggle` (Orca preset) | Review | epoch advance; grab set unchanged (Review = Focus set, modes.md §2); the command then runs on the new cursor |
| Browse (M7) | `EnterReview` | Review{from: Browse{..}} | origin = browse cursor node |
| Review | `ExitReview` (`review.to_focus`, `review.toggle`) | `from`: Focus, or Browse with its saved cursor | pending review op cancelled (§4.3) |
| Review | `FocusChanged` with `review.focus_follows = true` (default) | Focus | the next review command starts again from the new focus |
| Review | `FocusChanged` with `review.focus_follows = false` | unchanged | the focus is still tracked and spoken (MD-3) |
| Review | `ReviewSurfaceGone` *new* (the model's surface or the navigator's app is `Gone`) | Focus | speak nothing extra; focus speech follows |
| Review | any review command | Review | the cursor is updated by §4.2 |
| Review | `StartPassThrough`, `ToggleInputHelp` | as modes.md (resume = Review) | |

Initial cursor on `EnterReview`: `nav = focus` (or the active window if there is no focus);
`pos = Screen(None)` when `review.default_kind = "screen"` (default), `Object{offset: 0, ..}`
otherwise. A `Screen(None)` anchors at the navigator's first zone once a model exists (§6.6).

Default kind: **screen**, because the user asked for flat review of the window and Linux has no
display model, so NVDA's object-review default would only re-read the focused control's name.
NVDA users can set `review.default_kind = "object"`.

### 4.2 How the two positions follow each other

- **RC-1** An `object.*` move sets `nav`. In `Screen` kind it re-anchors `pos` to the first zone of
  `nav` in the current model (NVDA screen review, Orca `set_current_to_zone_with_object`). If `nav`
  has no zone (not showing, or pruned), the zone of its nearest showing ancestor is used, then that
  of its first showing descendant (Orca `_find_zone_with_object`). If none exists, `pos =
  Screen(None)`.
- **RC-2** A line/word/char move in `Screen` kind sets `nav = zone.node`, so `object.current`,
  `review.activate` and `review.move_focus` act on what was just read.
- **RC-3** `review.mode_next/prev` switches kind and re-anchors at `nav` (NVDA "positions the
  review position at the point of the navigator object"), then speaks the current line.

### 4.3 Step execution and pending fetches (no blocking, bounded queue)

```rust
enum ReviewOpState {
    Idle,
    /// A step is suspended on a fetch or a model build.
    Pending { what: PendingWhat, epoch: Epoch, deadline: Instant,
              cont: StepCont,                          // resumable walk state (§5.3)
              queued: ArrayVec<QueuedCmd, 8> },        // commands pressed meanwhile
}
enum PendingWhat { Nav(NavReqId), Build(BuildId), StatusBar(NavReqId) }
```

| From | Event | To | Effects |
|---|---|---|---|
| Idle | review command, all data in mirror/model | Idle | speak (`Interrupt`, new epoch) |
| Idle | review command, miss | Pending{..} | `ReviewNav`/`ReviewBuild` effect; deadline = now + Interactive (120 ms); no speech yet |
| Pending | another cursor-relative command (`object.*`, line/word/char) | Pending (queued) | appended; at most 8; the 9th replaces the newest queued entry and is counted `review_queue_overflow` |
| Pending | a command that does not depend on the cursor (`review.to_focus`, `info.status_bar`, `review.toggle`) or `FocusChanged` with follow on | Idle | pending result discarded by epoch (E-3); the new command runs |
| Pending | `NavFetched`/`Built` with matching epoch | Idle, or Pending again (the walk suspends again, OB-3) | resume `cont`; then run `queued` in order against the updated cursor; **only the last command speaks** (intermediate results are superseded, ADR-0001 R-1) |
| Pending | deadline or `Err(Timeout | Hung)` | Idle | speak the result of the **degraded** step (OB-4) or "not responding"; drop `queued`, count `review_step_timeout` |
| Pending | reply with an older epoch | unchanged | dropped |

Invariants: **RS-1** at most one review op is pending per Reader. **RS-2** a pending op never holds a
lock or awaits on the Reader task; it is plain state. **RS-3** every review command advances the
epoch when it speaks, so held keys (autorepeat) cut the previous utterance like arrows do.

## 5. Object navigation

### 5.1 Presentation classes (simple review, default on: `review.simple = true`)

`fn class(v: &NodeView) -> PresClass` (pure, `review/objnav.rs`), NVDA `presentationType`
(`NVDAObjects/__init__.py:930-978`) on gorca roles:

- `Unavailable`: states lack `SHOWING` or `VISIBLE`, or the node is `Defunct`.
- `Layout`: role in {`Unknown`, `Filler`, `Panel`, `ScrollPane`, `SplitPane`, `Section`,
  `Paragraph`, `Label`, `Separator`/whitespace, `RootPane`, `LayeredPane`, `Viewport`,
  `TitleBar`}; or role in {`Window`, `Panel`, `Grouping`, `Form`, `Region`, `InternalFrame`,
  `OptionPane`, `TableBody`} **and** name and description are empty or whitespace. NVDA treats
  `Paragraph` and `Label` as layout; we do the same, so their text is still reached through flat
  review and through the parent's content.
- `Content`: everything else.

`review.simple = false` gives the raw tree (`parent`, `children`, siblings). This is unbound by
default and available as `review.simple_toggle` (new `CoreCmd`, unbound in both presets).

### 5.2 Moves

| Command | Raw | Simple (NVDA `_findSimpleNext`, `simpleParent`, `simpleFirstChild`) | Empty result speaks |
|---|---|---|---|
| `object.parent` | `parent` | nearest ancestor of class `Content` | "No containing object" |
| `object.first_child` | `children[0]` | first `Content` in pre-order under `nav`, descending through `Layout` only | "No objects inside" |
| `object.next` / `object.prev` | next / previous sibling | next `Content` sibling, descending into `Layout` siblings, climbing through `Layout` parents | "No next" / "No previous" |
| `object.next_in_flow` | pre-order successor | NVDA `nextInFlow`: simple first child, else simple next, else the simple next of the nearest ancestor that has one | "No next" |
| `object.prev_in_flow` | pre-order predecessor | simple previous, then its deepest simple last child; else simple parent | "No previous" |
| `object.current` (×2 spell) | | | speaks `nav` in focus-like verbosity (role, name, value, states, description) |

The walk never leaves the navigator's app and never climbs above the app root (an app's tree is one
actor's data). Moving across apps is a later concern (desktop object, M4b+).

### 5.3 Mirror hit and miss

A walk step reads `view(k)` for each node it touches. The need is `NAV_MASK`
(`PRESENT ∪ CHILDREN ∪ PARENT`), and the **children list must be `Known`**.

| Read | Treated as | Action |
|---|---|---|
| hit, `stale ∩ NAV_MASK = ∅`, children `Known` | hit | continue |
| stale or partial in `DESCRIPTION`/`VALUE`/`STATES` only | hit, tagged `may_refine` | continue; one RV-1 `Fetch{PRESENT}` for the node finally spoken (ADR-0001 §8.1) |
| stale or partial in `NAME`/`ROLE`/`CHILDREN`/`PARENT`, children `Count`/`Unknown`, `Miss` | miss | suspend: `NavOp::Children{parent}` for sibling and child moves, `Ancestry` for parent moves, `Node` otherwise; resume on `NavFetched` |
| `Gone` | dead | skip this node and continue the walk from its last known parent; if `nav` itself is `Gone`, start from the nearest known live ancestor, else from the surface root, and speak the result as usual (§13) |

Bounds on one step:

- **OB-1** A step visits at most **512** nodes (raw or simple). On overflow it stops at the last
  `Content` node seen, or says "No next" if there is none, and counts `objnav_visit_cap`.
- **OB-2** `Layout` descent depth is at most 32.
- **OB-3** A step suspends at most **3** times (three sequential round trips), all within one
  Interactive deadline of 120 ms. `ChildSnapshots` asks for the whole child range (capped at 256
  children per call; a 10 000-row list is paged by range around the navigator's index).
- **OB-4** When a step runs out of suspensions or time, it **degrades to a raw move** (raw
  neighbours are always known after one `Children` fetch of the parent). It speaks that node with
  `may_refine` and counts `objnav_degraded`. A step never speaks nothing.
- **OB-5** Fetched nodes go into the mirror through the shared driver (B-1 §4.5, LR rules apply), so
  the next step over the same region is a hit. Review never keeps a private copy of the tree.

The app actor reports each read as a fill-policy `Demand { why: DemandWhy::NavStep, outcome }`
(B-1 §4.4), so the winning B-1 arm can prefetch siblings for review as it does for focus.

## 6. Flat review (`ReviewKind::Screen`)

### 6.1 Pipeline

```
Reader (review command, model missing/invalid)
   └─ ReviewBuild{surface, anchor} ─► app actor: GeomBuild state machine (review/build.rs)
         1 walk the mirror under `surface`, pruning !SHOWING|!VISIBLE subtrees; collect misses
         2 ChildSnapshots/Fetch with want ∋ BOUNDS for misses (≤ 8 in flight for review)
         3 VisibleLines for text nodes that intersect the surface rect
         4 hand (snapshots, lines) to the cpu pool: zones → cluster → LineModel  (pure fn)
   ◄─ ReviewMsg::Built(Arc<LineModel>)  (reply lane)
Reader: install model, anchor, speak
```

`fn build_line_model(input: &BuildInput, limits: &ReviewLimits) -> LineModel` is **pure**. It takes
owned snapshots and text lines, has no I/O and no clock, and is property-tested (§18).

### 6.2 Surface root (window-relative coordinates)

The model root is the **surface** that contains `nav`: the nearest ancestor (or self) whose role is
in {`Frame`, `Window`, `Dialog`, `Alert`, `FileChooser`, `Popover`/`Menu` with a popup parent,
`ToolTip`}, or the app root for Shell.

- **SF-1** Only one surface's coordinates are compared. A subtree that is itself a surface (a popover,
  a menu, a nested dialog) is **excluded** from its parent's model and gets its own. Since F-10 was
  corrected this is a choice of reading scope, not of coordinates: a GTK4 popover shares its parent window's
  coordinates, and keeping it out stops two overlapping layers from interleaving on the same lines.
- **SF-2** If `nav` is inside a menu or popover that is showing, that menu is the surface (Orca
  `flat_review.py:661-664`).
- **SF-3** Shell (F-5) uses stage coordinates. Its surfaces are the top-level St containers the
  focus is in (panel, overview, a modal dialog, notification banner, quick settings), found by the
  same role rule. `ext-gnome-shell` may name the surface explicitly through a later hook.
- **SF-4** Requests always use `CoordType::WINDOW` (`Component.GetExtents(1)`,
  `Text.GetRangeExtents(.., 1)`); `SCREEN` is never sent.

### 6.3 Zones (what each node contributes)

Walk in pre-order under the surface, skipping `Unavailable` subtrees and child surfaces (SF-1). A
node contributes:

1. **Text zones**, if it has `Text` with presentable text. One zone per visible line from
   `VisibleLines`, split at U+FFFC embedded-object characters (Orca `_split_text_into_zones`).
   Single-line editables, and every GTK4 editable (F-3), give **one** zone: the whole text at the
   node's `bounds`.
2. Otherwise, if it is `Content` (or focusable, or has an action) and **no descendant contributes**,
   one **object zone**: `present::flat_zone_text(snap)`. This is the name (or value), plus a short
   role or state word for controls, for example "Wi-Fi switch on", "Apply button",
   "Volume slider 40 %". The same verbosity rules as focus speech apply (ADR-0005 verbosity).
3. A node with a name **and** contributing descendants contributes its name as its own zone only if
   that name differs from the joined text of its descendants and is not computed from content (Orca
   `ax_utilities.py:1710-1720`).
4. Label elision: a `Label` zone is dropped when it labels a node in the same model
   (`relations.labelled_by` if `have ∋ RELATIONS`, else exact text equality with the labelled
   control's name on the same line).
5. Zero-size zones are dropped unless the node is focusable.

Each zone stores `content_hash = hash(role, states & REVIEW_STATES, name, value, bounds, text
range+text)` (§6.5).

### 6.4 Clustering into lines

- **CL-1** Zones with a rect are sorted by `(y_mid, x)`. A zone joins the current line if it
  vertically overlaps the line's **anchor** (its first zone) and `|y_mid − anchor.y_mid| ≤
  max(5, 0.25 × min(h, anchor.h))` px. Comparing against the anchor, not against the last zone,
  avoids Orca's drift on staggered layouts. A scroll bar only shares a line with itself (Orca
  `on_same_line`).
- **CL-2** Within a line, zones are ordered by `x`, or by descending `x` when the surface's text
  direction is RTL (`attrs` `direction`; LTR by default).
- **CL-3** Text zones of the same node keep their own source order even if their extents are
  inconsistent (one wrapped paragraph is never shuffled).
- **CL-4** The line text is the zone texts joined by a single space. A line with only whitespace is
  spoken as "blank".

### 6.5 Validity check on every step (no messages)

```
fn validity(model, mirror) -> Validity:
  if mirror.revision() == model.built_rev  → Valid            // O(1) fast path
  for each zone z (≤ ZONES_MAX):                             // O(zones) lock-free reads
     match mirror.view(z.node):
        Gone                         → dirty(z.node)
        Hit(v) if v.stale ∩ ZONE_MASK ≠ ∅ → dirty(z.node)
        Hit(v) if hash_of(v, z) ≠ z.content_hash → dirty(z.node)
        Miss                         → keep (eviction is not change)
  if no dirty → Valid (record the new rev as checked, no rebuild)
  else → Dirty(dirty set)
```

`ZONE_MASK = NAME | VALUE | ROLE | STATES | BOUNDS | CHILDREN`. A text change on a node arrives as
`TextChanged`, which invalidates `NAME|VALUE` (B-1 §5.1), so text zones are caught too. Measured
cost target: ≤ 150 µs for 2 048 zones (about 50 ns per `view`), well inside R1.

On `Dirty`:

- **VD-1** If no dirty node lies on the **target line** of the step, speak from the current model and
  send `ReviewBuild{mode: Refresh{dirty}}` with class Prefetch. The refresh refetches only the dirty
  nodes (snapshot + `bounds` + text lines) and re-clusters.
- **VD-2** If a dirty node lies on the target line, suspend (§4.3) on the refresh with the
  Interactive deadline. On timeout, speak from the old model with `may_refine` and count
  `review_stale_spoken`.
- **VD-3** If `ChildrenChanged` hit the surface itself or more than 25 % of the zones are dirty, the
  refresh becomes a full rebuild (the same pipeline). **VD-3 outranks VD-2** (§13, app floods): when most of
  the surface changed the step speaks from the old model and the rebuild runs in the background.

### 6.6 Geometry without events: RR-1, and re-anchoring

Toolkits do not reliably emit `bounds-changed` (resize, scrolling a GTK4 list without row
recycling). B-1 §5.1 is amended so that `PropertiesChanged{BOUNDS}`, when it does arrive,
invalidates `BOUNDS` (§17).

- **RR-1** After any step on a model whose `validated_at` is older than `review.geometry_ttl`
  (2 s), the Reader sends `ReviewRevalidate{probe}` (Prefetch class). The probe is the surface
  root, the nodes of the line just spoken (at most 8) and, for text zones, `VisibleLines` of that
  node. The verdict is `Same` (bump `validated_at`), `Moved` (rects changed: full rebuild in the
  background, no speech), or `ContentChanged{lines}`. If the spoken line's text changed, a short
  follow-up is spoken with the new text, as an RV-1 correction (ADR-0001 §8.1), and counted in
  M-6.
- **RA-1** When a model replaces another, the cursor re-anchors by `ZoneAnchor`: the same `node` and
  the zone whose `src` range contains `src_start`; else the same `node`'s first zone; else the line
  whose `y_mid` is closest to `anchor.y_mid`. The cursor never jumps to the top silently. If it
  cannot re-anchor, the next step says "review position reset" and starts at `nav`'s zone.

### 6.7 Missing or bogus extents

A rect is **missing** if `GetExtents` fails or times out, or returns width or height ≤ 0, or
`(0,0,0,0)`, or lies entirely outside the surface rect, or the node is a GTK4 node whose surface
rect is itself missing.

- **GX-1** A zone with a missing rect is placed in tree order: it goes on **its own line**, directly
  after the line of the nearest preceding zone (pre-order) that has a rect, or at the top if there
  is none. Its `rect = None`. It is never dropped: an object without geometry is still reviewable.
- **GX-2** If more than 50 % of the zones of a surface have no rect, the whole model switches to
  `Completeness::GeometryFallback`: one line per zone in pre-order (object-per-line). On the first
  build, "layout unavailable" is spoken once per surface.
- **GX-3** Text lines with no rect from `VisibleLines` keep the node's rect and their own order
  (CL-3).
- Counters: `review_geom_missing{toolkit}`, `review_geom_fallback`.

### 6.8 Budgets and bounds (`ReviewLimits`, hostile input)

| Bound | Value | On overflow |
|---|---|---|
| nodes visited per build | 4 096 | stop the walk; `Truncated{NodeCap}`; the lines built so far are kept; "more content not shown" is spoken once at the bottom |
| zones per model | 2 048 | same |
| `VisibleLines` per text node | `max_lines` 200, `max_bytes` 64 KiB | connector truncates and flags it |
| text per model | 512 KiB | stop adding text zones (`Truncated{TextCap}`) |
| review fetches in flight per app | 8 (within the app's `MAX_IN_FLIGHT`, ADR-0001 §6.2) | queued in the build state machine |
| models kept | 4 per app (LRU by surface), 32 in total, 16 MiB total | evict LRU; the Reader keeps only the model of the current cursor plus the previous one |
| build deadline | Interactive (120 ms) for the first build triggered by a key, then Prefetch (300 ms) for each continuation | a deadline publishes what is ready (`Truncated{Deadline}`) and continues in the background; the Reader swaps the model in with RA-1 |
| app health `Slow`/`Hung` | Prefetch and Background are rejected (ADR-0001 §6.4) | build from the mirror only, no `bounds` → GX-2 fallback, "not responding" spoken once |

Line and word moves never trigger IPC on a valid model (**R1**). The cold first build has its own
gate, R2 (§18).

### 6.9 Words and characters inside a line

The unit iterators are a `TextUnits` trait (`review/segment.rs`). M4a provides a minimal
implementation: a word is a maximal run of non-whitespace inside a zone; a character is one
extended grapheme cluster (`unicode-segmentation`, a new workspace dependency).
M3 replaces it with the shared segmentation and verbalizer (§10). A move past the end of a zone goes
to the next zone of the same line; past the end of a line it goes to the next line (NVDA wraps
word and character review across lines and says nothing extra; Orca says the line).

Boundary messages (NVDA wording): "Top" and "Bottom" for lines, words and characters at the start
or end of the model. `review.start_of_line`/`end_of_line` speak
the character there.

## 7. Review cursor ↔ focus

| Command | Behaviour | Fetch | Notes |
|---|---|---|---|
| `review.to_focus` | `nav = focus`; `pos` re-anchored at the focus zone, or at the caret when M3 lands (`caret_follows`); speak `object.current`; `ExitReview` to `from` | none on hit | NVDA+numpad minus |
| `review.move_focus` | `ActionReq::Focus(nav)` (`Component.GrabFocus`); on `NotSupported` (GTK4, F-4) try `Selection.SelectChild` when the parent supports selection, else speak "cannot move focus here"; the next `FocusChanged` confirms | Interactive action | NVDA+shift+numpad minus. In M4b with M3: also set the caret to the review offset (`SetCaret`) when `nav` is editable |
| `review.activate` | `ActionReq::DoDefault(nav)`; success is confirmed by an event, not by the reply | Interactive action | NVDA+numpad enter |
| `review.focus_follows_toggle` | `review.focus_follows`, default on | none | gorca+7 |
| `review.caret_follows_toggle` | `review.caret_follows`, default on; inert until M3 | none | gorca+6, M4b |

## 8. Status bar (`info.status_bar`)

Available in every mode (global), never enters Review. The cascade, each step bounded:

1. **Mirror search** (zero IPC): bounded BFS (≤ 2 048 nodes) under the active surface for role
   `StatusBar` with `SHOWING`. On a hit, speak `name` followed by the joined `flat_zone_text` of its
   showing descendants (NVDA `getStatusBarText`, `api.py:463-475`).
2. **Collection** (one call, Interactive): `NavOp::FindRole{root: surface, role: StatusBar}` →
   `Collection.GetMatches` with role `STATUS_BAR` and states `SHOWING|VISIBLE`, count 1 (Orca
   `ax_utilities_collection.py:854-860`). The result is fetched with `Children` and spoken as in 1.
3. **Bottom line**: if a line model of the surface exists, speak its last line (NVDA's display
   fallback).
4. Otherwise "no status bar found".

Extensions (M5) may supply an app-specific status bar node through the `present` hook. The core
never special-cases an app. The Orca preset binds `info.status_bar` as `Orca+KP_Enter ×2` (CR-4).

## 9. Interaction with the B-1 mirror (summary)

| Operation | Hit path | Miss path | Class / deadline |
|---|---|---|---|
| object step | mirror `view` only | `ChildSnapshots`/`Ancestry`/`Fetch` with `NAV_MASK`, ≤ 3 suspensions | Interactive, 120 ms per step |
| speak object | snapshot (may_refine rule §5.3) | covered by the step fetch | RV-1 `Fetch{PRESENT}` after a cache-served presentation |
| flat step | `Arc<LineModel>` + validity check | refresh (VD-1/2) or full build | Prefetch; Interactive only when the target line is dirty or no model exists |
| model build | snapshots from the mirror | `ChildSnapshots`/`Fetch` with `want ∋ BOUNDS`, `VisibleLines` | Interactive (first), Prefetch (continuation) |
| RR-1 | — | ≤ 9 calls | Prefetch |
| status bar | mirror BFS | `Collection` | Interactive |

Rules:

- **MX-1** Bounds are an ordinary mirror field. They are fetched only when asked for (`want ∋
  BOUNDS`), are surface-relative, and are invalidated by `PropertiesChanged{BOUNDS}` (amendment
  §17). Focus presentation never asks for them (`PRESENT` is unchanged), so M4 adds no IPC to the
  focus path.
- **MX-2** Text lines are **not** stored in the mirror (ADR-0001 §4.2: text is not in the
  snapshot). They live only in the zones of the model that requested them.
- **MX-3** Review fetches go through the app actor's driver like fill requests, with dedupe and the
  §7 caps of B-1. When they are refused (caps, health), the step degrades (OB-4, GX-2) and never
  waits.
- **MX-4** The Reader never calls the mirror writer. Only the app actor writes (PC-1).

## 10. Plug-in points for M3, M6 and M7

- **TX-1 `TextUnits`** (M3 owns the implementation): `fn next(&self, text: &str, at: u32, unit:
  Unit, dir: Dir) -> Option<Range<u32>>` with `Unit = Char | Word | Line | Sentence`, plus
  `fn spell(&self, s: &str, how: Spell) -> SpeechText` with `Spell = Letters | Phonetic | CodePoint`.
  M4a ships `SimpleUnits` (§6.9) and spells ×2 letter by letter; ×3 (phonetic or code point) uses
  `Letters` until M3's verbalizer lands.
- **TX-2 `TextQuery::AtOffset { node, offset, unit }`** (M3 adds it for the caret): object review over
  a whole `Text` in M4b (`ReviewPos::Object{text_line: Some(_)}`). It is fetched per step with
  Prefetch class and cached in a per-navigator 64-line window, so held keys stay mirror-like.
- **TX-3 Caret follows review** (`review.caret_follows`, M4b): the M3 caret state machine sends
  `TextMsg::CaretSettled{node, offset}`. The Review arm re-anchors `pos` when it is in Focus mode,
  or in Review with `caret_follows` on.
- **LS-1 Terminal (M6)**: `LineSource::Terminal` models are produced by the terminal presenter from
  its screen snapshot (ADR-0001 T-2) instead of `GeomBuild`. The cursor, keys and validity contract
  are the same; validity is the snapshot generation. Until M6, VTE goes through the generic
  geometry path (§13).
- **LS-2 Document (M7)**: `ReviewKind::Document` reviews the browse-mode document mirror (ADR-0003)
  with its own line model. `review.mode_next` then cycles Object → Document → Screen, as NVDA does.

## 11. Keymap (reconciled with `docs/design/modes.md`)

No chord changes in §4.3 or §5. Status per binding:

| Command | NVDA desktop | NVDA laptop | Orca desktop / laptop | Slice |
|---|---|---|---|---|
| `review.prev/cur/next_line` | KP_Home / KP_Up / KP_Prior | gorca+Up / gorca+Shift+period / gorca+Down | KP_Home / KP_Up / KP_Prior; Orca+U/I/O | M4a |
| `review.prev/cur/next_word` | KP_Left / KP_Begin / KP_Right | gorca+Ctrl+Left / gorca+Ctrl+period / gorca+Ctrl+Right | KP_Left/Begin/Right; Orca+J/K/L | M4a |
| `review.prev/cur/next_char` | KP_End / KP_Down / KP_Next | gorca+Left / gorca+period / gorca+Right | KP_End/Down/Next; Orca+M/comma/period | M4a |
| ×2 spell on cur line/word/char | same keys ×2 | same | same | M4a |
| ×3 phonetic / code point | same keys ×3 | same | same | M4a falls back to spelling; M4b after M3 |
| `review.top` / `review.bottom` | Shift+KP_Home / Shift+KP_Prior | gorca+Ctrl+Home / gorca+Ctrl+End | Orca+KP_Home / Orca+KP_Prior; Orca+Ctrl+U/O | M4a |
| `review.start_of_line` / `end_of_line` | Shift+KP_End / Shift+KP_Next | gorca+Home / gorca+End | — / Orca+KP_End; Orca+Ctrl+M | M4a |
| `review.mode_next` / `mode_prev` | gorca+KP_Home / gorca+KP_End | gorca+Prior / gorca+Next | unbound | M4a (Object ↔ Screen) |
| `review.to_focus` | gorca+KP_Subtract | gorca+BackSpace | (via `review.toggle`) | M4a |
| `review.move_focus` | gorca+Shift+KP_Subtract | gorca+Shift+BackSpace | unbound | M4a |
| `review.activate` | gorca+KP_Enter | gorca+Return | Orca+Ctrl+Return | M4a |
| `review.toggle` | unbound | unbound | KP_Subtract; Orca+P | M4a |
| `object.current` (×2 spell) | gorca+KP_Begin | gorca+Shift+O | Orca+KP_Begin; Orca+Ctrl+K | M4a |
| `object.parent/first_child/next/prev` | gorca+KP_Up/KP_Down/KP_Right/KP_Left | gorca+Shift+Up/Down/Right/Left | Orca+Ctrl+arrows | M4a |
| `object.next_in_flow` / `prev_in_flow` | gorca+KP_Next / gorca+KP_Prior | gorca+Shift+bracketright / bracketleft | unbound | M4a |
| `info.status_bar` | gorca+End | gorca+Shift+End | Orca+KP_Enter ×2; Orca+slash ×2 | M4a |
| `review.focus_follows_toggle` | gorca+7 | gorca+7 | gorca+7 (*gorca extra*) | M4a |
| `review.simple_toggle` *new* | unbound | unbound | unbound | M4a |
| `review.above` / `review.below` *new* | unbound | unbound | Orca+KP_Left / Orca+KP_Right; Orca+Ctrl+J / Orca+Ctrl+L | M4b (needs geometry: the zone on the previous/next line with the largest x overlap) |
| `review.say_all` | KP_Add | gorca+Shift+A | KP_Add ×2; Orca+semicolon ×2 | M4b |
| `review.prev_page` / `next_page` | gorca+Prior / gorca+Next | gorca+Shift+Prior / gorca+Shift+Next | unbound | M4b (page = lines filling the surface height; in GeometryFallback 20 lines) |
| `review.caret_follows_toggle` | gorca+6 | gorca+6 | gorca+6 | M4b |
| `review.start_of_selection` / `end_of_selection` | gorca+Alt+Home / End | same | unbound | M4b (needs M3) |
| `info.read_window` | gorca+B | gorca+B | unbound | M4b (say-all over the Screen model from the top) |
| `review.copy_mark` / `review.copy` | gorca+F9 / gorca+F10 | same | — | not in Phase 2 (clipboard) |

Keymap rules this ADR relies on, all already in ADR-0005:

- **KM-1** Unmodified keypad review keys are grabbed only once their command is `Implemented`
  (K-1, KC-10). In M4a this is the whole M4a column, in the Focus and Review grab sets (modes.md
  §2).
- **KM-2** Keypad chords have no Num Lock variants (K-6a). With Num Lock on, the keypad types digits
  and review is reached through the gorca+ chords. This matches NVDA, and gorca says so in learn
  mode.
- **KM-3** (rewritten, V-1 refuted) Chords match keycodes (K-8). Shift does **not** move a keypad key to
  its digit keysym. With Num Lock off, `KP7` arrives as `KP_Home` and `Shift+KP7` as `KP_Home` with state
  Shift; with Num Lock on, `KP7` arrives as `KP_7` with Mod2 and `Shift+KP7` as `KP_Home` with
  Shift|Mod2. Mutter matches the exact keysym and the exact state, so the grab compiler (K-8a,
  `gorca_core::chord_grabs`) registers, for every keypad review chord, the **Num Lock off** pair only
  (`(KP_Home, 0)`, `(KP_Home, Shift)`), each also with Caps Lock set, as **exact** grabs that are not expanded
  again. With Num Lock on nothing is grabbed (K-6a, KM-2): the digits and Home/End reach the application, and review
  is reached through the gorca chords. (An earlier revision also grabbed the Num Lock on pairs; that swallowed
  numpad typing and was withdrawn after review, F-1.) Keypad operators (`KP_Add`, `KP_Subtract`, `KP_Enter`, ...)
  have one keysym and are registered for Num Lock off only.
- **KM-4** `gorca` held together with a keypad key, where the gorca modifier is `KP_Insert` (numpad
  0): Mutter grabs every key while a grabbed modifier is held, so no extra keystroke grab is needed
  (memory: mutter-key-grab-facts; `meta-a11y-manager.c:540-553`).

## 12. What is spoken

- Line: the line text (CL-4). On a model `Truncated`: "more content not shown" once, after the last
  line. In `GeometryFallback`: the zone text.
- Word and character: the unit, with character verbosity from the verbalizer (caps, symbols: basic
  in M4a, full in M3).
- Object: focus-like presentation of `nav` (role, name, value, states, description, position in set
  when known), using the presenter registry (`present/`), never a review-specific generator.
- Boundaries: "Top", "Bottom", "No next", "No previous", "No containing object", "No objects
  inside", "blank".
- Every utterance is `Interrupt` with the new epoch and carries `may_refine` when it was served from
  stale or partial data. Review utterances never use `Prio::Notification`.

## 13. Failure modes

| Failure | Behaviour | Counter |
|---|---|---|
| `nav` becomes `Gone` | the next object step starts from the nearest known live ancestor, else the surface root; a flat step uses RA-1 | `review_nav_gone` |
| surface window destroyed | `ReviewSurfaceGone` → Focus | `review_surface_gone` |
| app `Hung` mid-step | OB-4 degraded step or "not responding"; queue dropped | `review_step_timeout` |
| app floods (dirty-all) | the stale epoch makes every zone dirty → VD-3 full rebuild in the background; steps speak from the old model with `may_refine` | `review_stale_spoken` |
| extents missing or bogus | GX-1..GX-3 | `review_geom_missing` |
| St nameless objects (icons, `StBin`, unlabeled buttons) | simple review classifies nameless containers as `Layout`; an unlabeled focusable or actionable node gives a zone with role text only ("button"); the ADR-0002 naming cascade is out of Phase 2 scope | `review_nameless_zones{toolkit}` |
| VTE (GNOME Terminal, GTK3) before M6 | generic geometry path: `VisibleLines` with `GetOffsetAtPoint` bounding to the visible rows, 200-line cap, no scrollback walk; may be slow on large scrollback, so the terminal app class gets `max_lines = rows + 2` | `review_terminal_generic` |
| GTK4 editables | one zone per entry (F-3); character review inside the entry text works, extents per character do not | — |
| `review.move_focus` on GTK4 | `NotSupported` (F-4) → selection fallback or a spoken refusal | `review_move_focus_unsupported` |
| huge lists (10k rows) | GTK4 realises only visible rows; the visit cap OB-1 and the build caps of §6.8 bound the work; `ChildSnapshots` is paged | `objnav_visit_cap` |
| a model build panics (bug) | contained in the cpu job (`catch_unwind` in the job runner); `BuildError::Internal`; the Reader speaks "review unavailable" once and stays usable | `review_build_panic` |

## 14. Invariants

- **RI-1** No review command performs IPC when the mirror hits (object) or the model is valid (flat).
  This is checked in tests by a fake connector that fails the test on any request.
- **RI-2** Coordinates from different surfaces are never compared (SF-1). Every `Rect` in a model is
  relative to `model.surface`.
- **RI-3** Every request made by review has a deadline and a budget class. None is `Startup`.
- **RI-4** The Reader holds at most 2 models; the app actors hold at most the §6.8 caps; the exact
  byte footprint is reported in `MirrorStats`-style counters (`ReviewStats`, MA-1 style, exact).
- **RI-5** A review step always ends in exactly one utterance (content, boundary message, degraded
  content, or "not responding"), unless it was superseded by a newer command (R-1).
- **RI-6** All state machines here (`ReviewOpState`, `GeomBuild`, `Validity`) are enums with
  exhaustive `match` and a pure `step` (ADR-0001 §7), and are unit-tested as such.

## 15. Slices

### 15.1 M4a (ships first): object navigation + flat review in GTK apps and the Shell

Scope:

- `Mode::Review` with `ReviewOrigin`/`ReviewCursor` (§3), transitions §4.1, `ReviewOpState` §4.3.
- Object navigation §5 (all seven `object.*` commands, simple review, OB-1..OB-5).
- Screen review §6 for GTK3, GTK4 and Shell: build, clustering, validity, VD-1..3, RR-1, RA-1,
  GX-1..3, the bounds of §6.8, `SimpleUnits`.
- `ReviewKind::Object` with the navigator's presentation text only.
- Commands marked M4a in §11, `info.status_bar` (§8), `review.to_focus`, `review.move_focus`,
  `review.activate`, `review.focus_follows_toggle`, `review.simple_toggle`.
- Connector: `want ∋ BOUNDS` in `Fetch`/`ChildSnapshots` (`Component.GetExtents(WINDOW)`, one call
  per node, pipelined), `TextQuery::VisibleLines` (`GetOffsetAtPoint` at the clip's top-left, then
  `GetStringAtOffset(LINE)` + `GetRangeExtents(WINDOW)` per line until below the clip; editables
  and `NotSupported` → whole text at the node's bounds), `Collection` for the status bar.
- B-1 §5.1 amendment for `BOUNDS` (§17).

Not in M4a: say-all, page moves, above/below, caret follow, selection review, object review over
whole texts, `info.read_window`, Firefox/Chromium documents (M7), terminal polish (M6).

Files (P0 seams, CF-3): `gorca-core/src/review/{mod.rs, cursor.rs, objnav.rs, model.rs,
build.rs (GeomBuild, app-actor side), cluster.rs (pure), validity.rs, segment.rs, status.rs}`,
`gorca-core/src/mode.rs` (types), `commands/handlers.rs` (dispatch only),
`gorca-connector-atspi/src/fetch/` (bounds, `VisibleLines`, `Collection`), `gorca-model/src/text.rs`.
`reader.rs` gets only the new `ReaderEffect` variants.

Ordering dependency: B-1's winning arm must be merged first, because `fetch/strategy.rs` is
arm-owned during the bake-off. If B-1 slips, M4a puts bounds into the shared `fetch/` skeleton
behind the frozen signatures, and the winning arm rebases onto it.

M4a acceptance:

1. R1 ≤ 3 ms p99 (target 1 ms), R2, R3 and RI-1 green (§18).
2. Scripted fake-app suite (test-engineer): a GTK4-like window with a 3-column form, a staggered
   layout, a wrapped multi-line label, an entry, a popover (SF-1), a node with no extents (GX-1), a
   50 %-no-extents window (GX-2), a hung app (OB-4), a flood during review (VD-3), and a removed
   navigator.
3. Live checklist with the user: Settings (Wi-Fi, Sound panels), Files (list view, path bar,
   status), Text Editor (document lines, status), a GTK4 dialog, the Shell top bar, quick settings,
   a notification banner. Lines must read in visual order, every visible control must be reachable
   by object navigation, and the status bar must be found where the app has one.

### 15.2 M4b (after M3)

Say-all over the Screen model (chunked per line, epoch-cancelled), page moves, `review.above/below`,
caret follow (TX-3), object review over whole texts (TX-2), selection start/end, ×3 phonetic and
code point (TX-1), `info.read_window`, and moving the navigator to the desktop/app list
(`object.parent` from an app root).

## 16. Alternatives considered

| Alternative | Why not |
|---|---|
| Orca-style synchronous rebuild of the flat context on every flat-review start | Hundreds of sequential calls on the key path; one slow app stalls the reader; violates "never block" and R1. |
| Per-key IPC (fetch the line under the cursor each time, NVDA object-review style) | 1–5 ms of IPC per key in the best case, unbounded when the app is busy; R1 needs zero IPC on hit. |
| Store text lines in the mirror | Contradicts ADR-0001 §4.2 (text is not in the snapshot); a large text would inflate every per-node byte cap; lines are only meaningful with a clip rect. |
| Eager model build on every window activation | Costs IPC for every window even if review is never used; B-1's arms already compete on prefetch. Policies may add it later through `Demand{NavStep}` statistics; not decided here. |
| Global (screen) coordinates and one desktop-wide model | Impossible on Wayland for GTK4 (F-1) and unsafe across surfaces (F-10). |
| Event messages to signal model dirtiness | Messages can be dropped or reordered against replies (B-1 §5.3). The pull check (§6.5) is exact, O(1) when nothing changed, and costs ≤ 150 µs otherwise. |
| A bake-off between line-clustering rules | Not a key architectural decision: the rule is a pure function behind a fixed interface. It is tuned with the fake-app suite and the live checklist. |
| NVDA's object-review default | No display model on Linux, so object review of a button reads its name, which focus speech already did. Screen is the default, and Object is one key away. |

## 17. Consequences and amendments

- ADR-0001 §7.2: `Mode::Review { origin: ReviewOrigin, cursor: ReviewCursor }` (§3);
  `ReviewCursor` gets the concrete type.
- ADR-0001 §4.2 note: `bounds` is surface-relative (`WINDOW` for apps, stage for Shell) and is
  fetched only on demand.
- B-1 SPEC §5.1 (and ADR-0001 §4.4 table reference): the row `PropertiesChanged{n, mask}` adds
  `BOUNDS` to the invalidated set: `invalidate(n, mask ∩ (NAME∣DESCRIPTION∣VALUE∣ROLE∣STATES∣ATTRS∣BOUNDS))`.
  The IC suite gains IC-24: a `bounds-changed` event leaves the node `Stale{BOUNDS}`.
- B-1 SPEC §4.4: `DemandWhy::NavStep` is used by review (it already exists).
- `gorca-model`: `TextQuery::VisibleLines`, `TextLine`.
- `docs/design/modes.md`: §3 row "Review → Focus on FocusChanged" stays; add `ReviewSurfaceGone`;
  §4.3 gets the M4a/M4b status column of §11; §5 maps Orca "review above/below" to the new commands.
- `docs/plan/phase-2.md`: the ADR table renumbers M3's text/caret ADR to 0007 and braille to 0008.
- `CoreCmd`: `ReviewSimpleToggle`, `ReviewAbove`, `ReviewBelow` (new, `Planned(M4)`/`Planned(M4b)`).
- Cost: one `GetExtents` per node and two or three calls per visible text line, on first use per
  surface. A 300-node Settings page is about 400 calls, pipelined 8 deep, at 0.1–0.3 ms each:
  roughly 5–15 ms of app time, paid once.

## 18. Metrics to validate (gates)

All latencies are gorca-side, from `KeyEvent` receipt to `SpeechIntent` enqueue, p99, median of
interleaved repeats, and must also hold under L3-50k with Δp99 ≤ 1 ms (phase-2 §2.1 conventions).

| Gate | Definition | Threshold |
|---|---|---|
| **R1** | review or object step → SPEAK, mirror hit / valid model (zero IPC) | p99 ≤ **3 ms**, target 1 ms |
| R1v | validity check alone, 2 048 zones, every zone touched by a write since the build | p99 ≤ 0.3 ms |
| R2 | first flat step in a cold 500-node window (fake app at 0.2 ms per call, healthy) | p95 ≤ 150 ms, p99 ≤ 250 ms |
| R3 | `build_line_model` CPU, 2 048 zones | p99 ≤ 2 ms on `cpu` |
| R4 | object step on a miss, healthy app | p95 ≤ 20 ms; never > 120 ms + 5 ms (OB-3) |
| RS | correctness on the fake-app suite: lines in visual order, no lost zone, no duplicate zone | 0 failures |
| RI-1 | IPC count during 10 000 random review steps on a static window after the build | 0 |
| RQ | held review key (30 Hz, 3 s) | 0 stale utterances; the last position is always spoken |
| RM | memory: Reader ≤ 2 models; app side ≤ 16 MiB total | exact counters |

## 19. Verification items (owner: linux-platform-engineer unless noted)

- **V-1** **Refuted, rule rewritten as KM-3** (Shift does not change keypad keysyms; see section 20 for V-2 to V-5).
- **V-2** `GetExtents(WINDOW)` sanity on GTK3 under Wayland (is it relative to the toplevel surface,
  including CSD shadow offsets?), GTK4 (F-1), Shell (F-5) and libadwaita dialogs (are they separate
  surfaces?).
- **V-3** GTK4 `GetOffsetAtPoint` on `GtkTextView`/`GtkLabel` for the `VisibleLines` start offset;
  fallback is line iteration from offset 0 with the 200-line cap.
- **V-4** Does GTK4 or St emit `bounds-changed` on resize or scroll? This decides whether RR-1's 2 s
  TTL can be relaxed.
- **V-5** VTE (GTK3) `GetOffsetAtPoint` and `GetRangeExtents` cost with a 10 000-line scrollback
  (input for M6).
- **V-6** (test-engineer) A fake app that reports bogus extents (negative sizes, huge values,
  overlapping everything) does not break clustering or exceed the §6.8 bounds (fuzz
  `build_line_model`).

## 20. Implementation notes (M4a, 2026-10-14)

What the code does where the text above left a choice, so the next slice does not rediscover it.

- **Where the work runs.** Review I/O (`review/build.rs`) runs in tasks owned by the Reader lane's runtime loop
  (a `JoinSet`, at most 32, aborted with the lane), not inside the app actor. Each task talks to the app
  through its request channel under Interactive or Prefetch deadlines, hands what it fetched to the app actor
  as `FillMsg::ReviewApply` (the actor stays the only mirror writer, MX-4) and waits at most 50 ms for the
  acknowledgement, so the Reader finds the nodes in the mirror when the result message arrives. The pure
  `build_line_model` runs inline in the task (R3: 2 ms at 2 048 zones), not on a separate `cpu` pool.
- **Matching.** `ReviewMsg` replies are matched to the pending operation by request id, not by epoch: a newer
  key advances the epoch while an operation is pending, and the operation's result must still finish it (the
  queued commands run behind it, and only the last speaks). The `epoch` fields of §3 are dropped.
- **Child lists.** The connector's `ChildSnapshots` returns children only, so the parent keeps `Count(n)`. The
  `Children` navigation fetch is a `Fetch` of the parent plus a `ChildSnapshots` of its children in flight
  together, and the task writes the parent back with `Known(keys)` when the count matches. A node with more than
  256 children stays `Count`, and a walk over it degrades (OB-4).
- **Text lines on unknown interfaces.** Fetched snapshots do not carry `Ifaces`. Text-bearing roles (label,
  static text, paragraph, heading, link, entry, document, terminal, list item, cell, alert) are asked for
  `VisibleLines` when the interfaces are unknown; a refusal means the node has no zone of its own text. A node
  known not to have `Text` is not asked.
- **Zone rules added.** A control whose name equals the text of its descendants is one zone ("OK button"); a window's
  title is not a line; a label or paragraph without `Text` still says its name; a single-line entry is one zone
  made of its label, role and typed text. A label is dropped when a control on its line has the same name.
- **Platform checks V-2 to V-5** (`docs/reports/2026-10-14-adr0006-platform-checks.md`): GTK3 SCREEN, WINDOW and PARENT are
  identical and include the CSD shadow; GTK4 WINDOW is correct, offset (14,12) from the surface; a GTK4 popover is
  parent-window-relative and `AdwAlertDialog` is an in-window child (F-10 corrected); the Shell uses stage coordinates. V-3:
  `GetOffsetAtPoint(WINDOW)` for the first visible offset (about 1 ms), GTK4 editables answer `NotSupported` and fall
  back to one whole-text line. V-4: scrolling emits no bounds events in GTK3, GTK4 or the Shell, so the 2 s TTL (RR-1) and the
  per-step validity check stay. V-5: VTE `Text` exposes only the viewport (about 1.6k characters), so the generic path is cheap.
- **Counters** are on `Reader::review_counters()`: `objnav_visit_cap`, `objnav_degraded`, `review_step_timeout`,
  `review_stale_spoken`, `review_queue_overflow`, `review_builds`, `review_nav_gone`, `review_surface_gone`,
  `review_move_focus_unsupported`, `review_build_failed`.
- **Not in M4a.** The `review.*` settings keys (the defaults of §4.1 are constants; `review.simple_toggle` and
  `review.focus_follows_toggle` change them at run time only), the RV-1 correction after a stale object
  speech (the utterance is tagged `may_refine` and a `Demand{NavStep}` is sent), label elision through
  `relations`, the `BoundsChanged` signal (the invalidation path accepts `BOUNDS`; the connector does not register
  or decode the signal, V-4 says toolkits do not send it on scroll anyway), and the `Review` grab set differing
  from the `Focus` one (both use the Focus context's chords).

### 20.1 Review fixes (2026-10-14, `docs/reports/2026-10-14-m4a-review.md`)

- **F-1** Only Num Lock off pairs are grabbed (KM-3 above). Test: `a_digit_typed_with_num_lock_on_reaches_the_application`.
- **F-2** One step, queue included, has one budget of 125 ms from the key press (`STEP_BUDGET`); the pending deadline is
  `min(now + wait, end of budget)`, and once the budget is spent a step is run degraded with no further fetch (OB-4).
- **F-3** A pending operation past its deadline is dropped silently when a new review key arrives; the timer handler re-arms the
  outstanding deadline (the runtime has one timer slot); a result whose key press was followed by another one (newer epoch,
  nothing queued) is dropped (`review_superseded`).
- **F-4** The line spoken at an RR-1 check is remembered; when the background rebuild that follows a `Changed` verdict shows
  different text and the user has not pressed another key, the corrected line is spoken (`review_corrections`). Not done: a per-node
  hash of text lines in the validity check (the mirror holds no text, so it could only hash `confirmed_seq`, which every RR-1 fetch
  changes; text changes stay caught by the `NAME|VALUE` invalidation of `TextChanged` and by RR-1).
- **F-5** The model records each node's own extents and the clip it asked text for; the RR-1 probe compares node extents with
  node extents and re-reads lines in that clip.
- **F-6** Review line, word and char moves and `object.next/prev/*_in_flow` are `repeatable`; the lane debounces.
- **F-7** A build runs up to 64 tree levels per pass and continues in the background (up to 8 passes) while it finds new nodes.
- **F-8** The R1 harness credits each speech to its key by text; late speeches are latency samples and are counted as `r1_late_keys`.
