// SPDX-License-Identifier: GPL-3.0-or-later
//! The document registry (browse-mode design sections 5.3 and 6, ADR-0003 AM-10): one task that
//! owns the web side of every application.
//!
//! * **Warm-up** (`WarmState`): a browser serves AT-SPI from a lazily populated cache; the
//!   registry activates it once per application at attach, in the background, and holds document
//!   builds for at most [`BUILD_HOLD`] waiting for it. A build that started earlier is rebuilt
//!   once (ids kept) when the warm-up completes.
//! * **Ensure**: when the Reader sees focus enter a web application it asks for the document
//!   around the focused node. The registry locates the top-level document, creates its source
//!   through the factory and starts its document actor.
//! * **Ownership**: every task (warm-up, locate, create, document actors) lives in one `JoinSet`
//!   owned here and ends on the shutdown token. Every factory call carries a timeout.
//!
//! The registry never touches a mirror and never blocks: factory calls run as tasks and come back
//! as `TaskDone`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc_source_api::{DocSource, DocSourceFactory, SourceError, WarmOutcome};
use gorca_model::{AppId, AppInfo, DocId, NodeKey};
use gorca_rt::{CancellationToken, mpsc};
use tokio::task::JoinSet;

use crate::doc_actor::repair::{RepairPolicy, RepairReason};
use crate::doc_actor::{
    DocActorSpec, DocCounters, DocCtl, DocDirectory, DocNotice, WebJoin, prepare,
};

/// Most notices the registry keeps while the Reader's queue is full.
const MAX_PENDING_NOTICES: usize = 64;
/// How long a document build waits for the cache warm-up (BD-5).
pub const BUILD_HOLD: Duration = Duration::from_secs(3);
const WARM_TIMEOUT: Duration = Duration::from_secs(5);
const LOCATE_TIMEOUT: Duration = Duration::from_secs(2);
const CREATE_TIMEOUT: Duration = Duration::from_secs(5);
/// Warm-up retries after a failure, and at most this many.
const FAILED_RETRY: Duration = Duration::from_secs(10);
const MAX_WARM_ATTEMPTS: u8 = 3;
/// Backoff of the search for a first document: 1 s doubling up to this.
const NO_DOC_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Nodes remembered as "not inside a web document" (browser chrome).
const NO_DOC_MEMORY: usize = 256;
/// An application whose document ended this recently adopts the next load-complete (navigation).
const RECENT_DOC: Duration = Duration::from_secs(30);
const HUB_QUEUE: usize = 256;

/// Milliseconds with a fraction, for the phase log.
fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Messages to the registry.
#[derive(Debug)]
pub enum HubMsg {
    Appeared {
        app: AppId,
        info: Box<AppInfo>,
    },
    Ready {
        app: AppId,
    },
    Gone {
        app: AppId,
    },
    /// Focus entered a web application: find or create the document around `focus`.
    Ensure {
        focus: NodeKey,
    },
    /// `document:load-complete` or `reload` on `node`.
    Loaded {
        node: NodeKey,
    },
}

/// Producer handle held by the app actors and the Reader's effect executor.
pub type HubTx = mpsc::Sender<HubMsg>;

pub fn hub_channel() -> (HubTx, mpsc::Receiver<HubMsg>) {
    mpsc::channel(HUB_QUEUE)
}

/// Cache-domain warm-up of one application (browse design 6).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WarmState {
    NotNeeded,
    /// Ready, but no document found yet.
    NoDocument {
        retry_at: Instant,
        attempt: u8,
    },
    Warming {
        started: Instant,
    },
    Warm,
    Failed {
        attempts: u8,
        retry_at: Instant,
    },
}

impl WarmState {
    /// Builds may proceed without waiting.
    fn open(self) -> bool {
        matches!(
            self,
            WarmState::NotNeeded | WarmState::Warm | WarmState::Failed { .. }
        )
    }
}

struct AppWeb {
    info: AppInfo,
    web: bool,
    ready: bool,
    warm: WarmState,
    /// Focus nodes waiting for the warm-up, with their deadline.
    waiting: Vec<(NodeKey, Instant)>,
    locating: HashSet<NodeKey>,
    /// Document roots being created, with the focus that asked.
    creating: HashMap<NodeKey, NodeKey>,
    /// Documents built before the warm-up finished (`attrs_suspect`).
    suspect: Vec<DocId>,
    no_doc: VecDeque<NodeKey>,
    last_doc_end: Option<Instant>,
}

