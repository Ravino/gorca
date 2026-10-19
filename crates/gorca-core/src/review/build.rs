// SPDX-License-Identifier: GPL-3.0-or-later
//! The I/O side of review (ADR-0006 6.1, 5.3, 6.6, 8): everything that talks to an application.
//!
//! These are plain `async fn`s over a [`ReviewIo`] (the app's request channel, its fill channel
//! and its mirror read handle). The runtime runs each in a task it owns and sends the outcome
//! to the Reader as a [`super::ReviewMsg`]; the Reader never awaits any of this (R3). Every
//! request carries a deadline of its budget class and is sent through
//! [`gorca_connector_api::request`], so a hung application costs this task and nothing else.
//!
//! Fetched nodes are applied to the mirror through the app actor ([`FillMsg::ReviewApply`]),
//! and the task waits (briefly) for the actor's acknowledgement before reporting, so the Reader
//! finds them in the mirror (OB-5, MX-4).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gorca_connector_api::{ActionReq, BackendError, BackendOp, BackendReply, MatchRule, request};
use gorca_mirror::{MirrorHandle, MirrorRead};
use gorca_model::{
    ChildList, Lookup, NodeKey, NodeView, PropMask, Rect, ReqId, Role, SnapshotBatch, StateSet,
    TextLine, TextQuery,
};
use gorca_rt::{BudgetClass, Clock, Deadline, mpsc, oneshot, with_deadline};
use tokio::task::JoinSet;

use super::cluster::{BuildInput, BuildNode, build_line_model, clip_of};
use super::collect::{Miss, collect_with};
use super::model::{LineModel, ReviewLimits, TruncWhy};
use super::objnav::{NAV_MASK, NavOp, NavSource};
use crate::app_actor::FillMsg;
use crate::health::HealthMsg;

/// Children fetched per `ChildSnapshots` call (ADR-0006 OB-3).
pub const CHILD_PAGE: u32 = 256;
/// How long a task waits for the app actor to apply what it fetched.
const APPLY_ACK: Duration = Duration::from_millis(50);
/// Collect-and-fetch rounds of one build: one tree level per round, so this is the depth reached
/// (the soft deadline normally ends a build first, and its continuations carry on).
const MAX_ROUNDS: usize = 64;
/// Most children of one node a review build reads (pages of [`CHILD_PAGE`]).
const MAX_CHILDREN: usize = 8192;
/// Ancestors fetched for a parent miss.
const ANCESTRY_DEPTH: u8 = 16;

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum NavFetchError {
    #[error("the application did not answer in time")]
    Timeout,
    #[error("the object is gone")]
    Gone,
    #[error("the request was refused")]
    Refused,
    #[error("the application could not answer")]
    Failed,
}

impl From<&BackendError> for NavFetchError {
    fn from(e: &BackendError) -> Self {
        match e {
            BackendError::Timeout => NavFetchError::Timeout,
            BackendError::Gone => NavFetchError::Gone,
            BackendError::Busy => NavFetchError::Refused,
            BackendError::NotSupported | BackendError::Protocol(_) | BackendError::Unresolved => {
                NavFetchError::Failed
            }
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum BuildError {
    #[error("the application is not responding")]
    Hung,
    #[error("the surface is gone")]
    Gone,
    #[error("the application refuses requests")]
    Refused,
    #[error("the model builder failed")]
    Internal,
}

/// What review needs to talk to one app.
#[derive(Clone, Debug)]
pub struct ReviewIo {
    pub requests: mpsc::Sender<gorca_connector_api::BackendRequest>,
    pub fill: mpsc::Sender<FillMsg>,
    pub health: Option<mpsc::Sender<HealthMsg>>,
    pub mirror: MirrorHandle,
    pub clock: Clock,
    pub next_req: Arc<AtomicU64>,
    pub limits: ReviewLimits,
    /// The engine's text nodes repeat their text in `static` children (`EditQuirks`).
    pub static_leaves_repeat: bool,
    /// Pages of children that a build stopped reading at its deadline, per parent: the next pass
    /// of the same task resumes after them instead of starting at the first page again.
    pub partial_pages: PartialPages,
}

/// See [`ReviewIo::partial_pages`]. Shared by the clones of one task's `ReviewIo`.
pub type PartialPages = Arc<std::sync::Mutex<HashMap<NodeKey, PartialKids>>>;

/// The pages read so far and the child count of the parent when they were read: a resume that
/// finds another count, or another child where the pages end, starts again at the first page.
#[derive(Clone, Debug)]
pub struct PartialKids {
    pub total: Option<usize>,
    pub nodes: Vec<Arc<gorca_model::NodeSnapshot>>,
}

/// The mirror as a [`NavSource`].
pub struct HandleSrc<'a>(pub &'a MirrorHandle);

impl NavSource for HandleSrc<'_> {
    fn view(&self, k: NodeKey) -> Lookup<NodeView> {
        self.0.view(k)
    }
}

impl ReviewIo {
    fn deadline(&self, class: BudgetClass) -> Deadline {
        Deadline::for_class(&self.clock, class)
    }

