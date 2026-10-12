//! Core entry point and structured concurrency root.
//!
//! [`Core::run`] owns every task of the engine (a [`TaskSet`] under one `CancellationToken`) and
//! the Reader lane thread:
//!
//! ```text
//! connector --announce--> [registrar service] --spawns--> [app actor task] (1 per app)
//!                                                              | focus lane (bounded)
//!                                                              v
//!                         [reader lane thread] <-- replies -- [fetch dispatcher] --> connector
//!                                |  SpeechIntent (epoch-tagged)         (deadline on every request)
//!                                v
//!                            SpeechOut (the speech lane)
//! ```
//!
//! Nothing here awaits an application: the Reader never awaits anything but its own inbox, the
//! fetch dispatcher awaits connector replies only under a deadline (`request`), and results come
//! back to the Reader as epoch-tagged messages (R3, E-3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use gorca_connector_api::{
    AppRegistrar, BackendError, BackendOp, BackendReply, BackendRequest, RegistrarRequest,
    ScopeRegistry, registrar_channel, request,
};
use gorca_ingress::{GLOBAL_SLOT_BUDGET, IngressConfig, SlotBudget};
use gorca_mirror::mirror;
use gorca_model::{AppId, ConnectorId, Epoch, NodeKey, PropMask, ReqId, Utterance};
use gorca_rt::{
    BudgetClass, CancellationToken, Clock, Deadline, EpochCell, Lane, TaskExit, TaskSet, mpsc,
    spawn_lane,
};
use tokio::task::JoinSet;

use crate::app_actor::{AppActor, ReaderLinks};
use crate::directory::MirrorDirectory;
use crate::health::{AppHealth, HealthEffect, HealthMsg};
use crate::mode::ModeKeys;
use crate::reader::{QueryReply, Reader, ReaderEffect, ReaderMsg, ReaderSenders, reader_channels};

/// Where utterances go. Implemented by the speech lane (`gorca-speech`), wired by the daemon.
/// Both methods must return immediately (S-3).
pub trait SpeechOut: Send + Sync + 'static {
    /// An epoch-tagged utterance. Never blocks.
    fn submit(&self, utterance: Utterance);
    /// The `EpochCell` advanced: wake the speech lane so it cancels stale speech (E-2).
    fn notify_epoch(&self);
}

/// Discards everything (tests, `--speech=none`).
#[derive(Debug, Default)]
pub struct NullSpeech;

impl SpeechOut for NullSpeech {
    fn submit(&self, _utterance: Utterance) {}
    fn notify_epoch(&self) {}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreConfig {
    /// How long tasks get to finish after shutdown before they are aborted.
    pub shutdown_grace: Duration,
    /// Global ingress slot budget shared by all applications (ADR-0004 `SlotBudget`).
    pub slot_budget: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            shutdown_grace: Duration::from_secs(2),
            slot_budget: GLOBAL_SLOT_BUDGET,
        }
    }
}

