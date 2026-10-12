use gorca_connector_api::{Connector, ConnectorCtx, ConnectorError};
use gorca_model::ConnectorKind;
use gorca_rt::BoxFuture;

/// The AT-SPI connector. Phase 0: starts, announces nothing, stops on shutdown.
#[derive(Debug, Default)]
pub struct AtspiConnector;

impl Connector for AtspiConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Atspi
    }

    fn run(self: Box<Self>, ctx: ConnectorCtx) -> BoxFuture<'static, Result<(), ConnectorError>> {
        Box::pin(async move {
            tracing::debug!(connector = ?ctx.id, "atspi connector started (no-op)");
            ctx.shutdown.cancelled().await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_connector_api::registrar_channel;
    use gorca_model::ConnectorId;
    use gorca_rt::{CancellationToken, Clock};
    use std::time::Duration;

    #[tokio::test]
    async fn run_ends_on_shutdown() {
        let (registry, _rx) = registrar_channel(ConnectorId(1), 4);
        let shutdown = CancellationToken::new();
        let ctx = ConnectorCtx {
            id: ConnectorId(1),
            registry,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        };
        let c = Box::new(AtspiConnector);
        assert_eq!(c.kind(), ConnectorKind::Atspi);
        let h = tokio::spawn(c.run(ctx));
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(!h.is_finished());
        shutdown.cancel();
        assert!(h.await.unwrap().is_ok());
    }
}