impl AppWeb {
    fn new(info: AppInfo, web: bool, needs_warm: bool, now: Instant) -> Self {
        Self {
            info,
            web,
            ready: false,
            warm: if web && needs_warm {
                WarmState::NoDocument {
                    retry_at: now,
                    attempt: 0,
                }
            } else {
                WarmState::NotNeeded
            },
            waiting: Vec::new(),
            locating: HashSet::new(),
            creating: HashMap::new(),
            suspect: Vec::new(),
            no_doc: VecDeque::new(),
            last_doc_end: None,
        }
    }

    fn remember_no_doc(&mut self, n: NodeKey) {
        if self.no_doc.contains(&n) {
            return;
        }
        if self.no_doc.len() >= NO_DOC_MEMORY {
            self.no_doc.pop_front();
        }
        self.no_doc.push_back(n);
    }
}

/// What a task of the registry returns.
enum TaskDone {
    Warmed {
        app: AppId,
        res: Result<WarmOutcome, SourceError>,
    },
    Located {
        app: AppId,
        focus: NodeKey,
        res: Result<Option<NodeKey>, SourceError>,
    },
    Created {
        app: AppId,
        root: NodeKey,
        res: Result<Box<dyn DocSource>, SourceError>,
    },
    DocEnded {
        doc: DocId,
        app: AppId,
    },
}

/// Wiring of the registry.
#[allow(missing_debug_implementations)]
pub struct HubCfg {
    pub factory: Arc<dyn DocSourceFactory>,
    /// How long a build waits for the warm-up ([`BUILD_HOLD`] in the product).
    pub build_hold: Duration,
    /// Unit of the warm-up backoff (1 s in the product).
    pub retry_unit: Duration,
    pub join: Arc<WebJoin>,
    pub docs: Arc<DocDirectory>,
    pub notices: mpsc::Sender<DocNotice>,
    pub counters: Arc<DocCounters>,
    /// The published profile snapshot: `browse.enabled` per application (ADR-0016 section 2.9).
    pub profiles: Arc<gorca_config::profiles::ProfileCell>,
}

struct Hub {
    cfg: HubCfg,
    apps: HashMap<AppId, AppWeb>,
    tasks: JoinSet<TaskDone>,
    tokens: HashMap<DocId, (AppId, CancellationToken)>,
    next_doc: u32,
    shutdown: CancellationToken,
    /// Notices the Reader's queue had no room for (N6): retried every 50 ms, never dropped
    /// except past [`MAX_PENDING_NOTICES`] (counted).
    pending: std::collections::VecDeque<DocNotice>,
}

async fn timed<T>(
    d: Duration,
    fut: impl std::future::Future<Output = Result<T, SourceError>>,
) -> Result<T, SourceError> {
    tokio::time::timeout(d, fut)
        .await
        .unwrap_or(Err(SourceError::Timeout))
}

