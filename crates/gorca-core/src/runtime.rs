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
use gorca_config::Settings;
use gorca_connector_api::{
    AppRegistrar, BackendError, BackendOp, BackendReply, BackendRequest, RegistrarRequest,
    ScopeRegistry, registrar_channel, request,
};
use gorca_ingress::{GLOBAL_SLOT_BUDGET, IngressConfig, SlotBudget};
use gorca_input_api::{GrabSet, LaneLinks};
use gorca_mirror::driver::GlobalRate;
use gorca_mirror::fill::{FillOp, FillPolicy, NullPolicy};
use gorca_mirror::limits::FILL_RATE_GLOBAL;
use gorca_mirror::{FillLimits, MirrorBudget, mirror_for};
use gorca_model::{AppId, ConnectorId, Epoch, NodeKey, PropMask, ReqId, Utterance};
use gorca_rt::{
    BudgetClass, CancellationToken, Clock, Deadline, EpochCell, FocusedApp, Lane, TaskExit,
    TaskSet, mpsc, spawn_lane, watch,
};
use tokio::task::JoinSet;

use crate::app_actor::{AppActor, FILL_QUEUE, FillMsg, ReaderLinks, TextLink, WebLink};
use crate::browse::commands::WebEffect;
use crate::commands::{ConfigRequest, SpeechCtl};
use crate::directory::MirrorDirectory;
use crate::doc_actor::{DocCounters, DocCtl, DocDirectory, WebJoin};
use crate::doc_registry::{HubCfg, HubMsg, HubTx, hub_channel, run_hub};
use crate::health::{AppHealth, HealthEffect, HealthMsg};
use crate::mode::ModeKeys;
use crate::present::PresentLog;
use crate::reader::{
    FetchKind, QueryReply, Reader, ReaderEffect, ReaderMsg, ReaderSenders, reader_channels,
};
use gorca_doc_source_api::{Activity, DocSourceFactory};

/// Where utterances go. Implemented by the speech lane (`gorca-speech`), wired by the daemon.
/// Both methods must return immediately (S-3).
pub trait SpeechOut: Send + Sync + 'static {
    /// An epoch-tagged utterance. Never blocks.
    fn submit(&self, utterance: Utterance);
    /// A chunk of a say-all reading: `first` interrupts like any utterance, the others queue
    /// behind it, and the progress of each is reported. Defaults to a plain submit.
    fn submit_say_all(&self, utterance: Utterance, _first: bool) {
        self.submit(utterance);
    }
    /// The `EpochCell` advanced: wake the speech lane so it cancels stale speech (E-2).
    fn notify_epoch(&self);
    /// Speech control, ordered with utterances (ADR-0005 6.2). Never blocks.
    fn control(&self, _ctl: SpeechCtl) {}
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
    /// Reader panics tolerated within [`RESTART_WINDOW`] before the lane gives up and the core
    /// stops with [`CoreError::ReaderLaneDied`] (the process then exits non-zero and the service
    /// manager restarts it). A reader that is silently gone is worse than a restart.
    pub max_reader_restarts: usize,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            shutdown_grace: Duration::from_secs(2),
            slot_budget: GLOBAL_SLOT_BUDGET,
            max_reader_restarts: MAX_READER_RESTARTS,
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
    /// The Reader kept panicking and its lane gave up. Never stay up mute: exit so that the
    /// service manager restarts the process.
    #[error("the reader lane died after repeated panics")]
    ReaderLaneDied,
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
    /// Reader demands and Reader fetch replies for the app actor's fill driver.
    fill: mpsc::Sender<FillMsg>,
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
    kind: FetchKind,
    /// The app's mirror write sequence when the Reader asked (late-reply rules).
    issued_at: u64,
}

const FETCH_QUEUE: usize = 64;
const MAX_FETCHES_IN_FLIGHT: usize = 64;
/// Health timer of an app actor; also drives `Hung` probes.
const HEALTH_TICK: Duration = Duration::from_millis(250);
const HEALTH_QUEUE: usize = 32;
const REGISTRAR_QUEUE: usize = 64;
const CONFIG_QUEUE: usize = 8;

