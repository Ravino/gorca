//! B-6 arms (ADR-0003 section 16). Both arms implement the same `DocSource` against the same
//! mirror, so they differ only below the boundary; the harness drives them through [`B6Arm`].
//!
//! * [`AtspiTunedArm`]: arm (a), `AtspiDocSource` as in ADR-0003 6.2, no flags.
//! * [`AgpFirefoxArm`]: arm (b) skeleton. The AGP client crate (`gorca-doc-source-agp-firefox`) and
//!   the privileged Firefox bootstrap (`browser/firefox-agp/`) do not exist yet; this arm records
//!   what the bake-off needs from them and fails with [`ArmError::NotImplemented`] so that a B-6
//!   run reports "arm (b) not available" instead of silently measuring arm (a) twice.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gorca_doc_source_api::{DocSource, DocSourceCtx, delta_channel, join_channel};
use gorca_doc_source_atspi::AtspiEvent;
use gorca_doc_source_atspi::bus;
use gorca_doc_source_atspi::source::{SourceConfig, SourceMetrics};
use gorca_doc_source_atspi::{
    AppConn, AtspiDocSource, AttrInterner, FetchOpts, FetchTimings, PathTable, fetch_document,
};
use gorca_model::DocId;
use gorca_rt::{CancellationToken, Clock, mpsc};

use crate::oracle::Oracle;

