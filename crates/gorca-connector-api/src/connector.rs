// SPDX-License-Identifier: GPL-3.0-or-later
//! The `Connector` trait and its context.

use gorca_model::{ConnectorId, ConnectorKind};
use gorca_rt::{BoxFuture, CancellationToken, Clock};

use crate::registry::AppRegistrar;

#[derive(Debug, thiserror::Error)]
pub enum ConnectorError {
    #[error("connector failed to start: {0}")]
    Startup(String),
    #[error("transport failure: {0}")]
    Transport(String),
}

pub struct ConnectorCtx {
    pub id: ConnectorId,
    /// Announce and retire apps; returns an `AppPort` per app.
    pub registry: AppRegistrar,
    pub shutdown: CancellationToken,
    /// Virtual in tests.
    pub clock: Clock,
}

impl std::fmt::Debug for ConnectorCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectorCtx")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

pub trait Connector: Send + 'static {
    /// `Atspi`, `Newton`, `Fake`.
    fn kind(&self) -> ConnectorKind;

    /// Runs until shutdown. Must never block the thread and must honour `ctx.shutdown`.
    fn run(self: Box<Self>, ctx: ConnectorCtx) -> BoxFuture<'static, Result<(), ConnectorError>>;
}