/// Errors surfaced by [`Core::run`].
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum CoreError {
    /// A core task panicked; its name is reported (section 6.5: panics are contained and
    /// reported, the process continues until the supervisor decides).
    #[error("core task `{0}` panicked")]
    TaskPanicked(&'static str),
    #[error("the reader lane could not be started: {0}")]
    Lane(String),
}

/// Counters, readable from any thread.
#[derive(Debug, Default)]
pub struct CoreStats {
    pub apps_announced: AtomicU64,
    pub focus_requests: AtomicU64,
    pub fetch_ok: AtomicU64,
    pub fetch_timeout: AtomicU64,
    pub fetch_failed: AtomicU64,
    pub fetch_refused: AtomicU64,
    pub stale_dropped: AtomicU64,
    pub utterances: AtomicU64,
    pub reader_restarts: AtomicU64,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CoreStatsSnapshot {
    pub apps_announced: u64,
    pub focus_requests: u64,
    pub fetch_ok: u64,
    pub fetch_timeout: u64,
    pub fetch_failed: u64,
    pub fetch_refused: u64,
    pub stale_dropped: u64,
    pub utterances: u64,
    pub reader_restarts: u64,
}

impl CoreStats {
    pub fn snapshot(&self) -> CoreStatsSnapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        CoreStatsSnapshot {
            apps_announced: g(&self.apps_announced),
            focus_requests: g(&self.focus_requests),
            fetch_ok: g(&self.fetch_ok),
            fetch_timeout: g(&self.fetch_timeout),
            fetch_failed: g(&self.fetch_failed),
            fetch_refused: g(&self.fetch_refused),
            stale_dropped: g(&self.stale_dropped),
            utterances: g(&self.utterances),
            reader_restarts: g(&self.reader_restarts),
        }
    }
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

/// What the dispatcher needs to talk to one app.
#[derive(Clone, Debug)]
struct AppHandle {
    requests: mpsc::Sender<BackendRequest>,
    health: mpsc::Sender<HealthMsg>,
    /// `AppHealth::admits_requests`, published by the app actor (I-4).
    admits: Arc<AtomicBool>,
}

/// Lock-free app table, copy-on-write like the mirror directory.
#[derive(Debug, Default)]
struct AppDirectory {
    apps: ArcSwap<HashMap<AppId, AppHandle>>,
}

impl AppDirectory {
    fn insert(&self, app: AppId, h: &AppHandle) {
        self.apps.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.insert(app, h.clone());
            next
        });
    }

    fn remove(&self, app: AppId) {
        self.apps.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.remove(&app);
            next
        });
    }

    fn get(&self, app: AppId) -> Option<AppHandle> {
        self.apps.load().get(&app).cloned()
    }
}

/// A request from the Reader lane to the dispatcher.
#[derive(Copy, Clone, Debug)]
struct FetchJob {
    node: NodeKey,
    mask: PropMask,
    class: BudgetClass,
    epoch: Epoch,
}

const FETCH_QUEUE: usize = 64;
const MAX_FETCHES_IN_FLIGHT: usize = 64;
/// Health timer of an app actor; also drives `Hung` probes.
const HEALTH_TICK: Duration = Duration::from_millis(250);
const HEALTH_QUEUE: usize = 32;
const REGISTRAR_QUEUE: usize = 64;

struct Shared {
    epoch: Arc<EpochCell>,
    mirrors: Arc<MirrorDirectory>,
    apps: AppDirectory,
    senders: ReaderSenders,
    speech: Arc<dyn SpeechOut>,
    stats: Arc<CoreStats>,
    clock: Clock,
    next_req: AtomicU64,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

pub struct Core {
    cfg: CoreConfig,
    shared: Arc<Shared>,
    inbox: crate::reader::ReaderInbox,
    registrars: Vec<(ConnectorId, mpsc::Receiver<RegistrarRequest>)>,
    budget: Arc<SlotBudget>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl Core {
    pub fn new(cfg: CoreConfig) -> Self {
        let (senders, inbox) = reader_channels();
        let budget = SlotBudget::new(cfg.slot_budget);
        Self {
            cfg,
            shared: Arc::new(Shared {
                epoch: Arc::new(EpochCell::new()),
                mirrors: Arc::new(MirrorDirectory::new()),
                apps: AppDirectory::default(),
                senders,
                speech: Arc::new(NullSpeech),
                stats: Arc::new(CoreStats::default()),
                clock: Clock::System,
                next_req: AtomicU64::new(1),
            }),
            inbox,
            registrars: Vec::new(),
            budget,
        }
    }

    /// Route utterances to `speech`. Call before [`Core::run`].
    #[must_use]
    pub fn with_speech(mut self, speech: Arc<dyn SpeechOut>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.speech = speech;
        }
        self
    }

    /// The process-wide epoch cell (written by the input lane and the Reader).
    pub fn epoch(&self) -> Arc<EpochCell> {
        Arc::clone(&self.shared.epoch)
    }

