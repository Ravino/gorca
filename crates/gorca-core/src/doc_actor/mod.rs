// SPDX-License-Identifier: GPL-3.0-or-later
//! Document actor (ADR-0001 section 6.1, browse-mode design section 5): one per web document.
//!
//! The actor is a CPU actor. A source (the only component that talks to the browser) pushes
//! deltas into a bounded sink; the actor collects them up to the source's `Settled` marker,
//! translates the batch ([`translate`]), applies it to the left-right mirror, publishes (one
//! atomic swap: readers never see half a batch, PUB-2), updates the focus join (JN-1: after the
//! publish) and tells the Reader. It never plans IPC and never awaits the source inline (DA-P3):
//! user-driven pulls (`Act`) run in actor-owned subtasks, bounded, each under a deadline.
//!
//! Crash isolation: the source runs as its own task; when it ends (navigation, a crashed
//! browser, a rejected protocol) the actor retires the document: join entries leave, the
//! directory entry goes, the Reader is told (`Gone`). The hub creates a new document when the
//! user's focus or the browser says there is one.

pub mod agree;
#[cfg(test)]
mod agree_tests;
pub mod fuse;
pub mod live;
pub mod repair;
pub mod translate;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use gorca_doc::web::{DocRead, DocWriter, channel};
use gorca_doc_source_api::{
    Activity, DeltaReceiver, DocEvent, DocRequest, DocSource, DocSourceCtx, RepairReport,
    SourceError, delta_channel, join_channel,
};
use gorca_model::sem::SrcRecord;
use gorca_model::{AppId, DocAction, DocDelta, DocId, DocNodeId, LocalId, NodeKey, SrcNodeId};
use gorca_rt::{CancellationToken, Clock, Deadline, mpsc, oneshot};
use tokio::task::JoinSet;

use fuse::{FuseStage, NoSem, SemLookup};
use live::{LiveMsg, LiveTracker};
use repair::{
    ORPHAN_SETTLE, Orphans, RepairGate, RepairPolicy, RepairReason, Verdict, YOUNG_CHECKS,
    YOUNG_NODES,
};
use translate::Translator;

/// Deltas the sink may hold: a 100k-node first block is ~50 upsert chunks plus one list per
/// parent, so this is generous; an overflow makes the source rebuild instead of stalling it.
const SINK_CAPACITY: usize = 1 << 16;
const REQUEST_CAPACITY: usize = 32;
/// Events routed to one document (EF-2); beyond it the source resyncs.
pub const EVENT_CAPACITY: usize = 1024;
const CTL_CAPACITY: usize = 64;
/// Concurrent user-driven pulls per document.
const MAX_ACTIONS: usize = 8;
/// Deltas taken from the sink per wake, so one document cannot monopolise a worker.
const MAX_DELTAS_PER_WAKE: usize = 4096;
/// A pull that the browser does not answer in this time is given up.
const ACTION_DEADLINE: Duration = Duration::from_millis(500);
/// A label inference (rectangles of the control and of the text around it) is a nicety on a
/// focus: it gets this long, then the focus stays as it was spoken.
const LABEL_DEADLINE: Duration = Duration::from_millis(120);

/// Transport key to mirror node, for the Reader's focus join (JN-1, JN-2). Each document is the
/// single writer of its own keys. Wait-free reads.
#[derive(Debug, Default)]
pub struct WebJoin {
    map: scc::HashIndex<NodeKey, (DocId, DocNodeId)>,
}

impl WebJoin {
    pub fn new() -> Self {
        Self::default()
    }

    /// The document and mirror node a transport key names.
    pub fn get(&self, key: NodeKey) -> Option<(DocId, DocNodeId)> {
        self.map.peek_with(&key, |_, v| *v)
    }

    /// Insert or replace. A replace is remove plus insert: a concurrent reader may miss once.
    pub fn set(&self, key: NodeKey, value: (DocId, DocNodeId)) {
        if self.map.insert_sync(key, value).is_err() {
            let _ = self.map.remove_sync(&key);
            let _ = self.map.insert_sync(key, value);
        }
    }

