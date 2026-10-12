//! The `gorca` daemon: configuration, wiring, signals. The only crate that wires concrete
//! implementations (ADR-0001 section 11, rule 3).
//!
//! Responsibility (section 11): "binary `gorca`: config, wiring, `comm=orca` (ID-2), signals
//! (ID-3), systemd user unit". Phase 0 does the structural minimum:
//!
//! - a multi-threaded tokio runtime for the `workers` pool (section 3.2),
//! - `tracing` set up from `GORCA_LOG` (falling back to `RUST_LOG`, default `info`),
//! - SIGINT and SIGTERM cancel one shared `CancellationToken`; every task ends on it,
//! - SIGUSR1 is caught and ignored. ID-3: `orca --setup` sends SIGUSR1 to a process named
//!   `orca`, whose default action would terminate gorca. Opening gorca settings is TBD.
//!
//! Not yet: the hot-lane threads, `prctl(PR_SET_NAME, "orca")` (ID-2; it must run before
//! `ScreenReaderEnabled` is set, and Phase 0 sets nothing, so it stays off so that Orca keeps
//! working next to this binary), connector/speech/input wiring, the watchdog, the systemd unit
//! with `RestartPreventExitStatus=SIGKILL`.
//!
//! Allowed dependencies: everything in the workspace, since this crate is the wiring point.

use std::io::IsTerminal;
use std::time::Duration;

use anyhow::{Context, Result};
use gorca_core::{Core, CoreConfig};
use gorca_rt::{CancellationToken, TaskSet};
use tokio::signal::unix::{Signal, SignalKind, signal};
use tracing_subscriber::EnvFilter;

/// Time given to tasks to finish after shutdown is requested.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

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

/// Run the daemon until `shutdown` is cancelled.
pub async fn run(shutdown: CancellationToken) -> Result<()> {
    // Handlers are installed before the "started" line, so a supervisor (or a test) that
    // signals right after seeing it cannot hit the default action.
    let signals = Signals::install()?;
    let mut tasks = TaskSet::new(shutdown.clone());
    tasks.spawn("signals", signals.serve(shutdown.clone()));

    let core = Core::new(CoreConfig {
        shutdown_grace: SHUTDOWN_GRACE,
    });
    tracing::info!(
        workers = std::thread::available_parallelism().map_or(1, usize::from),
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
    fn run_ends_when_the_token_is_cancelled() {
        let rt = build_runtime().unwrap();
        rt.block_on(async {
            let token = CancellationToken::new();
            let h = tokio::spawn(run(token.clone()));
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(!h.is_finished());
            token.cancel();
            h.await.unwrap().unwrap();
        });
    }
}
