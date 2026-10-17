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
    JoinKeyCaps, SourceCaps, SourceError, SourceKind, web_attrs,
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
use crate::wire::{self, AttrInterner, PathKey};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const TEXT: &str = "org.a11y.atspi.Text";
const COMPONENT: &str = "org.a11y.atspi.Component";
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
    /// Chromium reorders keyed lists without an event (`SourceCaps::silent_reorders`).
    pub silent_reorders: bool,
    /// Events that did not fit while dormant, after which a wake rebuilds from scratch anyway.
    pub dormant_marks: usize,
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
            silent_reorders: chromium,
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
            silent_reorders: false,
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
            },
            metrics,
        )
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
            silent_reorders: self.config.silent_reorders,
        }
    }

    fn attach(self: Box<Self>, ctx: DocSourceCtx) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move { (*self).run(ctx).await })
    }
}

fn map_dbus(e: &DbusError) -> SourceError {
    match e {
        DbusError::Timeout => SourceError::Timeout,
        DbusError::Zbus(z) => SourceError::Protocol(z.to_string()),
    }
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
type ReadTuple = (u32, u64, Option<Arc<str>>, Vec<(String, String)>);

/// Re-read role, state, name and attributes of one object (four pipelined calls).
async fn read_node_raw(app: &AppConn, path: &str, dl: Deadline) -> Option<ReadTuple> {
    let lane = app.next_lane();
    let role = app.call(lane, dl, path, ACCESSIBLE, "GetRole", &());
    let state = app.call(lane + 1, dl, path, ACCESSIBLE, "GetState", &());
    let name = app.call(lane + 2, dl, path, PROPERTIES, "Get", &(ACCESSIBLE, "Name"));
    let attrs = app.call(lane + 3, dl, path, ACCESSIBLE, "GetAttributes", &());
    let (role, state, name, attrs) = tokio::join!(role, state, name, attrs);
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
    let attrs = attrs
        .ok()
        .and_then(|m| wire::parse_string_map(m.body().data().bytes()).ok())
        .unwrap_or_default();
    Some((role, state, name, attrs))
}

/// Mutable per-document state of the running source.
struct Live {
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
}

/// Root list failures before the source gives up the document.
const ROOT_ERROR_LIMIT: u32 = 3;

impl AtspiDocSource {
    async fn run(mut self, mut ctx: DocSourceCtx) -> Result<(), SourceError> {
        let mut live = Live {
            ident: IdentityMap::new(self.config.use_dom_id),
            paths: PathTable::default(),
            dirty: DirtySet::default(),
            batch: 0,
            root_errors: 0,
            root_key: None,
            out: Pusher {
                runs: (self.config.runs && self.config.fetch.text == TextPolicy::All)
                    .then(RunLayer::new),
                batch: 0,
            },
            locals: HashMap::new(),
            activity: Activity::Awake,
        };

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
                    self.handle_request(&mut live, &ctx, req).await;
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
        live.root_key = docs.first().and_then(|d| d.keys.first().copied());
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
        let workers = (self.app.lanes() * 4).min(targets.len()).max(1);
        let mut tasks = Vec::with_capacity(workers);
        for _ in 0..workers {
            let app = Arc::clone(&self.app);
            let targets = Arc::clone(&targets);
            let cursor = Arc::clone(&cursor);
            let timeout = self.config.call_timeout;
            tasks.push(tokio::spawn(async move {
                let mut out = Vec::new();
                loop {
                    let k = cursor.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((id, path)) = targets.get(k) else {
                        break;
                    };
                    let dl = Deadline(Instant::now() + timeout);
                    out.push((*id, read_node_raw(&app, path, dl).await));
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
        let (raw_role, state, name, attrs) = read;
        let role = roles::map_role(raw_role);
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
                states: roles::map_states(state),
                name,
                text,
                attrs: amap,
                local,
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
        while let Ok(e) = self.events.try_recv() {
            note(live, &e);
        }
        let taken = live.dirty.take();
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
        let mut deltas: Vec<DocDelta> = Vec::new();

        if taken.map.len() > self.config.resync_threshold {
            rec.resync = true;
            self.full_resync(live, &mut deltas, &mut rec).await;
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

        rec.deltas = deltas.len();
        if !deltas.is_empty() {
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

    /// Overload response: refetch the whole document and re-match it against the mirror with the
    /// identity ladder, so logical ids survive. Only a changed document root (a navigation) resets.
    async fn full_resync(
        &mut self,
        live: &mut Live,
        deltas: &mut Vec<DocDelta>,
        rec: &mut UpdateRecord,
    ) {
        let mut opts = self.config.fetch.clone();
        opts.budget = Duration::from_secs(10);
        let Ok(doc) = fetch_document(
            &self.app,
            &self.interner,
            &mut live.paths,
            &self.root_path,
            &opts,
        )
        .await
        else {
            rec.errors += 1;
            return;
        };
        rec.nodes_fetched = doc.block.node_count();
        register_locals(live, &doc);
        let root_path = doc.keys.first().copied();
        let root_id = root_path.and_then(|p| live.ident.id_of(p));
        if let Some(root) = root_id {
            let out = live.ident.resync_subtree(root, &doc);
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
            live.ident.load(&doc);
            self.config.nav_id += 1;
            deltas.extend(
                doc.block
                    .into_deltas(Some(self.config.nav_id), self.config.chunk),
            );
        }
    }

    async fn handle_request(&mut self, live: &mut Live, ctx: &DocSourceCtx, req: DocRequest) {
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
                let r = self.action(live, node, &act, deadline).await;
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
            DocRequest::SetActivity(a) => {
                let was = live.activity;
                live.activity = a;
                // M7a: waking always rebuilds (AM-13); ids are kept, W0 was served from the
                // snapshot while the document slept.
                if was == Activity::Dormant && a == Activity::Awake {
                    self.rebuild(live, ctx).await;
                }
            }
            DocRequest::Rebuild => self.rebuild(live, ctx).await,
            // M7b: urgent marks and the W3 drift check.
            DocRequest::Prioritize { .. } | DocRequest::DriftCheck => {}
        }
    }

    /// Rebuild as resync (AM-4): refetch the document, re-match it against the issued ids and
    /// push the difference, then `Settled` even when nothing changed so the waiter wakes.
    async fn rebuild(&mut self, live: &mut Live, ctx: &DocSourceCtx) {
        let mut rec = UpdateRecord {
            events: 0,
            dirty_nodes: 0,
            first_event: None,
            last_event: None,
            planned_at: Instant::now(),
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
        // Whatever was marked while dormant is covered by the refetch.
        let _ = live.dirty.take();
        let mut deltas = Vec::new();
        self.full_resync(live, &mut deltas, &mut rec).await;
        rec.deltas = deltas.len();
        live.out.emit(&ctx.sink, deltas);
        rec.pushed_at = Instant::now();
        self.metrics.push(rec);
        if live.root_errors >= ROOT_ERROR_LIMIT {
            // The document object is gone: the doc actor attaches a new source.
            live.root_errors = ROOT_ERROR_LIMIT;
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
            .ok_or(SourceError::NotSupported)?;
        let lane = self.app.next_lane();
        let r = match act {
            DocAction::Focus => {
                // GrabFocus answers a bool: false means the object did not take focus (R-6).
                let m = self
                    .app
                    .call(lane, deadline, &path, COMPONENT, "GrabFocus", &())
                    .await
                    .map_err(|e| map_dbus(&e))?;
                return match wire::parse_u32(m.body().data().bytes()) {
                    Ok(0) => Err(SourceError::Refused),
                    Ok(_) | Err(_) => Ok(()),
                };
            }
            DocAction::Click => {
                self.app
                    .call(lane, deadline, &path, ACTION, "DoAction", &(0i32,))
                    .await
            }
            DocAction::ScrollIntoView => {
                self.app
                    .call(lane, deadline, &path, COMPONENT, "ScrollTo", &(0u32,))
                    .await
            }
            DocAction::SetCaret(o) => {
                let o = i32::try_from(*o).unwrap_or(i32::MAX);
                self.app
                    .call(lane, deadline, &path, TEXT, "SetCaretOffset", &(o,))
                    .await
            }
            DocAction::SetSelection(r) => {
                let (a, b) = (
                    i32::try_from(r.start).unwrap_or(0),
                    i32::try_from(r.end).unwrap_or(0),
                );
                self.app
                    .call(lane, deadline, &path, TEXT, "SetSelection", &(0i32, a, b))
                    .await
            }
        };
        r.map(|_| ()).map_err(|e| map_dbus(&e))
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
