//! Core entry point and structured concurrency root.
//!
//! Every task the core spawns belongs to a [`TaskSet`] (an owner) and ends on the shared
//! `CancellationToken` (a cancellation path). Phase 0 spawns no actors: the Reader lane, the
//! app/doc actors and the supervisor restart loop arrive with the wiring in `gorca-daemon`.

use std::sync::Arc;
use std::time::Duration;

use gorca_rt::{CancellationToken, EpochCell, TaskExit, TaskSet};

use crate::directory::MirrorDirectory;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CoreConfig {
    /// How long tasks get to finish after shutdown before they are aborted.
    pub shutdown_grace: Duration,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            shutdown_grace: Duration::from_secs(2),
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
}

#[derive(Debug)]
pub struct Core {
    cfg: CoreConfig,
    epoch: Arc<EpochCell>,
    mirrors: Arc<MirrorDirectory>,
}

impl Core {
    pub fn new(cfg: CoreConfig) -> Self {
        Self {
            cfg,
            epoch: Arc::new(EpochCell::new()),
            mirrors: Arc::new(MirrorDirectory::new()),
        }
    }

    /// The process-wide epoch cell (written by the input lane and the Reader).
    pub fn epoch(&self) -> Arc<EpochCell> {
        Arc::clone(&self.epoch)
    }

    pub fn mirrors(&self) -> Arc<MirrorDirectory> {
        Arc::clone(&self.mirrors)
    }

    /// Runs until `shutdown` is cancelled, then stops every task it owns.
    pub async fn run(self, shutdown: CancellationToken) -> Result<(), CoreError> {
        let mut tasks = TaskSet::new(shutdown.child_token());
        tracing::debug!("core running");
        shutdown.cancelled().await;
        tracing::debug!("core shutting down");
        let reports = tasks.shutdown(self.cfg.shutdown_grace).await;
        match reports.iter().find(|r| r.exit == TaskExit::Panicked) {
            Some(r) => Err(CoreError::TaskPanicked(r.name)),
            None => Ok(()),
        }
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
    }
}