struct Shared {
    epoch: Arc<EpochCell>,
    focused: Arc<FocusedApp>,
    mirrors: Arc<MirrorDirectory>,
    apps: AppDirectory,
    senders: ReaderSenders,
    speech: Arc<dyn SpeechOut>,
    stats: Arc<CoreStats>,
    clock: Clock,
    next_req: Arc<AtomicU64>,
    /// Lane cells shared with the input pump (keymap, settings, grab view, Reader ack).
    links: Arc<LaneLinks>,
    /// Effective settings, written by the Reader.
    settings: Arc<ArcSwap<Settings>>,
    /// The grab set (with its lookup context) the keyboard source should apply; latest wins.
    grabs: Arc<watch::Sender<GrabSet>>,
    config: mpsc::Sender<ConfigRequest>,
    /// Builds the fill policy of each new app (R0 unless the daemon chooses another).
    policy: Arc<dyn Fn() -> Box<dyn FillPolicy> + Send + Sync>,
    limits: FillLimits,
    mirror_budget: Arc<MirrorBudget>,
    global_rate: Arc<GlobalRate>,
    present_log: Option<Arc<PresentLog>>,
    web: WebShared,
    /// Text events the app actors could not queue for the Reader (ADR-0007 EV-5).
    text_dropped: Arc<AtomicU64>,
}

