//! The `gorca` daemon: configuration, wiring, signals. The only crate that wires concrete
//! implementations (ADR-0001 section 11, rule 3).
//!
//! Phase 1 (first vertical slice) wires, on one multi-threaded tokio runtime:
//!
//! ```text
//! AT-SPI connector --announce/events/requests--> core (app actors, Reader lane, fetch)
//!                                                   | epoch-tagged utterances
//!                                                   v
//!                       speech lane --> FailoverClient --> voicego (primary) | speech-dispatcher
//! ```
//!
//! - SIGINT and SIGTERM cancel one shared `CancellationToken`; every task ends on it.
//! - SIGUSR1 is caught and ignored. ID-3: `orca --setup` sends SIGUSR1 to a process named
//!   `orca`, whose default action would terminate gorca.
//! - The connector is supervised: if the a11y bus is missing or the connector fails, it is
//!   restarted with backoff while the rest of the daemon (speech, signals) keeps running.
//! - Settings (`gorca.toml`) are loaded, watched (inotify) and published by the config task;
//!   the keymap and lane settings reach the input lane through `ArcSwap` cells.
//! - Keyboard monitoring needs `--impersonate-orca` (the daemon then owns
//!   `org.gnome.Orca.KeyboardMonitor`, never displacing a running Orca). The input lane
//!   classifies keys, runs commands as Reader messages, and applies grab sets through the
//!   Mutter key source. `prctl(comm=orca)` stays off so that a running Orca keeps working next
//!   to this binary.
//!
//! Allowed dependencies: everything in the workspace, since this crate is the wiring point.

mod cli;
mod config_task;
mod input;
mod platform;
mod power;
mod probe;
mod rtkit;
mod shell_policy;
mod terminal_policy;
mod topology;
mod wire;

pub use probe::{DaemonProbe, ProbeRead};
pub use shell_policy::ShellPolicy as NativeShellPolicy;
pub use terminal_policy::NativeTerminal as NativeTerminalPolicy;
pub use topology::{Arm, LaneRuntimes, build_runtime_for, bulk_pool, finish as finish_topology};

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gorca_connector_api::{Connector, ConnectorCtx};
use gorca_connector_atspi::{AtspiConnector, AtspiOptions, AtspiState};
use gorca_core::{Core, CoreConfig, SpeechCtl, SpeechOut};
use gorca_model::{ConnectorId, Prio, Segment, SegmentKind, UttId, Utterance};
use gorca_output_api::SpeechClient;
use gorca_output_api::VoiceParams;
use gorca_rt::{CancellationToken, Clock, EpochCell, TaskSet, mpsc};
use gorca_speech::{
    BackendConnectError, FailoverMode, FailoverNotice, FnBackend, SpeechBackend, SpeechControl,
    SpeechHandle,
};
use gorca_speech_ssip::{SsipClient, SsipOptions};
use gorca_speech_voicego::{VoicegoClient, VoicegoOptions};
use smallvec::smallvec;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing_subscriber::EnvFilter;

pub use cli::{
    CliError, ConfigCmd, FillChoice, Options, Parsed, ShellPolicyMode, SpeechMode, USAGE,
    default_ext_dirs, parse,
};
pub use config_task::{Built, ConfigSource, ConfigTask, build as build_settings, run_config_cmd};
pub use platform::{PlatformOverrides, probe as probe_platform};

/// Time given to tasks to finish after shutdown is requested.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const CONNECTOR_BACKOFF_MIN: Duration = Duration::from_millis(500);
const CONNECTOR_BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Utterance ids of daemon announcements start here, clear of the Reader's counter.
const NOTICE_UTT_BASE: u64 = 1 << 62;

