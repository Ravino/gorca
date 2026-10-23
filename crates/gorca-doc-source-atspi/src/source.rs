// SPDX-License-Identifier: GPL-3.0-or-later
//! `AtspiDocSource`: the `DocSource` that feeds one browser document into the mirror.
//!
//! Life cycle: build (block fetch, [`crate::fetch`]) -> live (events -> dirty set -> settle ->
//! plan -> refetch only what changed -> deltas) -> detached. Every IPC call carries a deadline
//! (ADR-0001 D-4) and a hung browser only stalls this actor's own loop; the sink never blocks
//! (SL-1).
//!
//! Contract notes for the consumer (`gorca-doc`), also recorded in the web-prep report:
//!
//! * The first batch is `Reset`, chunked `Upsert`s in DFS order, then every `Children` list.
//! * Incremental batches are `Upsert`s (parents before children), complete `Children` lists, then
//!   `Removed` for subtrees that continue nowhere. A node that the page re-created keeps its
//!   `SrcNodeId` (identity variant C), so the mirror sees an in-place `Upsert`, not remove + add.
//! * `Upsert` replaces `text` wholesale. With the text tier disabled (`TextPolicy::None`) `text`
//!   is always `None` and text is only delivered through `DocRequest::Text`.
//! * `Settled` is not emitted: the doc actor infers settle (ADR-0003 9.3).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gorca_dbus::DbusError;
use gorca_doc_source_api::{
    ActionCaps, Activity, DeltaSink, DocAction, DocEvent, DocRequest, DocSource, DocSourceCtx,
    JoinKeyCaps, RepairReport, SourceCaps, SourceError, SourceKind, web_attrs,
};
use gorca_model::{
    DocDelta, LocalId, NodeEvent, PropMask, Role, SrcNode, SrcNodeId, TextEdit, TextUnit,
};
use gorca_rt::{BoxFuture, Deadline, mpsc};
use smallvec::SmallVec;

use crate::events::{AtspiEvent, Dirty, DirtySet, EventKind, SettlePolicy};
use crate::fetch::{
    FetchError, FetchOpts, FetchTimings, FetchedDoc, PathTable, TextPolicy, fetch_document,
    fetch_first_block, fetch_forest,
};
use crate::identity::{IdentityMap, MatchStats, SibInfo, Sig};
use crate::p2p::AppConn;
use crate::roles;
use crate::runs::RunLayer;
use crate::wake;
use crate::wire::{self, AttrInterner, PathKey};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const TEXT: &str = "org.a11y.atspi.Text";
const COMPONENT: &str = "org.a11y.atspi.Component";
/// Frames woken per document, polls of one frame and the gap between them.
const WAKE_FRAMES_MAX: usize = 12;
const ACTION: &str = "org.a11y.atspi.Action";

#[derive(Clone, Debug)]
pub struct SourceConfig {
    pub fetch: FetchOpts,
    pub settle: SettlePolicy,
    pub text_unit: TextUnit,
    /// More distinct dirty nodes than this in one burst: re-fetch the whole document instead of
    /// planning per node (ADR-0003 9.5 overload).
    pub resync_threshold: usize,
    /// Chunk size of the initial `Upsert`s.
    pub chunk: usize,
    pub nav_id: u64,
    /// Deadline for one `GetChildren` / property read during updates (Prefetch class).
    pub call_timeout: Duration,
    /// Use DOM ids as the IL-2 rung. Off only for the identity ablation in the benchmark.
    pub use_dom_id: bool,
    /// Viewport-first build (ADR-0003 section 8): push the top of the document, at least this many
    /// nodes, as soon as they are complete, then the rest. 0 disables it.
    pub first_nodes: usize,
    /// Split container text into run leaves (TX-1, FT-2). Needs `fetch.text == All` to do
    /// anything; off for the B-6 harness, which compares against raw fetches.
    pub runs: bool,
    /// What this engine's AT-SPI tree needs on top of the generic build (data from the
    /// connector's quirk table, never decided here from a name).
    pub quirks: EngineQuirks,
    /// Events that did not fit while dormant, after which a wake rebuilds from scratch anyway.
    pub dormant_marks: usize,
}

/// The hypertext range Gecko takes for `sp` (Firefox 153 ESR, measured): an end offset that names
/// an embedded hypertext child includes it, so a range that ends on such a child names the child
/// (`end - 1`), and a range that ends right before one leaves out its white space (`end - 1`) or
/// takes the child along. An image is not hypertext: ending after it needs no adjustment.
pub(crate) fn gecko_range(sp: &gorca_model::SelSpan<SrcNodeId>) -> std::ops::Range<u32> {
    let end = sp.range.end;
    let end = if (sp.end_on_child || (sp.child_follows && sp.trim_ws)) && end > sp.range.start {
        end - 1
    } else {
        end
    };
    sp.range.start..end
}

/// Engine quirks of the tree the source reads (Blink today). A quirk table: one flag per quirk.
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct EngineQuirks {
    /// Chromium reorders keyed lists without an event (`SourceCaps::silent_reorders`).
    pub silent_reorders: bool,
    /// Blink: `static` children repeat the text the parent's hypertext already holds (connector
    /// quirk row `Toolkit::text_leaf_children_repeat_parent_text`), so the run layer drops them.
    pub static_leaves_repeat_text: bool,
    /// Blink renders (and so serializes) a cross-process iframe only while it intersects the
    /// viewport: an `internal frame` that is empty and not showing has its content as soon as it
    /// is scrolled into view. The source scrolls each such frame into view once, then back
    /// (`docs/reports/m9-impl.md`, iframes).
    pub wake_offscreen_frames: bool,
    /// Blink keeps a single selection range and reads a selection end offset as exclusive
    /// (Gecko adds ranges and ends after an embedded child that the offset names).
    pub selection_single_exclusive: bool,
}

impl EngineQuirks {
    /// Every Blink quirk on, none otherwise.
    pub fn blink(on: bool) -> Self {
        Self {
            silent_reorders: on,
            static_leaves_repeat_text: on,
            wake_offscreen_frames: on,
            selection_single_exclusive: on,
        }
    }
}

impl SourceConfig {
    /// The browse-mode configuration (browse design FT-1): text tier on, runs on.
    pub fn browse(text_unit: TextUnit, chromium: bool) -> Self {
        Self {
            fetch: FetchOpts {
                text: TextPolicy::All,
                ..FetchOpts::default()
            },
            text_unit,
            runs: true,
            quirks: EngineQuirks::blink(chromium),
            ..Self::default()
        }
    }
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            fetch: FetchOpts::default(),
            settle: SettlePolicy::default(),
            text_unit: TextUnit::Utf16Bom,
            resync_threshold: 400,
            chunk: 2048,
            nav_id: 1,
            call_timeout: Duration::from_millis(300),
            use_dom_id: true,
            first_nodes: 400,
            runs: false,
            quirks: EngineQuirks::default(),
            dormant_marks: 100_000,
        }
    }
}

/// What one processed burst cost and did, for the B-6 harness (WM-4, M-13).
#[derive(Clone, Debug)]
pub struct UpdateRecord {
    pub events: u32,
    pub dirty_nodes: usize,
    pub first_event: Option<Instant>,
    pub last_event: Option<Instant>,
    pub planned_at: Instant,
    pub pushed_at: Instant,
    pub parents_refreshed: u32,
    pub props_refreshed: u32,
    pub nodes_fetched: usize,
    pub deltas: usize,
    pub stats: MatchStats,
    pub resync: bool,
    pub rewrite: bool,
    /// Why a rewrite was declared (parent id, changed siblings, first old/new signature).
    pub rewrite_note: Option<String>,
    pub errors: u32,
}

#[derive(Debug, Default)]
struct MetricsInner {
    build: Option<FetchTimings>,
    build_nodes: usize,
    first_block: Option<(Duration, usize)>,
    updates: Vec<UpdateRecord>,
}

/// Shared handle to the source's measurements.
#[derive(Clone, Debug, Default)]
pub struct SourceMetrics {
    inner: Arc<Mutex<MetricsInner>>,
}

impl SourceMetrics {
    pub fn build(&self) -> Option<(FetchTimings, usize)> {
        let g = self.inner.lock().ok()?;
        g.build.clone().map(|b| (b, g.build_nodes))
    }

    pub fn updates(&self) -> Vec<UpdateRecord> {
        self.inner
            .lock()
            .map(|g| g.updates.clone())
            .unwrap_or_default()
    }

    /// Time from attach to the first pushed block (the top of the document, speakable) and its size.
    pub fn first_block(&self) -> Option<(Duration, usize)> {
        self.inner.lock().ok().and_then(|g| g.first_block)
    }

    fn set_first_block(&self, d: Duration, nodes: usize) {
        if let Ok(mut g) = self.inner.lock() {
            g.first_block = Some((d, nodes));
        }
    }

    fn set_build(&self, t: FetchTimings, nodes: usize) {
        if let Ok(mut g) = self.inner.lock() {
            g.build = Some(t);
            g.build_nodes = nodes;
        }
    }

    fn push(&self, r: UpdateRecord) {
        if let Ok(mut g) = self.inner.lock()
            && g.updates.len() < 100_000
        {
            g.updates.push(r);
        }
    }
}

pub struct AtspiDocSource {
    app: Arc<AppConn>,
    root_path: String,
    interner: Arc<AttrInterner>,
    config: SourceConfig,
    events: mpsc::Receiver<AtspiEvent>,
    metrics: SourceMetrics,
    /// The engine's fusion tables for the document actor (`None`: no fused semantics).
    fuse: Option<gorca_doc_source_api::FuseSetup>,
}

impl std::fmt::Debug for AtspiDocSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtspiDocSource")
            .field("root_path", &self.root_path)
            .finish_non_exhaustive()
    }
}

impl AtspiDocSource {
    /// `events` is fed by whoever owns the bus ingress (the connector) or, in the B-6 harness,
    /// by [`crate::bus::subscribe`].
    pub fn new(
        app: Arc<AppConn>,
        root_path: String,
        config: SourceConfig,
        events: mpsc::Receiver<AtspiEvent>,
    ) -> (Self, SourceMetrics) {
        let metrics = SourceMetrics::default();
        (
            Self {
                app,
                root_path,
                interner: Arc::new(AttrInterner::default()),
                config,
                events,
                metrics: metrics.clone(),
                fuse: None,
            },
            metrics,
        )
    }