/// The document side of the core: the focus join, the directory of documents, the registry's
/// inbox and the factory that creates sources (browse mode).
struct WebShared {
    join: Arc<WebJoin>,
    docs: Arc<DocDirectory>,
    hub: HubTx,
    factory: Option<Arc<dyn DocSourceFactory>>,
    counters: Arc<DocCounters>,
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
    config_rx: Option<mpsc::Receiver<ConfigRequest>>,
    hub_rx: Option<mpsc::Receiver<HubMsg>>,
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
        let (grabs, _) = watch::channel(GrabSet::default());
        let (config, config_rx) = mpsc::channel(CONFIG_QUEUE);
        let (hub, hub_rx) = hub_channel();
        Self {
            cfg,
            shared: Arc::new(Shared {
                epoch: Arc::new(EpochCell::new()),
                focused: Arc::new(FocusedApp::new()),
                mirrors: Arc::new(MirrorDirectory::new()),
                apps: AppDirectory::default(),
                senders,
                speech: Arc::new(NullSpeech),
                stats: Arc::new(CoreStats::default()),
                clock: Clock::System,
                next_req: Arc::new(AtomicU64::new(1)),
                links: LaneLinks::new(),
                settings: Arc::new(ArcSwap::from_pointee(Settings::default())),
                grabs: Arc::new(grabs),
                config,
                policy: Arc::new(|| Box::new(NullPolicy)),
                limits: FillLimits::default(),
                mirror_budget: MirrorBudget::global(),
                global_rate: GlobalRate::new(FILL_RATE_GLOBAL),
                present_log: None,
                web: WebShared {
                    join: Arc::new(WebJoin::new()),
                    docs: Arc::new(DocDirectory::new()),
                    hub,
                    factory: None,
                    counters: Arc::new(DocCounters::default()),
                },
                text_dropped: Arc::new(AtomicU64::new(0)),
            }),
            inbox,
            registrars: Vec::new(),
            budget,
            config_rx: Some(config_rx),
            hub_rx: Some(hub_rx),
        }
    }

    /// Turn browse mode on: web documents are mirrored through sources made by `factory`
    /// (ADR-0003 AM-10). Call before [`Core::run`].
    #[must_use]
    pub fn with_web(mut self, factory: Arc<dyn DocSourceFactory>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.web.factory = Some(factory);
        }
        self
    }

    /// The directory of live web documents (tests, the debug probe).
    pub fn docs(&self) -> Arc<DocDirectory> {
        Arc::clone(&self.shared.web.docs)
    }

    /// The focus join (tests, the debug probe).
    pub fn web_join(&self) -> Arc<WebJoin> {
        Arc::clone(&self.shared.web.join)
    }

    /// Document actor counters.
    pub fn doc_counters(&self) -> Arc<DocCounters> {
        Arc::clone(&self.shared.web.counters)
    }

    /// Route utterances to `speech`. Call before [`Core::run`].
    #[must_use]
    pub fn with_speech(mut self, speech: Arc<dyn SpeechOut>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.speech = speech;
        }
        self
    }

    /// Choose the fill policy of every app announced from now on (default: R0, `NullPolicy`).
    #[must_use]
    pub fn with_fill_policy(
        mut self,
        policy: Arc<dyn Fn() -> Box<dyn FillPolicy> + Send + Sync>,
    ) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.policy = policy;
        }
        self
    }

    /// Record every node presentation in `log` (B-1 PW-10).
    #[must_use]
    pub fn with_present_log(mut self, log: Arc<PresentLog>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.present_log = Some(log);
        }
        self
    }

    /// The process-wide epoch cell (written by the input lane and the Reader).
    pub fn epoch(&self) -> Arc<EpochCell> {
        Arc::clone(&self.shared.epoch)
    }

    /// The focused application, published by the Reader for the connector's eviction policy.
    pub fn focused_app(&self) -> Arc<FocusedApp> {
        Arc::clone(&self.shared.focused)
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

    /// The cells the input lane reads and the Reader acknowledges through.
    pub fn lane_links(&self) -> Arc<LaneLinks> {
        Arc::clone(&self.shared.links)
    }

    /// The effective settings (file plus runtime changes); the Reader is the only writer.
    pub fn settings_cell(&self) -> Arc<ArcSwap<Settings>> {
        Arc::clone(&self.shared.settings)
    }

    /// The grab set the keyboard source should apply, with its lookup context. Latest wins.
    pub fn grab_requests(&self) -> watch::Receiver<GrabSet> {
        self.shared.grabs.subscribe()
    }

    /// Write access to the grab set, for the input lane's own safety nets (LM-3): when the
    /// Reader is stalled, the lane must be able to hand the keyboard back by itself.
    pub fn grab_sender(&self) -> Arc<watch::Sender<GrabSet>> {
        Arc::clone(&self.shared.grabs)
    }

    /// Requests for the config task (`settings.reload`, `settings.save`). Taken once.
    pub fn take_config_requests(&mut self) -> Option<mpsc::Receiver<ConfigRequest>> {
        self.config_rx.take()
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
            config_rx: _,
            hub_rx,
        } = self;
        let mut tasks = TaskSet::new(shutdown.child_token());
        let (fetch_tx, fetch_rx) = mpsc::channel::<FetchJob>(FETCH_QUEUE);

        tasks.spawn(
            "fetch-dispatcher",
            fetch_dispatcher(Arc::clone(&shared), fetch_rx),
        );
        if let (Some(factory), Some(rx)) = (shared.web.factory.clone(), hub_rx) {
            let cfg = HubCfg {
                factory,
                build_hold: crate::doc_registry::BUILD_HOLD,
                retry_unit: Duration::from_secs(1),
                join: Arc::clone(&shared.web.join),
                docs: Arc::clone(&shared.web.docs),
                notices: shared.senders.docs.clone(),
                counters: Arc::clone(&shared.web.counters),
            };
            tasks.spawn("doc-registry", run_hub(cfg, rx, tasks.token()));
        }
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
        let lane_dead = CancellationToken::new();
        let lane = {
            let (shared, token, dead) =
                (Arc::clone(&shared), lane_token.clone(), lane_dead.clone());
            let max_restarts = cfg.max_reader_restarts;
            spawn_lane(Lane::Reader, move || {
                reader_lane(&shared, inbox, &fetch_tx, &token, &dead, max_restarts);
            })
            .map_err(|e| CoreError::Lane(e.to_string()))?
        };

        tracing::debug!("core running");
        let lane_died = tokio::select! {
            () = shutdown.cancelled() => false,
            () = lane_dead.cancelled() => {
                tracing::error!("the reader lane is dead; stopping so that it is restarted");
                shutdown.cancel();
                true
            }
        };
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
        if lane_died {
            return Err(CoreError::ReaderLaneDied);
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
    let (writer, handle) = mirror_for(app, Some(Arc::clone(&shared.mirror_budget)));
    shared.mirrors.insert(app, &handle);
    let (health_tx, health_rx) = mpsc::channel(HEALTH_QUEUE);
    let (fill_tx, fill_rx) = mpsc::channel(FILL_QUEUE);
    let admits = Arc::new(AtomicBool::new(true));
    shared.apps.insert(
        app,
        &AppHandle {
            requests: requests.clone(),
            health: health_tx.clone(),
            admits: Arc::clone(&admits),
            fill: fill_tx,
        },
    );
    let links = ReaderLinks {
        focus: shared.senders.focus.clone(),
        background: shared.senders.background.clone(),
    };
    let actor = AppActor::new(app, writer, ingress, control, links)
        .with_policy(
            (shared.policy)(),
            shared.limits.clone(),
            Some(Arc::clone(&shared.global_rate)),
        )
        .with_fill_channel(fill_rx)
        .with_focused(Arc::clone(&shared.focused))
        .with_directory(Arc::clone(&shared.mirrors))
        .with_text(TextLink {
            tx: shared.senders.text.clone(),
            dropped: Arc::clone(&shared.text_dropped),
        });
    let actor = if shared.web.factory.is_some() {
        actor.with_web(WebLink {
            hub: shared.web.hub.clone(),
            join: Arc::clone(&shared.web.join),
            docs: Arc::clone(&shared.web.docs),
        })
    } else {
        actor
    };
    let shared = Arc::clone(shared);
    actors.spawn(
        "app-actor",
        run_app_actor(actor, health_rx, health_tx, requests, admits, shared, token),
    );
}

/// The async shell around [`AppActor::pump`]: waits for work, pumps, sends the requests the fill
/// driver issued under their deadlines (each in a task owned by `fills`, cancelled with the
/// actor) and feeds their replies back.
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
    let mut fills: JoinSet<FillDone> = JoinSet::new();
    loop {
        let mut effects = Vec::new();
        let wake = actor.next_wake();
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
            Some(done) = fills.join_next() => {
                if let Ok(d) = done {
                    effects.extend(actor.on_fill_reply(d.id, d.issued_at, d.result, Instant::now()));
                }
            }
            () = sleep_until(wake), if wake.is_some() => {
                actor.poll_timers(Instant::now());
            }
        }
        let route = actor.pump(Instant::now());
        admits.store(actor.health().admits_requests(), Ordering::Release);
        let mut issued = route.issued;
        issued.extend(actor.take_issued());
        for f in issued {
            if fills.len() >= FILL_TASKS_MAX {
                // The driver bounds this far below the cap; a refused task is a lost reply.
                tracing::warn!(?app, "fill task cap reached; dropping a request");
                continue;
            }
            let requests = requests.clone();
            let id = ReqId(shared.next_req.fetch_add(1, Ordering::Relaxed));
            fills.spawn(async move {
                let op = backend_op(&f.op);
                let result = request(&requests, id, f.class, op, Deadline(f.deadline)).await;
                FillDone {
                    id: f.id,
                    issued_at: f.issued_at,
                    result,
                }
            });
        }
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
    fills.abort_all();
    shared.mirrors.remove(app);
    shared.apps.remove(app);
}