/// Install the global `tracing` subscriber (stderr). Call once, before the runtime starts.
pub fn init_tracing() -> Result<()> {
    let filter = EnvFilter::try_from_env("GORCA_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .try_init()
        .map_err(|e| anyhow::anyhow!("installing tracing subscriber: {e}"))
}

/// The multi-threaded runtime for the `workers` lane: one thread per available core (topology
/// arm (a), the default; see [`build_runtime_for`] for the other arms).
pub fn build_runtime() -> Result<tokio::runtime::Runtime> {
    build_runtime_for(Arm::Single, None, None)
}

/// Installed signal streams. Installing is synchronous (it must run inside the runtime), so
/// once [`Signals::install`] returns, the default actions of SIGINT, SIGTERM and SIGUSR1 are
/// already replaced.
#[derive(Debug)]
pub struct Signals {
    int: Signal,
    term: Signal,
    usr1: Signal,
}

impl Signals {
    pub fn install() -> Result<Self> {
        Ok(Self {
            int: signal(SignalKind::interrupt()).context("installing SIGINT handler")?,
            term: signal(SignalKind::terminate()).context("installing SIGTERM handler")?,
            usr1: signal(SignalKind::user_defined1()).context("installing SIGUSR1 handler")?,
        })
    }

    /// Wait for SIGINT or SIGTERM and then cancel `shutdown`. SIGUSR1 is logged and ignored.
    /// Also returns when `shutdown` is cancelled by someone else.
    pub async fn serve(mut self, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                _ = self.int.recv() => {
                    tracing::info!("SIGINT received, shutting down");
                    shutdown.cancel();
                }
                _ = self.term.recv() => {
                    tracing::info!("SIGTERM received, shutting down");
                    shutdown.cancel();
                }
                _ = self.usr1.recv() => {
                    // ID-3: `orca --setup` signals us. TBD: open gorca settings.
                    tracing::info!("SIGUSR1 received, ignored");
                }
            }
        }
    }
}

/// Feeds the core's utterances to the speech lane.
#[derive(Debug, Clone)]
struct LaneOut(SpeechHandle);

impl SpeechOut for LaneOut {
    fn submit(&self, utterance: Utterance) {
        self.0.submit(utterance);
    }

    fn submit_say_all(&self, utterance: Utterance, first: bool) {
        self.0.submit_say_all(utterance, first);
    }

    fn notify_epoch(&self) {
        self.0.notify_epoch();
    }

    fn control(&self, ctl: SpeechCtl) {
        self.0.control(match ctl {
            SpeechCtl::SetParams(l) => SpeechControl::SetParams(voice_params(l)),
            SpeechCtl::SetCapitals(c) => SpeechControl::SetCapitals(gorca_speech::CapitalFx {
                pitch_delta: capital_pitch_delta(c.pitch_percent),
                cue: c.cue,
            }),
            SpeechCtl::Pause => SpeechControl::Pause,
            SpeechCtl::Resume => SpeechControl::Resume,
            SpeechCtl::Mute(m) => SpeechControl::Mute(m),
        });
    }
}

/// Core levels are 0 to 100 with 50 (rate, pitch) and 100 (volume) as the service's default;
/// the speech clients take -100 to 100 around their own default. Volume only attenuates, so a
/// default configuration never makes the voice louder than the service's setting.
fn voice_params(l: gorca_core::VoiceLevels) -> VoiceParams {
    let centred = |v: u8| i8::try_from((i16::from(v) - 50) * 2).unwrap_or(0);
    VoiceParams {
        rate: centred(l.rate),
        pitch: centred(l.pitch),
        volume: i8::try_from(i16::from(l.volume) - 100).unwrap_or(0),
    }
}

/// `speech.caps_pitch_change` is in steps of the 0 to 100 pitch setting (NVDA's
/// `capPitchChange`); the wire scale has two units per step.
fn capital_pitch_delta(percent: u8) -> i8 {
    i8::try_from((u16::from(percent.min(50)) * 2).min(100)).unwrap_or(100)
}

