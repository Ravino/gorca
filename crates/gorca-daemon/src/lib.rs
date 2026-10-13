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
//! - Keyboard monitoring is not wired in this slice (`--no-keyboard` is accepted; with
//!   `--impersonate-orca` the daemon would own `org.gnome.Orca.KeyboardMonitor`, which is
//!   deliberately not done yet). `prctl(comm=orca)` stays off so that a running Orca keeps
//!   working next to this binary; a running Orca is never killed or replaced.
//!
//! Allowed dependencies: everything in the workspace, since this crate is the wiring point.

mod cli;
mod wire;

use std::io::IsTerminal;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gorca_connector_api::{Connector, ConnectorCtx};
use gorca_connector_atspi::{AtspiConnector, AtspiOptions};
use gorca_core::{Core, CoreConfig, SpeechOut};
use gorca_input_api::{KeyClass, KeyEvent, KeyIntent};
use gorca_model::{ConnectorId, Prio, Segment, SegmentKind, UttId, Utterance};
use gorca_output_api::SpeechClient;
use gorca_rt::{CancellationToken, Clock, EpochCell, TaskSet, mpsc};
use gorca_speech::{
    BackendConnectError, FailoverMode, FailoverNotice, FnBackend, SpeechBackend, SpeechHandle,
};
use gorca_speech_ssip::{SsipClient, SsipOptions};
use gorca_speech_voicego::{VoicegoClient, VoicegoOptions};
use smallvec::smallvec;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing_subscriber::EnvFilter;

pub use cli::{CliError, Options, Parsed, SpeechMode, USAGE, parse};

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

/// The multi-threaded runtime for the `workers` lane: one thread per available core.
pub fn build_runtime() -> Result<tokio::runtime::Runtime> {
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(threads)
        .thread_name("gorca-worker")
        .enable_all()
        .build()
        .context("building the tokio runtime")
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

    fn notify_epoch(&self) {
        self.0.notify_epoch();
    }
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
        FailoverNotice::Fallback { from, to } => format!("{from} unavailable, using {to}"),
        FailoverNotice::Restored { backend } => format!("{backend} restored"),
        FailoverNotice::Unavailable => "speech service unavailable".to_owned(),
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
        };
        speech.submit(u);
    }
}

/// The input lane's glue between the key source and the Reader (K-3): every key press advances
/// the epoch, which cancels stale speech at once, and the Reader adopts that epoch (E-4) so the
/// echo of this key is never cancelled by the key itself.
async fn pump_keys(
    mut keys: mpsc::Receiver<KeyEvent>,
    reader: mpsc::Sender<KeyIntent>,
    epoch: Arc<EpochCell>,
    speech: SpeechHandle,
) {
    while let Some(event) = keys.recv().await {
        let e = if event.pressed {
            let e = epoch.advance();
            speech.notify_epoch();
            e
        } else {
            epoch.current()
        };
        let intent = KeyIntent {
            event,
            // No command grabs yet: every key is observed for echo and interrupt only.
            class: KeyClass::PassToApp,
            epoch: e,
        };
        if reader.try_send(intent).is_err() {
            tracing::warn!("reader key lane full; dropped a key");
        }
    }
}

/// Keep the AT-SPI connector running: restart it with backoff when it fails (bus missing,
/// connection lost). Returns when `shutdown` is cancelled.
async fn supervise_connector(
    opts: AtspiOptions,
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
        let connector = Box::new(AtspiConnector::new(opts.clone()));
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
    let mut tasks = TaskSet::new(shutdown.clone());
    let mut core = Core::new(CoreConfig {
        shutdown_grace: SHUTDOWN_GRACE,
        ..CoreConfig::default()
    });
    let registry = core.connector_registrar(ConnectorId(1));
    let epoch = core.epoch();
    let focused = core.focused_app();

    let speech = wire::wire_speech(&mut tasks, &opts, &epoch);
    let speech_for_keys = speech.clone();
    let core = core.with_speech(Arc::new(LaneOut(speech)));
    wire::wire_keyboard(
        &mut tasks,
        &opts,
        &shutdown,
        &epoch,
        speech_for_keys,
        core.reader_senders().key,
    );
    wire::wire_atspi(&mut tasks, &opts, focused, registry, &shutdown);

    tracing::info!(
        workers = std::thread::available_parallelism().map_or(1, usize::from),
        speech = ?opts.speech,
        "gorca started"
    );
    let result = core.run(shutdown.clone()).await;
    shutdown.cancel();
    let reports = tasks.shutdown(SHUTDOWN_GRACE).await;
    for r in &reports {
        tracing::debug!(task = r.name, exit = ?r.exit, "task finished");
    }
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