/// Most fill requests an app actor awaits at once (the driver admits at most 8).
const FILL_TASKS_MAX: usize = 64;

struct FillDone {
    id: gorca_mirror::fill::FillReqId,
    issued_at: u64,
    result: Result<BackendReply, BackendError>,
}

fn backend_op(op: &FillOp) -> BackendOp {
    match op {
        FillOp::Fetch { node, want } => BackendOp::Fetch {
            node: *node,
            want: *want,
        },
        FillOp::ChildSnapshots { node, range, want } => BackendOp::ChildSnapshots {
            node: *node,
            range: range.clone(),
            want: *want,
        },
        FillOp::Ancestry {
            node,
            max_depth,
            want,
        } => BackendOp::Ancestry {
            node: *node,
            max_depth: *max_depth,
            want: *want,
        },
        FillOp::Subtree { root, max_nodes } => BackendOp::Subtree {
            root: *root,
            max_nodes: *max_nodes,
        },
        FillOp::CacheDump { max_nodes } => BackendOp::CacheDump {
            max_nodes: *max_nodes,
        },
    }
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
        kind: job.kind,
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
        Err(
            BackendError::Gone
            | BackendError::NotSupported
            | BackendError::Protocol(_)
            | BackendError::Unresolved,
        ) => {
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
        | BackendReply::Matches(_)
        | BackendReply::Lines(_)
        | BackendReply::Unit(_)
        | BackendReply::Selections(_)
        | BackendReply::Window(_) => {
            Err(BackendError::Protocol("unexpected reply kind".to_owned()))
        }
    });
    // The app actor owns the mirror: it applies the reply (late-reply rules, tombstone on Gone).
    // The Reader gets the answer first; this is off the latency path.
    let _ = handle.fill.try_send(FillMsg::ReaderFetch {
        node: job.node,
        issued_at: job.issued_at,
        result: result.clone(),
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
    dead: &CancellationToken,
    max_restarts: usize,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "reader runtime could not be built");
            dead.cancel();
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
        if token.is_cancelled() {
            return;
        }
        if restarts.len() > max_restarts {
            dead.cancel();
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
    )
    .with_focused(Arc::clone(&shared.focused))
    .with_links(Arc::clone(&shared.links))
    .with_settings_cell(Arc::clone(&shared.settings))
    .with_web(Arc::clone(&shared.web.docs), Arc::clone(&shared.web.join))
    .with_text_dropped(Arc::clone(&shared.text_dropped));
    if let Some(log) = &shared.present_log {
        reader = reader.with_present_log(Arc::clone(log));
    }
    // N1: every armed deadline stays armed until it fires (a set, not one slot).
    let mut timers: std::collections::BTreeSet<Instant> = std::collections::BTreeSet::new();
    // Review fetches, builds and actions: each in a task this loop owns (aborted with it).
    let mut review_tasks: JoinSet<()> = JoinSet::new();
    // Text fetches, likewise: bounded, owned here, aborted with the loop (ADR-0007 TW-5).
    let mut text_tasks: JoinSet<()> = JoinSet::new();
    loop {
        let msg = tokio::select! {
            biased;
            () = token.cancelled() => {
                tracing::info!(text = ?reader.text_counters(), "reader text counters");
                return;
            }
            Some(_) = review_tasks.join_next(), if !review_tasks.is_empty() => continue,
            Some(_) = text_tasks.join_next(), if !text_tasks.is_empty() => continue,
            m = inbox.next() => match m {
                Some(m) => m,
                None => return,
            },
            () = sleep_until(timers.first().copied()), if !timers.is_empty() => {
                let now = Instant::now();
                while timers.first().is_some_and(|t| *t <= now) {
                    timers.pop_first();
                }
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
                    kind,
                } => {
                    let job = FetchJob {
                        node,
                        mask,
                        class,
                        epoch,
                        kind,
                        issued_at: shared.mirrors.bump_seq(node.app),
                    };
                    if fetch_tx.try_send(job).is_err() {
                        bump(&shared.stats.fetch_refused);
                        let q = QueryReply {
                            node,
                            epoch,
                            kind,
                            result: Err(BackendError::Busy),
                        };
                        let _ = shared.senders.replies.try_send(q);
                    }
                }
                ReaderEffect::Demand { node, why, outcome } => {
                    if let Some(h) = shared.apps.get(node.app) {
                        let _ = h.fill.try_send(FillMsg::Demand { node, why, outcome });
                    }
                }
                ReaderEffect::FocusSettled { node } => {
                    if let Some(h) = shared.apps.get(node.app) {
                        let _ = h.fill.try_send(FillMsg::FocusSettled { node });
                    }
                }
                ReaderEffect::ArmTimer(at) => {
                    // Deadlines within a millisecond share one wake; the set is bounded.
                    if timers.len() < MAX_TIMERS {
                        timers.insert(at);
                    }
                }
                ReaderEffect::QueryActiveWindowFocus => {
                    tracing::debug!("active-window focus query is not implemented in this slice");
                }
                ReaderEffect::SetGrabSet(set) => {
                    shared.grabs.send_replace(set);
                }
                ReaderEffect::SpeechCtl(ctl) => shared.speech.control(ctl),
                ReaderEffect::Config(req) => {
                    if shared.config.try_send(req).is_err() {
                        tracing::warn!("config request queue full; dropped a request");
                    }
                }
                ReaderEffect::Web(w) => exec_web(shared, w),
                ReaderEffect::SayAllChunk { intent, first } => {
                    bump(&shared.stats.utterances);
                    shared.speech.submit_say_all(intent.utterance, first);
                }
                ReaderEffect::TextFetch {
                    req,
                    op,
                    class,
                    epoch,
                } => spawn_text(&mut text_tasks, shared, req, op, class, epoch),
                e @ (ReaderEffect::ReviewNav { .. }
                | ReaderEffect::ReviewBuild(_)
                | ReaderEffect::ReviewRevalidate { .. }
                | ReaderEffect::ReviewStatusBar { .. }
                | ReaderEffect::ReviewAct { .. }) => spawn_review(&mut review_tasks, shared, e),
            }
        }
    }
}