fn unavailable(name: &'static str) -> Arc<dyn SpeechBackend> {
    Arc::new(FnBackend::new(name, move |_deadline| async move {
        Err(BackendConnectError::new(name, "no socket path is known"))
    }))
}

fn voicego_backend(opts: &Options) -> Arc<dyn SpeechBackend> {
    let Some(path) = opts.voicego_socket.clone() else {
        return unavailable("voicego");
    };
    let mut vopts = VoicegoOptions::with_path(path);
    vopts.allowed_peer_uids.clone_from(&opts.speech_peer_uids);
    Arc::new(FnBackend::new("voicego", move |deadline| {
        let vopts = vopts.clone();
        async move {
            VoicegoClient::connect(&vopts, deadline)
                .await
                .map(|c| Box::new(c) as Box<dyn SpeechClient>)
                .map_err(|e| BackendConnectError::new("voicego", e.to_string()))
        }
    }))
}

fn ssip_backend(opts: &Options) -> Arc<dyn SpeechBackend> {
    let Some(path) = opts.ssip_socket.clone() else {
        return unavailable("speech-dispatcher");
    };
    let mut sopts = SsipOptions::with_path(path);
    sopts.allowed_peer_uids.clone_from(&opts.speech_peer_uids);
    Arc::new(FnBackend::new("speech-dispatcher", move |deadline| {
        let sopts = sopts.clone();
        async move {
            SsipClient::connect(&sopts, deadline)
                .await
                .map(|c| Box::new(c) as Box<dyn SpeechClient>)
                .map_err(|e| BackendConnectError::new("speech-dispatcher", e.to_string()))
        }
    }))
}

/// The speech backends in order and the failover mode for `--speech`.
fn speech_plan(opts: &Options) -> (Vec<Arc<dyn SpeechBackend>>, FailoverMode) {
    match opts.speech {
        SpeechMode::Auto => (
            vec![voicego_backend(opts), ssip_backend(opts)],
            FailoverMode::Auto,
        ),
        SpeechMode::Voicego => (vec![voicego_backend(opts)], FailoverMode::Only(0)),
        SpeechMode::Ssip => (vec![ssip_backend(opts)], FailoverMode::Only(0)),
    }
}

fn notice_text(n: FailoverNotice) -> String {
    match n {
        FailoverNotice::Fallback { from, to } => {
            gorca_i18n::msg!(SpeechFallback, from = from, to = to)
        }
        FailoverNotice::Restored { backend } => gorca_i18n::msg!(SpeechRestored, backend = backend),
        FailoverNotice::Unavailable => {
            gorca_i18n::t(gorca_i18n::MsgId::SpeechUnavailable).to_owned()
        }
    }
}

/// Speaks failover notices ("voicego unavailable, using speech-dispatcher"). They are
/// announcements, not answers to a key press, so they never cancel and survive focus changes.
async fn announce_notices(
    mut notices: mpsc::Receiver<FailoverNotice>,
    speech: SpeechHandle,
    epoch: Arc<EpochCell>,
) {
    let mut next = NOTICE_UTT_BASE;
    while let Some(n) = notices.recv().await {
        tracing::warn!(notice = ?n, "speech backend changed");
        next += 1;
        let u = Utterance {
            id: UttId(next),
            segments: smallvec![Segment {
                text: Arc::from(notice_text(n)),
                kind: SegmentKind::Text,
            }],
            lang: None,
            epoch: epoch.current(),
            prio: Prio::Message,
            may_refine: false,
            survives_focus_change: true,
            ttl_secs: 0,
            refines: None,
        };
        speech.submit(u);
    }
}