#[derive(Debug)]
pub enum ArmError {
    NotImplemented(&'static str),
    Failed(String),
}

impl std::fmt::Display for ArmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArmError::NotImplemented(m) => write!(f, "not implemented: {m}"),
            ArmError::Failed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ArmError {}

/// What an arm is pointed at.
#[derive(Clone)]
pub struct Target {
    pub bus: zbus::Connection,
    pub app_bus_name: String,
    pub app: Arc<AppConn>,
    pub doc_path: String,
    /// Object path of the application root (`.../accessible/root`), to search for documents.
    pub app_root: String,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("doc_path", &self.doc_path)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct BuildReport {
    pub timings: FetchTimings,
    pub nodes: usize,
    pub incomplete: bool,
}

/// A live incremental session: the source runs, deltas feed `oracle`.
pub struct Session {
    pub oracle: Arc<Mutex<Oracle>>,
    pub metrics: SourceMetrics,
    pub shutdown: CancellationToken,
    pub started: Instant,
    pub dropped_events: tokio::task::JoinHandle<u64>,
    pub source_task: tokio::task::JoinHandle<Result<(), gorca_doc_source_api::SourceError>>,
    pub applier: tokio::task::JoinHandle<()>,
    pub source_requests: mpsc::Sender<gorca_doc_source_api::DocRequest>,
    /// Every normalised event the source was fed (research scenarios; capped).
    pub event_log: Arc<Mutex<Vec<AtspiEvent>>>,
}

impl Session {
    /// Wait until the oracle has a root and no delta arrived for `quiet`, or `timeout`.
    pub async fn settle(&self, quiet: Duration, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        let mut last = (0u64, Instant::now());
        loop {
            let n = self
                .oracle
                .lock()
                .map_or((0, false), |o| (o.deltas, o.root.is_some()));
            if n.0 != last.0 {
                last = (n.0, Instant::now());
            } else if n.1 && last.1.elapsed() >= quiet {
                return true;
            }
            if Instant::now() > end {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn stop(self) {
        self.shutdown.cancel();
        let _ = self.source_task.await;
        let _ = self.dropped_events.await;
        self.applier.abort();
    }
}

pub trait B6Arm {
    fn name(&self) -> &'static str;
    /// One block build of the document, nothing kept.
    fn build(&self, t: &Target) -> impl Future<Output = Result<BuildReport, ArmError>> + Send;
    /// Start the source for incremental updates (includes its initial build).
    fn attach(&self, t: &Target) -> impl Future<Output = Result<Session, ArmError>> + Send;
}

#[derive(Clone, Debug)]
pub struct AtspiTunedArm {
    pub fetch: FetchOpts,
    pub source: SourceConfig,
}

impl Default for AtspiTunedArm {
    fn default() -> Self {
        let fetch = FetchOpts::default();
        Self {
            source: SourceConfig {
                fetch: fetch.clone(),
                ..SourceConfig::default()
            },
            fetch,
        }
    }
}

impl B6Arm for AtspiTunedArm {
    fn name(&self) -> &'static str {
        "ATSPI-TUNED"
    }

    async fn build(&self, t: &Target) -> Result<BuildReport, ArmError> {
        let interner = Arc::new(AttrInterner::default());
        let mut paths = PathTable::default();
        let d = fetch_document(&t.app, &interner, &mut paths, &t.doc_path, &self.fetch)
            .await
            .map_err(|e| ArmError::Failed(e.to_string()))?;
        Ok(BuildReport {
            nodes: d.block.node_count(),
            incomplete: d.incomplete,
            timings: d.timings,
        })
    }

    async fn attach(&self, t: &Target) -> Result<Session, ArmError> {
        let shutdown = CancellationToken::new();
        // Events first: no event may be missed between the build and the subscription.
        let (raw_tx, mut raw_rx) = mpsc::channel::<AtspiEvent>(65_536);
        let (ev_tx, ev_rx) = mpsc::channel(65_536);
        let event_log = Arc::new(Mutex::new(Vec::new()));
        let tee_log = Arc::clone(&event_log);
        tokio::spawn(async move {
            while let Some(e) = raw_rx.recv().await {
                if let Ok(mut l) = tee_log.lock()
                    && l.len() < 2_000_000
                {
                    l.push(e.clone());
                }
                let _ = ev_tx.try_send(e);
            }
        });
        bus::register_object_events(&t.bus)
            .await
            .map_err(|e| ArmError::Failed(e.to_string()))?;
        let unique = t.app_bus_name.clone();
        let dropped_events = bus::subscribe(&t.bus, &unique, raw_tx, shutdown.clone())
            .await
            .map_err(|e| ArmError::Failed(e.to_string()))?;
        let (source, metrics) = AtspiDocSource::new(
            Arc::clone(&t.app),
            t.doc_path.clone(),
            self.source.clone(),
            ev_rx,
        );
        let (sink, mut rx) = delta_channel(1 << 16);
        let (req_tx, req_rx) = mpsc::channel(64);
        let (join, _jrx) = join_channel(16);
        let ctx = DocSourceCtx {
            doc: DocId(1),
            sink,
            requests: req_rx,
            join,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        };
        let oracle = Arc::new(Mutex::new(Oracle::default()));
        let o2 = Arc::clone(&oracle);
        let applier = tokio::spawn(async move {
            while let Some(d) = rx.deltas.recv().await {
                if let Ok(mut o) = o2.lock() {
                    o.apply(d);
                }
            }
        });
        let source_task = tokio::spawn(Box::new(source).attach(ctx));
        Ok(Session {
            oracle,
            metrics,
            shutdown,
            started: Instant::now(),
            dropped_events: tokio::spawn(async move { dropped_events.await.unwrap_or(0) }),
            source_task,
            applier,
            source_requests: req_tx,
            event_log,
        })
    }
}

/// Arm (b) skeleton: `HYBRID-AGP-FF` = AT-SPI authority for focus/caret plus a privileged
/// in-process Firefox source (ADR-0003 6.3-6.4).
///
/// What the real arm needs (none of it exists yet; all of it is in scope of B-6):
/// * `browser/firefox-agp/`: bootstrap (`-remote-allow-system-access` + Marionette, or AutoConfig),
///   a `JSWindowActor` that walks `nsIAccessibilityService` (about 3 us per node, 229k nodes in
///   0.75 s) and listens to `accessible-event` (REORDER, NAME_CHANGE, subtree dominance in JS).
/// * `gorca-doc-source-agp-firefox`: Unix-socket framing (0600, `SO_PEERCRED`), columnar snapshot
///   decode, sequenced batches with `Settled`, gap detection, lockstep aligner against `GetTree`
///   (TBD-B6-7).
/// * Harness hooks: `Target` already carries the AT-SPI side; the arm adds a socket path.
#[derive(Clone, Debug, Default)]
pub struct AgpFirefoxArm {
    /// Unix socket the in-browser actor listens on (set by the bootstrap).
    pub socket: Option<std::path::PathBuf>,
}

impl AgpFirefoxArm {
    /// Preconditions to check before the arm can run, for the report.
    pub fn requirements() -> &'static [&'static str] {
        &[
            "Firefox started with -remote-allow-system-access (or AutoConfig) in the private profile",
            "browser/firefox-agp/ actor loaded; socket path exported to the harness",
            "gorca-doc-source-agp-firefox crate (client, framing, aligner)",
            "same corpus pages, same Oracle comparison, same Session type",
        ]
    }
}

impl B6Arm for AgpFirefoxArm {
    fn name(&self) -> &'static str {
        "HYBRID-AGP-FF"
    }

    fn build(&self, _t: &Target) -> impl Future<Output = Result<BuildReport, ArmError>> + Send {
        std::future::ready(Err(ArmError::NotImplemented(
            "AGP client and Firefox bootstrap are created by B-6 arm (b)",
        )))
    }

    fn attach(&self, _t: &Target) -> impl Future<Output = Result<Session, ArmError>> + Send {
        std::future::ready(Err(ArmError::NotImplemented(
            "AGP client and Firefox bootstrap are created by B-6 arm (b)",
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agp_arm_is_a_skeleton_with_requirements() {
        assert!(!AgpFirefoxArm::requirements().is_empty());
        assert_eq!(AgpFirefoxArm::default().name(), "HYBRID-AGP-FF");
    }
}