    /// Hand the document actor the engine's fusion tables (chosen by `norm::fuse_setup`).
    #[must_use]
    pub fn with_fuse_setup(mut self, setup: Option<gorca_doc_source_api::FuseSetup>) -> Self {
        self.fuse = setup;
        self
    }

    pub fn interner(&self) -> Arc<AttrInterner> {
        Arc::clone(&self.interner)
    }

    /// Share the attribute interner of the application (keys must agree across its documents).
    #[must_use]
    pub fn with_interner(mut self, interner: Arc<AttrInterner>) -> Self {
        self.interner = interner;
        self
    }
}

impl DocSource for AtspiDocSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Atspi
    }

    fn caps(&self) -> SourceCaps {
        SourceCaps {
            bulk_snapshot: true,
            push_deltas: true,
            settled_markers: true,
            text_unit: self.config.text_unit,
            actions: ActionCaps::FOCUS
                | ActionCaps::CLICK
                | ActionCaps::SCROLL_INTO_VIEW
                | ActionCaps::SET_CARET
                | ActionCaps::SET_SELECTION,
            // The object path validated by a DOM id and a signature is the heuristic key.
            join_keys: JoinKeyCaps::EXACT | JoinKeyCaps::HEURISTIC,
            silent_reorders: self.config.quirks.silent_reorders,
        }
    }

    fn attach(self: Box<Self>, ctx: DocSourceCtx) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move { (*self).run(ctx).await })
    }

    fn fuse_setup(&self) -> Option<gorca_doc_source_api::FuseSetup> {
        self.fuse.clone()
    }
}

fn map_dbus(e: &DbusError) -> SourceError {
    match e {
        DbusError::Timeout => SourceError::Timeout,
        DbusError::Zbus(z) => SourceError::Protocol(z.to_string()),
    }
}

/// Like [`map_dbus`] for a call that targets one object: a bus error that says the object does
/// not exist (the browser released it, the page replaced the node) is `Gone`, which the caller
/// may answer by re-resolving the node.
/// `ATSPI_SCROLL_ANYWHERE`: scroll the least that brings the object into view (a cursor that is
/// already visible does not move the page; `TOP_LEFT` would pin every line to the top edge).
const SCROLL_ANYWHERE: u32 = 6;

fn map_target_err(e: &DbusError) -> SourceError {
    if let DbusError::Zbus(z) = e {
        let text = z.to_string();
        if text.contains("UnknownObject")
            || text.contains("Defunct")
            || text.contains("NoSuchObject")
        {
            return SourceError::Gone;
        }
    }
    map_dbus(e)
}

fn map_fetch(e: &FetchError) -> SourceError {
    match e {
        FetchError::Dbus(d) => map_dbus(d),
        FetchError::Deadline(_) => SourceError::Timeout,
        FetchError::Wire(_) | FetchError::Shape(_) | FetchError::BadRoot => {
            SourceError::Protocol(e.to_string())
        }
    }
}

/// `(raw role, raw state bits, name, attributes)` of one object.
///
/// The last element is the `aria-errormessage` target the error text came from, if any.
type ReadTuple = (
    u32,
    u64,
    Option<Arc<str>>,
    Vec<(String, String)>,
    Option<String>,
);

/// How many children of `parent` have exactly this signature (role, tag, name).
fn twin_count(live: &Live, parent: SrcNodeId, sig: &crate::identity::Sig) -> usize {
    live.ident.children_of(parent).map_or(0, |kids| {
        kids.iter()
            .filter(|k| live.ident.info_of(**k).is_some_and(|i| i.sig == *sig))
            .count()
    })
}

/// How many ancestors an action whose target is gone refreshes at most.
const REFRESH_LEVELS: usize = 4;

/// Error texts one re-read batch fetches (a toggle storm over hundreds of invalid fields must
/// not turn into hundreds of relation round trips; a focused field reads its own on focus).
const MAX_REREAD_ERROR_TEXTS: usize = 16;

/// Re-read role, state, name, attributes and description of one object (five pipelined calls),
/// plus the error message of an invalid control (M8b).
async fn read_node_raw(
    app: &AppConn,
    path: &str,
    dl: Deadline,
    quirks: crate::extras::WebQuirks,
    error_budget: &std::sync::atomic::AtomicUsize,
) -> Option<ReadTuple> {
    let lane = app.next_lane();
    let role = app.call(lane, dl, path, ACCESSIBLE, "GetRole", &());
    let state = app.call(lane + 1, dl, path, ACCESSIBLE, "GetState", &());
    let name = app.call(lane + 2, dl, path, PROPERTIES, "Get", &(ACCESSIBLE, "Name"));
    let attrs = app.call(lane + 3, dl, path, ACCESSIBLE, "GetAttributes", &());
    let desc = crate::extras::description(app, path, dl);
    let (role, state, name, attrs, desc) = tokio::join!(role, state, name, attrs, desc);
    let role = wire::parse_u32(role.ok()?.body().data().bytes()).ok()?;
    let state = wire::parse_state(state.ok()?.body().data().bytes()).ok()?;
    let name = name
        .ok()
        .and_then(|m| {
            wire::parse_variant_string(m.body().data().bytes())
                .ok()
                .map(str::to_owned)
        })
        .filter(|s| !s.is_empty())
        .map(|s| Arc::<str>::from(s.as_str()));
    let mut attrs = attrs
        .ok()
        .and_then(|m| wire::parse_string_map(m.body().data().bytes()).ok())
        .unwrap_or_default();
    for (_, v) in &mut attrs {
        gorca_doc_source_api::limits::clip_text(
            v,
            gorca_doc_source_api::limits::MAX_ATTR_VALUE_BYTES,
        );
    }
    if let Some(d) = desc {
        attrs.push(("description".to_owned(), d.to_string()));
    }
    let invalid = state & (1u64 << roles::state_bit::INVALID_ENTRY) != 0;
    let mut target = None;
    if invalid
        && crate::extras::takes_error_text(roles::map_role(role))
        && error_budget.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < MAX_REREAD_ERROR_TEXTS
    {
        crate::extras::warm_relations(app, path, quirks, dl).await;
        if let Some((tp, t)) = crate::extras::error_text(app, path, dl).await {
            attrs.push(("error-text".to_owned(), t.to_string()));
            target = Some(tp);
        }
    }
    Some((role, state, name, attrs, target))
}

/// Mutable per-document state of the running source.
struct Live {
    /// The rectangle request in flight (label inference); a newer one aborts it.
    extents_task: Option<tokio::task::JoinHandle<()>>,
    ident: IdentityMap,
    paths: PathTable,
    dirty: DirtySet,
    batch: u64,
    /// Consecutive failures to list the document root's children. The root object is gone when
    /// the page navigated (a new document accessible replaces it): the source detaches (IR-7)
    /// and the doc actor attaches a new one.
    root_errors: u32,
    root_key: Option<PathKey>,
    out: Pusher,
    /// Transport key of every object the source has seen, by the hash the drain computes.
    locals: HashMap<LocalId, PathKey>,
    activity: Activity,
    /// Empty frames found by the build (`wake_offscreen_frames`) and the nodes the wake takes
    /// its scroll reference from.
    wake: wake::WakePlan,
    /// The burst was too big to refresh piecemeal: the loop starts a background resync (B-1).
    resync_wanted: bool,
    /// Frontier leaves whose `GetTree` was heavy (B-3), handed to the next fetch.
    heavy: std::collections::HashSet<String>,
    /// A background refetch runs: incremental updates keep being applied (review F-1) and the
    /// nodes they touched are re-marked when the refetch lands, so an older snapshot never
    /// leaves them reverted.
    flight_active: bool,
    touched: std::collections::HashSet<PathKey>,
    touched_overflow: bool,
}

/// Most nodes remembered as touched during one refetch; beyond it the refetch runs again.
const MAX_TOUCHED: usize = 4096;

/// A background refetch of the whole document (CHR-0 B-1). The fetch interns into a forked path
/// table and never touches the mirror, so the request loop keeps serving `Action`, `Text` and
/// `Revalidate` against the old mirror while it runs; the result is applied by the loop.
struct Flight {
    task: tokio::task::JoinHandle<Result<(FetchedDoc, PathTable), FetchError>>,
    started: Instant,
    waiters: Vec<gorca_doc_source_api::Reply<RepairReport>>,
    /// A plain rebuild arrived while this one ran: run once more after it.
    again: bool,
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The one place deltas leave the source: run synthesis, then the sink, then `Settled` (AM-4).
struct Pusher {
    runs: Option<RunLayer>,
    batch: u64,
}

impl Pusher {
    fn emit(&mut self, sink: &DeltaSink, deltas: Vec<DocDelta>) {
        let deltas = match &mut self.runs {
            Some(r) => r.process(deltas),
            None => deltas,
        };
        for d in deltas {
            sink.push(d);
        }
        self.batch += 1;
        sink.push(DocDelta::Settled { batch: self.batch });
    }
}

/// Remember the transport keys of a fetched document for event routing.
fn register_locals(live: &mut Live, doc: &FetchedDoc) {
    for (n, k) in doc.block.nodes.iter().zip(&doc.keys) {
        if let Some(l) = n.local {
            live.locals.insert(l, *k);
        }
    }
}

/// Map a routed core event to the source's own event type. Focus, caret and selection are not
/// the document source's business (SL-5).
fn to_atspi(live: &Live, e: &DocEvent) -> Option<AtspiEvent> {
    let key_of = |n: &gorca_model::NodeKey| live.locals.get(&n.local).copied();
    let (path, kind) = match &e.ev {
        NodeEvent::ChildrenChanged { node } => (
            key_of(node)?,
            EventKind::ChildAdded {
                index: -1,
                child: None,
            },
        ),
        NodeEvent::NodeRemoved { node } => (key_of(node)?, EventKind::Defunct),
        NodeEvent::PropertiesChanged { node, mask } => {
            let kind = if mask.contains(PropMask::NAME) {
                EventKind::NameChanged
            } else if mask.contains(PropMask::VALUE) {
                EventKind::ValueChanged
            } else if mask.contains(PropMask::DESCRIPTION) {
                EventKind::DescriptionChanged
            } else if mask.intersects(PropMask::STATES | PropMask::ROLE | PropMask::ATTRS) {
                EventKind::StateChanged {
                    state: Arc::from("props"),
                    on: true,
                }
            } else {
                return None;
            };
            (key_of(node)?, kind)
        }
        NodeEvent::TextChanged { node, .. } => {
            (key_of(node)?, EventKind::TextChanged { insert: true })
        }
        NodeEvent::FocusChanged { .. }
        | NodeEvent::ActiveDescendantChanged { .. }
        | NodeEvent::CaretMoved { .. }
        | NodeEvent::SelectionChanged { .. }
        | NodeEvent::Announcement { .. }
        | NodeEvent::WindowEvent { .. }
        | NodeEvent::DocumentEvent { .. } => return None,
    };
    Some(AtspiEvent {
        path,
        kind,
        at: e.at,
    })
}

/// Note one event in the dirty set; the document root going defunct detaches the source.
fn note(live: &mut Live, e: &AtspiEvent) {
    if matches!(e.kind, crate::events::EventKind::Defunct) && Some(e.path) == live.root_key {
        live.root_errors = ROOT_ERROR_LIMIT;
    }
    live.dirty.note(e);
    // The text an invalid field shows lives in another node (`aria-errormessage`): when that
    // node's text or name changes, the fields are re-read too.
    if matches!(
        e.kind,
        crate::events::EventKind::TextChanged { .. }
            | crate::events::EventKind::NameChanged
            | crate::events::EventKind::ChildAdded { .. }
            | crate::events::EventKind::ChildRemoved { .. }
    ) && !live.paths.error_fields(e.path).is_empty()
    {
        let fields: Vec<PathKey> = live.paths.error_fields(e.path).to_vec();
        for f in fields {
            live.dirty.note(&AtspiEvent {
                path: f,
                kind: crate::events::EventKind::DescriptionChanged,
                at: e.at,
            });
        }
    }
}

/// What the cheap look found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Look {
    Same,
    /// A child list differs, or the look could not tell: refetch.
    Changed,
    /// A user request is waiting: the look stopped.
    Busy,
}