/// Keep the AT-SPI connector running: restart it with backoff when it fails (bus missing,
/// connection lost). Returns when `shutdown` is cancelled.
async fn supervise_connector(
    opts: AtspiOptions,
    state: AtspiState,
    registry: gorca_connector_api::AppRegistrar,
    shutdown: CancellationToken,
) {
    let mut backoff = CONNECTOR_BACKOFF_MIN;
    loop {
        let started = tokio::time::Instant::now();
        let ctx = ConnectorCtx {
            id: registry.connector(),
            registry: registry.clone(),
            shutdown: shutdown.child_token(),
            clock: Clock::System,
        };
        let connector = Box::new(AtspiConnector::with_state(opts.clone(), state.clone()));
        match connector.run(ctx).await {
            Ok(()) => return,
            Err(e) => tracing::warn!(error = %e, ?backoff, "AT-SPI connector failed; retrying"),
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        // A connector that ran for a while was healthy: start the backoff over.
        backoff = if started.elapsed() > Duration::from_secs(30) {
            CONNECTOR_BACKOFF_MIN
        } else {
            (backoff * 2).min(CONNECTOR_BACKOFF_MAX)
        };
    }
}

/// Run the daemon with signal handling until a signal or `shutdown`.
pub async fn run(opts: Options, shutdown: CancellationToken) -> Result<()> {
    // Handlers are installed before the "started" line, so a supervisor (or a test) that
    // signals right after seeing it cannot hit the default action.
    let signals = Signals::install()?;
    let mut tasks = TaskSet::new(shutdown.clone());
    tasks.spawn("signals", signals.serve(shutdown.clone()));
    let result = run_with(opts, shutdown.clone()).await;
    shutdown.cancel();
    tasks.shutdown(SHUTDOWN_GRACE).await;
    result
}

/// The daemon without signal handlers (tests run several in one process).
pub async fn run_with(opts: Options, shutdown: CancellationToken) -> Result<()> {
    run_inner(opts, shutdown, None, None).await
}

/// How a test chooses the fill policy of every application (the product always runs R0).
pub type PolicyFactory = Arc<dyn Fn() -> Box<dyn gorca_mirror::fill::FillPolicy> + Send + Sync>;

/// [`run_with`] that hands a [`DaemonProbe`] to `on_ready` once the daemon is wired (the
/// test-only debug accessor of the B-1 conformance suite).
pub async fn run_with_probe(
    opts: Options,
    shutdown: CancellationToken,
    on_ready: impl FnOnce(DaemonProbe) + Send + 'static,
) -> Result<()> {
    run_inner(opts, shutdown, None, Some(Box::new(on_ready))).await
}

/// [`run_with_probe`] with the fill policy of the apps chosen by the caller. Only tests of the
/// shared fill code need it (the null policy never requests anything).
pub async fn run_with_policy(
    opts: Options,
    shutdown: CancellationToken,
    policy: PolicyFactory,
    on_ready: impl FnOnce(DaemonProbe) + Send + 'static,
) -> Result<()> {
    run_inner(opts, shutdown, Some(policy), Some(Box::new(on_ready))).await
}

async fn run_inner(
    mut opts: Options,
    shutdown: CancellationToken,
    policy: Option<PolicyFactory>,
    on_ready: Option<Box<dyn FnOnce(DaemonProbe) + Send>>,
) -> Result<()> {
    // `[speech] backend` applies when neither `--speech` nor the environment chose one.
    if opts.speech == SpeechMode::Auto
        && let Some(path) = opts.config_path.clone()
        && let Some(b) = config_task::startup_backend(&path).await
    {
        opts.speech = match b {
            gorca_config::Backend::Auto => SpeechMode::Auto,
            gorca_config::Backend::Voicego => SpeechMode::Voicego,
            gorca_config::Backend::Ssip => SpeechMode::Ssip,
        };
    }
    // `info.battery`: UPower over the system bus, read-only, under a 250 ms deadline.
    gorca_core::install_power_source(std::sync::Arc::new(power::UPower));
    let mut tasks = TaskSet::new(shutdown.clone());
    let mut core = Core::new(CoreConfig {
        shutdown_grace: SHUTDOWN_GRACE,
        ..CoreConfig::default()
    });
    // A caller-supplied factory (tests of the shared fill code) wins over the flag.
    let policy = policy.or_else(|| match opts.fill_policy {
        cli::FillChoice::R0 => None,
        cli::FillChoice::Prefetch => {
            let cfg = gorca_mirror::fill::PrefetchConfig {
                shell_dump: opts.shell_dump,
            };
            let f: PolicyFactory =
                Arc::new(move || Box::new(gorca_mirror::fill::FocusPrefetch::new(cfg)));
            Some(f)
        }
    });
    if let Some(p) = policy {
        core = core.with_fill_policy(p);
    }
    // Extensions first: their commands must be known before the settings are compiled.
    // The first-party extensions' message catalogs: registered up front from the embedded copies.
    // They are built-in namespaces: the files next to a WASM component never replace them.
    ext_gnome_shell::register_catalogs();
    ext_vte::register_catalogs();
    let (mut core, shell_in_host, terminal_in_host) = wire::wire_ext(core, &opts).await;
    // The terminal policy (ADR-0008 amendment 1): the `ext-vte` WASM component when the extension
    // host loaded it, else the native build of the same source behind the core's `TermProvider`.
    // They are exclusive, so the gates exercise the one the log line names.
    if terminal_in_host {
        tracing::info!("terminal policy: ext-vte WASM component");
    } else {
        tracing::info!("terminal policy: ext-vte native build");
        core = core.with_term_provider(Arc::new(terminal_policy::NativeTerminal::default()));
    }
    // The Shell's presentation policy (ADR-0008 amendment 1): the `ext-gnome-shell` WASM component
    // when the extension host loaded it, else the native build of the same source behind the
    // core's `AppPolicy` seam.
    if shell_in_host {
        tracing::info!("shell policy: ext-gnome-shell WASM component");
    } else {
        tracing::info!("shell policy: ext-gnome-shell native build");
        core = core.with_app_policy(Arc::new(shell_policy::ShellPolicy::default()));
    }
    let registry = core.connector_registrar(ConnectorId(1));
    let epoch = core.epoch();
    let focused = core.focused_app();

    // B-5: in the lane arms the key input and the speech lane get their own thread and runtime;
    // in arm (a) the handles are `None` and `placed_on` leaves the tasks where they are.
    let mut lanes = LaneRuntimes::start(opts.topology)?;
    let mut lane_handle: Option<SpeechHandle> = None;
    let core = if opts.shadow {
        // B-1 PW-11: beside the user's real screen reader gorca only reads. Speech goes to the
        // core's null sink, and nothing grabs keys or takes the Orca identity.
        tracing::info!("shadow mode: read-only, no speech, no keyboard, no event registration");
        core
    } else {
        let speech = tasks.placed_on(lanes.speech(), |t| wire::wire_speech(t, &opts, &epoch));
        let speech_for_keys = speech.clone();
        lane_handle = Some(speech.clone());
        let mut core = core.with_speech(Arc::new(LaneOut(speech)));
        let senders = core.reader_senders();
        wire::wire_text(&mut tasks, &speech_for_keys, senders.text_io.clone());
        let links = core.lane_links();
        // Settings first: the keymap must be in place before the first key is classified.
        if let Some(requests) = core.take_config_requests() {
            wire::wire_config(
                &mut tasks,
                &opts,
                &shutdown,
                Arc::clone(&links),
                senders.control.clone(),
                requests,
                speech_for_keys.clone(),
            );
        }
        if let Some(requests) = core.take_clipboard_requests() {
            wire::wire_clipboard(&mut tasks, &opts, &shutdown, requests);
        }
        let (grabs, grab_control) = (core.grab_requests(), core.grab_sender());
        tasks.placed_on(lanes.input(), |t| {
            wire::wire_input(
                t,
                &opts,
                &shutdown,
                &epoch,
                speech_for_keys,
                senders,
                links,
                grabs,
                grab_control,
            );
        });
        core
    };
    let atspi_state = AtspiState::default();
    let core = wire::wire_present_log(core, &opts, &atspi_state);
    let core = wire::wire_web(core, &opts, &atspi_state);
    if let Some(f) = on_ready {
        f(DaemonProbe::new(core.mirrors(), atspi_state.clone()));
    }
    wire::wire_atspi(&mut tasks, &opts, focused, registry, atspi_state, &shutdown);

    tracing::info!(
        workers = std::thread::available_parallelism().map_or(1, usize::from),
        speech = ?opts.speech,
        topology = opts.topology.name(),
        "gorca started"
    );
    let core_stats = core.stats();
    let result = core.run(shutdown.clone()).await;
    tracing::info!(stats = ?core_stats.snapshot(), "core");
    if let Some(h) = &lane_handle {
        tracing::info!(stats = ?h.stats(), "speech lane");
    }
    shutdown.cancel();
    let reports = tasks.shutdown(SHUTDOWN_GRACE).await;
    for r in &reports {
        tracing::debug!(task = r.name, exit = ?r.exit, "task finished");
    }
    // The lane threads outlive their tasks; end them now (joining blocks, so off the workers).
    let _ = tokio::time::timeout(
        SHUTDOWN_GRACE,
        tokio::task::spawn_blocking(move || lanes.stop()),
    )
    .await;
    tracing::info!("gorca stopped");
    result.map_err(anyhow::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_multi_threaded() {
        let rt = build_runtime().unwrap();
        let metrics = rt.metrics();
        assert_eq!(
            metrics.num_workers(),
            std::thread::available_parallelism().map_or(1, usize::from)
        );
    }

    #[test]
    fn notices_are_plain_sentences() {
        assert_eq!(
            notice_text(FailoverNotice::Fallback {
                from: "voicego",
                to: "speech-dispatcher"
            }),
            "voicego unavailable, using speech-dispatcher"
        );
    }

    #[test]
    fn speech_plan_follows_the_flag() {
        let mut o = Options::default();
        assert_eq!(speech_plan(&o).0.len(), 2);
        assert_eq!(speech_plan(&o).1, FailoverMode::Auto);
        o.speech = SpeechMode::Ssip;
        let (b, m) = speech_plan(&o);
        assert_eq!((b.len(), m), (1, FailoverMode::Only(0)));
        assert_eq!(b[0].name(), "speech-dispatcher");
        o.speech = SpeechMode::Voicego;
        assert_eq!(speech_plan(&o).0[0].name(), "voicego");
    }

    /// The daemon must survive a missing a11y bus and a missing speech service, and stop on
    /// request.
    #[test]
    fn run_survives_missing_services_and_stops_on_cancel() {
        let rt = build_runtime().unwrap();
        rt.block_on(async {
            let token = CancellationToken::new();
            let opts = Options::isolated(
                "unix:path=/nonexistent/gorca-a11y",
                "unix:path=/nonexistent/gorca-session",
                std::path::Path::new("/nonexistent/voicego.sock"),
                std::path::Path::new("/nonexistent/speechd.sock"),
            );
            let h = tokio::spawn(run_with(opts, token.clone()));
            tokio::time::sleep(Duration::from_millis(300)).await;
            assert!(!h.is_finished(), "the daemon stays up without its services");
            token.cancel();
            let r = tokio::time::timeout(Duration::from_secs(5), h).await;
            assert!(matches!(r, Ok(Ok(Ok(())))), "{r:?}");
        });
    }
}

#[cfg(test)]
mod capital_tests {
    use super::capital_pitch_delta;

    #[test]
    fn the_capital_change_is_two_wire_units_per_pitch_step_and_stays_in_range() {
        assert_eq!(capital_pitch_delta(0), 0);
        assert_eq!(capital_pitch_delta(30), 60);
        assert_eq!(capital_pitch_delta(50), 100);
        assert_eq!(capital_pitch_delta(100), 100);
    }
}