    async fn call(&self, class: BudgetClass, op: BackendOp) -> Result<BackendReply, BackendError> {
        let id = ReqId(self.next_req.fetch_add(1, Ordering::Relaxed));
        let r = request(&self.requests, id, class, op, self.deadline(class)).await;
        if let Some(h) = &self.health {
            match &r {
                Ok(_) => {
                    let _ = h.try_send(HealthMsg::Success);
                }
                Err(BackendError::Timeout) => {
                    let _ = h.try_send(HealthMsg::Timeout { class });
                }
                Err(
                    BackendError::Gone
                    | BackendError::NotSupported
                    | BackendError::Busy
                    | BackendError::Protocol(_)
                    | BackendError::Unresolved,
                ) => {}
            }
        }
        r
    }

    /// Hand `batch` to the app actor and wait (bounded) until it is in the mirror.
    async fn apply(
        &self,
        node: NodeKey,
        issued_at: u64,
        result: Result<SnapshotBatch, BackendError>,
    ) {
        let (ack, wait) = oneshot::channel();
        let msg = FillMsg::ReviewApply {
            node,
            issued_at,
            result,
            ack: Some(ack),
        };
        if self.fill.try_send(msg).is_ok() {
            let _ = tokio::time::timeout(APPLY_ACK, wait).await;
        }
    }