/// Nodes and time of the cheap look before a guessed repair ([`AtspiDocSource::top_unchanged`]).
const CHEAP_NODES: usize = 48;
const CHEAP_BUDGET: Duration = Duration::from_millis(150);

/// How long the pre-click read of the target may take before the click goes ahead unchecked.
const VERIFY_TIMEOUT: Duration = Duration::from_millis(150);

/// The net effect of a repair batch (a refetch's difference followed by the re-read of nodes the
/// page changed meanwhile): a node the old snapshot lacked is removed by the first part and
/// upserted again by the second, which must not reach the mirror as a removal (the cursor on it
/// would be lost); of several child lists of one parent the last one wins.
fn net_effect(deltas: Vec<DocDelta>) -> Vec<DocDelta> {
    let mut upserted: HashSet<SrcNodeId> = HashSet::new();
    for d in &deltas {
        if let DocDelta::Upsert(v) = d {
            upserted.extend(v.iter().map(|n| n.id));
        }
    }
    let mut last_list: HashMap<SrcNodeId, usize> = HashMap::new();
    for (i, d) in deltas.iter().enumerate() {
        if let DocDelta::Children { parent, .. } = d {
            last_list.insert(*parent, i);
        }
    }
    let mut out = Vec::with_capacity(deltas.len());
    for (i, d) in deltas.into_iter().enumerate() {
        match d {
            DocDelta::Removed(v) => {
                let kept: Vec<SrcNodeId> =
                    v.into_iter().filter(|id| !upserted.contains(id)).collect();
                if !kept.is_empty() {
                    out.push(DocDelta::Removed(kept));
                }
            }
            DocDelta::Children { parent, children } => {
                if last_list.get(&parent) == Some(&i) {
                    out.push(DocDelta::Children { parent, children });
                }
            }
            other @ (DocDelta::Reset { .. }
            | DocDelta::Upsert(_)
            | DocDelta::Text { .. }
            | DocDelta::Hint(_)
            | DocDelta::Settled { .. }) => out.push(other),
        }
    }
    out
}

/// Root list failures before the source gives up the document.
const ROOT_ERROR_LIMIT: u32 = 3;

