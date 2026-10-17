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

pub mod live;
pub mod translate;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use gorca_doc::web::{DocRead, DocWriter, channel};
use gorca_doc_source_api::{
    Activity, DeltaReceiver, DocEvent, DocRequest, DocSource, DocSourceCtx, SourceError,
    delta_channel, join_channel,
};
use gorca_model::{AppId, DocAction, DocDelta, DocId, DocNodeId, NodeKey};
use gorca_rt::{CancellationToken, Clock, Deadline, mpsc, oneshot};
use tokio::task::JoinSet;

use live::{LiveMsg, LiveTracker};
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
    /// A user-driven pull on a node: focus it, click it, scroll it into view (IR-5).
    Act { node: DocNodeId, act: DocAction },
    /// A page-selection action whose result comes back as [`DocNotice::SelectionDone`] with the
    /// same `req`: never fire-and-forget, never dropped without a reply.
    Select {
        req: u32,
        node: DocNodeId,
        act: DocAction,
    },
}

/// How a page-selection action ended.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SelOutcome {
    Done,
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
    /// A live region changed (ADR-0003 13): already classified, deduplicated and rate-limited by
    /// the document, so the Reader only decides whether this document is the one in front.
    Live { doc: DocId, msg: LiveMsg },
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
    };
    let fut = run(spec, writer, handle.read.clone(), ctl_rx, ev_rx);
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
}

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
        if batch.is_empty() {
            return true;
        }
        self.counters
            .batches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t_apply = Instant::now();
        match self.tr.apply(&mut self.writer, batch) {
            Ok(out) => {
                let published = self.writer.publish();
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
                if published && self.awake {
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
                if published {
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

    fn retire(&mut self) {
        for k in std::mem::take(&mut self.keys) {
            self.join.remove(k, self.spec_doc);
        }
    }
}

const MAX_PENDING_NOTICES: usize = 8;

/// Sleep until the tracker's next deadline; never ready when it has none.
async fn live_wait(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
        None => std::future::pending::<()>().await,
    }
}
/// How long retiring waits for room in the Reader's notice queue.
const GONE_DELIVERY: Duration = Duration::from_secs(2);

async fn run(
    spec: DocActorSpec,
    writer: DocWriter,
    read: DocRead,
    mut ctl_rx: mpsc::Receiver<DocCtl>,
    ev_rx: mpsc::Receiver<DocEvent>,
) {
    let DocActorSpec {
        doc,
        app,
        source,
        join,
        directory,
        notices,
        shutdown,
        counters,
        ..
    } = spec;
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
    };

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
                    DocCtl::Act { node, act } => {
                        if actions.len() < MAX_ACTIONS {
                            spawn_action(&mut actions, &a, node, act, None);
                        } else if matches!(act, DocAction::Focus) {
                            // A dropped focus request must not leave the grabbed Tab silent.
                            let _ = a.notices.try_send(DocNotice::FocusFailed { doc: a.spec_doc });
                        }
                    }
                    DocCtl::Select { req, node, act } => {
                        if actions.len() < MAX_ACTIONS {
                            spawn_action(&mut actions, &a, node, act, Some(req));
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
            _ = notice_tick.tick() => a.retry_notice(),
        }
    }
    // Retire: no key may outlive its document.
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

/// IR-5: the action goes to the source that declared it, by the source's own node id.
fn spawn_action(
    set: &mut JoinSet<()>,
    a: &Actor,
    node: DocNodeId,
    act: DocAction,
    select_req: Option<u32>,
) {
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
                        end_on_child: s.end_on_child,
                        child_follows: s.child_follows,
                        trim_ws: s.trim_ws,
                    })
                };
                let spans = spec.spans.iter().map(map).collect::<Option<Vec<_>>>()?;
                let snap = map(&spec.snap)?;
                let root = src_of(d, spec.root)?;
                Some(gorca_model::SelectionSpec { root, spans, snap })
            });
            let Some(mapped) = mapped else {
                fail(a);
                return;
            };
            DocAction::SetSourceSelection(mapped)
        }
        other @ (DocAction::Focus
        | DocAction::Click
        | DocAction::ScrollIntoView
        | DocAction::SetCaret(_)
        | DocAction::SetSourceSelection(_)
        | DocAction::ClearSelection) => other,
    };
    let req = a.requests.clone();
    let notices = a.notices.clone();
    let doc = a.spec_doc;
    let is_focus = matches!(act, DocAction::Focus);
    set.spawn(async move {
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
        if let Some(req) = select_req {
            let outcome = match result {
                Some(Ok(())) => SelOutcome::Done,
                Some(Err(SourceError::NotSupported)) => SelOutcome::NotSupported,
                Some(Err(_)) | None => SelOutcome::Failed,
            };
            let _ = notices.try_send(DocNotice::SelectionDone { doc, req, outcome });
        }
    });
}

#[cfg(test)]
mod tests;