    fn seq(&self) -> u64 {
        self.mirror.bump_seq()
    }
}

fn subtree(r: BackendReply) -> Result<SnapshotBatch, BackendError> {
    match r {
        BackendReply::Subtree(b) => Ok(b),
        BackendReply::Node(n) => Ok(SnapshotBatch::new(vec![n])),
        BackendReply::Children(_)
        | BackendReply::Text(_)
        | BackendReply::CaretOffset(_)
        | BackendReply::TextAttributes(_)
        | BackendReply::ActionDone
        | BackendReply::Hit(_)
        | BackendReply::Matches(_)
        | BackendReply::Lines(_)
        | BackendReply::Unit(_)
        | BackendReply::Selections(_)
        | BackendReply::Format(_)
        | BackendReply::Target(_)
        | BackendReply::Window(_) => {
            Err(BackendError::Protocol("unexpected reply kind".to_owned()))
        }
    }
}

/// `node` and its children, applied to the mirror, with the node's child list `Known` when the
/// reply covers all of them. One `Fetch` and one `ChildSnapshots` in flight together.
///
/// `until`: the build's soft deadline. The pages after the first are read one by one and each can
/// take a full budget class on a slow app, so no further page is asked for once it has passed;
/// the node then stays without a `Known` list and the build says it is truncated (R2-1).
pub async fn expand(
    io: &ReviewIo,
    node: NodeKey,
    class: BudgetClass,
    bounds: bool,
    until: Option<Instant>,
) -> Result<(), NavFetchError> {
    let want = if bounds {
        NAV_MASK | PropMask::BOUNDS
    } else {
        NAV_MASK
    };
    let issued = io.seq();
    // Pages an earlier pass stopped after (its deadline): resume behind them.
    let resumed = io
        .partial_pages
        .lock()
        .ok()
        .and_then(|m| m.get(&node).cloned())
        .filter(|p| !p.nodes.is_empty());
    // A resumed call always reads one more page, whatever the clock says: the time before it
    // went to the other misses of the pass, and a call that read nothing would loop forever.
    let mut owed_page = resumed.is_some();
    let saved_total = resumed.as_ref().map(|r| r.total);
    let (parent, kids) = match resumed {
        Some(prev) => {
            let parent = io.call(class, BackendOp::Fetch { node, want }).await;
            (
                parent,
                Ok(BackendReply::Subtree(SnapshotBatch::new(prev.nodes))),
            )
        }
        None => {
            tokio::join!(
                io.call(class, BackendOp::Fetch { node, want }),
                io.call(
                    class,
                    BackendOp::ChildSnapshots {
                        node,
                        range: 0..CHILD_PAGE,
                        want
                    }
                )
            )
        }
    };
    let parent = match parent {
        Ok(BackendReply::Node(n)) => n,
        Ok(_) => return Err(NavFetchError::Failed),
        Err(e) => {
            if e == BackendError::Gone {
                io.apply(node, issued, Err(e.clone())).await;
            }
            return Err(NavFetchError::from(&e));
        }
    };
    let kids = match kids.and_then(subtree) {
        Ok(b) => b,
        Err(e) => {
            // The node itself is still worth keeping.
            io.apply(node, issued, Ok(SnapshotBatch::new(vec![parent])))
                .await;
            return Err(NavFetchError::from(&e));
        }
    };
    // More than one page of children (a long list, a web page's main element): the next pages
    // follow one after the other, up to `MAX_CHILDREN`, so the node is not expanded again and
    // again with the same first page.
    let mut kids = kids;
    let total = match parent.children {
        ChildList::Count(n) => usize::try_from(n).ok(),
        ChildList::Known(_) | ChildList::Unknown => None,
    };
    let page = CHILD_PAGE as usize;
    // The saved pages are only good while the list is the one they were read from: another
    // count means children came or went (a shifted list repeats and loses entries).
    let mut verify: Option<NodeKey> = None;
    if let Some(saved) = saved_total {
        if saved == total {
            verify = kids.nodes.last().map(|n| n.key);
        } else {
            kids.nodes.clear();
            owed_page = true;
        }
    }
    let mut last_page_len = if kids.nodes.is_empty() || verify.is_some() {
        page
    } else if kids.nodes.len() % page == 0 {
        page.min(kids.nodes.len())
    } else {
        kids.nodes.len() % page
    };
    while last_page_len == page
        && kids.nodes.len() < MAX_CHILDREN
        && total.is_none_or(|t| kids.nodes.len() < t)
        && (owed_page || until.is_none_or(|t| Instant::now() < t))
    {
        owed_page = false;
        // A resume asks from the last saved child on and checks it is still where it was.
        let from = kids.nodes.len() - usize::from(verify.is_some());
        let start = u32::try_from(from).unwrap_or(u32::MAX);
        let more = io
            .call(
                class,
                BackendOp::ChildSnapshots {
                    node,
                    range: start..start.saturating_add(CHILD_PAGE),
                    want,
                },
            )
            .await
            .and_then(subtree);
        match more {
            Ok(mut b) if !b.nodes.is_empty() => {
                if let Some(last) = verify.take() {
                    if b.nodes.first().map(|n| n.key) != Some(last) {
                        // The list moved under the saved pages: read it again from the start.
                        kids.nodes.clear();
                        last_page_len = page;
                        owed_page = true;
                        continue;
                    }
                    last_page_len = b.nodes.len();
                    b.nodes.remove(0);
                    if b.nodes.is_empty() {
                        break;
                    }
                } else {
                    last_page_len = b.nodes.len();
                }
                kids.truncated = b.truncated;
                kids.nodes.extend(b.nodes);
            }
            Ok(_) | Err(_) => break,
        }
    }
    // Stopped by the deadline with pages left: keep what was read for the next pass.
    let unfinished = last_page_len == page
        && kids.nodes.len() < MAX_CHILDREN
        && total.is_none_or(|t| kids.nodes.len() < t)
        && until.is_some_and(|t| Instant::now() >= t);
    if let Ok(mut m) = io.partial_pages.lock() {
        if unfinished {
            m.insert(
                node,
                PartialKids {
                    total,
                    nodes: kids.nodes.clone(),
                },
            );
        } else {
            m.remove(&node);
        }
    }
    // A known count settles it (a strategy may flag `truncated` when it reads past the last
    // child); without one, an unflagged short last page is the whole list.
    let complete = match parent.children {
        ChildList::Count(n) => {
            usize::try_from(n).is_ok_and(|n| n == kids.nodes.len())
                || kids.nodes.len() >= MAX_CHILDREN
        }
        ChildList::Known(_) => false,
        ChildList::Unknown => !kids.truncated && last_page_len < page,
    };
    tracing::debug!(
        ?node,
        parent_children = ?parent.children,
        kids = kids.nodes.len(),
        truncated = kids.truncated,
        complete,
        "review expand"
    );
    let mut p = (*parent).clone();
    if complete {
        p.children = ChildList::Known(kids.nodes.iter().map(|k| k.key).collect());
        p.have |= PropMask::CHILDREN;
    }
    let mut nodes = Vec::with_capacity(kids.nodes.len() + 1);
    nodes.push(Arc::new(p));
    nodes.extend(kids.nodes);
    io.apply(node, issued, Ok(SnapshotBatch::new(nodes))).await;
    Ok(())
}

/// One navigation fetch (ADR-0006 5.3), applied to the mirror.
pub async fn nav_fetch(io: &ReviewIo, op: NavOp) -> Result<(), NavFetchError> {
    let class = BudgetClass::Interactive;
    match op {
        NavOp::Children { node } => expand(io, node, class, false, None).await,
        NavOp::Ancestry { node } => {
            let issued = io.seq();
            let r = io
                .call(
                    class,
                    BackendOp::Ancestry {
                        node,
                        max_depth: ANCESTRY_DEPTH,
                        want: NAV_MASK,
                    },
                )
                .await
                .and_then(subtree);
            finish(io, node, issued, r).await
        }
        NavOp::Node { node } => {
            let issued = io.seq();
            let r = io
                .call(
                    class,
                    BackendOp::Fetch {
                        node,
                        want: NAV_MASK,
                    },
                )
                .await
                .and_then(subtree);
            finish(io, node, issued, r).await
        }
    }
}

async fn finish(
    io: &ReviewIo,
    node: NodeKey,
    issued: u64,
    r: Result<SnapshotBatch, BackendError>,
) -> Result<(), NavFetchError> {
    let out = r.as_ref().map(|_| ()).map_err(NavFetchError::from);
    if r.is_ok() || r.as_ref().err() == Some(&BackendError::Gone) {
        io.apply(node, issued, r).await;
    }
    out
}

/// Run `jobs` with at most `limit` in flight; results in completion order.
async fn bounded<T: Send + 'static>(
    jobs: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>>>,
    limit: usize,
) -> Vec<T> {
    let mut set: JoinSet<T> = JoinSet::new();
    let mut out = Vec::with_capacity(jobs.len());
    let mut it = jobs.into_iter();
    loop {
        while set.len() < limit.max(1) {
            match it.next() {
                Some(j) => {
                    set.spawn(j);
                }
                None => break,
            }
        }
        match set.join_next().await {
            Some(Ok(v)) => out.push(v),
            Some(Err(_)) => {}
            None => break,
        }
    }
    out
}