impl Hub {
    fn notify(&mut self, n: DocNotice) {
        if self.pending.len() >= MAX_PENDING_NOTICES {
            self.pending.pop_front();
            self.cfg
                .counters
                .notices_dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.pending.push_back(n);
        self.flush_notices();
    }

    fn flush_notices(&mut self) {
        while let Some(n) = self.pending.front().cloned() {
            if self.cfg.notices.try_send(n).is_err() {
                break;
            }
            self.pending.pop_front();
        }
    }

    fn start_warm(&mut self, app: AppId, hint: Option<NodeKey>, now: Instant) {
        let Some(a) = self.apps.get_mut(&app) else {
            return;
        };
        a.warm = WarmState::Warming { started: now };
        tracing::debug!(?app, ?hint, "warm-up started");
        let fut = self.cfg.factory.warm(app, &a.info, hint);
        self.tasks.spawn(async move {
            let t = Instant::now();
            let res = timed(WARM_TIMEOUT, fut).await;
            if let Ok(WarmOutcome::Done(r)) = &res {
                tracing::info!(
                    target: "gorca_phase",
                    phase = "warm",
                    wall_ms = ms(t.elapsed()),
                    waited_ms = ms(r.waited),
                    blocked_ms = ms(r.blocked),
                    slept_ms = ms(r.slept),
                    rounds = r.rounds,
                    probes = r.probes,
                    sampled_nodes = r.sampled_nodes,
                    tagged_nodes = r.tagged_nodes,
                    ready = r.ready,
                );
            }
            TaskDone::Warmed { app, res }
        });
    }

    fn start_locate(&mut self, app: AppId, focus: NodeKey) {
        let Some(a) = self.apps.get_mut(&app) else {
            return;
        };
        if !a.locating.insert(focus) {
            return;
        }
        let fut = self.cfg.factory.locate(app, &a.info, focus);
        self.tasks.spawn(async move {
            let t = Instant::now();
            let res = timed(LOCATE_TIMEOUT, fut).await;
            tracing::info!(target: "gorca_phase", phase = "locate", wall_ms = ms(t.elapsed()));
            TaskDone::Located { app, focus, res }
        });
    }

    fn ensure(&mut self, focus: NodeKey, now: Instant, silent: bool) {
        let app = focus.app;
        tracing::debug!(?focus, silent, "ensure document");
        let Some(a) = self.apps.get_mut(&app) else {
            if !silent {
                self.notify(DocNotice::NoDocument { focus });
            }
            return;
        };
        if !a.web || a.no_doc.contains(&focus) {
            if !silent {
                self.notify(DocNotice::NoDocument { focus });
            }
            return;
        }
        if self.cfg.join.get(focus).is_some() {
            return;
        }
        match a.warm {
            WarmState::NotNeeded | WarmState::Warm | WarmState::Failed { .. } => {
                self.start_locate(app, focus);
            }
            WarmState::Warming { .. } => a.waiting.push((focus, now + self.cfg.build_hold)),
            WarmState::NoDocument { .. } => {
                // The focus is inside a document: warm with it as the hint, now.
                a.waiting.push((focus, now + self.cfg.build_hold));
                self.start_warm(app, Some(focus), now);
            }
        }
    }

    fn release_waiting(&mut self, app: AppId, only_due: Option<Instant>) {
        let Some(a) = self.apps.get_mut(&app) else {
            return;
        };
        let (go, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut a.waiting)
            .into_iter()
            .partition(|(_, due)| only_due.is_none_or(|now| *due <= now));
        a.waiting = keep;
        for (focus, _) in go {
            self.start_locate(app, focus);
        }
    }

    fn on_warmed(&mut self, app: AppId, res: Result<WarmOutcome, SourceError>, now: Instant) {
        tracing::debug!(?app, ?res, "warm-up finished");
        let Some(a) = self.apps.get_mut(&app) else {
            return;
        };
        let attempts_so_far = match a.warm {
            WarmState::Failed { attempts, .. } => attempts,
            WarmState::NoDocument { .. }
            | WarmState::Warming { .. }
            | WarmState::Warm
            | WarmState::NotNeeded => 0,
        };
        let attempt_no = match a.warm {
            WarmState::NoDocument { attempt, .. } => attempt,
            WarmState::Failed { .. }
            | WarmState::Warming { .. }
            | WarmState::Warm
            | WarmState::NotNeeded => 0,
        };
        a.warm = match res {
            Ok(WarmOutcome::Done(r)) if r.ready => WarmState::Warm,
            Ok(WarmOutcome::Done(_)) | Err(_) => {
                let attempts = attempts_so_far.saturating_add(1);
                WarmState::Failed {
                    attempts,
                    retry_at: now + FAILED_RETRY,
                }
            }
            Ok(WarmOutcome::NoDocument) => {
                let attempt = attempt_no.saturating_add(1);
                let back = (self.cfg.retry_unit * (1u32 << attempt.min(5))).min(NO_DOC_BACKOFF_MAX);
                WarmState::NoDocument {
                    retry_at: now + back,
                    attempt,
                }
            }
        };
        let warm_now = a.warm == WarmState::Warm;
        let suspects = if warm_now {
            std::mem::take(&mut a.suspect)
        } else {
            Vec::new()
        };
        match a.warm {
            WarmState::Warm | WarmState::Failed { .. } | WarmState::NotNeeded => {
                self.release_waiting(app, None);
            }
            WarmState::NoDocument { .. } | WarmState::Warming { .. } => {}
        }
        // A late success rebuilds the documents that were built too early.
        for d in suspects {
            if let Some(h) = self.cfg.docs.get(d) {
                let _ = h.ctl.try_send(DocCtl::Rebuild);
            }
        }
    }

    fn on_located(
        &mut self,
        app: AppId,
        focus: NodeKey,
        res: Result<Option<NodeKey>, SourceError>,
        now: Instant,
    ) {
        if let Some(a) = self.apps.get_mut(&app) {
            a.locating.remove(&focus);
        }
        tracing::debug!(?focus, ?res, "document located");
        match res {
            Ok(Some(root)) => {
                if let Some(h) = self.cfg.docs.by_root(root) {
                    // The focus is inside a document we already mirror but the mirror does not
                    // know the node: it is stale. One repair at a time per document, metered by
                    // the document's gate (CHR-0).
                    h.request_repair(RepairReason::FocusMiss);
                    self.notify(DocNotice::InDocument { focus, doc: h.doc });
                    return;
                }
                let Some(a) = self.apps.get_mut(&app) else {
                    return;
                };
                if a.creating.contains_key(&root) {
                    return;
                }
                a.creating.insert(root, focus);
                let fut = self.cfg.factory.create(app, &a.info, root);
                self.tasks.spawn(async move {
                    let t = Instant::now();
                    let res = timed(CREATE_TIMEOUT, fut).await;
                    tracing::info!(target: "gorca_phase", phase = "create", wall_ms = ms(t.elapsed()));
                    TaskDone::Created { app, root, res }
                });
            }
            Ok(None) => {
                if let Some(a) = self.apps.get_mut(&app) {
                    a.remember_no_doc(focus);
                }
                self.notify(DocNotice::NoDocument { focus });
            }
            Err(e) => {
                tracing::debug!(error = %e, "locating the document failed");
                self.notify(DocNotice::NoDocument { focus });
            }
        }
        let _ = now;
    }

    fn on_created(
        &mut self,
        app: AppId,
        root: NodeKey,
        res: Result<Box<dyn DocSource>, SourceError>,
    ) {
        let focus = self
            .apps
            .get_mut(&app)
            .and_then(|a| a.creating.remove(&root));
        let source = match res {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "creating the document source failed");
                if let Some(f) = focus {
                    self.notify(DocNotice::NoDocument { focus: f });
                }
                return;
            }
        };
        if !self.apps.contains_key(&app) {
            return;
        }
        let doc = DocId(self.next_doc);
        self.next_doc = self.next_doc.wrapping_add(1);
        let token = self.shutdown.child_token();
        let repair = self.apps.get(&app).map_or(RepairPolicy::NONE, |a| {
            RepairPolicy::for_engine(a.info.web_engine)
        });
        let (handle, fut) = prepare(DocActorSpec {
            doc,
            app,
            root,
            source,
            join: Arc::clone(&self.cfg.join),
            directory: Arc::clone(&self.cfg.docs),
            notices: self.cfg.notices.clone(),
            shutdown: token.clone(),
            counters: Arc::clone(&self.cfg.counters),
            repair,
        });
        self.cfg.docs.insert(handle);
        self.tokens.insert(doc, (app, token));
        // Built while the warm-up was still running: rebuilt once when it completes.
        if let Some(a) = self.apps.get_mut(&app)
            && !a.warm.open()
        {
            a.suspect.push(doc);
        }
        self.tasks.spawn(async move {
            fut.await;
            TaskDone::DocEnded { doc, app }
        });
    }

    fn on_doc_ended(&mut self, doc: DocId, app: AppId, now: Instant) {
        if let Some((_, token)) = self.tokens.remove(&doc) {
            token.cancel();
        }
        self.cfg.docs.remove(doc);
        if let Some(a) = self.apps.get_mut(&app) {
            a.last_doc_end = Some(now);
            a.suspect.retain(|d| *d != doc);
        }
    }

    /// `browse.enabled` for the application, from the published profile snapshot. The product
    /// row (safe mode and suspend included) and the user's profiles decide.
    fn browse_enabled(&self, app_name: &str) -> bool {
        let app = gorca_config::profiles::AppFacts {
            name: app_name,
            exe: None,
            desktop_id: None,
        };
        self.cfg.profiles.load().resolve_app(&app).browse_enabled()
    }

    fn on_msg(&mut self, msg: HubMsg, now: Instant) {
        match msg {
            HubMsg::Appeared { app, info } => {
                // The first-party mode rows (DP-3): an application whose row says no buffer is
                // not hosted as a web application, so no document is mirrored and browse mode
                // never starts there. Everything else is browse mode with automatic focus mode.
                let web = self.cfg.factory.is_web(&info) && self.browse_enabled(&info.app_name);
                let needs = self.cfg.factory.needs_warm(&info);
                let ready = self.apps.get(&app).is_some_and(|a| a.ready);
                let mut st = AppWeb::new(*info, web, needs, now);
                st.ready = ready;
                if web {
                    self.notify(DocNotice::WebApp { app });
                }
                self.apps.insert(app, st);
            }
            HubMsg::Ready { app } => {
                let start = self.apps.get_mut(&app).is_some_and(|a| {
                    a.ready = true;
                    a.web && matches!(a.warm, WarmState::NoDocument { .. })
                });
                if start {
                    self.start_warm(app, None, now);
                }
            }
            HubMsg::Gone { app } => {
                self.apps.remove(&app);
                let dead: Vec<DocId> = self
                    .tokens
                    .iter()
                    .filter(|(_, (a, _))| *a == app)
                    .map(|(d, _)| *d)
                    .collect();
                for d in dead {
                    if let Some((_, t)) = self.tokens.get(&d) {
                        t.cancel();
                    }
                }
            }
            HubMsg::Ensure { focus } => self.ensure(focus, now, false),
            HubMsg::Loaded { node } => {
                if self.apps.get(&node.app).is_some_and(|a| a.web) {
                    self.notify(DocNotice::Loaded { node });
                }
                let adopt = self.apps.get(&node.app).is_some_and(|a| {
                    a.web
                        && a.last_doc_end
                            .is_some_and(|t| now.saturating_duration_since(t) < RECENT_DOC)
                });
                if adopt {
                    self.ensure(node, now, true);
                }
            }
        }
    }

    fn on_done(&mut self, done: TaskDone, now: Instant) {
        match done {
            TaskDone::Warmed { app, res } => self.on_warmed(app, res, now),
            TaskDone::Located { app, focus, res } => self.on_located(app, focus, res, now),
            TaskDone::Created { app, root, res } => self.on_created(app, root, res),
            TaskDone::DocEnded { doc, app } => self.on_doc_ended(doc, app, now),
        }
    }

    /// Timers: warm-up retries and builds that waited long enough.
    fn on_timers(&mut self, now: Instant) {
        self.flush_notices();
        let apps: Vec<AppId> = self.apps.keys().copied().collect();
        for app in apps {
            let Some(a) = self.apps.get_mut(&app) else {
                continue;
            };
            let retry = match a.warm {
                WarmState::NoDocument { retry_at, .. } => a.ready && retry_at <= now,
                WarmState::Failed { attempts, retry_at } => {
                    attempts < MAX_WARM_ATTEMPTS && retry_at <= now
                }
                WarmState::Warming { .. } | WarmState::Warm | WarmState::NotNeeded => false,
            };
            let due = a.waiting.iter().any(|(_, d)| *d <= now);
            if due {
                self.release_waiting(app, Some(now));
            }
            if retry {
                self.start_warm(app, None, now);
            }
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let mut best: Option<Instant> = None;
        let mut take = |t: Instant| best = Some(best.map_or(t, |b| b.min(t)));
        for a in self.apps.values() {
            match a.warm {
                WarmState::NoDocument { retry_at, .. } if a.ready => take(retry_at),
                WarmState::Failed { attempts, retry_at } if attempts < MAX_WARM_ATTEMPTS => {
                    take(retry_at);
                }
                WarmState::NoDocument { .. }
                | WarmState::Failed { .. }
                | WarmState::Warming { .. }
                | WarmState::Warm
                | WarmState::NotNeeded => {}
            }
            for (_, d) in &a.waiting {
                take(*d);
            }
        }
        if !self.pending.is_empty() {
            take(Instant::now() + Duration::from_millis(50));
        }
        best
    }
}

/// Run the registry until `shutdown`.
pub async fn run_hub(cfg: HubCfg, mut rx: mpsc::Receiver<HubMsg>, shutdown: CancellationToken) {
    let mut hub = Hub {
        cfg,
        apps: HashMap::new(),
        tasks: JoinSet::new(),
        tokens: HashMap::new(),
        next_doc: 1,
        shutdown: shutdown.clone(),
        pending: std::collections::VecDeque::new(),
    };
    loop {
        let wake = hub.next_wake();
        let sleep = async {
            match wake {
                Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            m = rx.recv() => {
                let Some(m) = m else { break };
                hub.on_msg(m, Instant::now());
            }
            done = hub.tasks.join_next(), if !hub.tasks.is_empty() => {
                match done {
                    Some(Ok(d)) => hub.on_done(d, Instant::now()),
                    Some(Err(e)) => tracing::warn!(error = %e, "a registry task panicked"),
                    None => {}
                }
            }
            () = sleep => hub.on_timers(Instant::now()),
        }
    }
    hub.tasks.abort_all();
    while hub.tasks.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests;