/// Browse-mode effects of the Reader: fire and forget, never awaited (BI-1).
fn exec_web(shared: &Shared, w: WebEffect) {
    match w {
        WebEffect::Ensure { focus } => {
            let _ = shared.web.hub.try_send(HubMsg::Ensure { focus });
        }
        WebEffect::Activity { doc, awake } => {
            if let Some(h) = shared.web.docs.get(doc) {
                let a = if awake {
                    Activity::Awake
                } else {
                    Activity::Dormant
                };
                if h.ctl.try_send(DocCtl::Activity(a)).is_err() {
                    bump(&shared.web.counters.controls_dropped);
                }
            }
        }
        WebEffect::Act { doc, node, act } => {
            if let Some(h) = shared.web.docs.get(doc)
                && h.ctl.try_send(DocCtl::Act { node, act }).is_err()
            {
                bump(&shared.web.counters.controls_dropped);
            }
        }
    }
}

/// Most review tasks in flight at once (a held review key queues behind the Reader's own
/// pending-operation rule long before this).
const REVIEW_TASKS_MAX: usize = 32;
/// Soft deadlines of a model build (ADR-0006 6.8): the key press waits for the first, the
/// continuation runs in the background.
const BUILD_FIRST: Duration = Duration::from_millis(120);
const BUILD_MORE: Duration = Duration::from_millis(300);
/// Background passes after the first one while a build keeps finding more.
const BUILD_CONTINUATIONS: usize = 8;