/// Fetch one miss of a build.
async fn serve_miss(
    io: ReviewIo,
    m: Miss,
    class: BudgetClass,
    until: Instant,
) -> Result<(), NavFetchError> {
    match m {
        Miss::Expand(n) => expand(&io, n, class, true, Some(until)).await,
        Miss::Node(n) | Miss::Bounds(n) => {
            let issued = io.seq();
            let r = io
                .call(
                    class,
                    BackendOp::Fetch {
                        node: n,
                        want: NAV_MASK | PropMask::BOUNDS,
                    },
                )
                .await
                .and_then(subtree);
            finish(&io, n, issued, r).await
        }
    }
}

/// Attempts at one node's lines when the app is busy (its request slots are taken).
const LINES_TRIES: usize = 4;

/// Why a node has no lines.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LinesFail {
    /// Nothing went wrong, or the app cannot say (not worth asking again).
    No,
    /// The app refused for now (its request slots are taken): asked again at once.
    Busy,
    /// No answer in time: not asked again in this pass (each try costs a class deadline, and
    /// timeouts in a row make the app count as hung), the continuation tries later.
    Slow,
}

/// What [`lines_of`] found: the lines, `None` when the app cannot say, and why.
type LinesAnswer = (NodeKey, Option<Arc<[TextLine]>>, LinesFail);