    /// Remove the entry only if `doc` owns it.
    pub fn remove(&self, key: NodeKey, doc: DocId) {
        let _ = self.map.remove_if_sync(&key, |v| v.0 == doc);
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// What the Reader and the hub send to a document actor.
#[derive(Debug)]
pub enum DocCtl {
    /// Awake or dormant (DM-I5). Forwarded to the source.
    Activity(Activity),
    /// Rebuild as resync, ids kept (a late warm-up, a rejected batch).
    Rebuild,
    /// Repair a mirror that may be stale (CHR-0). The actor's gate decides whether and when:
    /// one repair in flight, a gap that follows the cost of the last one.
    Repair(RepairReason),
    /// `DocHandle::request_repair` set the coalescing flag: take it and ask the gate.
    RepairPending,
    /// A user-driven pull on a node: focus it, click it, scroll it into view (IR-5).
    Act { node: DocNodeId, act: DocAction },
    /// A page-selection action whose result comes back as [`DocNotice::SelectionDone`] with the
    /// same `req`: never fire-and-forget, never dropped without a reply.
    Select {
        req: u32,
        node: DocNodeId,
        act: DocAction,
    },
    /// Infer the label of the nameless control `node` (taken to be of `role`) from the text
    /// around it and its layout (M22a C3b). The answer, if there is one, comes back as
    /// [`DocNotice::Label`] with the same `req`; nothing is sent when nothing is found, the
    /// source cannot say where things are, or the deadline passed.
    Label {
        req: u32,
        node: DocNodeId,
        role: gorca_model::Role,
    },
    /// An `atspi` fact about a node of this document (SM-8): the app actor fetched it (a focus
    /// fetch of a web node) and the document, the node's single writer, fuses it. `issued_at` is
    /// [`DocHandle::timeline`] read when the fetch was issued, so a fact never overwrites what a
    /// later batch wrote.
    Fact {
        local: LocalId,
        rec: Arc<SrcRecord>,
        issued_at: u64,
    },
}

/// How a page-selection action ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SelOutcome {
    Done,
    /// Applied, but the page selection is not exactly the virtual one (the copy may differ).
    Approximate,
    /// The source cannot select in this document (no text path, no selection support).
    NotSupported,
    /// Refused, timed out, node gone or the action queue was full.
    Failed,
}

/// What a document tells the Reader (DA-P2). Background lane semantics: never blocks the actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocNotice {
    /// The first batch of a document is published and speakable. `navigated` is true when it
    /// replaced an earlier page of the same document (a new load), false for a fresh document.
    FirstUseful { doc: DocId, app: AppId },
    /// A later batch was published.
    Changed { doc: DocId, rev: u64 },
    /// The document ended (tab closed, navigation, source gone).
    Gone { doc: DocId },
    /// `Ensure` found no web document around this node (browser chrome, a dialog).
    NoDocument { focus: NodeKey },
    /// `Ensure` found that this node is inside a document that already exists.
    InDocument { focus: NodeKey, doc: DocId },
    /// The application can host web documents (told once at announce).
    WebApp { app: AppId },
    /// The browser reported `load-complete` (or a reload) for the document rooted at `node`.
    Loaded { node: NodeKey },
    /// A focus request on a node of this document failed (refused, timed out, gone): the key
    /// that asked (a grabbed Tab) must not stay silent.
    FocusFailed { doc: DocId },
    /// A click on a node of this document did nothing: the node is gone (`gone`, the page
    /// replaced it and no live object continues it) or the page did not take the action. The
    /// key that asked must not stay silent.
    ActionFailed { doc: DocId, gone: bool },
    /// A click was refused because the object at the node is no longer what was spoken (the
    /// source re-read its role and name before clicking). Nothing was clicked.
    ActionDisplaced {
        doc: DocId,
        /// What the object says now (its name), for the Reader to speak.
        now: Option<Arc<str>>,
    },
    /// A live region changed (ADR-0003 13): already classified, deduplicated and rate-limited by
    /// the document, so the Reader only decides whether this document is the one in front.
    Live { doc: DocId, msg: LiveMsg },
    /// The label [`DocCtl::Label`] inferred for `node`.
    Label {
        doc: DocId,
        req: u32,
        node: DocNodeId,
        text: Arc<str>,
    },
    /// The result of a [`DocCtl::Select`].
    SelectionDone {
        doc: DocId,
        req: u32,
        outcome: SelOutcome,
    },
}

/// Everything the Reader needs to read and steer one document.
#[derive(Clone, Debug)]
pub struct DocHandle {
    pub doc: DocId,
    pub app: AppId,
    /// Transport key of the document root.
    pub root: NodeKey,
    pub read: DocRead,
    pub ctl: mpsc::Sender<DocCtl>,
    pub events: mpsc::Sender<DocEvent>,
    /// Events the router saw on nodes no join knows (the repair trigger of CHR-0).
    pub orphans: Arc<Orphans>,
    /// The strongest repair reason asked for and not yet taken by the actor (0: none). The router
    /// and the hub set it and send one `DocCtl::RepairPending` per transition from empty, so a
    /// storm of load or state events costs one control slot, never the 64 that also carry clicks
    /// (review F-6).
    pub repair_flag: Arc<std::sync::atomic::AtomicU8>,
    /// Batches the document has applied (the actor timeline): read it when a fetch is issued and
    /// send the value with the resulting [`DocCtl::Fact`].
    pub timeline: Arc<std::sync::atomic::AtomicU64>,
}

impl DocHandle {
    /// Ask the document to repair its mirror (the actor's gate decides whether and when).
    pub fn request_repair(&self, r: RepairReason) {
        use std::sync::atomic::Ordering;
        let prev = self.repair_flag.fetch_max(r.code(), Ordering::AcqRel);
        // A full control queue leaves the flag set: the actor's tick takes it (never wiped here,
        // another caller may have raised it meanwhile).
        if prev == 0 {
            let _ = self.ctl.try_send(DocCtl::RepairPending);
        }
    }
}

/// Lock-free directory of live documents (DR-2).
#[derive(Debug, Default)]
pub struct DocDirectory {
    docs: ArcSwap<HashMap<DocId, Arc<DocHandle>>>,
}

impl DocDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, h: DocHandle) {
        let h = Arc::new(h);
        self.docs.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.insert(h.doc, Arc::clone(&h));
            next
        });
    }

    pub fn remove(&self, doc: DocId) {
        self.docs.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.remove(&doc);
            next
        });
    }

    pub fn get(&self, doc: DocId) -> Option<Arc<DocHandle>> {
        self.docs.load().get(&doc).cloned()
    }

    /// The document of `app` whose root is `root`.
    pub fn by_root(&self, root: NodeKey) -> Option<Arc<DocHandle>> {
        self.docs
            .load()
            .values()
            .find(|h| h.root == root)
            .map(Arc::clone)
    }

    /// Any document of `app` (the target of events about nodes no join knows).
    pub fn of_app(&self, app: AppId) -> Option<Arc<DocHandle>> {
        self.docs
            .load()
            .values()
            .find(|h| h.app == app)
            .map(Arc::clone)
    }

    pub fn len(&self) -> usize {
        self.docs.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.docs.load().is_empty()
    }
}

/// Counters, readable from tests and the debug probe.
#[derive(Debug, Default)]
pub struct DocCounters {
    pub batches: std::sync::atomic::AtomicU64,
    pub rejected: std::sync::atomic::AtomicU64,
    pub overflow: std::sync::atomic::AtomicU64,
    pub publishes: std::sync::atomic::AtomicU64,
    /// Registry notices dropped because the Reader's queue stayed full (N6).
    pub notices_dropped: std::sync::atomic::AtomicU64,
    /// Wake/sleep and action controls dropped by a full control queue (N6).
    pub controls_dropped: std::sync::atomic::AtomicU64,
}