    pub fn mirrors(&self) -> Arc<MirrorDirectory> {
        Arc::clone(&self.shared.mirrors)
    }

    pub fn stats(&self) -> Arc<CoreStats> {
        Arc::clone(&self.shared.stats)
    }

    /// The shared ingress budget; every application announced to this core charges it.
    pub fn slot_budget(&self) -> Arc<SlotBudget> {
        Arc::clone(&self.budget)
    }

    /// Producer side of the Reader's inbox, for the input lane.
    pub fn reader_senders(&self) -> ReaderSenders {
        self.shared.senders.clone()
    }

    /// Create the registrar a connector announces its applications through. The core serves it
    /// while [`Core::run`] runs.
    pub fn connector_registrar(&mut self, connector: ConnectorId) -> AppRegistrar {
        let (registrar, rx) = registrar_channel(connector, REGISTRAR_QUEUE);
        self.registrars.push((connector, rx));
        registrar
    }

    /// Runs until `shutdown` is cancelled, then stops every task it owns.
    pub async fn run(self, shutdown: CancellationToken) -> Result<(), CoreError> {
        let Core {
            cfg,
            shared,
            inbox,
            registrars,
            budget,
        } = self;
        let mut tasks = TaskSet::new(shutdown.child_token());
        let (fetch_tx, fetch_rx) = mpsc::channel::<FetchJob>(FETCH_QUEUE);

        tasks.spawn(
            "fetch-dispatcher",
            fetch_dispatcher(Arc::clone(&shared), fetch_rx),
        );
        // One registry for all connectors: C-5 holds across them.
        let table = Arc::new(Mutex::new(ScopeRegistry::new(budget)));
        for (connector, rx) in registrars {
            tasks.spawn(
                "registrar",
                registrar_service(
                    connector,
                    rx,
                    Arc::clone(&table),
                    Arc::clone(&shared),
                    shutdown.child_token(),
                ),
            );
        }

        let lane_token = shutdown.child_token();
        let lane = {
            let (shared, token) = (Arc::clone(&shared), lane_token.clone());
            spawn_lane(Lane::Reader, move || {
                reader_lane(&shared, inbox, &fetch_tx, &token);
            })
            .map_err(|e| CoreError::Lane(e.to_string()))?
        };

        tracing::debug!("core running");
        shutdown.cancelled().await;
        tracing::debug!("core shutting down");
        lane_token.cancel();
        let reports = tasks.shutdown(cfg.shutdown_grace).await;
        // The lane wakes on its token; joining it must not block a worker for long.
        let joined = tokio::task::spawn_blocking(move || lane.join());
        let lane_exit = tokio::time::timeout(cfg.shutdown_grace, joined).await;
        if let Ok(Ok(exit)) = lane_exit
            && exit != gorca_rt::LaneExit::Clean
        {
            return Err(CoreError::TaskPanicked("reader"));
        }
        match reports.iter().find(|r| r.exit == TaskExit::Panicked) {
            Some(r) => Err(CoreError::TaskPanicked(r.name)),
            None => Ok(()),
        }
    }
}

/// Serves `Announce`/`Retire` for one connector and owns the app actors it creates.
async fn registrar_service(
    connector: ConnectorId,
    mut rx: mpsc::Receiver<RegistrarRequest>,
    table: Arc<Mutex<ScopeRegistry>>,
    shared: Arc<Shared>,
    token: CancellationToken,
) {
    let mut actors = TaskSet::new(token.child_token());
    let mut tokens: HashMap<AppId, CancellationToken> = HashMap::new();
    loop {
        tokio::select! {
            biased;
            () = token.cancelled() => break,
            req = rx.recv() => {
                let Some(req) = req else { break };
                match req {
                    RegistrarRequest::Announce { scope, reply, .. } => {
                        let channel = table
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .register(connector, scope, IngressConfig::default());
                        match channel {
                            Ok(ch) => {
                                let app = ch.port.app;
                                let app_token = token.child_token();
                                tokens.insert(app, app_token.clone());
                                spawn_app_actor(&mut actors, &shared, ch.ingress, ch.control,
                                    ch.requests, app, app_token);
                                bump(&shared.stats.apps_announced);
                                let _ = reply.send(Ok(ch.port));
                            }
                            Err(e) => {
                                let _ = reply.send(Err(e));
                            }
                        }
                    }
                    RegistrarRequest::Retire { app } => {
                        table.lock().unwrap_or_else(PoisonError::into_inner).retire(app);
                        if let Some(t) = tokens.remove(&app) {
                            t.cancel();
                        }
                        shared.mirrors.remove(app);
                        shared.apps.remove(app);
                    }
                }
            }
            Some(_) = actors.join_next() => {}
        }
    }
    actors.shutdown(Duration::from_millis(500)).await;
}

fn spawn_app_actor(
    actors: &mut TaskSet,
    shared: &Arc<Shared>,
    ingress: gorca_ingress::IngressReceiver,
    control: mpsc::Receiver<gorca_connector_api::AppControl>,
    requests: mpsc::Sender<BackendRequest>,
    app: AppId,
    token: CancellationToken,
) {
    let (writer, handle) = mirror();
    shared.mirrors.insert(app, &handle);
    let (health_tx, health_rx) = mpsc::channel(HEALTH_QUEUE);
    let admits = Arc::new(AtomicBool::new(true));
    shared.apps.insert(
        app,
        &AppHandle {
            requests: requests.clone(),
            health: health_tx.clone(),
            admits: Arc::clone(&admits),
        },
    );
    let links = ReaderLinks {
        focus: shared.senders.focus.clone(),
        background: shared.senders.background.clone(),
    };
    let actor = AppActor::new(app, writer, ingress, control, links);
    let shared = Arc::clone(shared);
    actors.spawn(
        "app-actor",
        run_app_actor(actor, health_rx, health_tx, requests, admits, shared, token),
    );
}

/// The async shell around [`AppActor::pump`].
async fn run_app_actor(
    mut actor: AppActor,
    mut health_rx: mpsc::Receiver<HealthMsg>,
    health_tx: mpsc::Sender<HealthMsg>,
    requests: mpsc::Sender<BackendRequest>,
    admits: Arc<AtomicBool>,
    shared: Arc<Shared>,
    token: CancellationToken,
) {
    let app = actor.app();
    let mut ticker = tokio::time::interval(HEALTH_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut probes: JoinSet<()> = JoinSet::new();
    loop {
        let mut effects = Vec::new();
        tokio::select! {
            biased;
            () = token.cancelled() => break,
            () = actor.ready() => {}
            Some(msg) = health_rx.recv() => {
                effects.extend(actor.on_health(msg, Instant::now()));
            }
            _ = ticker.tick() => {
                effects.extend(actor.on_health(HealthMsg::Tick, Instant::now()));
            }
            Some(_) = probes.join_next() => {}
        }
        let route = actor.pump(Instant::now());
        admits.store(actor.health().admits_requests(), Ordering::Release);
        for e in effects {
            if matches!(e, HealthEffect::SendProbe) && probes.is_empty() {
                let (requests, health, clock) =
                    (requests.clone(), health_tx.clone(), shared.clock.clone());
                let id = ReqId(shared.next_req.fetch_add(1, Ordering::Relaxed));
                probes.spawn(async move {
                    // I-4: one cheap probe with an Interactive deadline.
                    let deadline = Deadline::for_class(&clock, BudgetClass::Interactive);
                    let r = request(
                        &requests,
                        id,
                        BudgetClass::Interactive,
                        BackendOp::Probe,
                        deadline,
                    )
                    .await;
                    let msg = if r.is_ok() {
                        HealthMsg::ProbeOk
                    } else {
                        HealthMsg::ProbeTimeout
                    };
                    let _ = health.try_send(msg);
                });
            }
        }
        if route.gone || matches!(actor.health(), AppHealth::Gone) {
            break;
        }
    }
    probes.abort_all();
    shared.mirrors.remove(app);
    shared.apps.remove(app);
}

async fn fetch_dispatcher(shared: Arc<Shared>, mut rx: mpsc::Receiver<FetchJob>) {
    let mut set: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            Some(_) = set.join_next() => {}
            job = rx.recv() => {
                let Some(job) = job else { break };
                if set.len() >= MAX_FETCHES_IN_FLIGHT {
                    bump(&shared.stats.fetch_refused);
                    reply(&shared, job, Err(BackendError::Busy));
                } else {
                    set.spawn(do_fetch(Arc::clone(&shared), job));
                }
            }
        }
    }
    set.abort_all();
}