/// Visible lines of `node` inside `clip`. A busy app is asked again a few times while `until`
/// (the build's soft deadline) allows: a model built while eight such requests are in flight must
/// not lose the rows whose request was refused. A timeout is never retried.
async fn lines_of(
    io: ReviewIo,
    node: NodeKey,
    clip: Rect,
    class: BudgetClass,
    until: Instant,
) -> LinesAnswer {
    let mut last = (node, None, LinesFail::No);
    for attempt in 0..LINES_TRIES {
        if attempt > 0 {
            if Instant::now() >= until {
                break;
            }
            let wait = 5 * u64::try_from(attempt).unwrap_or(1);
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
        last = lines_once(&io, node, clip, class).await;
        if last.2 != LinesFail::Busy {
            break;
        }
    }
    last
}

async fn lines_once(io: &ReviewIo, node: NodeKey, clip: Rect, class: BudgetClass) -> LinesAnswer {
    let q = TextQuery::VisibleLines {
        node,
        clip,
        max_lines: io.limits.lines_per_node,
        max_bytes: io.limits.bytes_per_node,
    };
    match io.call(class, BackendOp::Text(q)).await {
        Ok(BackendReply::Lines(l)) => {
            // A line of nothing but embedded-object markers (a web document's own text is one
            // marker per child) says nothing and its extents cover its children: dropped.
            let lines: Vec<TextLine> = l
                .lines
                .into_iter()
                .filter(|x| {
                    x.text
                        .chars()
                        .any(|c| !c.is_whitespace() && c != '\u{FFFC}')
                })
                .collect();
            (node, Some(Arc::from(lines)), LinesFail::No)
        }
        Ok(_) => (node, None, LinesFail::No),
        Err(e) => {
            tracing::debug!(?node, error = ?e, "review text lines failed");
            let why = match e {
                BackendError::Busy => LinesFail::Busy,
                BackendError::Timeout => LinesFail::Slow,
                BackendError::Gone
                | BackendError::NotSupported
                | BackendError::Protocol(_)
                | BackendError::Unresolved => LinesFail::No,
            };
            (node, None, why)
        }
    }
}

/// What a build is asked to do.
#[derive(Clone, Debug)]
pub struct BuildSpec {
    pub surface: NodeKey,
    /// Nodes whose text lines must be fetched again (dirty); everything else reuses `prev`.
    pub dirty: Option<Arc<[NodeKey]>>,
    pub prev: Option<Arc<LineModel>>,
    /// Soft deadline: publish what is ready when it passes (ADR-0006 6.8).
    pub budget: Duration,
    pub class: BudgetClass,
}

/// Build the line model of one surface (ADR-0006 6.1). Never panics; a panic inside the pure
/// builder is contained and reported as `BuildError::Internal`.
pub async fn build_model(io: &ReviewIo, spec: BuildSpec) -> Result<Arc<LineModel>, BuildError> {
    let started = Instant::now();
    let src = HandleSrc(&io.mirror);
    let mut truncated: Option<TruncWhy> = None;
    let mut last_error: Option<NavFetchError> = None;
    let mut collected = collect_with(&src, spec.surface, io.limits, io.static_leaves_repeat);
    for _ in 0..MAX_ROUNDS {
        if collected.misses.is_empty() {
            break;
        }
        if started.elapsed() >= spec.budget {
            truncated = Some(TruncWhy::Deadline);
            break;
        }
        let jobs = collected
            .misses
            .iter()
            .map(|m| {
                let (io, m, class) = (io.clone(), *m, spec.class);
                Box::pin(serve_miss(io, m, class, started + spec.budget))
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<(), NavFetchError>> + Send>,
                    >
            })
            .collect();
        let results = bounded(jobs, io.limits.in_flight).await;
        let progressed = results.iter().any(Result::is_ok);
        tracing::debug!(
            misses = collected.misses.len(),
            ok = results.iter().filter(|r| r.is_ok()).count(),
            nodes = collected.nodes.len(),
            "review build round"
        );
        if let Some(Err(e)) = results.into_iter().find(Result::is_err) {
            last_error = Some(e);
        }
        collected = collect_with(&src, spec.surface, io.limits, io.static_leaves_repeat);
        if !progressed {
            break;
        }
    }
    if collected.nodes.is_empty() {
        return Err(match last_error {
            Some(NavFetchError::Gone) => BuildError::Gone,
            Some(NavFetchError::Refused) => BuildError::Refused,
            Some(NavFetchError::Timeout | NavFetchError::Failed) | None => BuildError::Hung,
        });
    }
    if truncated.is_none() && !collected.misses.is_empty() {
        // Whatever stayed missing is not shown: the model says so.
        truncated = Some(TruncWhy::Deadline);
    }
    if truncated.is_none() {
        truncated = collected.truncated;
    }

    // Text lines: reuse the previous model's for clean nodes, ask for the rest.
    let clip = clip_of(collected.nodes.first().and_then(|n| n.snap.bounds));
    let dirty: Option<&[NodeKey]> = spec.dirty.as_deref();
    let mut lines: HashMap<NodeKey, Arc<[TextLine]>> = HashMap::new();
    let mut ask: Vec<NodeKey> = Vec::new();
    for k in &collected.text_nodes {
        let reusable = spec
            .prev
            .as_ref()
            .and_then(|p| p.texts.get(k))
            .filter(|_| dirty.is_some_and(|d| !d.contains(k)));
        match reusable {
            Some(l) => {
                lines.insert(*k, Arc::clone(l));
            }
            None => ask.push(*k),
        }
    }
    if started.elapsed() >= spec.budget && !ask.is_empty() {
        truncated.get_or_insert(TruncWhy::Deadline);
    } else {
        let jobs = ask
            .iter()
            .map(|k| {
                Box::pin(lines_of(
                    io.clone(),
                    *k,
                    collected.text_clips.get(k).copied().unwrap_or(clip),
                    spec.class,
                    started + spec.budget,
                ))
                    as std::pin::Pin<Box<dyn std::future::Future<Output = LinesAnswer> + Send>>
            })
            .collect();
        for (k, l, why) in bounded(jobs, io.limits.in_flight).await {
            if let Some(l) = l {
                lines.insert(k, l);
            } else if why != LinesFail::No {
                // Not "complete": the continuation asks for this node again.
                truncated.get_or_insert(TruncWhy::Deadline);
            }
        }
    }

    let built_rev = io.mirror.revision();
    let final_pass = collect_with(&src, spec.surface, io.limits, io.static_leaves_repeat);
    let nodes: Vec<BuildNode> = final_pass
        .nodes
        .into_iter()
        .map(|mut n| {
            n.lines = lines.get(&n.snap.key).cloned();
            n
        })
        .collect();
    let input = BuildInput {
        app: spec.surface.app,
        surface: spec.surface,
        nodes,
        built_rev,
        built_at: Instant::now(),
        truncated: truncated.or(final_pass.truncated),
    };
    let limits = io.limits;
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        build_line_model(&input, &limits)
    }));
    match built {
        Ok(m) => {
            if tracing::enabled!(tracing::Level::TRACE) {
                for (i, l) in m.lines.iter().enumerate() {
                    tracing::trace!(i, top = l.top, bottom = l.bottom, text = %l.text, "review line");
                    for z in m.line_zones(i) {
                        tracing::trace!(kind = ?z.kind, text = %z.text, rect = ?z.rect, "review zone");
                    }
                }
            }
            tracing::debug!(
                lines = m.lines.len(),
                zones = m.zones.len(),
                completeness = ?m.completeness,
                still_missing = ?collected.misses,
                "review model built"
            );
            Ok(Arc::new(m))
        }
        Err(_) => Err(BuildError::Internal),
    }
}