/// The app side of one review effect (ADR-0006 9): a task bounded by request deadlines whose
/// outcome goes to the Reader as a `ReviewMsg`. An app that is gone, hung or refusing answers
/// at once so the Reader degrades instead of waiting (I-4).
fn spawn_review(set: &mut JoinSet<()>, shared: &Arc<Shared>, e: ReaderEffect) {
    use crate::review::build::{self, BuildSpec};
    use crate::review::model::TruncWhy;
    use crate::review::{BuildError, Completeness, NavFetchError, ReviewIo, ReviewMsg, RrVerdict};
    use gorca_mirror::MirrorRead as _;
    let app = match &e {
        ReaderEffect::ReviewNav { op, .. } => op.node().app,
        ReaderEffect::ReviewBuild(b) => b.surface.app,
        ReaderEffect::ReviewRevalidate { app, .. } => *app,
        ReaderEffect::ReviewStatusBar { root, .. } => root.app,
        ReaderEffect::ReviewAct { act, .. } => action_node(act).app,
        ReaderEffect::Speak(_)
        | ReaderEffect::Fetch { .. }
        | ReaderEffect::Demand { .. }
        | ReaderEffect::FocusSettled { .. }
        | ReaderEffect::QueryActiveWindowFocus
        | ReaderEffect::ArmTimer(_)
        | ReaderEffect::SetGrabSet(_)
        | ReaderEffect::SpeechCtl(_)
        | ReaderEffect::TextFetch { .. }
        | ReaderEffect::SayAllChunk { .. }
        | ReaderEffect::Config(_)
        | ReaderEffect::Web(_) => return,
    };
    let tx = shared.senders.review.clone();
    let io = match (shared.apps.get(app), shared.mirrors.handle(app)) {
        (Some(h), Some(mirror))
            if h.admits.load(Ordering::Acquire) && set.len() < REVIEW_TASKS_MAX =>
        {
            Some(ReviewIo {
                requests: h.requests,
                fill: h.fill,
                health: Some(h.health),
                mirror,
                clock: shared.clock.clone(),
                next_req: Arc::clone(&shared.next_req),
                limits: crate::review::ReviewLimits::default(),
            })
        }
        _ => None,
    };
    let send = move |m: ReviewMsg| {
        if tx.try_send(m).is_err() {
            tracing::warn!("reader review queue full; dropped a review result");
        }
    };
    match e {
        ReaderEffect::ReviewNav { req, op } => {
            set.spawn(async move {
                let result = match io {
                    Some(io) => build::nav_fetch(&io, op).await,
                    None => Err(NavFetchError::Refused),
                };
                send(ReviewMsg::NavFetched { req, result });
            });
        }
        ReaderEffect::ReviewBuild(b) => {
            set.spawn(async move {
                let Some(io) = io else {
                    send(ReviewMsg::Built {
                        build: b.build,
                        result: Err(BuildError::Refused),
                    });
                    return;
                };
                let first = build::build_model(
                    &io,
                    BuildSpec {
                        surface: b.surface,
                        dirty: b.dirty.clone(),
                        prev: b.prev.clone(),
                        budget: if b.interactive {
                            BUILD_FIRST
                        } else {
                            BUILD_MORE
                        },
                        class: if b.interactive {
                            BudgetClass::Interactive
                        } else {
                            BudgetClass::Prefetch
                        },
                    },
                )
                .await;
                // A surface that the first budget did not finish keeps being built in the
                // background, each pass picking up where the last stopped (deep trees, F-7).
                let mut carry = first.as_ref().ok().cloned();
                let mut again = carry
                    .as_ref()
                    .is_some_and(|m| m.completeness == Completeness::Truncated(TruncWhy::Deadline));
                send(ReviewMsg::Built {
                    build: b.build,
                    result: first,
                });
                let mut passes = 0;
                while again && b.interactive && passes < BUILD_CONTINUATIONS {
                    passes += 1;
                    let before = io.mirror.stats().live;
                    let next = build::build_model(
                        &io,
                        BuildSpec {
                            surface: b.surface,
                            dirty: Some(Arc::from([])),
                            prev: carry.clone(),
                            budget: BUILD_MORE,
                            class: BudgetClass::Prefetch,
                        },
                    )
                    .await;
                    carry = next.as_ref().ok().cloned();
                    again = carry.as_ref().is_some_and(|m| {
                        m.completeness == Completeness::Truncated(TruncWhy::Deadline)
                            && io.mirror.stats().live > before
                    });
                    send(ReviewMsg::Built {
                        build: b.build,
                        result: next,
                    });
                }
            });
        }
        ReaderEffect::ReviewRevalidate {
            model, probe, clip, ..
        } => {
            set.spawn(async move {
                let verdict = match io {
                    Some(io) => build::revalidate(&io, probe, clip).await,
                    None => RrVerdict::Failed,
                };
                send(ReviewMsg::Revalidated { model, verdict });
            });
        }
        ReaderEffect::ReviewStatusBar { req, root } => {
            set.spawn(async move {
                let result = match io {
                    Some(io) => build::find_status_bar(&io, root).await,
                    None => Err(NavFetchError::Refused),
                };
                send(ReviewMsg::StatusBarFound { req, result });
            });
        }
        ReaderEffect::ReviewAct { req, act } => {
            set.spawn(async move {
                let result = match io {
                    Some(io) => build::act(&io, act).await,
                    None => Err(NavFetchError::Refused),
                };
                send(ReviewMsg::ActionDone { req, result });
            });
        }
        ReaderEffect::Speak(_)
        | ReaderEffect::Fetch { .. }
        | ReaderEffect::Demand { .. }
        | ReaderEffect::FocusSettled { .. }
        | ReaderEffect::QueryActiveWindowFocus
        | ReaderEffect::ArmTimer(_)
        | ReaderEffect::SetGrabSet(_)
        | ReaderEffect::SpeechCtl(_)
        | ReaderEffect::TextFetch { .. }
        | ReaderEffect::SayAllChunk { .. }
        | ReaderEffect::Config(_)
        | ReaderEffect::Web(_) => {}
    }
}

