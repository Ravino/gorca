// SPDX-License-Identifier: GPL-3.0-or-later
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
use crate::commands::{ConfigRequest, SourceDirectory, SpeechCtl};
use crate::directory::MirrorDirectory;
use crate::doc_actor::{DocCounters, DocCtl, DocDirectory, DocNotice, SelOutcome, WebJoin};
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
    /// M-6b fallback: the cold focus fetch leaves `ATTRS` out (`GORCA_LAZY_ATTRS=1`).
    pub lazy_attrs: bool,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            shutdown_grace: Duration::from_secs(2),
            slot_budget: GLOBAL_SLOT_BUDGET,
            max_reader_restarts: MAX_READER_RESTARTS,
            lazy_attrs: false,
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
    /// Cues handed to the cue sink.
    pub cues: AtomicU64,
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
    pub cues: u64,
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
            cues: g(&self.cues),
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
const CONFIG_QUEUE: usize = 16;
/// Prewarm requests leave this many slots free for the requests that matter (a save is never
/// dropped for a prewarm, R-5).
const CONFIG_RESERVED: usize = 8;

struct Shared {
    epoch: Arc<EpochCell>,
    focused: Arc<FocusedApp>,
    mirrors: Arc<MirrorDirectory>,
    apps: AppDirectory,
    senders: ReaderSenders,
    speech: Arc<dyn SpeechOut>,
    /// Where non-speech cues go (the silent placeholder until sounds exist).
    cues: Arc<dyn gorca_output_api::CueSink>,
    stats: Arc<CoreStats>,
    clock: Clock,
    next_req: Arc<AtomicU64>,
    /// Lane cells shared with the input pump (keymap, settings, grab view, Reader ack).
    links: Arc<LaneLinks>,
    /// Effective settings, written by the Reader.
    settings: Arc<ArcSwap<Settings>>,
    /// The published profile snapshot (ADR-0016 SS-1): the Reader writes it, the document
    /// registry and the other resolvers read it.
    profiles: Arc<gorca_config::profiles::ProfileCell>,
    /// The grab set (with its lookup context) the keyboard source should apply; latest wins.
    grabs: Arc<watch::Sender<GrabSet>>,
    config: mpsc::Sender<ConfigRequest>,
    /// Builds the fill policy of each new app (R0 unless the daemon chooses another).
    policy: Arc<dyn Fn() -> Box<dyn FillPolicy> + Send + Sync>,
    limits: FillLimits,
    mirror_budget: Arc<MirrorBudget>,
    global_rate: Arc<GlobalRate>,
    present_log: Option<Arc<PresentLog>>,
    sources: Arc<SourceDirectory>,
    web: WebShared,
    /// Text events the app actors could not queue for the Reader (ADR-0007 EV-5).
    text_dropped: Arc<AtomicU64>,
    lazy_attrs: bool,
    /// The extension host (ADR-0001 section 10), when the daemon wires one.
    ext: Option<Arc<dyn crate::ExtSource>>,
    /// The application policy the Reader consults (ADR-0008 W3), if the daemon installed one.
    app_policy: Option<Arc<dyn crate::builtin_ext::AppPolicy>>,
    /// The native terminal policy (ADR-0008 amendment 1), if the daemon installed one.
    term_provider: Option<Arc<dyn crate::TermProvider>>,
    /// The pointer output (ADR-0025): the GNOME Shell helper client, when the daemon wires one.
    pointer: Option<Arc<dyn gorca_output_api::PointerSink>>,
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
        let lazy_attrs = cfg.lazy_attrs;
        Self {
            cfg,
            shared: Arc::new(Shared {
                epoch: Arc::new(EpochCell::new()),
                focused: Arc::new(FocusedApp::new()),
                mirrors: Arc::new(MirrorDirectory::new()),
                apps: AppDirectory::default(),
                senders,
                speech: Arc::new(NullSpeech),
                cues: Arc::new(gorca_output_api::NullCueSink::default()),
                stats: Arc::new(CoreStats::default()),
                clock: Clock::System,
                next_req: Arc::new(AtomicU64::new(1)),
                links: LaneLinks::new(),
                settings: Arc::new(ArcSwap::from_pointee(Settings::default())),
                profiles: Arc::new(gorca_config::profiles::ProfileCell::new(
                    gorca_config::profiles::initial_snapshot(),
                )),
                grabs: Arc::new(grabs),
                config,
                policy: Arc::new(|| Box::new(NullPolicy)),
                limits: FillLimits::default(),
                mirror_budget: MirrorBudget::global(),
                global_rate: GlobalRate::new(FILL_RATE_GLOBAL),
                present_log: None,
                sources: Arc::new(SourceDirectory::new()),
                web: WebShared {
                    join: Arc::new(WebJoin::new()),
                    docs: Arc::new(DocDirectory::new()),
                    hub,
                    factory: None,
                    counters: Arc::new(DocCounters::default()),
                },
                text_dropped: Arc::new(AtomicU64::new(0)),
                lazy_attrs,
                ext: None,
                app_policy: None,
                term_provider: None,
                pointer: None,
            }),
            inbox,
            registrars: Vec::new(),
            budget,
            config_rx: Some(config_rx),
            hub_rx: Some(hub_rx),
        }
    }

    /// Install the presentation policy of the applications whose profile asks for one (the
    /// native `ext-gnome-shell` today). Call before [`Core::run`].
    #[must_use]
    pub fn with_app_policy(mut self, policy: Arc<dyn crate::builtin_ext::AppPolicy>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.app_policy = Some(policy);
        }
        self
    }

    /// Install the native terminal policy (the `ext-vte` build the daemon links in when its
    /// component is not loaded). An extension host's `terminal` interface takes precedence.
    /// Call before [`Core::run`].
    #[must_use]
    pub fn with_term_provider(mut self, provider: Arc<dyn crate::TermProvider>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.term_provider = Some(provider);
        }
        self
    }

    /// Wire the pointer output (ADR-0025 MO-4): its availability gates the keypad mouse chords, the
    /// `mouse.*` commands run on it. Call before [`Core::run`].
    #[must_use]
    pub fn with_pointer(mut self, sink: Arc<dyn gorca_output_api::PointerSink>) -> Self {
        let Some(sh) = Arc::get_mut(&mut self.shared) else {
            // A silent no-op would leave the mouse keys without a sink.
            tracing::error!("Core::with_pointer called after the shared state was cloned: ignored");
            debug_assert!(
                false,
                "Core::with_pointer must run before anything clones the shared state"
            );
            return self;
        };
        sh.pointer = Some(sink);
        self
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

    /// Use `cues` as the cue sink (default: the silent placeholder). Call before [`Core::run`].
    #[must_use]
    pub fn with_cues(mut self, cues: Arc<dyn gorca_output_api::CueSink>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.cues = cues;
        }
        self
    }

    /// Route utterances to `speech`. Call before [`Core::run`].
    #[must_use]
    pub fn with_speech(mut self, speech: Arc<dyn SpeechOut>) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.speech = speech;
        }
        self
    }

    /// Wire an extension host: app actors offer it their events, the Reader its presentations
    /// and commands, and its speech returns through the Reader's background lane. Call before
    /// [`Core::run`].
    #[must_use]
    pub fn with_ext(mut self, src: Arc<dyn crate::ExtSource>) -> Self {
        let Some(sh) = Arc::get_mut(&mut self.shared) else {
            // A silent no-op would load extensions that never run.
            tracing::error!("Core::with_ext called after the shared state was cloned: ignored");
            debug_assert!(
                false,
                "Core::with_ext must run before anything clones the shared state"
            );
            return self;
        };
        src.set_sink(Arc::new(crate::ext::BackgroundSink::new(
            sh.senders.background.clone(),
        )));
        sh.ext = Some(src);
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

    /// Register the fact sources the daemon wired (named by `debug.explain_last`).
    #[must_use]
    pub fn with_sources(mut self, sources: SourceDirectory) -> Self {
        if let Some(sh) = Arc::get_mut(&mut self.shared) {
            sh.sources = Arc::new(sources);
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

    /// The published profile snapshot; the Reader is the only writer.
    pub fn profile_cell(&self) -> Arc<gorca_config::profiles::ProfileCell> {
        Arc::clone(&self.shared.profiles)
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
                profiles: Arc::clone(&shared.profiles),
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
    let actor = match &shared.ext {
        Some(src) => actor.with_ext(Arc::clone(src)),
        None => actor,
    };
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
        FillOp::Siblings {
            parent,
            node,
            before,
            after,
            indexed,
            want,
        } => BackendOp::Siblings {
            parent: *parent,
            node: *node,
            before: *before,
            after: *after,
            indexed: *indexed,
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
        | BackendReply::Format(_)
        | BackendReply::Target(_)
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
    .with_profile_cell(Arc::clone(&shared.profiles))
    .with_web(Arc::clone(&shared.web.docs), Arc::clone(&shared.web.join))
    .with_text_dropped(Arc::clone(&shared.text_dropped))
    .with_lazy_attrs(shared.lazy_attrs);
    if let Some(log) = &shared.present_log {
        reader = reader.with_present_log(Arc::clone(log));
    }
    reader = reader.with_sources(Arc::clone(&shared.sources));
    if let Some(src) = &shared.ext {
        reader = reader.with_ext(Arc::clone(src));
    }
    if let Some(p) = &shared.app_policy {
        reader = reader.with_app_policy(Arc::clone(p));
    }
    if let Some(p) = &shared.term_provider {
        reader = reader.with_term_provider(Arc::clone(p));
    }
    // N1: every armed deadline stays armed until it fires (a set, not one slot).
    let mut timers: std::collections::BTreeSet<Instant> = std::collections::BTreeSet::new();
    // Review fetches, builds and actions: each in a task this loop owns (aborted with it).
    let mut review_tasks: JoinSet<()> = JoinSet::new();
    // Desktop reads (windows, notices, the OSD level) have their own bound, so slow or hostile
    // applications can never use up the slots of the user's review keys (M-3).
    let mut desktop_tasks: JoinSet<()> = JoinSet::new();
    // Text fetches, likewise: bounded, owned here, aborted with the loop (ADR-0007 TW-5).
    let mut text_tasks: JoinSet<()> = JoinSet::new();
    // Pointer calls (ADR-0025): one task per request, bounded, plus the availability forwarder.
    let mut pointer_tasks: JoinSet<()> = JoinSet::new();
    if let Some(sink) = &shared.pointer {
        pointer_tasks.spawn(forward_pointer_availability(
            Arc::clone(sink),
            shared.senders.control.clone(),
        ));
    }
    loop {
        let msg = tokio::select! {
            biased;
            () = token.cancelled() => {
                tracing::info!(text = ?reader.text_counters(), "reader text counters");
                tracing::info!(terminal = ?reader.terminal_counters(), reads = ?reader.terminal_reads(), "reader terminal counters");
                return;
            }
            Some(_) = review_tasks.join_next(), if !review_tasks.is_empty() => continue,
            Some(_) = desktop_tasks.join_next(), if !desktop_tasks.is_empty() => continue,
            Some(_) = text_tasks.join_next(), if !text_tasks.is_empty() => continue,
            Some(_) = pointer_tasks.join_next(), if !pointer_tasks.is_empty() => continue,
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
                    shared.speech.submit(reader.symbolize(intent.utterance));
                }
                ReaderEffect::Cue(cue) => {
                    // A missed cue is not worth a stall: the sink never blocks and errors drop.
                    if shared.cues.play(cue).is_ok() {
                        bump(&shared.stats.cues);
                    }
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
                    } else if let Some(last) = timers.last().copied()
                        && at < last
                    {
                        // Full: the farthest deadline gives way to a nearer one. Every subsystem re-arms its own
                        // pending deadline when the Reader timer fires (N1), so nothing due soon is ever lost,
                        // in particular not the drag-lock release (ADR-0025 MI-6).
                        timers.pop_last();
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
                ReaderEffect::Pointer(req) => spawn_pointer(&mut pointer_tasks, shared, req),
                ReaderEffect::Config(req) => send_config(shared, req),
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
                | ReaderEffect::ReviewAct { .. }
                | ReaderEffect::PointerAt(_)) => spawn_review(&mut review_tasks, shared, e),
                ReaderEffect::Desktop(d) => spawn_desktop(&mut desktop_tasks, shared, d),
            }
        }
    }
}

/// The app side of a desktop effect (windows, notifications; M4b): a task bounded by request
/// deadlines whose outcome goes to the Reader on the background lane. An app that is gone, hung
/// or refusing answers with nothing, so the Reader keeps what it has (I-4).
/// Queue a request for the config task. A prewarm is dropped first (the Reader folds on a
/// miss); a save that cannot be queued is answered as failed, so the Reader never waits for it.
fn send_config(shared: &Shared, req: ConfigRequest) {
    if matches!(req, ConfigRequest::Prewarm(_)) && shared.config.capacity() <= CONFIG_RESERVED {
        return;
    }
    let failed_save = match &req {
        ConfigRequest::SaveProfiles(r) => Some((r.id, r.kind.clone())),
        ConfigRequest::Reload | ConfigRequest::Save(_) | ConfigRequest::Prewarm(_) => None,
    };
    if shared.config.try_send(req).is_err() {
        tracing::warn!("config request queue full; dropped a request");
        if let Some((id, kind)) = failed_save {
            let answer = crate::ProfilesSaved {
                id,
                plan: None,
                outcome: crate::ProfileSaveOutcome::Failed { done: 0 },
                kind,
            };
            let _ = shared.senders.control.try_send(crate::ControlMsg::Settings(
                crate::SettingsMsg::ProfilesSaved(Arc::new(answer)),
            ));
        }
    }
}

fn spawn_desktop(set: &mut JoinSet<()>, shared: &Arc<Shared>, d: crate::present::DesktopEffect) {
    use crate::present::{DesktopEffect, fetch_level, fetch_window};
    use crate::reader::BackgroundMsg;
    let app = match &d {
        DesktopEffect::Battery { epoch } => {
            // No application is involved: one read of the power source under its deadline.
            if !desktop_has_room(set) {
                return;
            }
            let epoch = *epoch;
            let tx = shared.senders.background.clone();
            set.spawn(async move {
                let info = match crate::quick::power_source() {
                    Some(src) => tokio::time::timeout(crate::quick::BATTERY_DEADLINE, src.read())
                        .await
                        .ok()
                        .flatten(),
                    None => None,
                };
                if tx.try_send(BackgroundMsg::Battery { info, epoch }).is_err() {
                    tracing::warn!("reader background queue full; dropped a battery result");
                }
            });
            return;
        }
        DesktopEffect::Window { window } => window.app,
        DesktopEffect::Notice { node, .. } | DesktopEffect::Level { node } => node.app,
    };
    if !desktop_has_room(set) {
        return;
    }
    let io = match (shared.apps.get(app), shared.mirrors.handle(app)) {
        (Some(h), Some(mirror)) if h.admits.load(Ordering::Acquire) => {
            Some(crate::review::ReviewIo {
                requests: h.requests,
                fill: h.fill,
                health: Some(h.health),
                mirror,
                clock: shared.clock.clone(),
                next_req: Arc::clone(&shared.next_req),
                limits: crate::review::ReviewLimits::default(),
                static_leaves_repeat: shared.mirrors.edit_quirks(app).text_leaves_repeat_parent,
                partial_pages: Arc::default(),
            })
        }
        _ => None,
    };
    let Some(io) = io else { return };
    let tx = shared.senders.background.clone();
    set.spawn(async move {
        let msg = match d {
            DesktopEffect::Window { window } => {
                BackgroundMsg::Window(fetch_window(&io, window).await)
            }
            DesktopEffect::Level { node } => BackgroundMsg::LevelRead {
                info: fetch_level(&io, node).await,
            },
            DesktopEffect::Notice { node, .. } => BackgroundMsg::Notice {
                info: fetch_window(&io, node).await,
            },
            DesktopEffect::Battery { .. } => return,
        };
        if tx.try_send(msg).is_err() {
            tracing::warn!("reader background queue full; dropped a desktop result");
        }
    });
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
        WebEffect::Select {
            doc,
            node,
            act,
            req,
        } => {
            let sent = shared
                .web
                .docs
                .get(doc)
                .is_some_and(|h| h.ctl.try_send(DocCtl::Select { req, node, act }).is_ok());
            if !sent {
                bump(&shared.web.counters.controls_dropped);
                // The Reader waits for this request: say it failed.
                let _ = shared.senders.docs.try_send(DocNotice::SelectionDone {
                    doc,
                    req,
                    outcome: SelOutcome::Failed,
                });
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
/// Most desktop reads in flight at once; over it the read is dropped (the Reader keeps what it
/// has), never a review task refused.
const DESKTOP_TASKS_MAX: usize = 16;

/// Pointer requests in flight at once (ADR-0025 EX-3 bounds the sink's own queue; this bounds the tasks).
const POINTER_TASKS_MAX: usize = 16;
/// One pointer call (ADR-0025 EX-2).
const POINTER_CALL: Duration = Duration::from_millis(150);

/// Mirror the sink's availability watch into the Reader (the runtime gate, ADR-0025 RG-5).
async fn forward_pointer_availability(
    sink: Arc<dyn gorca_output_api::PointerSink>,
    control: mpsc::Sender<crate::ControlMsg>,
) {
    let mut rx = sink.availability();
    if let Some(h) = sink.permission_hint() {
        let _ = control.send(crate::ControlMsg::PointerHint(h)).await;
    }
    loop {
        let a = *rx.borrow_and_update();
        let msg = crate::ControlMsg::Pointer(crate::PointerMsg::Avail(a));
        if control.send(msg).await.is_err() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Run one pointer request against the sink; the answer goes back to the Reader as a message
/// (EX-1: nothing here is on the key to speech path).
fn spawn_pointer(tasks: &mut JoinSet<()>, shared: &Shared, req: crate::PointerReq) {
    use gorca_output_api::{ButtonAct, PointerAvail, PointerError};
    let reply =
        move |result| crate::ControlMsg::PointerReply(crate::PointerReply { id: req.id, result });
    let Some(sink) = shared.pointer.clone() else {
        let r = Err(PointerError::Unavailable(PointerAvail::Absent));
        let _ = shared.senders.control.try_send(reply(r));
        return;
    };
    // The first task is the availability forwarder: it does not count.
    if tasks.len() > POINTER_TASKS_MAX {
        let _ = shared
            .senders
            .control
            .try_send(reply(Err(PointerError::Busy)));
        return;
    }
    let control = shared.senders.control.clone();
    tasks.spawn(async move {
        use crate::PointerDone as D;
        let d = gorca_rt::Deadline(Instant::now() + POINTER_CALL);
        let result = match req.op {
            crate::PointerOp::Click(b, act) => sink.button(b, act, d).await.map(D::Outcome),
            crate::PointerOp::Press(b) => sink.button(b, ButtonAct::Press, d).await.map(D::Outcome),
            crate::PointerOp::Release(b) => {
                sink.button(b, ButtonAct::Release, d).await.map(D::Outcome)
            }
            crate::PointerOp::ReleaseAll => sink.release_all(d).await.map(D::Released),
            crate::PointerOp::Position => sink.position(d).await.map(D::Pos),
        };
        let _ = control.send(reply(result)).await;
    });
}

fn review_has_room(review_tasks: &JoinSet<()>) -> bool {
    review_tasks.len() < REVIEW_TASKS_MAX
}

fn desktop_has_room(desktop_tasks: &JoinSet<()>) -> bool {
    desktop_tasks.len() < DESKTOP_TASKS_MAX
}
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
        ReaderEffect::PointerAt(r) => r.node.app,
        ReaderEffect::Speak(_)
        | ReaderEffect::Cue(_)
        | ReaderEffect::Fetch { .. }
        | ReaderEffect::Demand { .. }
        | ReaderEffect::FocusSettled { .. }
        | ReaderEffect::QueryActiveWindowFocus
        | ReaderEffect::ArmTimer(_)
        | ReaderEffect::SetGrabSet(_)
        | ReaderEffect::Pointer(_)
        | ReaderEffect::SpeechCtl(_)
        | ReaderEffect::TextFetch { .. }
        | ReaderEffect::SayAllChunk { .. }
        | ReaderEffect::Config(_)
        | ReaderEffect::Desktop(_)
        | ReaderEffect::Web(_) => return,
    };
    let tx = shared.senders.review.clone();
    let io = match (shared.apps.get(app), shared.mirrors.handle(app)) {
        (Some(h), Some(mirror)) if h.admits.load(Ordering::Acquire) && review_has_room(set) => {
            Some(ReviewIo {
                requests: h.requests,
                fill: h.fill,
                health: Some(h.health),
                mirror,
                clock: shared.clock.clone(),
                next_req: Arc::clone(&shared.next_req),
                limits: crate::review::ReviewLimits::default(),
                static_leaves_repeat: shared.mirrors.edit_quirks(app).text_leaves_repeat_parent,
                partial_pages: Arc::default(),
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
        ReaderEffect::PointerAt(req) => {
            // Locate in the app (Interactive budget), then one call to the pointer sink (150 ms); the answer is
            // a message, never a wait on the key path (EX-1).
            let sink = shared.pointer.clone();
            let control = shared.senders.control.clone();
            set.spawn(async move {
                use gorca_output_api::{PointerAvail, PointerError};
                let result = async {
                    let sink = sink.ok_or(PointerError::Unavailable(PointerAvail::Absent))?;
                    let io = io.ok_or(PointerError::NoLocation)?;
                    let target = build::locate(&io, req.node, req.offset).await?;
                    let d = gorca_rt::Deadline(Instant::now() + POINTER_CALL);
                    let opts = gorca_output_api::ClickOpts {
                        if_covered: req.if_covered,
                        ..gorca_output_api::ClickOpts::default()
                    };
                    match req.act {
                        crate::AtAct::Move => sink.move_to(target, opts, d).await,
                        crate::AtAct::Click(b) => sink.click_at(target, b, opts, d).await,
                    }
                    .map(crate::PointerDone::Outcome)
                }
                .await;
                let _ = control
                    .send(crate::ControlMsg::PointerReply(crate::PointerReply {
                        id: req.id,
                        result,
                    }))
                    .await;
            });
        }
        ReaderEffect::Speak(_)
        | ReaderEffect::Cue(_)
        | ReaderEffect::Fetch { .. }
        | ReaderEffect::Demand { .. }
        | ReaderEffect::FocusSettled { .. }
        | ReaderEffect::QueryActiveWindowFocus
        | ReaderEffect::ArmTimer(_)
        | ReaderEffect::SetGrabSet(_)
        | ReaderEffect::Pointer(_)
        | ReaderEffect::SpeechCtl(_)
        | ReaderEffect::TextFetch { .. }
        | ReaderEffect::SayAllChunk { .. }
        | ReaderEffect::Config(_)
        | ReaderEffect::Desktop(_)
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
        TextOp::Format { node, offset } => BackendOp::Text(TextQuery::Format { node, offset }),
        TextOp::Screen { node } => BackendOp::Text(TextQuery::Screen { node }),
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
            BackendReply::Format(f) => Ok(Reply::Format(f)),
            BackendReply::ActionDone => Ok(Reply::Done),
            BackendReply::Node(_)
            | BackendReply::Children(_)
            | BackendReply::Subtree(_)
            | BackendReply::Text(_)
            | BackendReply::CaretOffset(_)
            | BackendReply::TextAttributes(_)
            | BackendReply::Hit(_)
            | BackendReply::Matches(_)
            | BackendReply::Target(_)
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

    #[tokio::test]
    async fn slow_desktop_reads_can_never_use_up_the_review_slots() {
        let mut desktop: JoinSet<()> = JoinSet::new();
        let review: JoinSet<()> = JoinSet::new();
        while desktop_has_room(&desktop) {
            desktop.spawn(std::future::pending());
        }
        assert_eq!(desktop.len(), DESKTOP_TASKS_MAX);
        assert!(!desktop_has_room(&desktop), "the desktop reads are bounded");
        assert!(
            review_has_room(&review),
            "and review navigation still has all its slots"
        );
        desktop.abort_all();
    }
}