/// Inputs of one document actor.
#[allow(missing_debug_implementations)]
pub struct DocActorSpec {
    pub doc: DocId,
    pub app: AppId,
    pub root: NodeKey,
    pub source: Box<dyn DocSource>,
    pub join: Arc<WebJoin>,
    pub directory: Arc<DocDirectory>,
    pub notices: mpsc::Sender<DocNotice>,
    pub shutdown: CancellationToken,
    pub counters: Arc<DocCounters>,
    /// Which repair triggers this document's engine uses.
    pub repair: RepairPolicy,
}

/// Create the channels, register the document in the directory and return the actor future.
/// The caller (the hub) owns the task that runs it.
pub fn prepare(
    spec: DocActorSpec,
) -> (
    DocHandle,
    impl std::future::Future<Output = ()> + Send + 'static,
) {
    let (ctl_tx, ctl_rx) = mpsc::channel(CTL_CAPACITY);
    let (ev_tx, ev_rx) = mpsc::channel(EVENT_CAPACITY);
    let (writer, read) = channel(u64::from(spec.doc.0).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let handle = DocHandle {
        doc: spec.doc,
        app: spec.app,
        root: spec.root,
        read,
        ctl: ctl_tx,
        events: ev_tx,
        orphans: Arc::default(),
        repair_flag: Arc::default(),
        timeline: Arc::default(),
    };
    let shared = Shared {
        read: handle.read.clone(),
        orphans: Arc::clone(&handle.orphans),
        repair_flag: Arc::clone(&handle.repair_flag),
        timeline: Arc::clone(&handle.timeline),
    };
    let fut = run(spec, writer, shared, ctl_rx, ev_rx);
    (handle, fut)
}

struct Actor {
    spec_doc: DocId,
    app: AppId,
    writer: DocWriter,
    tr: Translator,
    join: Arc<WebJoin>,
    notices: mpsc::Sender<DocNotice>,
    counters: Arc<DocCounters>,
    requests: mpsc::Sender<DocRequest>,
    /// Keys this document wrote into the join (so retiring removes exactly them).
    keys: std::collections::HashSet<NodeKey>,
    announced: bool,
    batch: Vec<DocDelta>,
    /// Notices the Reader's queue had no room for, oldest first. `Changed` is coalesced; the
    /// first-useful and gone notices are kept (bounded by [`MAX_PENDING_NOTICES`]).
    pending_notices: std::collections::VecDeque<DocNotice>,
    read: DocRead,
    live: LiveTracker,
    /// The tab is in front: dormant documents track nothing (DM-I5).
    awake: bool,
    /// Mirror freshness (CHR-0).
    gate: RepairGate,
    orphans: Arc<Orphans>,
    orphan_due: Option<Instant>,
    /// A repair that must wait for the gate's gap, with the instant it may go.
    deferred: Option<(RepairReason, Instant)>,
    born: Instant,
    young_done: usize,
    /// Why the repair in flight was started, and the mirror's fingerprint when it started.
    repair_reason: Option<RepairReason>,
    repair_before: (usize, usize),
    /// The orphan keys the last settle period sampled, and whether the next orphan repair must
    /// skip the cheap look because the last one passed while those keys were still unknown.
    orphan_keys: Vec<NodeKey>,
    escalate_orphans: bool,
    /// Follow-up checks spent after a load-end repair that found the mirror still tiny.
    load_retries: usize,
    /// Fusion of the source's records (`None`: the source does not normalise, SM-8).
    fuse: Option<FuseStage>,
    timeline: Arc<std::sync::atomic::AtomicU64>,
    /// Nodes whose fused entry changed and whose meta is not patched yet (a fact arrived).
    pending_changed: Vec<SrcNodeId>,
}

type RepairRx = oneshot::Receiver<Result<RepairReport, SourceError>>;
/// A root-state flip counts as the end of a load only this long after the document appeared.
const YOUNG_WINDOW: Duration = Duration::from_secs(30);

impl Actor {
    fn key(&self, local: gorca_model::LocalId) -> NodeKey {
        NodeKey {
            app: self.app,
            local,
        }
    }

    /// Translate and publish the collected batch.
    fn flush(&mut self) -> bool {
        let batch = std::mem::take(&mut self.batch);
        if batch.is_empty() && self.pending_changed.is_empty() {
            return true;
        }
        self.counters
            .batches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t_apply = Instant::now();
        // Fusion first: the translator builds the back copy with the fused entries in place, so
        // the publish below is the one atomic swap of content and semantics (SM-8).
        let sem_only = batch.is_empty();
        let mut changed = std::mem::take(&mut self.pending_changed);
        if let Some(f) = self.fuse.as_mut()
            && !batch.is_empty()
        {
            let t_fuse = Instant::now();
            changed.extend(f.apply(&batch));
            let took = t_fuse.elapsed();
            if took >= Duration::from_millis(1) {
                tracing::info!(
                    target: "gorca_phase",
                    phase = "fuse",
                    wall_ms = took.as_secs_f64() * 1000.0,
                    nodes = f.len(),
                    pending = f.pending(),
                );
            }
            self.timeline
                .store(f.timeline(), std::sync::atomic::Ordering::Release);
        }
        let lookup: &dyn SemLookup = match &self.fuse {
            Some(f) => f,
            None => &NoSem,
        };
        match self
            .tr
            .apply_fused(&mut self.writer, batch, lookup, &changed)
        {
            Ok(out) => {
                let published = self.writer.publish();
                let flush_ms = t_apply.elapsed().as_secs_f64() * 1000.0;
                if flush_ms >= 3.0 {
                    tracing::info!(
                        target: "gorca_phase",
                        phase = "flush",
                        wall_ms = flush_ms,
                        sem_only,
                        changed = changed.len(),
                        nodes = self.writer.doc().len(),
                    );
                }
                if !self.announced && published {
                    tracing::info!(
                        target: "gorca_phase",
                        phase = "translate",
                        wall_ms = t_apply.elapsed().as_secs_f64() * 1000.0,
                        nodes = self.writer.doc().len(),
                    );
                }
                if published {
                    self.counters
                        .publishes
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                if published && self.awake && !sem_only {
                    if out.reset {
                        self.live.reset();
                    }
                    self.live
                        .on_batch(self.writer.doc(), &out.touched, Instant::now());
                }
                // JN-1: keys appear only after the publish that made their node visible.
                for (l, id) in out.join_add {
                    let k = self.key(l);
                    self.join.set(k, (self.spec_doc, id));
                    self.keys.insert(k);
                }
                for l in out.join_del {
                    let k = self.key(l);
                    self.join.remove(k, self.spec_doc);
                    self.keys.remove(&k);
                }
                // A publish that only carries fused entries is no content change: the Reader is
                // not told.
                if published && !sem_only {
                    let notice = if self.announced {
                        DocNotice::Changed {
                            doc: self.spec_doc,
                            rev: self.writer.doc().revision(),
                        }
                    } else {
                        self.announced = true;
                        DocNotice::FirstUseful {
                            doc: self.spec_doc,
                            app: self.app,
                        }
                    };
                    self.notify(notice);
                }
                true
            }
            Err(e) => {
                tracing::warn!(doc = self.spec_doc.0, error = %e, "doc batch rejected; rebuilding");
                self.counters
                    .rejected
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // Whatever was applied before the rejection is consistent (ops are atomic);
                // publish it so the Reader never sees a mirror that disagrees with the join.
                self.writer.publish();
                false
            }
        }
    }

    /// Continue a fusion the last run left unfinished: one more budget, then the entries that
    /// changed are patched into the mirror and published (the Reader is not told).
    fn fuse_tick(&mut self) {
        let Some(f) = self.fuse.as_mut() else { return };
        let changed = f.step();
        self.pending_changed.extend(changed);
        if !self.pending_changed.is_empty() {
            self.flush();
        }
    }

    fn fuse_pending(&self) -> bool {
        self.fuse.as_ref().is_some_and(|f| f.pending() > 0)
    }

    /// A fact about one of this document's nodes (a focus fetch the app actor forwarded).
    fn on_fact(&mut self, local: LocalId, rec: &SrcRecord, issued_at: u64) {
        if self.fuse.is_none() {
            return;
        }
        let Some((doc, id)) = self.join.get(self.key(local)) else {
            return;
        };
        if doc != self.spec_doc {
            return;
        }
        let Some(src) = self
            .writer
            .doc()
            .node(id)
            .and_then(|v| v.meta)
            .and_then(|m| m.src)
        else {
            return;
        };
        let fused = self.fuse.as_mut().and_then(|f| f.fact(src, rec, issued_at));
        if let Some(changed) = fused {
            self.pending_changed.extend(changed);
            if !self.pending_changed.is_empty() {
                self.flush();
            }
        }
    }

    /// Deliver the live-region messages that are due.
    fn live_tick(&mut self) {
        let msgs = self.live.tick(self.writer.doc(), Instant::now());
        for msg in msgs {
            tracing::info!(target: "gorca_phase", phase = "live_msg", text = %msg.text, assertive = msg.assertive);
            self.notify(DocNotice::Live {
                doc: self.spec_doc,
                msg,
            });
        }
    }

    fn notify(&mut self, n: DocNotice) {
        // Coalesce `Changed` with a waiting `Changed`; never lose the first-useful or a gone
        // notice (a lost `Gone` leaves browse keys grabbed on a dead document).
        if matches!(n, DocNotice::Changed { .. })
            && let Some(last @ DocNotice::Changed { .. }) = self.pending_notices.back_mut()
        {
            *last = n;
        } else {
            // A polite update is superseded by a newer one that has not been delivered yet.
            if let DocNotice::Live { msg, .. } = &n
                && !msg.assertive
                && let Some(at) = self
                    .pending_notices
                    .iter()
                    .position(|p| matches!(p, DocNotice::Live { msg: m, .. } if !m.assertive))
            {
                self.pending_notices.remove(at);
            }
            if self.pending_notices.len() >= MAX_PENDING_NOTICES {
                // Make room by dropping the oldest `Changed`, then the oldest live message
                // (they are news that goes stale); a live message never evicts anything else.
                let live_new = matches!(n, DocNotice::Live { .. });
                let at = if live_new {
                    // News never evicts structure: only older news makes room for it.
                    self.pending_notices
                        .iter()
                        .position(|p| matches!(p, DocNotice::Live { .. }))
                } else {
                    self.pending_notices
                        .iter()
                        .position(|p| matches!(p, DocNotice::Changed { .. }))
                        .or_else(|| {
                            self.pending_notices
                                .iter()
                                .position(|p| matches!(p, DocNotice::Live { .. }))
                        })
                };
                match at {
                    Some(at) => {
                        self.pending_notices.remove(at);
                    }
                    None if live_new => return,
                    None => {
                        self.pending_notices.remove(0);
                    }
                }
            }
            self.pending_notices.push_back(n);
        }
        self.retry_notice();
    }

    fn retry_notice(&mut self) {
        while let Some(n) = self.pending_notices.front().cloned() {
            if self.notices.try_send(n).is_err() {
                break;
            }
            self.pending_notices.pop_front();
        }
    }

    /// What a repair is measured against: node count and text length of the published mirror.
    fn fingerprint(&mut self) -> (usize, usize) {
        let d = self.writer.doc();
        (d.len(), d.text_len() as usize)
    }

    /// A repair request (CHR-0): the gate decides, the actor sends or keeps it.
    fn repair_request(&mut self, r: RepairReason, now: Instant, rx: &mut Option<RepairRx>) {
        // A dormant document tracks nothing (DM-I5) and its wake rebuilds it anyway (F-4).
        if !self.awake {
            return;
        }
        if r == RepairReason::RootState
            && (self.gate.loaded_seen() || now.saturating_duration_since(self.born) > YOUNG_WINDOW)
        {
            return;
        }
        match self.gate.request(r, now) {
            Verdict::Start => self.repair_start(r, now, rx),
            Verdict::Wait(at) => {
                self.deferred = Some(match self.deferred {
                    Some((d, t)) => (d.max(r), t.min(at)),
                    None => (r, at),
                });
            }
            Verdict::Queued | Verdict::Refused => {}
        }
    }

    fn repair_start(&mut self, r: RepairReason, now: Instant, rx: &mut Option<RepairRx>) {
        let (reply, rcv) = oneshot::channel();
        if self
            .requests
            .try_send(DocRequest::Repair {
                reply,
                cheap_check: r.cheap_check()
                    && !(r == RepairReason::Orphans && self.escalate_orphans),
            })
            .is_ok()
        {
            self.gate.started(r);
            if r == RepairReason::Orphans {
                self.escalate_orphans = false;
            }
            self.repair_reason = Some(r);
            self.repair_before = self.fingerprint();
            *rx = Some(rcv);
            tracing::info!(
                target: "gorca_phase",
                phase = "repair_start",
                doc = self.spec_doc.0,
                reason = r.label(),
                nodes = self.writer.doc().len(),
            );
        } else if let Some(next) = self.gate.aborted(now) {
            self.deferred = Some((next, now));
        }
    }

    /// The browser announces the end of a load at the same moment it publishes the content
    /// (Electron holds the whole tree back until then): a repair started by that announcement
    /// can still see the old tree. When it found nothing and the mirror is tiny, look again
    /// shortly, twice at most.
    fn load_end_follow_up(&mut self, report: RepairReport, changed: bool, now: Instant) {
        const AFTER: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];
        let load_end = matches!(
            self.repair_reason.take(),
            Some(RepairReason::LoadComplete | RepairReason::RootState)
        );
        if load_end
            && !changed
            && !report.failed
            && report.nodes < YOUNG_NODES
            && self.load_retries < AFTER.len()
        {
            self.deferred = Some((RepairReason::Young, now + AFTER[self.load_retries]));
            self.load_retries += 1;
        }
    }

    /// Timers of the repair triggers: a deferred repair, events on unknown nodes after their
    /// settle period, the young-document checks.
    fn repair_tick(&mut self, now: Instant, rx: &mut Option<RepairRx>) {
        if !self.awake {
            self.deferred = None;
            self.orphan_due = None;
            let _ = self.orphans.take();
            return;
        }
        if let Some((r, at)) = self.deferred
            && now >= at
            && !self.gate.in_flight()
        {
            self.deferred = None;
            self.repair_request(r, now, rx);
        }
        let policy = self.gate.policy();
        if policy.orphans {
            match self.orphan_due {
                Some(due) if now >= due => {
                    self.orphan_due = None;
                    let keys = self.orphans.take();
                    self.orphan_keys.clone_from(&keys);
                    // Still unknown after the settle period: the incremental update did not
                    // catch up, the mirror is behind.
                    if keys.iter().any(|k| self.join.get(*k).is_none()) {
                        self.repair_request(RepairReason::Orphans, now, rx);
                    }
                }
                None if self.orphans.pending() => self.orphan_due = Some(now + ORPHAN_SETTLE),
                Some(_) | None => {}
            }
        }
        if policy.young
            && self.announced
            && self.young_done < YOUNG_CHECKS.len()
            && now.saturating_duration_since(self.born) >= YOUNG_CHECKS[self.young_done]
        {
            self.young_done += 1;
            if !self.gate.loaded_seen() && self.writer.doc().len() < YOUNG_NODES {
                self.repair_request(RepairReason::Young, now, rx);
            }
        }
    }

    fn retire(&mut self) {
        for k in std::mem::take(&mut self.keys) {
            self.join.remove(k, self.spec_doc);
        }
    }
}

const MAX_PENDING_NOTICES: usize = 8;

/// Ready shortly when the fusion stage still owes work; never ready otherwise. Interactive
/// messages are served between the slices (the select is biased towards them).
async fn fuse_wait(pending: bool) {
    if pending {
        tokio::time::sleep(Duration::from_millis(1)).await;
    } else {
        std::future::pending::<()>().await;
    }
}

/// Sleep until the tracker's next deadline; never ready when it has none.
async fn live_wait(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending::<()>().await,
    }
}
/// How long retiring waits for room in the Reader's notice queue.
const GONE_DELIVERY: Duration = Duration::from_secs(2);