/// Most text fetches in flight at once: a Reader that asks more than this is being driven
/// faster than any application answers.
const TEXT_TASKS_MAX: usize = 16;

/// The app side of one text fetch (ADR-0007 TW-5): a task bounded by the request deadline of
/// `class` whose outcome goes to the Reader as a `TextMsg::Fetched`. An app that is gone, hung
/// or refusing answers at once, so the Reader degrades instead of waiting (I-4).
fn spawn_text(
    set: &mut JoinSet<()>,
    shared: &Arc<Shared>,
    req: crate::text::TextReqId,
    op: crate::text::TextOp,
    class: BudgetClass,
    epoch: Epoch,
) {
    use crate::text::caret::Reply;
    use crate::text::{TextMsg, TextOp};
    use gorca_connector_api::ActionReq;
    use gorca_model::TextQuery;
    let tx = shared.senders.text_io.clone();
    let send = move |result: Result<Reply, BackendError>| {
        if tx
            .try_send(TextMsg::Fetched { req, epoch, result })
            .is_err()
        {
            tracing::warn!("reader text queue full; dropped a fetch result");
        }
    };
    let handle = shared.apps.get(op.node().app);
    let (Some(h), true) = (handle, set.len() < TEXT_TASKS_MAX) else {
        send(Err(BackendError::Busy));
        return;
    };
    if !h.admits.load(Ordering::Acquire) {
        bump(&shared.stats.fetch_refused);
        send(Err(BackendError::Busy));
        return;
    }
    let clock = shared.clock.clone();
    let next_req = Arc::clone(&shared.next_req);
    let backend = match op {
        TextOp::Window {
            node,
            around,
            radius,
        } => BackendOp::Text(TextQuery::Window {
            node,
            around,
            radius,
        }),
        TextOp::AtOffset { node, offset, unit } => {
            BackendOp::Text(TextQuery::AtOffset { node, offset, unit })
        }
        TextOp::Selections { node } => BackendOp::Text(TextQuery::Selections { node }),
        TextOp::SetCaret { node, offset } => {
            BackendOp::Action(ActionReq::SetCaret { node, offset })
        }
    };
    set.spawn(async move {
        let deadline = Deadline::for_class(&clock, class);
        let id = ReqId(next_req.fetch_add(1, Ordering::Relaxed));
        let r = request(&h.requests, id, class, backend, deadline).await;
        match &r {
            Ok(_) => {
                let _ = h.health.try_send(HealthMsg::Success);
            }
            Err(BackendError::Timeout) => {
                let _ = h.health.try_send(HealthMsg::Timeout { class });
            }
            Err(
                BackendError::Gone
                | BackendError::NotSupported
                | BackendError::Busy
                | BackendError::Protocol(_)
                | BackendError::Unresolved,
            ) => {}
        }
        send(r.and_then(|reply| match reply {
            BackendReply::Window(w) => Ok(Reply::Window(w)),
            BackendReply::Unit(u) => Ok(Reply::Unit(u)),
            BackendReply::Selections(s) => Ok(Reply::Selections(s)),
            BackendReply::ActionDone => Ok(Reply::Done),
            BackendReply::Node(_)
            | BackendReply::Children(_)
            | BackendReply::Subtree(_)
            | BackendReply::Text(_)
            | BackendReply::CaretOffset(_)
            | BackendReply::TextAttributes(_)
            | BackendReply::Hit(_)
            | BackendReply::Matches(_)
            | BackendReply::Lines(_) => {
                Err(BackendError::Protocol("unexpected reply kind".to_owned()))
            }
        }));
    });
}

fn action_node(a: &gorca_connector_api::ActionReq) -> NodeKey {
    use gorca_connector_api::ActionReq;
    match a {
        ActionReq::DoDefault { node }
        | ActionReq::Focus { node }
        | ActionReq::SetCaret { node, .. }
        | ActionReq::Select { node }
        | ActionReq::ScrollTo { node }
        | ActionReq::SetValue { node, .. } => *node,
    }
}

/// Most deadlines the Reader loop keeps armed at once.
const MAX_TIMERS: usize = 256;

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