/// Find the status bar below `root` with one `Collection` call (ADR-0006 8 step 2), and bring it
/// and its children into the mirror.
pub async fn find_status_bar(
    io: &ReviewIo,
    root: NodeKey,
) -> Result<Option<NodeKey>, NavFetchError> {
    let rule = MatchRule {
        root,
        roles: vec![Role::StatusBar],
        states_all: StateSet::SHOWING,
        states_none: StateSet::empty(),
        max_results: 1,
    };
    let found = match io
        .call(BudgetClass::Interactive, BackendOp::Collection(rule))
        .await
    {
        Ok(BackendReply::Matches(v)) => v.into_iter().next(),
        Ok(_) => None,
        Err(e) => return Err(NavFetchError::from(&e)),
    };
    let Some(bar) = found else { return Ok(None) };
    if bar.app != root.app {
        return Ok(None);
    }
    expand(io, bar, BudgetClass::Interactive, false, None).await?;
    Ok(Some(bar))
}

/// One node the revalidation compares (RR-1).
#[derive(Clone, Debug)]
pub struct ProbeItem {
    pub node: NodeKey,
    pub rect: Option<Rect>,
    /// The visible lines the model holds for this node, if it is a text node.
    pub lines: Option<Arc<[TextLine]>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RrVerdict {
    Same,
    /// Extents or text changed: the fetched nodes are in the mirror, so the next validity check
    /// finds the model dirty and refreshes it.
    Changed {
        text_changed: bool,
    },
    Failed,
}

/// RR-1: re-read the extents of the probe nodes (and the visible lines of text ones) and compare
/// them with what the model holds. The fresh extents go into the mirror.
pub async fn revalidate(io: &ReviewIo, probe: Vec<ProbeItem>, clip: Rect) -> RrVerdict {
    let class = BudgetClass::Prefetch;
    let issued = io.seq();
    let nodes: Vec<NodeKey> = probe.iter().map(|p| p.node).collect();
    let mut batch = Vec::new();
    let mut moved = false;
    let mut any_ok = false;
    for n in &nodes {
        let r = io
            .call(
                class,
                BackendOp::Fetch {
                    node: *n,
                    want: NAV_MASK | PropMask::BOUNDS,
                },
            )
            .await;
        if let Ok(BackendReply::Node(s)) = r {
            any_ok = true;
            let item = probe.iter().find(|p| p.node == *n);
            if let Some(item) = item
                && item.rect != s.bounds
            {
                moved = true;
            }
            batch.push(s);
        }
    }
    if !batch.is_empty()
        && let Some(first) = nodes.first()
    {
        io.apply(*first, issued, Ok(SnapshotBatch::new(batch)))
            .await;
    }
    let mut text_changed = false;
    for p in &probe {
        let Some(old) = &p.lines else { continue };
        let (_, fresh, _) = lines_of(io.clone(), p.node, clip, class, Instant::now()).await;
        if let Some(fresh) = fresh {
            any_ok = true;
            let same = fresh.len() == old.len()
                && fresh.iter().zip(old.iter()).all(|(a, b)| a.text == b.text);
            if !same {
                text_changed = true;
            }
        }
    }
    if !any_ok {
        RrVerdict::Failed
    } else if moved || text_changed {
        RrVerdict::Changed { text_changed }
    } else {
        RrVerdict::Same
    }
}

/// Where a pointer must go to hit `node` (or its character `offset`): the pointer resolver of the node's app
/// (ADR-0025 2.4). Errors are the neutral pointer errors: "no location" for everything that means the node
/// has no usable place on screen.
pub async fn locate(
    io: &ReviewIo,
    node: NodeKey,
    offset: Option<u32>,
) -> Result<gorca_output_api::PointTarget, gorca_output_api::PointerError> {
    use gorca_output_api::PointerError;
    let r = with_deadline(
        io.deadline(BudgetClass::Interactive),
        io.call(BudgetClass::Interactive, BackendOp::Locate { node, offset }),
    )
    .await;
    match r {
        Ok(Ok(BackendReply::Target(t))) => Ok(t),
        Ok(Ok(_)) => Err(PointerError::Failed("unexpected reply kind".to_owned())),
        Ok(Err(BackendError::Timeout)) | Err(_) => Err(PointerError::Timeout),
        Ok(Err(
            BackendError::Gone
            | BackendError::NotSupported
            | BackendError::Unresolved
            | BackendError::Busy
            | BackendError::Protocol(_),
        )) => Err(PointerError::NoLocation),
    }
}

/// An action on the navigator (`review.activate`, `review.move_focus`).
pub async fn act(io: &ReviewIo, req: ActionReq) -> Result<(), NavFetchError> {
    // A user's keypress: Interactive.
    let r = with_deadline(
        io.deadline(BudgetClass::Interactive),
        io.call(BudgetClass::Interactive, BackendOp::Action(req)),
    )
    .await;
    match r {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(NavFetchError::from(&e)),
        Err(_) => Err(NavFetchError::Timeout),
    }
}