fn reply(
    shared: &Shared,
    job: FetchJob,
    result: Result<Arc<gorca_model::NodeSnapshot>, BackendError>,
) {
    // E-3: an answer to a superseded question is dropped here, never spoken.
    if job.epoch < shared.epoch.current() {
        bump(&shared.stats.stale_dropped);
        return;
    }
    let q = QueryReply {
        node: job.node,
        epoch: job.epoch,
        result,
    };
    if shared.senders.replies.try_send(q).is_err() {
        tracing::warn!(node = ?job.node, "reader reply queue full; dropping an answer");
    }
}

async fn do_fetch(shared: Arc<Shared>, job: FetchJob) {
    let Some(handle) = shared.apps.get(job.node.app) else {
        reply(&shared, job, Err(BackendError::Gone));
        return;
    };
    if !handle.admits.load(Ordering::Acquire) {
        // I-4: a Hung app gets no request but its probe; the Reader falls back to the mirror.
        bump(&shared.stats.fetch_refused);
        reply(&shared, job, Err(BackendError::Busy));
        return;
    }
    let deadline = Deadline::for_class(&shared.clock, job.class);
    let id = ReqId(shared.next_req.fetch_add(1, Ordering::Relaxed));
    let op = BackendOp::Fetch {
        node: job.node,
        want: job.mask,
    };
    let result = request(&handle.requests, id, job.class, op, deadline).await;
    if let Err(e) = &result {
        tracing::debug!(node = ?job.node, error = %e, "fetch failed");
    }
    let health = match &result {
        Ok(_) => {
            bump(&shared.stats.fetch_ok);
            Some(HealthMsg::Success)
        }
        Err(BackendError::Timeout) => {
            bump(&shared.stats.fetch_timeout);
            Some(HealthMsg::Timeout { class: job.class })
        }
        Err(BackendError::Busy) => {
            bump(&shared.stats.fetch_refused);
            None
        }
        Err(BackendError::Gone | BackendError::NotSupported | BackendError::Protocol(_)) => {
            bump(&shared.stats.fetch_failed);
            None
        }
    };
    if let Some(h) = health {
        let _ = handle.health.try_send(h);
    }
    let result = result.and_then(|r| match r {
        BackendReply::Node(n) => Ok(n),
        BackendReply::Children(_)
        | BackendReply::Subtree(_)
        | BackendReply::Text(_)
        | BackendReply::CaretOffset(_)
        | BackendReply::TextAttributes(_)
        | BackendReply::ActionDone
        | BackendReply::Hit(_)
        | BackendReply::Matches(_) => {
            Err(BackendError::Protocol("unexpected reply kind".to_owned()))
        }
    });
    reply(&shared, job, result);
}