impl AtspiDocSource {
    async fn run(mut self, mut ctx: DocSourceCtx) -> Result<(), SourceError> {
        let mut live = Live {
            extents_task: None,
            ident: IdentityMap::new(self.config.use_dom_id),
            paths: PathTable::default(),
            dirty: DirtySet::default(),
            batch: 0,
            root_errors: 0,
            root_key: None,
            out: Pusher {
                runs: (self.config.runs && self.config.fetch.text == TextPolicy::All).then(|| {
                    if self.config.quirks.static_leaves_repeat_text {
                        RunLayer::dropping_static_children()
                    } else {
                        RunLayer::new()
                    }
                }),
                batch: 0,
            },
            locals: HashMap::new(),
            activity: Activity::Awake,
            wake: wake::WakePlan::default(),
            resync_wanted: false,
            heavy: std::collections::HashSet::new(),
            flight_active: false,
            touched: std::collections::HashSet::new(),
            touched_overflow: false,
        };
        let mut flight: Option<Flight> = None;

        // Build. A failed block fetch is the one hard error: nothing to mirror.
        let t_attach = Instant::now();
        let progressive = self.config.first_nodes > 0;
        let first = if progressive {
            match fetch_first_block(
                &self.app,
                &self.interner,
                &mut live.paths,
                &self.root_path,
                &self.config.fetch,
                self.config.first_nodes,
            )
            .await
            {
                Ok((a, plan, consumed)) if consumed < plan.leaves.len() => {
                    Some((a, plan, consumed))
                }
                Ok((a, _, _)) => {
                    // Everything fitted in the first block.
                    self.finish_build(&mut live, &ctx, &[&a], t_attach, true);
                    None
                }
                Err(e) => return Err(map_fetch(&e)),
            }
        } else {
            let doc = fetch_document(
                &self.app,
                &self.interner,
                &mut live.paths,
                &self.root_path,
                &self.config.fetch,
            )
            .await
            .map_err(|e| map_fetch(&e))?;
            self.finish_build(&mut live, &ctx, &[&doc], t_attach, true);
            None
        };
        if let Some((a, plan, consumed)) = first {
            // Push the top of the document now; fetch the rest while the mirror is already usable.
            let n_a = a.block.node_count();
            register_locals(&mut live, &a);
            let deltas = a
                .block
                .clone()
                .into_deltas(Some(self.config.nav_id), self.config.chunk);
            live.out.emit(&ctx.sink, deltas);
            self.metrics.set_first_block(t_attach.elapsed(), n_a);
            tracing::info!(
                target: "gorca_phase",
                phase = "first_block",
                wall_ms = t_attach.elapsed().as_secs_f64() * 1000.0,
                nodes = n_a,
            );
            if let Ok(rest) = fetch_forest(
                &self.app,
                &self.interner,
                &mut live.paths,
                &plan.leaves[consumed..],
                &self.config.fetch,
            )
            .await
            {
                register_locals(&mut live, &rest);
                let deltas = rest.block.clone().into_deltas(None, self.config.chunk);
                live.out.emit(&ctx.sink, deltas);
                self.finish_build(&mut live, &ctx, &[&a, &rest], t_attach, false);
            } else {
                // The top of the page is already in the mirror but the rest did not come:
                // fetch everything once more as a single block that replaces it.
                let doc = fetch_document(
                    &self.app,
                    &self.interner,
                    &mut live.paths,
                    &self.root_path,
                    &self.config.fetch,
                )
                .await
                .map_err(|e| map_fetch(&e))?;
                self.config.nav_id += 1;
                self.finish_build(&mut live, &ctx, &[&doc], t_attach, true);
            }
        }

        self.wake_frames(&mut live, &ctx.shutdown);
        let mut events_open = true;
        let mut ctx_events_open = true;
        loop {
            let due = live.dirty.deadline(&self.config.settle);
            let sleep = async {
                match due {
                    Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                () = ctx.shutdown.cancelled() => return Ok(()),
                req = ctx.requests.recv() => {
                    let Some(req) = req else { return Ok(()) };
                    self.handle_request(&mut live, &ctx, &mut flight, req).await;
                }
                res = async {
                    match flight.as_mut() {
                        Some(f) => (&mut f.task).await,
                        None => std::future::pending().await,
                    }
                }, if flight.is_some() => {
                    if let Some(f) = flight.take() {
                        self.finish_flight(&mut live, &ctx, &mut flight, f, res).await;
                    }
                    if live.root_errors >= ROOT_ERROR_LIMIT {
                        return Err(SourceError::Detached);
                    }
                }
                ev = ctx.events.recv(), if ctx_events_open => {
                    // Events routed by the core (EF-1): an unknown node is not ours.
                    let Some(ev) = ev else {
                        ctx_events_open = false;
                        continue;
                    };
                    if let Some(a) = to_atspi(&live, &ev) {
                        note(&mut live, &a);
                    }
                    while let Ok(ev) = ctx.events.try_recv() {
                        if let Some(a) = to_atspi(&live, &ev) {
                            note(&mut live, &a);
                        }
                    }
                    if live.root_errors >= ROOT_ERROR_LIMIT {
                        return Err(SourceError::Detached);
                    }
                }
                ev = self.events.recv(), if events_open => {
                    match ev {
                        Some(e) => {
                            note(&mut live, &e);
                            // Take everything already queued: bigger bursts, fewer passes.
                            while let Ok(e) = self.events.try_recv() {
                                note(&mut live, &e);
                            }
                            if live.root_errors >= ROOT_ERROR_LIMIT {
                                return Err(SourceError::Detached);
                            }
                        }
                        None => events_open = false,
                    }
                }
                () = sleep, if live.activity == Activity::Awake => {
                    self.process_dirty(&mut live, &ctx).await;
                    if live.resync_wanted {
                        live.resync_wanted = false;
                        self.start_flight(&mut live, &mut flight, None);
                    }
                    if live.root_errors >= ROOT_ERROR_LIMIT {
                        return Err(SourceError::Detached);
                    }
                }
            }
        }
    }

    /// Register the built blocks with the identity layer and record the build cost. With
    /// `push` the (single) block is also sent to the sink.
    fn finish_build(
        &self,
        live: &mut Live,
        ctx: &DocSourceCtx,
        docs: &[&FetchedDoc],
        t_attach: Instant,
        push: bool,
    ) {
        live.ident.load_parts(docs);
        for d in docs {
            for r in &d.timings.heavy_roots {
                if live.heavy.len() < 64 {
                    live.heavy.insert(r.clone());
                }
            }
        }
        live.root_key = docs.first().and_then(|d| d.keys.first().copied());
        if self.config.quirks.wake_offscreen_frames {
            Self::note_sleeping_frames(live, docs);
        }
        let nodes: usize = docs.iter().map(|d| d.block.node_count()).sum();
        let mut timings = docs[0].timings.clone();
        timings.total = t_attach.elapsed();
        let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
        tracing::info!(
            target: "gorca_phase",
            phase = "fetch",
            total_ms = ms(timings.total),
            warm_ms = ms(timings.warm),
            frontier_ms = ms(timings.frontier),
            get_tree_ms = ms(timings.get_tree),
            roles_ms = ms(timings.roles),
            states_ms = ms(timings.states),
            text_ms = ms(timings.text),
            assemble_ms = ms(timings.assemble),
            heaviest_get_tree_ms = ms(timings.heaviest_get_tree),
            calls = timings.calls,
            tree_bytes = timings.tree_bytes,
            nodes,
        );
        self.metrics.set_build(timings, nodes);
        for d in docs {
            register_locals(live, d);
        }
        if push {
            for d in docs {
                let deltas = d
                    .block
                    .clone()
                    .into_deltas(Some(self.config.nav_id), self.config.chunk);
                live.out.emit(&ctx.sink, deltas);
            }
            self.metrics.set_first_block(t_attach.elapsed(), nodes);
        }
    }

    /// Remember the empty `internal frame`s of the blocks (at most [`WAKE_FRAMES_MAX`]) and the
    /// first nodes of the page, one of which the wake measures the scroll position with.
    fn note_sleeping_frames(live: &mut Live, docs: &[&FetchedDoc]) {
        /// ATSPI_ROLE_INTERNAL_FRAME.
        const INTERNAL_FRAME: u32 = 28;
        let mut ref_keys: Vec<PathKey> = Vec::new();
        for d in docs {
            for i in 0..d.block.nodes.len() {
                let (Some(&raw), Some(&count), Some(&key)) =
                    (d.raw_roles.get(i), d.child_counts.get(i), d.keys.get(i))
                else {
                    continue;
                };
                if raw != INTERNAL_FRAME {
                    // Any role will do (a heading or a paragraph measures a scroll as well as
                    // a link): the task asks the node's real state, the block holds only the
                    // plain default for roles that are not stateful.
                    if i > 0 && ref_keys.len() < wake::REFS_MAX {
                        ref_keys.push(key);
                    }
                    continue;
                }
                if count == 0
                    && live.wake.frames.len() < WAKE_FRAMES_MAX
                    && let Some(p) = live.paths.path_of(key)
                {
                    live.wake.frames.push(p);
                }
            }
        }
        if !live.wake.frames.is_empty() {
            live.wake.refs = ref_keys
                .into_iter()
                .filter_map(|k| live.paths.path_of(k))
                .collect();
        }
    }

    /// Start the one-off task that scrolls every sleeping frame into view and back. It ends
    /// with the document (`shutdown`).
    fn wake_frames(&self, live: &mut Live, shutdown: &gorca_rt::CancellationToken) {
        let plan = std::mem::take(&mut live.wake);
        if plan.frames.is_empty() {
            return;
        }
        tokio::spawn(wake::run(
            Arc::clone(&self.app),
            plan,
            self.config.call_timeout,
            shutdown.clone(),
        ));
    }

    fn call_deadline(&self) -> Deadline {
        Deadline(Instant::now() + self.config.call_timeout)
    }

    /// `GetChildren` of `path`, as path keys. A node over `ATSPI_MAX_CHILDREN` errors on the
    /// server; the caller treats an error as "cannot refresh this parent".
    async fn list_children(&self, live: &mut Live, path: &str) -> Result<Vec<PathKey>, DbusError> {
        let m = self
            .app
            .call(
                self.app.next_lane(),
                self.call_deadline(),
                path,
                ACCESSIBLE,
                "GetChildren",
                &(),
            )
            .await?;
        let kids = wire::parse_children(m.body().data().bytes())
            .map_err(|e| DbusError::Zbus(zbus::Error::Failure(e.to_string())))?;
        Ok(kids.iter().map(|(_, p)| live.paths.intern(p)).collect())
    }

    /// Re-read role, state, name and attributes of many objects with a window of in-flight reads.
    async fn read_nodes(
        &self,
        targets: Vec<(SrcNodeId, String)>,
    ) -> Vec<(SrcNodeId, Option<ReadTuple>)> {
        let targets = Arc::new(targets);
        let cursor = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let error_budget = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let quirks = self.config.fetch.quirks;
        let workers = (self.app.lanes() * 4).min(targets.len()).max(1);
        let mut tasks = Vec::with_capacity(workers);
        for _ in 0..workers {
            let app = Arc::clone(&self.app);
            let targets = Arc::clone(&targets);
            let cursor = Arc::clone(&cursor);
            let error_budget = Arc::clone(&error_budget);
            let timeout = self.config.call_timeout;
            tasks.push(tokio::spawn(async move {
                let mut out = Vec::new();
                loop {
                    let k = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((id, path)) = targets.get(k) else {
                        break;
                    };
                    let dl = Deadline(Instant::now() + timeout);
                    out.push((
                        *id,
                        read_node_raw(&app, path, dl, quirks, &error_budget).await,
                    ));
                }
                out
            }));
        }
        let mut all = Vec::with_capacity(targets.len());
        for t in tasks {
            if let Ok(v) = t.await {
                all.extend(v);
            }
        }
        all
    }

    async fn read_text(&self, path: &str) -> Option<Arc<str>> {
        let m = self
            .app
            .call(
                self.app.next_lane(),
                self.call_deadline(),
                path,
                TEXT,
                "GetText",
                &(0i32, -1i32),
            )
            .await
            .ok()?;
        wire::parse_string(m.body().data().bytes())
            .ok()
            .map(Arc::from)
    }

    fn node_from_read(
        &self,
        id: SrcNodeId,
        read: ReadTuple,
        text: Option<Arc<str>>,
        id_fresh: bool,
        local: Option<gorca_model::LocalId>,
    ) -> (SrcNode, SibInfo) {
        let (raw_role, state, name, attrs, _) = read;
        let role = roles::map_role(raw_role);
        let rec = {
            let pairs: SmallVec<[(&str, &str); 16]> = attrs
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            crate::norm::record(raw_role, Some(state), name.as_deref(), &pairs)
        };
        let authored = attrs
            .iter()
            .any(|(k, v)| k == "explicit-name" && v == "true");
        let name = self.config.fetch.quirks.name(role, authored, name);
        let mut amap: SmallVec<[(gorca_model::AttrKey, Arc<str>); 4]> = SmallVec::new();
        for (k, v) in attrs {
            amap.push((self.interner.key(&k), Arc::from(v.as_str())));
        }
        if role == Role::Unknown {
            amap.push((
                web_attrs::RAW_ROLE,
                Arc::from(raw_role.to_string().as_str()),
            ));
        }
        let tag = amap
            .iter()
            .find(|(k, _)| *k == web_attrs::TAG)
            .map(|(_, v)| Arc::clone(v));
        let dom_id = amap
            .iter()
            .find(|(k, _)| *k == web_attrs::DOM_ID)
            .map(|(_, v)| Arc::clone(v));
        let info = SibInfo {
            sig: Sig {
                role: raw_role,
                tag,
                name: name.clone(),
            },
            // Chromium leaves a stale `id` on an object whose attribute changed in place (WF-7): an id
            // re-read from an existing object is never trusted, only ids of new objects are.
            dom_id_fresh: id_fresh && dom_id.is_some() && self.config.use_dom_id,
            dom_id: dom_id.filter(|_| self.config.use_dom_id),
        };
        (
            SrcNode {
                id,
                role,
                states: self
                    .config
                    .fetch
                    .quirks
                    .states(role, roles::map_web_states(state)),
                name,
                text,
                attrs: amap,
                local,
                rec: Some(rec),
            },
            info,
        )
    }

    /// Fetch the subtrees of `paths` and return them as one forest (empty forest for no paths).
    async fn fetch_added(
        &self,
        live: &mut Live,
        paths: &[PathKey],
    ) -> Result<FetchedDoc, FetchError> {
        if paths.is_empty() {
            return Ok(FetchedDoc::empty());
        }
        let strs: Vec<String> = paths
            .iter()
            .filter_map(|p| live.paths.path_of(*p))
            .collect();
        let mut opts = self.config.fetch.clone();
        opts.budget = Duration::from_secs(5);
        fetch_forest(&self.app, &self.interner, &mut live.paths, &strs, &opts).await
    }

    /// One settled burst: plan, refetch what changed, push deltas.
    async fn process_dirty(&mut self, live: &mut Live, ctx: &DocSourceCtx) {
        self.process_dirty_with(live, ctx, Vec::new(), false).await;
    }

    /// `process_dirty` that starts from `carry` (deltas already decided, a repair's difference)
    /// and pushes everything as one batch ending in one `Settled`; `force_emit` pushes even when
    /// the batch is empty (the repair's waiter needs its `Settled`).
    async fn process_dirty_with(
        &mut self,
        live: &mut Live,
        ctx: &DocSourceCtx,
        carry: Vec<DocDelta>,
        force_emit: bool,
    ) {
        while let Ok(e) = self.events.try_recv() {
            note(live, &e);
        }
        let taken = live.dirty.take();
        if live.flight_active {
            for path in taken.map.keys() {
                if live.touched.len() >= MAX_TOUCHED {
                    live.touched_overflow = true;
                    break;
                }
                live.touched.insert(*path);
            }
        }
        let planned_at = Instant::now();
        let mut rec = UpdateRecord {
            events: taken.events,
            dirty_nodes: taken.map.len(),
            first_event: taken.first_event,
            last_event: taken.last_event,
            planned_at,
            pushed_at: planned_at,
            parents_refreshed: 0,
            props_refreshed: 0,
            nodes_fetched: 0,
            deltas: 0,
            stats: MatchStats::default(),
            resync: false,
            rewrite: false,
            rewrite_note: None,
            errors: 0,
        };
        let mut deltas: Vec<DocDelta> = carry;

        if taken.map.len() > self.config.resync_threshold {
            rec.resync = true;
            live.resync_wanted = true;
        } else {
            // Resolve to logical ids; unknown paths are inside subtrees we have not seen yet.
            let mut child_targets: Vec<(usize, SrcNodeId)> = Vec::new();
            let mut prop_targets: Vec<SrcNodeId> = Vec::new();
            let mut text_targets: Vec<SrcNodeId> = Vec::new();
            for (path, bits) in &taken.map {
                let Some(id) = live.ident.id_of(*path) else {
                    continue;
                };
                if bits.contains(Dirty::CHILDREN) {
                    child_targets.push((live.ident.depth_of(id), id));
                }
                if bits.contains(Dirty::PROPS) {
                    prop_targets.push(id);
                }
                if bits.contains(Dirty::TEXT) {
                    text_targets.push(id);
                }
            }
            child_targets.sort_unstable();
            let mut fresh: HashSet<SrcNodeId> = HashSet::new();

            // Property refreshes first: they may reveal an in-place rewrite of a sibling list.
            let mut by_parent: HashMap<SrcNodeId, Vec<(SrcNodeId, SrcNode, SibInfo)>> =
                HashMap::new();
            let reads_in: Vec<(SrcNodeId, String)> = prop_targets
                .iter()
                .filter_map(|id| {
                    live.ident
                        .path_of(*id)
                        .and_then(|p| live.paths.path_of(p))
                        .map(|p| (*id, p))
                })
                .collect();
            for (id, read) in self.read_nodes(reads_in).await {
                let Some(read) = read else {
                    rec.errors += 1;
                    continue;
                };
                rec.props_refreshed += 1;
                if let Some(t) = &read.4 {
                    let tk = live.paths.intern(t);
                    if let Some(f) = live.ident.path_of(id) {
                        live.paths.note_error_target(tk, f);
                    }
                }
                let local = live
                    .ident
                    .path_of(id)
                    .and_then(|p| live.paths.path_of(p))
                    .map(|p| gorca_dbus::drain::local_id(&p));
                let (node, info) = self.node_from_read(id, read, None, false, local);
                let parent = live.ident.parent_of(id).unwrap_or(id);
                by_parent.entry(parent).or_default().push((id, node, info));
            }
            let mut rewrite_parents: Vec<(SrcNodeId, HashSet<PathKey>)> = Vec::new();
            let mut plain_upserts: Vec<SrcNode> = Vec::new();
            for (parent, items) in by_parent {
                let changed: Vec<&(SrcNodeId, SrcNode, SibInfo)> = items
                    .iter()
                    .filter(|(id, _, info)| {
                        live.ident
                            .info_of(*id)
                            .is_none_or(|old| old.sig != info.sig)
                    })
                    .collect();
                // Rewrite = signatures moved between siblings (the new signature of at least two
                // changed siblings is the old signature of another sibling). Novel signatures are
                // plain edits: the logical item stays, only its content changed.
                let old_sigs: HashSet<Sig> = live
                    .ident
                    .child_infos(parent)
                    .into_iter()
                    .map(|(_, i)| i.sig)
                    .collect();
                let moved = changed
                    .iter()
                    .filter(|(_, _, i)| old_sigs.contains(&i.sig))
                    .count();
                if changed.len() >= 2 && moved >= 2 && parent != changed[0].0 {
                    // IL-1 signature check: two siblings changed signature in one burst.
                    let force: HashSet<PathKey> = live
                        .ident
                        .child_infos(parent)
                        .into_iter()
                        .map(|(p, _)| p)
                        .collect();
                    if rec.rewrite_note.is_none() {
                        let old = live.ident.info_of(changed[0].0).map(|i| i.sig.clone());
                        rec.rewrite_note = Some(format!(
                            "parent {:?}: {} of {} siblings changed; first {:?} -> {:?}",
                            parent,
                            changed.len(),
                            items.len(),
                            old,
                            changed[0].2.sig
                        ));
                    }
                    rewrite_parents.push((parent, force));
                } else {
                    for (id, node, info) in items {
                        live.ident.set_info(id, info);
                        plain_upserts.push(node);
                    }
                }
            }
            if !plain_upserts.is_empty() {
                // Keep text: with the text tier on, re-read it so the Upsert does not clear it.
                if self.config.fetch.text == TextPolicy::All {
                    for n in &mut plain_upserts {
                        if let Some(p) =
                            live.ident.path_of(n.id).and_then(|p| live.paths.path_of(p))
                        {
                            n.text = self.read_text(&p).await;
                        }
                    }
                }
                deltas.push(DocDelta::Upsert(plain_upserts));
            }

            // Structural refreshes, top-down. A rewrite forces re-matching of the sibling list.
            let mut forced: HashMap<SrcNodeId, HashSet<PathKey>> =
                rewrite_parents.into_iter().collect();
            rec.rewrite = !forced.is_empty();
            for (_, id) in &child_targets {
                forced.entry(*id).or_default();
            }
            let mut order: Vec<(usize, SrcNodeId)> = forced
                .keys()
                .map(|id| (live.ident.depth_of(*id), *id))
                .collect();
            order.sort_unstable();
            for (_, id) in order {
                if fresh.contains(&id) || live.ident.path_of(id).is_none() {
                    continue;
                }
                let force = forced.remove(&id).unwrap_or_default();
                self.refresh_children(live, id, &force, &mut deltas, &mut rec, &mut fresh)
                    .await;
            }

            // Text.
            if self.config.fetch.text == TextPolicy::All {
                for id in text_targets {
                    if fresh.contains(&id) {
                        continue;
                    }
                    let Some(path) = live.ident.path_of(id).and_then(|p| live.paths.path_of(p))
                    else {
                        continue;
                    };
                    if let Some(t) = self.read_text(&path).await {
                        deltas.push(DocDelta::Text {
                            node: id,
                            edit: TextEdit::Replace(t),
                        });
                    }
                }
            }
        }

        if force_emit {
            deltas = net_effect(deltas);
        }
        rec.deltas = deltas.len();
        if force_emit || !deltas.is_empty() {
            live.out.emit(&ctx.sink, deltas);
        }
        rec.pushed_at = Instant::now();
        self.metrics.push(rec);
        live.batch += 1;
    }

    async fn refresh_children(
        &self,
        live: &mut Live,
        id: SrcNodeId,
        force: &HashSet<PathKey>,
        deltas: &mut Vec<DocDelta>,
        rec: &mut UpdateRecord,
        fresh: &mut HashSet<SrcNodeId>,
    ) {
        let Some(path) = live.ident.path_of(id).and_then(|p| live.paths.path_of(p)) else {
            return;
        };
        let is_root = live.ident.parent_of(id).is_none();
        let new_paths = if let Ok(v) = self.list_children(live, &path).await {
            if is_root {
                live.root_errors = 0;
            }
            v
        } else {
            rec.errors += 1;
            if is_root {
                live.root_errors += 1;
            }
            return;
        };
        rec.parents_refreshed += 1;
        let unknown = live.ident.unknown_paths(&new_paths, force);
        if unknown.is_empty() && force.is_empty() {
            let burst = live.ident.classify(id, &new_paths);
            if burst == crate::identity::Burst::Quiet {
                return;
            }
        }
        let Ok(added) = self.fetch_added(live, &unknown).await else {
            rec.errors += 1;
            return;
        };
        register_locals(live, &added);
        rec.nodes_fetched += added.block.node_count();
        let out = live.ident.apply_children(id, &new_paths, &added, force);
        rec.stats.add(&out.stats);
        for n in &out.upserts {
            fresh.insert(n.id);
        }
        if !out.upserts.is_empty() {
            deltas.push(DocDelta::Upsert(out.upserts));
        }
        for (parent, children) in out.children {
            deltas.push(DocDelta::Children { parent, children });
        }
        if !out.removed.is_empty() {
            deltas.push(DocDelta::Removed(out.removed));
        }
    }

    /// Match a refetched document against the mirror with the identity ladder, so logical ids
    /// survive. Only a changed document root (a navigation) resets.
    fn apply_resync(
        &mut self,
        live: &mut Live,
        doc: &FetchedDoc,
        deltas: &mut Vec<DocDelta>,
        rec: &mut UpdateRecord,
    ) {
        rec.nodes_fetched = doc.block.node_count();
        register_locals(live, doc);
        let root_path = doc.keys.first().copied();
        let root_id = root_path.and_then(|p| live.ident.id_of(p));
        if let Some(root) = root_id {
            let out = live.ident.resync_subtree(root, doc);
            rec.stats.add(&out.stats);
            if !out.upserts.is_empty() {
                deltas.push(DocDelta::Upsert(out.upserts));
            }
            for (parent, children) in out.children {
                deltas.push(DocDelta::Children { parent, children });
            }
            if !out.removed.is_empty() {
                deltas.push(DocDelta::Removed(out.removed));
            }
        } else {
            live.ident.load(doc);
            self.config.nav_id += 1;
            deltas.extend(
                doc.block
                    .clone()
                    .into_deltas(Some(self.config.nav_id), self.config.chunk),
            );
        }
    }

    /// Start the background refetch of the whole document, or fold the request into the one that
    /// runs. `waiter` is answered with the report when the difference is in the sink.
    fn start_flight(
        &self,
        live: &mut Live,
        flight: &mut Option<Flight>,
        waiter: Option<gorca_doc_source_api::Reply<RepairReport>>,
    ) {
        if let Some(f) = flight.as_mut() {
            match waiter {
                Some(w) => f.waiters.push(w),
                None => f.again = true,
            }
            return;
        }
        live.flight_active = true;
        live.touched.clear();
        live.touched_overflow = false;
        let app = Arc::clone(&self.app);
        let interner = Arc::clone(&self.interner);
        let mut paths = live.paths.fork();
        let root_path = self.root_path.clone();
        let mut opts = self.config.fetch.clone();
        opts.budget = Duration::from_secs(10);
        opts.heavy = Arc::new(live.heavy.clone());
        let task = tokio::spawn(async move {
            let d = fetch_document(&app, &interner, &mut paths, &root_path, &opts).await?;
            Ok((d, paths))
        });
        *flight = Some(Flight {
            task,
            started: Instant::now(),
            waiters: waiter.into_iter().collect(),
            again: false,
        });
    }

    /// The background refetch ended: apply it, push the difference followed by `Settled` (also
    /// when nothing changed, so the waiter wakes), answer the waiters.
    async fn finish_flight(
        &mut self,
        live: &mut Live,
        ctx: &DocSourceCtx,
        flight: &mut Option<Flight>,
        mut fl: Flight,
        res: Result<Result<(FetchedDoc, PathTable), FetchError>, tokio::task::JoinError>,
    ) {
        let mut rec = UpdateRecord {
            events: 0,
            dirty_nodes: 0,
            first_event: None,
            last_event: None,
            planned_at: fl.started,
            pushed_at: Instant::now(),
            parents_refreshed: 0,
            props_refreshed: 0,
            nodes_fetched: 0,
            deltas: 0,
            stats: MatchStats::default(),
            resync: true,
            rewrite: false,
            rewrite_note: None,
            errors: 0,
        };
        let mut deltas = Vec::new();
        let mut report = RepairReport::default();
        if let Ok(Ok((doc, paths))) = res {
            live.paths.absorb(paths);
            for r in &doc.timings.heavy_roots {
                if live.heavy.len() < 64 {
                    live.heavy.insert(r.clone());
                }
            }
            tracing::info!(
                target: "gorca_phase",
                phase = "repair_fetch",
                wall_ms = fl.started.elapsed().as_secs_f64() * 1000.0,
                nodes = doc.block.node_count(),
                heaviest_get_tree_ms = doc.timings.heaviest_get_tree.as_secs_f64() * 1000.0,
                heavy_roots = doc.timings.heavy_roots.len(),
            );
            report.nodes = doc.block.node_count();
            self.apply_resync(live, &doc, &mut deltas, &mut rec);
        } else {
            rec.errors += 1;
            report.failed = true;
        }
        rec.deltas = deltas.len();
        report.deltas = deltas.len();
        rec.pushed_at = Instant::now();
        report.took = fl.started.elapsed();
        self.metrics.push(rec);
        if live.root_errors >= ROOT_ERROR_LIMIT {
            // The document object is gone: the doc actor attaches a new source.
            live.root_errors = ROOT_ERROR_LIMIT;
        }
        // Updates applied while the snapshot was being fetched are newer than it: read those
        // nodes again so the older snapshot cannot leave them reverted.
        live.flight_active = false;
        let at = Instant::now();
        for path in std::mem::take(&mut live.touched) {
            for kind in [
                EventKind::ChildAdded {
                    index: -1,
                    child: None,
                },
                EventKind::StateChanged {
                    state: Arc::from("props"),
                    on: true,
                },
                EventKind::TextChanged { insert: true },
            ] {
                live.dirty.note(&AtspiEvent { path, kind, at });
            }
        }
        if std::mem::take(&mut live.touched_overflow) {
            fl.again = true;
        }
        // The repair's difference and the re-read of the touched nodes go out as one batch with
        // one `Settled`: the Reader never sees an update vanish and come back (review D-1).
        self.process_dirty_with(live, ctx, deltas, true).await;
        if live.resync_wanted {
            live.resync_wanted = false;
            fl.again = true;
        }
        for w in std::mem::take(&mut fl.waiters) {
            let _ = w.send(Ok(report));
        }
        if fl.again {
            self.start_flight(live, flight, None);
        }
    }

    async fn handle_request(
        &mut self,
        live: &mut Live,
        ctx: &DocSourceCtx,
        flight: &mut Option<Flight>,
        req: DocRequest,
    ) {
        match req {
            DocRequest::Subtree {
                root,
                max_nodes,
                deadline,
                reply,
            } => {
                let r = self.subtree(live, root, max_nodes, deadline).await;
                let _ = reply.send(r);
            }
            DocRequest::Text {
                node,
                range,
                deadline,
                reply,
            } => {
                let r = self
                    .text(live, node, range.start, range.end, deadline)
                    .await;
                let _ = reply.send(r);
            }
            DocRequest::Action {
                node,
                act,
                deadline,
                reply,
            } => {
                if matches!(act, DocAction::Click | DocAction::ClickAnnounced { .. })
                    && let Some(now) = self.displaced(live, node, &act).await
                {
                    let _ = reply.send(Err(SourceError::Displaced(now)));
                    return;
                }
                let mut r = self.action(live, node, &act, deadline).await;
                if matches!(r, Err(SourceError::Gone))
                    && matches!(
                        act,
                        DocAction::Click | DocAction::ClickAnnounced { .. } | DocAction::Focus
                    )
                {
                    r = self
                        .reresolve_and_retry(live, ctx, node, &act, deadline)
                        .await;
                }
                let _ = reply.send(r);
            }
            DocRequest::Revalidate {
                parent,
                deadline,
                reply,
            } => {
                let r = self.revalidate(live, ctx, parent, deadline).await;
                let _ = reply.send(r);
            }
            DocRequest::Validate {
                nodes,
                deadline,
                reply,
            } => {
                let mut out = Vec::with_capacity(nodes.len());
                for n in &nodes {
                    out.push(self.validate(live, *n, deadline).await);
                }
                let _ = reply.send(Ok(out));
            }
            DocRequest::Extents {
                nodes,
                deadline,
                reply,
            } => {
                // Off the request loop (a Pull or an Act never waits behind it), one at a time
                // per document: a newer request supersedes the older, whose reply is dropped.
                let targets = Self::extent_targets(live, &nodes);
                let app = Arc::clone(&self.app);
                if let Some(old) = live.extents_task.take() {
                    old.abort();
                }
                live.extents_task = Some(tokio::spawn(async move {
                    let r = measure(&app, targets, deadline).await;
                    let _ = reply.send(Ok(r));
                }));
            }
            DocRequest::SetActivity(a) => {
                let was = live.activity;
                live.activity = a;
                // M7a: waking always rebuilds (AM-13); ids are kept, W0 was served from the
                // snapshot while the document slept.
                if was == Activity::Dormant && a == Activity::Awake {
                    // Whatever was marked while dormant is covered by the refetch.
                    let _ = live.dirty.take();
                    self.start_flight(live, flight, None);
                }
            }
            DocRequest::Rebuild => {
                let _ = live.dirty.take();
                self.start_flight(live, flight, None);
            }
            DocRequest::Repair { cheap_check, reply } => {
                if cheap_check && flight.is_none() {
                    let t = Instant::now();
                    // Same, or a user request is waiting (skipped: the next guess looks again).
                    if self.top_unchanged(live, ctx).await != Look::Changed {
                        let _ = reply.send(Ok(RepairReport {
                            took: t.elapsed(),
                            ..RepairReport::default()
                        }));
                        return;
                    }
                }
                self.start_flight(live, flight, Some(reply));
            }
            // M7b: urgent marks and the W3 drift check.
            DocRequest::Prioritize { .. } | DocRequest::DriftCheck => {}
        }
    }

    /// IL-R1: one `GetChildren`, refetch only the children the mirror does not know, push the
    /// deltas, answer with the current child ids.
    async fn revalidate(
        &mut self,
        live: &mut Live,
        ctx: &DocSourceCtx,
        parent: SrcNodeId,
        deadline: Deadline,
    ) -> Result<SmallVec<[SrcNodeId; 16]>, SourceError> {
        let path = live
            .ident
            .path_of(parent)
            .and_then(|p| live.paths.path_of(p))
            .ok_or(SourceError::NotSupported)?;
        let m = self
            .app
            .call(
                self.app.next_lane(),
                deadline,
                &path,
                ACCESSIBLE,
                "GetChildren",
                &(),
            )
            .await
            .map_err(|e| map_dbus(&e))?;
        let kids = wire::parse_children(m.body().data().bytes())
            .map_err(|e| SourceError::Protocol(e.to_string()))?;
        let new_paths: Vec<PathKey> = kids.iter().map(|(_, p)| live.paths.intern(p)).collect();
        let none = HashSet::new();
        if live.ident.classify(parent, &new_paths) != crate::identity::Burst::Quiet {
            let unknown = live.ident.unknown_paths(&new_paths, &none);
            let added = self
                .fetch_added(live, &unknown)
                .await
                .map_err(|e| map_fetch(&e))?;
            register_locals(live, &added);
            let out = live.ident.apply_children(parent, &new_paths, &added, &none);
            let mut deltas = Vec::new();
            if !out.upserts.is_empty() {
                deltas.push(DocDelta::Upsert(out.upserts));
            }
            for (p, children) in out.children {
                deltas.push(DocDelta::Children {
                    parent: p,
                    children,
                });
            }
            if !out.removed.is_empty() {
                deltas.push(DocDelta::Removed(out.removed));
            }
            if !deltas.is_empty() {
                live.out.emit(&ctx.sink, deltas);
            }
        }
        Ok(live
            .ident
            .children_of(parent)
            .unwrap_or_default()
            .iter()
            .copied()
            .collect())
    }

    async fn subtree(
        &mut self,
        live: &mut Live,
        root: SrcNodeId,
        _max_nodes: u32,
        deadline: Deadline,
    ) -> Result<Vec<DocDelta>, SourceError> {
        let path = live
            .ident
            .path_of(root)
            .and_then(|p| live.paths.path_of(p))
            .ok_or(SourceError::NotSupported)?;
        let mut opts = self.config.fetch.clone();
        opts.budget = deadline
            .remaining(Instant::now())
            .max(Duration::from_millis(1));
        let doc = fetch_document(&self.app, &self.interner, &mut live.paths, &path, &opts)
            .await
            .map_err(|e| map_fetch(&e))?;
        let out = live.ident.resync_subtree(root, &doc);
        let mut deltas = Vec::new();
        if !out.upserts.is_empty() {
            deltas.push(DocDelta::Upsert(out.upserts));
        }
        for (parent, children) in out.children {
            deltas.push(DocDelta::Children { parent, children });
        }
        if !out.removed.is_empty() {
            deltas.push(DocDelta::Removed(out.removed));
        }
        Ok(deltas)
    }

    async fn text(
        &self,
        live: &Live,
        node: SrcNodeId,
        start: u32,
        end: u32,
        deadline: Deadline,
    ) -> Result<String, SourceError> {
        let path = live
            .ident
            .path_of(node)
            .and_then(|p| live.paths.path_of(p))
            .ok_or(SourceError::NotSupported)?;
        let (s, e) = (
            i32::try_from(start).unwrap_or(i32::MAX),
            i32::try_from(end).unwrap_or(-1),
        );
        let m = self
            .app
            .call(
                self.app.next_lane(),
                deadline,
                &path,
                TEXT,
                "GetText",
                &(s, e),
            )
            .await
            .map_err(|e| map_dbus(&e))?;
        wire::parse_string(m.body().data().bytes())
            .map(str::to_owned)
            .map_err(|e| SourceError::Protocol(e.to_string()))
    }

    async fn action(
        &self,
        live: &Live,
        node: SrcNodeId,
        act: &DocAction,
        deadline: Deadline,
    ) -> Result<(), SourceError> {
        let path = live
            .ident
            .path_of(node)
            .and_then(|p| live.paths.path_of(p))
            .ok_or(SourceError::Gone)?;
        let lane = self.app.next_lane();
        let r = match act {
            DocAction::Focus => {
                // GrabFocus answers a bool: false means the object did not take focus (R-6).
                let m = self
                    .app
                    .call(lane, deadline, &path, COMPONENT, "GrabFocus", &())
                    .await
                    .map_err(|e| map_target_err(&e))?;
                return match wire::parse_u32(m.body().data().bytes()) {
                    Ok(0) => Err(SourceError::Refused),
                    Ok(_) | Err(_) => Ok(()),
                };
            }
            DocAction::ReleaseField => return self.release_field(live, &path, deadline).await,
            DocAction::Click | DocAction::ClickAnnounced { .. } => {
                self.app
                    .call(lane, deadline, &path, ACTION, "DoAction", &(0i32,))
                    .await
            }
            DocAction::ScrollIntoView => {
                self.app
                    .call(
                        lane,
                        deadline,
                        &path,
                        COMPONENT,
                        "ScrollTo",
                        &(SCROLL_ANYWHERE,),
                    )
                    .await
            }
            DocAction::SetCaret(o) => {
                let o = i32::try_from(*o).unwrap_or(i32::MAX);
                self.app
                    .call(lane, deadline, &path, TEXT, "SetCaretOffset", &(o,))
                    .await
            }
            DocAction::SetSourceSelection(spec) => {
                return self.select(live, spec, deadline).await;
            }
            DocAction::ClearSelection => {
                return self.clear_selection(&path, deadline).await;
            }
            DocAction::SetSelection(_) => return Err(SourceError::NotSupported),
        };
        r.map(|_| ()).map_err(|e| map_target_err(&e))
    }

    /// An action found its target gone: the page replaced the node and the events have not
    /// reached the mirror yet. Refresh the target's parents (the same `GetChildren` + identity
    /// pass an event would run), and repeat the action only if the logical node continues in a
    /// live object with the same role and name. Anything else is `Gone`: an action is never
    /// redirected to a different element.
    async fn reresolve_and_retry(
        &mut self,
        live: &mut Live,
        ctx: &DocSourceCtx,
        node: SrcNodeId,
        act: &DocAction,
        deadline: Deadline,
    ) -> Result<(), SourceError> {
        let Some(before) = live.ident.info_of(node).cloned() else {
            return Err(SourceError::Gone);
        };
        let old_path = live.ident.path_of(node);
        let old_parent = live.ident.parent_of(node);
        // Siblings that look the same as the target before the refresh: with more than one, the
        // identity ladder pairs them in sibling order and a removal above the target shifts the
        // pairing by one, so the match proves nothing.
        let twins_before = old_parent.map_or(0, |p| twin_count(live, p, &before.sig));
        let mut parent = old_parent;
        for _ in 0..REFRESH_LEVELS {
            let Some(par) = parent else { break };
            if self.revalidate(live, ctx, par, deadline).await.is_ok() {
                break;
            }
            // The parent is gone too: go up.
            parent = live.ident.parent_of(par);
        }
        let Some(after) = live.ident.info_of(node).cloned() else {
            return Err(SourceError::Gone);
        };
        let moved = live
            .ident
            .path_of(node)
            .is_some_and(|p| Some(p) != old_path);
        let same_sig = after.sig.role == before.sig.role
            && after.sig.name.is_some()
            && after.sig.name == before.sig.name;
        // Unambiguous: the page's own DOM id says it is the same element, or the signature is
        // unique among the siblings both before and after.
        let same_dom_id = before.dom_id.is_some()
            && before.dom_id_fresh
            && after.dom_id == before.dom_id
            && after.dom_id_fresh;
        let unique = twins_before == 1
            && live
                .ident
                .parent_of(node)
                .is_some_and(|p| twin_count(live, p, &after.sig) == 1);
        if same_sig && moved && (same_dom_id || unique) {
            self.action(live, node, act, deadline).await
        } else {
            Err(SourceError::Gone)
        }
    }

    /// Act only on what was spoken (CHR-0): before a click, read the object's role and name again
    /// and compare them with what the mirror says (which is what the user heard). When the page
    /// re-used or re-ordered its rows since, nothing is clicked; the node is marked for a refresh
    /// and the caller says so. A read that fails or is slow lets the click go: this guard must
    /// never turn a working Enter into a silent one.
    async fn displaced(
        &self,
        live: &mut Live,
        node: SrcNodeId,
        act: &DocAction,
    ) -> Option<Option<Arc<str>>> {
        let path_key = live.ident.path_of(node)?;
        let path = live.paths.path_of(path_key)?;
        let mut old = live.ident.info_of(node).map(|i| i.sig.clone())?;
        // What the user heard replaces what the mirror last knew.
        if let DocAction::ClickAnnounced { name } = act {
            old.name.clone_from(name);
        }
        let read = tokio::time::timeout(VERIFY_TIMEOUT, self.read_nodes(vec![(node, path)])).await;
        let (_, Some(read)) = read.ok().and_then(|mut v| v.pop())? else {
            return None;
        };
        let (_, fresh) = self.node_from_read(node, read, None, false, None);
        let name_differs = match (&old.name, &fresh.sig.name) {
            (Some(a), Some(b)) => a.trim() != b.trim(),
            (Some(_) | None, None) | (None, Some(_)) => false,
        };
        let differs = old.role != fresh.sig.role || name_differs;
        if !differs {
            return None;
        }
        tracing::info!(
            target: "gorca_phase",
            phase = "click_displaced",
            role_changed = old.role != fresh.sig.role,
        );
        live.dirty.note(&AtspiEvent {
            path: path_key,
            kind: EventKind::StateChanged {
                state: Arc::from("props"),
                on: true,
            },
            at: Instant::now(),
        });
        // What the object says now: the caller speaks it, so the next Enter acts on a name the
        // user has heard.
        Some(fresh.sig.name.clone())
    }

    /// A cheap look before a guessed repair: do the child lists of the first nodes of the mirror
    /// (breadth first, [`CHEAP_NODES`] of them, [`CHEAP_BUDGET`] at most) still match the browser?
    /// A node that gained children, the empty `main` of a shell that rendered, shows here without
    /// a refetch of the whole document. Any doubt answers false (the repair runs).
    async fn top_unchanged(&self, live: &mut Live, ctx: &DocSourceCtx) -> Look {
        let Some(root) = live.root_key.and_then(|k| live.ident.id_of(k)) else {
            return Look::Changed;
        };
        let look = async {
            let mut queue = std::collections::VecDeque::from([root]);
            let mut seen = 0;
            while let Some(id) = queue.pop_front() {
                if seen >= CHEAP_NODES {
                    break;
                }
                // A click or selection is queued behind this look: give way (review D-3).
                if !ctx.requests.is_empty() {
                    return Some(Look::Busy);
                }
                seen += 1;
                let path = live.ident.path_of(id).and_then(|p| live.paths.path_of(p))?;
                let kids = self.list_children(live, &path).await.ok()?;
                let mine: Vec<SrcNodeId> = live.ident.children_of(id).unwrap_or_default().to_vec();
                if kids.len() != mine.len()
                    || kids
                        .iter()
                        .zip(&mine)
                        .any(|(k, c)| live.ident.path_of(*c) != Some(*k))
                {
                    return Some(Look::Changed);
                }
                queue.extend(mine);
            }
            Some(Look::Same)
        };
        tokio::time::timeout(CHEAP_BUDGET, look)
            .await
            .ok()
            .flatten()
            .unwrap_or(Look::Changed)
    }

    fn path_of(live: &Live, node: SrcNodeId) -> Option<String> {
        live.ident.path_of(node).and_then(|p| live.paths.path_of(p))
    }

    /// `Text.GetNSelections` and `RemoveSelection` of every range but the first: the browser
    /// keeps its ranges for the whole document, so stale extras of an earlier mirror go.
    async fn drop_extra_ranges(&self, path: &str, deadline: Deadline) -> Result<(), SourceError> {
        self.remove_ranges(path, 1, deadline).await
    }

    /// `RemoveSelection` of every range from the last down to index `keep` (0 removes all).
    async fn remove_ranges(
        &self,
        path: &str,
        keep: u32,
        deadline: Deadline,
    ) -> Result<(), SourceError> {
        let lane = self.app.next_lane();
        let m = self
            .app
            .call(lane, deadline, path, TEXT, "GetNSelections", &())
            .await
            .map_err(|e| map_dbus(&e))?;
        let n = wire::parse_u32(m.body().data().bytes())
            .unwrap_or(0)
            .min(64);
        for i in (keep..n).rev() {
            let i = i32::try_from(i).unwrap_or(1);
            self.app
                .call(
                    self.app.next_lane(),
                    deadline,
                    path,
                    TEXT,
                    "RemoveSelection",
                    &(i,),
                )
                .await
                .map_err(|e| map_dbus(&e))?;
        }
        Ok(())
    }

    /// One `Text.SetSelection(0, ..)` or `AddSelection`; the reply is the browser's yes or no.
    async fn range_call(
        &self,
        path: &str,
        first: bool,
        range: &std::ops::Range<u32>,
        deadline: Deadline,
    ) -> Result<bool, SourceError> {
        let (a, b) = (
            i32::try_from(range.start).unwrap_or(0),
            i32::try_from(range.end).unwrap_or(0),
        );
        let lane = self.app.next_lane();
        let m = if first {
            self.app
                .call(lane, deadline, path, TEXT, "SetSelection", &(0i32, a, b))
                .await
        } else {
            self.app
                .call(lane, deadline, path, TEXT, "AddSelection", &(a, b))
                .await
        }
        .map_err(|e| map_dbus(&e))?;
        Ok(wire::parse_u32(m.body().data().bytes()).unwrap_or(0) != 0)
    }

    /// Mirror a page selection.
    ///
    /// Blink takes the exact interval in one call (`Document.SetTextSelections`, ATK 2.52+,
    /// offsets in scalars) and keeps a single range with exclusive ends, so when the call
    /// answers false the rounded-out single range is the fallback. Gecko adds ranges, so it gets
    /// the exact legs (UTF-16 offsets, ends adapted by `gecko_range`), and the single range when
    /// it refuses a second. A plan that had to fall back, or that Gecko cannot take exactly, ends
    /// `Approximate`. Never leaves a half set.
    async fn select(
        &self,
        live: &Live,
        spec: &gorca_model::SelectionSpec<SrcNodeId>,
        deadline: Deadline,
    ) -> Result<(), SourceError> {
        let blink = self.config.quirks.selection_single_exclusive;
        let root = Self::path_of(live, spec.root);
        if let Some(root) = root.as_deref() {
            self.drop_extra_ranges(root, deadline).await?;
        }
        let mut approximate = false;
        if blink {
            if let (Some(root), Some(start), Some(end)) = (
                root.as_deref(),
                Self::path_of(live, spec.doc_range.start.0),
                Self::path_of(live, spec.doc_range.end.0),
            ) && self
                .set_text_selections(
                    root,
                    (&start, spec.doc_range.start.1),
                    (&end, spec.doc_range.end.1),
                    deadline,
                )
                .await?
            {
                return Ok(());
            }
            approximate = true;
        } else if let Some(root) = root.as_deref()
            && !spec.spans.is_empty()
        {
            let mut all = true;
            for (i, sp) in spec.spans.iter().enumerate() {
                let Some(p) = Self::path_of(live, sp.node) else {
                    all = false;
                    break;
                };
                let range = gecko_range(sp);
                if !self.range_call(&p, i == 0, &range, deadline).await? {
                    all = false;
                    break;
                }
            }
            if all {
                return if spec.gecko_lossy {
                    Err(SourceError::Approximate)
                } else {
                    Ok(())
                };
            }
            self.drop_extra_ranges(root, deadline).await?;
            approximate = true;
        }
        let sp = &spec.snap;
        let path = Self::path_of(live, sp.node).ok_or(SourceError::NotSupported)?;
        let range = if blink {
            sp.range_cp.clone()
        } else {
            gecko_range(sp)
        };
        if self.range_call(&path, true, &range, deadline).await? {
            if approximate || spec.gecko_lossy {
                Err(SourceError::Approximate)
            } else {
                Ok(())
            }
        } else {
            Err(SourceError::Refused)
        }
    }

    /// Ctrl+C copies the selection of the focused `input` or `textarea`, not the document's, so
    /// a page selection set while such a control has the focus would never reach the clipboard
    /// (measured on Firefox 153: "No selection owner available"). `field` is the node that holds
    /// the focus; when its `tag` is `input` or `textarea` the focus moves to the document. A
    /// contenteditable host copies the document selection and keeps its focus. The answer says
    /// whether the focus moved.
    async fn release_field(
        &self,
        live: &Live,
        field_path: &str,
        deadline: Deadline,
    ) -> Result<(), SourceError> {
        let attrs = self
            .app
            .call(
                self.app.next_lane(),
                deadline,
                field_path,
                ACCESSIBLE,
                "GetAttributes",
                &(),
            )
            .await
            .map_err(|e| map_target_err(&e))
            .map(|m| wire::parse_string_map(m.body().data().bytes()).unwrap_or_default())?;
        let plain = attrs
            .iter()
            .any(|(k, v)| k == "tag" && matches!(v.as_str(), "input" | "textarea"));
        if !plain {
            return Ok(());
        }
        let root = live
            .root_key
            .and_then(|k| live.paths.path_of(k))
            .ok_or(SourceError::Gone)?;
        let m = self
            .app
            .call(
                self.app.next_lane(),
                deadline,
                &root,
                COMPONENT,
                "GrabFocus",
                &(),
            )
            .await
            .map_err(|e| map_target_err(&e))?;
        tracing::info!(target: "gorca_phase", phase = "release_text_control");
        match wire::parse_u32(m.body().data().bytes()) {
            Ok(0) => Err(SourceError::Refused),
            Ok(_) | Err(_) => Ok(()),
        }
    }

    /// `Document.SetTextSelections` with one range from `start` to `end` (object path and offset
    /// in scalars each). The bus name inside an object reference is ignored by Chromium
    /// (measured), the path is what counts.
    async fn set_text_selections(
        &self,
        document: &str,
        start: (&str, u32),
        end: (&str, u32),
        deadline: Deadline,
    ) -> Result<bool, SourceError> {
        use zbus::zvariant::ObjectPath;
        let name = self.app.bus_name();
        let (Ok(a), Ok(b)) = (ObjectPath::try_from(start.0), ObjectPath::try_from(end.0)) else {
            return Ok(false);
        };
        let range = vec![(
            (name.clone(), a),
            i32::try_from(start.1).unwrap_or(0),
            (name, b),
            i32::try_from(end.1).unwrap_or(0),
            true,
        )];
        let m = self
            .app
            .call(
                self.app.next_lane(),
                deadline,
                document,
                "org.a11y.atspi.Document",
                "SetTextSelections",
                &(range,),
            )
            .await;
        match m {
            Ok(m) => Ok(wire::parse_u32(m.body().data().bytes()).unwrap_or(0) != 0),
            // An engine without the method (Gecko, ATK older than 2.52) answers an error: not a
            // failure of the request, the caller falls back.
            Err(e) => {
                tracing::debug!(error = %e, "SetTextSelections unavailable; using one range");
                Ok(false)
            }
        }
    }

    /// Remove every range of the document. Never `SetSelection(0, 0, 0)`: Firefox resolves
    /// offset 0 to the first text leaf and focuses its container when that is focusable (a skip
    /// link), `RemoveSelection` only edits the DOM selection.
    async fn clear_selection(&self, path: &str, deadline: Deadline) -> Result<(), SourceError> {
        self.remove_ranges(path, 0, deadline).await
    }

    /// The paths a rectangle request needs, resolved while the mirror's tables are at hand, so
    /// the calls themselves can run as a task off the request loop.
    fn extent_targets(live: &Live, probes: &[gorca_doc_source_api::ExtentProbe]) -> Vec<Target> {
        let path = |n: SrcNodeId| live.ident.path_of(n).and_then(|p| live.paths.path_of(p));
        probes
            .iter()
            .map(|p| Target {
                own: path(p.node).filter(|_| !crate::runs::is_run(p.node)),
                parent: path(p.parent),
                range: p.range.clone(),
            })
            .collect()
    }

    /// IR-4: a heuristic join is confirmed when the object still exists with the same role.
    async fn validate(&self, live: &Live, node: SrcNodeId, deadline: Deadline) -> bool {
        let Some(path) = live.ident.path_of(node).and_then(|p| live.paths.path_of(p)) else {
            return false;
        };
        let Some(old) = live.ident.info_of(node).map(|i| i.sig.role) else {
            return false;
        };
        match self
            .app
            .call(
                self.app.next_lane(),
                deadline,
                &path,
                ACCESSIBLE,
                "GetRole",
                &(),
            )
            .await
        {
            Ok(m) => wire::parse_u32(m.body().data().bytes()).ok() == Some(old),
            Err(_) => false,
        }
    }
}

/// Where one rectangle of a [`DocRequest::Extents`] is read.
struct Target {
    /// The object's own path (not for a text run).
    own: Option<String>,
    /// The container of a run, and the run's scalar range in it.
    parent: Option<String>,
    range: std::ops::Range<u32>,
}

async fn rect_call(
    app: &AppConn,
    deadline: Deadline,
    path: &str,
    iface: &str,
    member: &str,
    body: &(impl serde::Serialize + zbus::zvariant::DynamicType),
) -> Option<gorca_model::Rect> {
    let m = app
        .call(app.next_lane(), deadline, path, iface, member, body)
        .await
        .ok()?;
    let (x, y, w, h): (i32, i32, i32, i32) = m.body().deserialize().ok()?;
    Some(gorca_model::Rect {
        x,
        y,
        w: u32::try_from(w).ok()?,
        h: u32::try_from(h).ok()?,
    })
}

/// The screen rectangles of `targets` (`Component.GetExtents` of the object, or
/// `Text.GetRangeExtents` of the run inside its container when the node is a text run with no
/// object, falling back to the container's box when the engine answers an empty one), one call
/// each, in order and never past `deadline`. A node that is unknown or whose call fails has
/// `None`.
async fn measure(
    app: &AppConn,
    targets: Vec<Target>,
    deadline: Deadline,
) -> SmallVec<[Option<gorca_model::Rect>; 6]> {
    let mut out = SmallVec::new();
    for t in targets {
        let r = if let Some(path) = &t.own {
            rect_call(app, deadline, path, COMPONENT, "GetExtents", &(0u32,)).await
        } else if let Some(parent) = &t.parent {
            let range = (
                i32::try_from(t.range.start).ok(),
                i32::try_from(t.range.end).ok(),
            );
            let run = match range {
                (Some(a), Some(b)) => {
                    rect_call(
                        app,
                        deadline,
                        parent,
                        TEXT,
                        "GetRangeExtents",
                        &(a, b, 0u32),
                    )
                    .await
                }
                _ => None,
            };
            match run {
                Some(r) if r.w > 0 && r.h > 0 => Some(r),
                // Gecko answers an empty box for a range it has not laid out for the reader:
                // the container's own box is the best that is left.
                _ => rect_call(app, deadline, parent, COMPONENT, "GetExtents", &(0u32,)).await,
            }
        } else {
            None
        };
        out.push(r);
    }
    out
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use gorca_model::SelSpan;

    fn sp(
        r: std::ops::Range<u32>,
        end_on_child: bool,
        child_follows: bool,
        trim_ws: bool,
    ) -> SelSpan<SrcNodeId> {
        SelSpan {
            node: SrcNodeId(1),
            range_cp: r.clone(),
            end_on_object: false,
            range: r,
            end_on_child,
            child_follows,
            trim_ws,
        }
    }

    #[test]
    fn gecko_names_the_last_child_and_leaves_out_a_space_before_one() {
        // Measured on Firefox 153 ESR: an end offset that is a child's character includes the child.
        assert_eq!(gecko_range(&sp(2..4, true, false, false)), 2..3);
        assert_eq!(gecko_range(&sp(1..2, true, false, false)), 1..1);
        assert_eq!(gecko_range(&sp(0..5, false, true, true)), 0..4);
        // A letter before the child stays, and the child comes along (nothing else is possible).
        assert_eq!(gecko_range(&sp(0..5, false, true, false)), 0..5);
        // An image is not hypertext: ending after it needs no adjustment.
        let mut img = sp(0..6, false, false, false);
        img.end_on_object = true;
        assert_eq!(gecko_range(&img), 0..6);
        assert_eq!(gecko_range(&sp(6..24, false, false, false)), 6..24);
    }
}