/// What the actor shares with its [`DocHandle`].
struct Shared {
    read: DocRead,
    orphans: Arc<Orphans>,
    repair_flag: Arc<std::sync::atomic::AtomicU8>,
    timeline: Arc<std::sync::atomic::AtomicU64>,
}

async fn run(
    spec: DocActorSpec,
    writer: DocWriter,
    shared: Shared,
    mut ctl_rx: mpsc::Receiver<DocCtl>,
    ev_rx: mpsc::Receiver<DocEvent>,
) {
    let Shared {
        read,
        orphans,
        repair_flag,
        timeline,
    } = shared;
    let DocActorSpec {
        doc,
        app,
        source,
        join,
        directory,
        notices,
        shutdown,
        counters,
        repair,
        ..
    } = spec;
    // The source hands over the fusion tables of its engine before it is attached.
    let fuse = source.fuse_setup().map(FuseStage::new);
    let (sink, mut deltas): (_, DeltaReceiver) = delta_channel(SINK_CAPACITY);
    let (req_tx, req_rx) = mpsc::channel(REQUEST_CAPACITY);
    let (join_port, _join_rx) = join_channel(1);
    let src_shutdown = shutdown.child_token();
    let ctx = DocSourceCtx {
        doc,
        app,
        sink,
        requests: req_rx,
        events: ev_rx,
        join: join_port,
        shutdown: src_shutdown.clone(),
        clock: Clock::System,
    };
    let mut src_task: JoinSet<Result<(), SourceError>> = JoinSet::new();
    src_task.spawn(source.attach(ctx));
    let mut actions: JoinSet<()> = JoinSet::new();
    let mut label_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut sel_chain: Option<oneshot::Receiver<()>> = None;
    let mut a = Actor {
        spec_doc: doc,
        app,
        writer,
        tr: Translator::new(),
        join,
        notices,
        counters,
        requests: req_tx,
        keys: std::collections::HashSet::new(),
        announced: false,
        batch: Vec::new(),
        pending_notices: std::collections::VecDeque::new(),
        read,
        live: LiveTracker::new(),
        awake: true,
        gate: RepairGate::new(repair),
        orphans,
        orphan_due: None,
        deferred: None,
        born: Instant::now(),
        young_done: 0,
        repair_reason: None,
        repair_before: (0, 0),
        orphan_keys: Vec::new(),
        escalate_orphans: false,
        load_retries: 0,
        fuse,
        timeline,
        pending_changed: Vec::new(),
    };
    let mut repair_rx: Option<RepairRx> = None;

    let mut notice_tick = tokio::time::interval(Duration::from_millis(100));
    notice_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    'main: loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break 'main,
            ended = src_task.join_next() => {
                // The source ended: navigation, a dead browser or a protocol error. Apply what
                // it pushed last, then retire the document.
                let _ = flush_all(&mut a, &mut deltas);
                tracing::debug!(doc = doc.0, ended = ?ended.map(|r| r.map(|x| x.is_ok())), "doc source ended");
                break 'main;
            }
            d = deltas.deltas.recv() => {
                let Some(d) = d else { break 'main };
                let mut settled = push(&mut a, d);
                let mut n = 1;
                while !settled && n < MAX_DELTAS_PER_WAKE {
                    match deltas.deltas.try_recv() {
                        Ok(d) => { settled = push(&mut a, d); n += 1; }
                        Err(_) => break,
                    }
                }
                if settled {
                    let ok = a.flush();
                    let overflow = deltas.take_overflow();
                    if overflow {
                        a.counters.overflow.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    if !ok || overflow {
                        let _ = a.requests.try_send(DocRequest::Rebuild);
                    }
                }
            }
            ctl = ctl_rx.recv() => {
                let Some(ctl) = ctl else { break 'main };
                match ctl {
                    DocCtl::Activity(act) => {
                        a.awake = matches!(act, Activity::Awake);
                        if !a.awake {
                            a.live.reset();
                        }
                        let _ = a.requests.try_send(DocRequest::SetActivity(act));
                    }
                    DocCtl::Rebuild => { let _ = a.requests.try_send(DocRequest::Rebuild); }
                    DocCtl::Repair(r) => a.repair_request(r, Instant::now(), &mut repair_rx),
                    DocCtl::RepairPending => {
                        let code = repair_flag.swap(0, std::sync::atomic::Ordering::AcqRel);
                        if let Some(r) = RepairReason::from_code(code) {
                            a.repair_request(r, Instant::now(), &mut repair_rx);
                        }
                    }
                    DocCtl::Act { node, act } => {
                        if actions.len() < MAX_ACTIONS {
                            spawn_action(&mut actions, &a, node, act, None);
                        } else if matches!(act, DocAction::Focus) {
                            // A dropped focus request must not leave the grabbed Tab silent.
                            let _ = a.notices.try_send(DocNotice::FocusFailed { doc: a.spec_doc });
                        }
                    }
                    DocCtl::Label { req, node, role } => {
                        // Labels have a slot of their own (never one of the action slots, so a
                        // Tab auto-repeat cannot crowd out a Focus or a Click): the newest
                        // request replaces the one in flight.
                        if let Some(old) = label_task.take() {
                            old.abort();
                        }
                        label_task = spawn_label(&a, req, node, role);
                    }
                    DocCtl::Fact { local, rec, issued_at } => a.on_fact(local, &rec, issued_at),
                    DocCtl::Select { req, node, act } => {
                        if actions.len() < MAX_ACTIONS {
                            // One selection at a time, in request order (ADR-0021 I-3): this
                            // one waits for the previous to finish, so a clear can never
                            // overtake the set it follows.
                            let (done, next) = oneshot::channel();
                            let prev = sel_chain.replace(next);
                            spawn_action(&mut actions, &a, node, act, Some((req, prev, done)));
                        } else {
                            let _ = a.notices.try_send(DocNotice::SelectionDone {
                                doc: a.spec_doc,
                                req,
                                outcome: SelOutcome::Failed,
                            });
                        }
                    }
                }
            }
            _ = actions.join_next(), if !actions.is_empty() => {}
            () = live_wait(a.live.next_deadline()) => a.live_tick(),
            r = async {
                match repair_rx.as_mut() {
                    Some(rx) => rx.await,
                    None => std::future::pending().await,
                }
            }, if repair_rx.is_some() => {
                repair_rx = None;
                let _ = flush_all(&mut a, &mut deltas);
                let report = match r {
                    Ok(Ok(rep)) => rep,
                    Ok(Err(_)) | Err(_) => RepairReport { failed: true, ..RepairReport::default() },
                };
                let now = Instant::now();
                let reason = a.repair_reason;
                let after = a.fingerprint();
                let changed = after != a.repair_before;
                tracing::info!(
                    target: "gorca_phase",
                    phase = "repair_done",
                    doc = doc.0,
                    nodes = report.nodes,
                    deltas = report.deltas,
                    took_ms = report.took.as_secs_f64() * 1000.0,
                    failed = report.failed,
                    changed,
                    quiet = a.gate.quiet(),
                );
                a.load_end_follow_up(report, changed, now);
                // An orphan repair that the cheap look answered while the sampled keys are still
                // unknown: the staleness sits below what the look sees, so look no more (D-2).
                if reason == Some(RepairReason::Orphans)
                    && report.nodes == 0
                    && !report.failed
                    && a.orphan_keys.iter().any(|k| a.join.get(*k).is_none())
                {
                    a.escalate_orphans = true;
                }
                if let Some(next) = a.gate.finished(report, changed, now) {
                    a.repair_request(next, now, &mut repair_rx);
                }
            }
            () = fuse_wait(a.fuse_pending()) => a.fuse_tick(),
            _ = notice_tick.tick() => {
                a.retry_notice();
                let code = repair_flag.swap(0, std::sync::atomic::Ordering::AcqRel);
                if let Some(r) = RepairReason::from_code(code) {
                    a.repair_request(r, Instant::now(), &mut repair_rx);
                }
                a.repair_tick(Instant::now(), &mut repair_rx);
            }
        }
    }
    // Retire: no key may outlive its document.
    if let Some(f) = &a.fuse {
        // The CI counter of the dual-carry period (process-wide totals, see `agree`).
        let agree = agree::stats();
        tracing::info!(
            target: "gorca_phase",
            phase = "agree",
            doc = doc.0,
            profile = f.profile_name(),
            nodes = f.len(),
            empty = f.is_empty(),
            stats = ?f.stats(),
            checked = agree.checked,
            disagreements = agree.total(),
            by_aspect = ?agree.mismatched,
            "document fusion stage retired"
        );
    }
    a.retire();
    directory.remove(doc);
    // A task the source detached (the frame wake) holds a clone of this token: abort_all does
    // not reach it, so the document's end cancels it.
    src_shutdown.cancel();
    src_task.abort_all();
    actions.abort_all();
    // `Gone` must arrive: wait for room (bounded), after the notices queued before it.
    a.retry_notice();
    for n in std::mem::take(&mut a.pending_notices) {
        let _ = tokio::time::timeout(GONE_DELIVERY, a.notices.send(n)).await;
    }
    let _ = tokio::time::timeout(GONE_DELIVERY, a.notices.send(DocNotice::Gone { doc })).await;
}

/// Append a delta to the batch; true when it was the `Settled` marker.
fn push(a: &mut Actor, d: DocDelta) -> bool {
    let settled = matches!(d, DocDelta::Settled { .. });
    a.batch.push(d);
    settled
}

/// Drain everything still queued and apply it as one batch.
fn flush_all(a: &mut Actor, deltas: &mut DeltaReceiver) -> bool {
    while let Ok(d) = deltas.deltas.try_recv() {
        push(a, d);
    }
    a.flush()
}

/// Label inference for the nameless control `node` (Orca's rules, `gorca_doc::web::label`): the
/// candidate texts come from the mirror, one batched rectangle request goes to the source with a
/// short deadline, and the geometry picks the winner. Anything missing (no source ids, an
/// author-given ARIA role, a source that cannot measure, the deadline) ends silently.
fn spawn_label(
    a: &Actor,
    req: u32,
    node: DocNodeId,
    role: gorca_model::Role,
) -> Option<tokio::task::JoinHandle<()>> {
    use gorca_doc::web::label;
    if !label::infers_for(role) {
        return None;
    }
    let planned = a.read.with(|d| {
        let v = d.node(node)?;
        // Orca does not infer for a control whose role the author set (`has_role_from_aria`).
        if v.meta.is_some_and(|m| {
            m.attrs
                .iter()
                .any(|(k, t)| *k == gorca_doc_source_api::web_attrs::XML_ROLES && !t.is_empty())
        }) {
            return None;
        }
        let plan = label::plan(d, node, role)?;
        let src_of = |n| d.node(n).and_then(|v| v.meta).and_then(|m| m.src);
        let srcs = plan
            .probes()
            .into_iter()
            .map(|n| {
                let node = src_of(n)?;
                // A text run is measured inside its container when it has no object.
                let (parent, range) = match label::hyper_range(d, n) {
                    Some((p, a, b)) => (src_of(p).unwrap_or(node), a..b),
                    None => (node, 0..0),
                };
                Some(gorca_doc_source_api::ExtentProbe {
                    node,
                    parent,
                    range,
                })
            })
            .collect::<Option<smallvec::SmallVec<[_; 6]>>>()?;
        Some((plan, srcs))
    });
    let Some((plan, srcs)) = planned else {
        tracing::info!(target: "gorca_phase", phase = "web_label", outcome = "no_plan");
        return None;
    };
    let requests = a.requests.clone();
    let notices = a.notices.clone();
    let doc = a.spec_doc;
    Some(tokio::spawn(async move {
        let (reply, rx) = oneshot::channel();
        let deadline = Deadline(Instant::now() + LABEL_DEADLINE);
        // The send is inside the timeout too: a source whose queue is full costs this task
        // its deadline, nothing else.
        let ask = async {
            requests
                .send(DocRequest::Extents {
                    nodes: srcs,
                    deadline,
                    reply,
                })
                .await
                .ok()?;
            rx.await.ok()?.ok()
        };
        let Ok(Some(rects)) =
            tokio::time::timeout(LABEL_DEADLINE + Duration::from_millis(30), ask).await
        else {
            tracing::info!(target: "gorca_phase", phase = "web_label", outcome = "no_rects");
            return;
        };
        let chosen = label::choose(&plan, &rects);
        tracing::info!(
            target: "gorca_phase",
            phase = "web_label",
            outcome = if chosen.is_some() { "label" } else { "far" },
            cands = plan.cands.len(),
            rects = ?rects,
        );
        if let Some(text) = chosen {
            let _ = notices.try_send(DocNotice::Label {
                doc,
                req,
                node,
                text: Arc::from(text),
            });
        }
    }))
}

/// IR-5: the action goes to the source that declared it, by the source's own node id.
fn spawn_action(
    set: &mut JoinSet<()>,
    a: &Actor,
    node: DocNodeId,
    act: DocAction,
    select: Option<(u32, Option<oneshot::Receiver<()>>, oneshot::Sender<()>)>,
) {
    let select_req = select.as_ref().map(|(r, _, _)| *r);
    // A selection request always gets an answer, also when it cannot even be sent.
    let fail = |a: &Actor| {
        if let Some(req) = select_req {
            let _ = a.notices.try_send(DocNotice::SelectionDone {
                doc: a.spec_doc,
                req,
                outcome: SelOutcome::Failed,
            });
        }
    };
    let src_of = |d: &gorca_doc::web::WebDoc, n: DocNodeId| {
        d.node(n).and_then(|v| v.meta).and_then(|m| m.src)
    };
    let src = a.read.with(|d| src_of(d, node));
    let Some(src) = src else {
        fail(a);
        if matches!(act, DocAction::Click | DocAction::ClickAnnounced { .. }) {
            // A node the source never issued an id for (a synthetic container) cannot be
            // clicked; it never existed, so it is not "gone".
            let _ = a.notices.try_send(DocNotice::ActionFailed {
                doc: a.spec_doc,
                gone: false,
            });
        }
        return;
    };
    // A page selection names core nodes; the source knows its own ids. A span whose node has no
    // source id (a synthetic container) cannot be addressed, so the whole request is dropped.
    let act = match act {
        DocAction::SetSelection(spec) => {
            let mapped = a.read.with(|d| {
                let map = |s: &gorca_model::SelSpan<DocNodeId>| {
                    src_of(d, s.node).map(|node| gorca_model::SelSpan {
                        node,
                        range: s.range.clone(),
                        range_cp: s.range_cp.clone(),
                        end_on_child: s.end_on_child,
                        end_on_object: s.end_on_object,
                        child_follows: s.child_follows,
                        trim_ws: s.trim_ws,
                    })
                };
                let spans = spec.spans.iter().map(map).collect::<Option<Vec<_>>>()?;
                let snap = map(&spec.snap)?;
                let root = src_of(d, spec.root)?;
                let doc_range = gorca_model::DocRange {
                    start: (src_of(d, spec.doc_range.start.0)?, spec.doc_range.start.1),
                    end: (src_of(d, spec.doc_range.end.0)?, spec.doc_range.end.1),
                };
                Some(gorca_model::SelectionSpec {
                    root,
                    spans,
                    snap,
                    doc_range,
                    gecko_lossy: spec.gecko_lossy,
                })
            });
            let Some(mapped) = mapped else {
                fail(a);
                return;
            };
            DocAction::SetSourceSelection(mapped)
        }
        other @ (DocAction::Focus
        | DocAction::Click
        | DocAction::ClickAnnounced { .. }
        | DocAction::ReleaseField
        | DocAction::ScrollIntoView
        | DocAction::SetCaret(_)
        | DocAction::SetSourceSelection(_)
        | DocAction::ClearSelection) => other,
    };
    let req = a.requests.clone();
    let notices = a.notices.clone();
    let doc = a.spec_doc;
    let is_focus = matches!(act, DocAction::Focus);
    let is_click = matches!(act, DocAction::Click | DocAction::ClickAnnounced { .. });
    set.spawn(async move {
        // Wait for the selection request before this one; `done` is dropped at the end, which
        // releases the next one in line (also when this task is aborted).
        let (prev, _done) = match select {
            Some((_, prev, done)) => (prev, Some(done)),
            None => (None, None),
        };
        if let Some(prev) = prev {
            let _ = prev.await;
        }
        let (reply, rx) = oneshot::channel();
        let deadline = Deadline(Instant::now() + ACTION_DEADLINE);
        let sent = req
            .send(DocRequest::Action {
                node: src,
                act,
                deadline,
                reply,
            })
            .await;
        let result = if sent.is_ok() {
            match tokio::time::timeout(ACTION_DEADLINE + Duration::from_millis(50), rx).await {
                Ok(Ok(r)) => Some(r),
                Ok(Err(_)) | Err(_) => None,
            }
        } else {
            None
        };
        let ok = matches!(result, Some(Ok(())));
        if is_focus && !ok {
            let _ = notices.try_send(DocNotice::FocusFailed { doc });
        }
        // A click that timed out may still run (a busy page works through its queue): say
        // nothing rather than claim it failed. Definite answers are spoken.
        if is_click
            && let Some(Err(e)) = &result
            && !matches!(e, SourceError::Timeout)
        {
            if let SourceError::Displaced(now) = e {
                let _ = notices.try_send(DocNotice::ActionDisplaced {
                    doc,
                    now: now.clone(),
                });
            } else {
                let gone = matches!(e, SourceError::Gone);
                let _ = notices.try_send(DocNotice::ActionFailed { doc, gone });
            }
        }
        if let Some(req) = select_req {
            let outcome = match result {
                Some(Ok(())) => SelOutcome::Done,
                Some(Err(SourceError::Approximate)) => SelOutcome::Approximate,
                Some(Err(SourceError::NotSupported)) => SelOutcome::NotSupported,
                Some(Err(_)) | None => SelOutcome::Failed,
            };
            let _ = notices.try_send(DocNotice::SelectionDone { doc, req, outcome });
        }
    });
}

#[cfg(test)]
mod tests;