/// At most this many Reader restarts inside [`RESTART_WINDOW`] before the lane gives up.
const MAX_READER_RESTARTS: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(10);

/// The Reader lane thread body: its own current-thread runtime, a restart loop (section 6.5:
/// "Reader panic: restarted with state machines reset to NoFocus").
fn reader_lane(
    shared: &Arc<Shared>,
    mut inbox: crate::reader::ReaderInbox,
    fetch_tx: &mpsc::Sender<FetchJob>,
    token: &CancellationToken,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "reader runtime could not be built");
            return;
        }
    };
    let mut restarts: Vec<Instant> = Vec::new();
    loop {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            rt.block_on(reader_loop(shared, &mut inbox, fetch_tx, token));
        }));
        if run.is_ok() {
            return;
        }
        let now = Instant::now();
        restarts.retain(|t| now.saturating_duration_since(*t) < RESTART_WINDOW);
        restarts.push(now);
        bump(&shared.stats.reader_restarts);
        tracing::error!("the reader panicked; restarting it with fresh state");
        if restarts.len() > MAX_READER_RESTARTS || token.is_cancelled() {
            return;
        }
    }
}

async fn reader_loop(
    shared: &Arc<Shared>,
    inbox: &mut crate::reader::ReaderInbox,
    fetch_tx: &mpsc::Sender<FetchJob>,
    token: &CancellationToken,
) {
    let mut reader = Reader::new(
        Arc::clone(&shared.epoch),
        Arc::clone(&shared.mirrors),
        ModeKeys::default(),
    );
    let mut timer: Option<Instant> = None;
    loop {
        let msg = tokio::select! {
            biased;
            () = token.cancelled() => return,
            m = inbox.next() => match m {
                Some(m) => m,
                None => return,
            },
            () = sleep_until(timer), if timer.is_some() => {
                timer = None;
                ReaderMsg::Timer
            }
        };
        if matches!(
            msg,
            ReaderMsg::Focus(crate::reader::FocusLaneMsg::FocusChanged { .. })
        ) {
            bump(&shared.stats.focus_requests);
        }
        let before = shared.epoch.current();
        let effects = reader.handle(msg, Instant::now());
        if shared.epoch.current() != before {
            // E-2: wake the speech lane first, so stale speech stops before anything new starts.
            shared.speech.notify_epoch();
        }
        for e in effects {
            match e {
                ReaderEffect::Speak(intent) => {
                    bump(&shared.stats.utterances);
                    shared.speech.submit(intent.utterance);
                }
                ReaderEffect::Fetch {
                    node,
                    mask,
                    class,
                    epoch,
                } => {
                    let job = FetchJob {
                        node,
                        mask,
                        class,
                        epoch,
                    };
                    if fetch_tx.try_send(job).is_err() {
                        bump(&shared.stats.fetch_refused);
                        let q = QueryReply {
                            node,
                            epoch,
                            result: Err(BackendError::Busy),
                        };
                        let _ = shared.senders.replies.try_send(q);
                    }
                }
                ReaderEffect::ArmTimer(at) => timer = Some(at),
                ReaderEffect::QueryActiveWindowFocus => {
                    tracing::debug!("active-window focus query is not implemented in this slice");
                }
                ReaderEffect::SetGrabSet(_) | ReaderEffect::RunCommand(_) => {
                    tracing::debug!("input effect ignored: no keyboard lane in this slice");
                }
            }
        }
    }
}

async fn sleep_until(at: Option<Instant>) {
    if let Some(at) = at {
        tokio::time::sleep(at.saturating_duration_since(Instant::now())).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_returns_when_shutdown_is_cancelled() {
        let core = Core::new(CoreConfig::default());
        let token = CancellationToken::new();
        let h = tokio::spawn(core.run(token.clone()));
        tokio::task::yield_now().await;
        assert!(!h.is_finished());
        token.cancel();
        assert_eq!(h.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn run_returns_immediately_if_already_cancelled() {
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(Core::new(CoreConfig::default()).run(token).await, Ok(()));
    }

    #[test]
    fn core_exposes_shared_state() {
        let core = Core::new(CoreConfig::default());
        assert_eq!(core.epoch().current(), gorca_model::Epoch(0));
        assert!(core.mirrors().is_empty());
        assert_eq!(core.slot_budget().limit(), GLOBAL_SLOT_BUDGET);
        assert_eq!(core.stats().snapshot(), CoreStatsSnapshot::default());
    }
}
