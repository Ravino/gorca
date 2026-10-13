//! `AppPort`: what a connector holds per announced app.

use std::sync::Arc;

use gorca_ingress::{
    IngressConfig, IngressReceiver, IngressSender, SlotBudget, ingress, ingress_with_budget,
};
use gorca_model::{AppId, AppLifecycle, RawEvent, SnapshotBatch};
use gorca_rt::mpsc;

use crate::backend::BackendRequest;

/// In-flight request bound per app (section 6.2: "32 in flight per app").
pub const REQUEST_QUEUE: usize = 32;
const CONTROL_QUEUE: usize = 64;

/// Non-event traffic from connector to app actor.
#[derive(Debug)]
pub enum AppControl {
    Snapshot(SnapshotBatch),
    Lifecycle(AppLifecycle),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PortError {
    /// The app actor is not keeping up. The caller may retry later or mark itself dirty.
    #[error("app port queue is full")]
    Full,
    #[error("app actor is gone")]
    Closed,
}

/// Given to the connector when it announces an app.
#[derive(Debug)]
pub struct AppPort {
    pub app: AppId,
    ingress: IngressSender,
    control: mpsc::Sender<AppControl>,
    /// Core to connector, bounded.
    pub requests: mpsc::Receiver<BackendRequest>,
}

impl AppPort {
    /// A handle on this app's ingress for a transport that pushes from its own thread (the
    /// socket drain, `gorca_dbus::drain::AppRoute`). Cloning is cheap and not a hot path.
    pub fn ingress_sender(&self) -> IngressSender {
        self.ingress.clone()
    }

    /// O(1), never blocks; may coalesce or mark dirty (C-1).
    pub fn push(&self, ev: RawEvent) {
        self.ingress.push(ev);
    }

    /// Bulk cache fill. Never blocks.
    pub fn push_snapshot(&self, nodes: SnapshotBatch) -> Result<(), PortError> {
        self.send(AppControl::Snapshot(nodes))
    }

    /// `Appeared`, `Ready`, `Gone`. Never blocks; on `Full` the connector must retry, because a
    /// lost `Gone` would leave the app actor waiting.
    pub fn lifecycle(&self, ev: AppLifecycle) -> Result<(), PortError> {
        self.send(AppControl::Lifecycle(ev))
    }

    fn send(&self, msg: AppControl) -> Result<(), PortError> {
        self.control.try_send(msg).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => PortError::Full,
            mpsc::error::TrySendError::Closed(_) => PortError::Closed,
        })
    }
}

/// Both ends of one app's channels. The registrar side keeps the core half.
#[derive(Debug)]
pub struct AppChannel {
    pub port: AppPort,
    pub ingress: IngressReceiver,
    pub requests: mpsc::Sender<BackendRequest>,
    pub control: mpsc::Receiver<AppControl>,
}

/// The channels of one app with an ingress that is not charged to any budget (tests, tools).
/// Production apps are created through [`app_channel_with_budget`].
pub fn app_channel(app: AppId, cfg: IngressConfig) -> AppChannel {
    channels(app, ingress(cfg))
}

/// The channels of one app whose ingress is charged to `budget` (SPEC 3.6): one budget is shared
/// by every app of the core, so a flood from many applications is bounded as a whole and no
/// application can starve another (AU-3). The budget is passed explicitly, never global.
pub fn app_channel_with_budget(
    app: AppId,
    cfg: IngressConfig,
    budget: &Arc<SlotBudget>,
) -> AppChannel {
    channels(app, ingress_with_budget(cfg, budget))
}

fn channels(app: AppId, (itx, irx): (IngressSender, IngressReceiver)) -> AppChannel {
    let (ctx, crx) = mpsc::channel(CONTROL_QUEUE);
    let (rtx, rrx) = mpsc::channel(REQUEST_QUEUE);
    AppChannel {
        port: AppPort {
            app,
            ingress: itx,
            control: ctx,
            requests: rrx,
        },
        ingress: irx,
        requests: rtx,
        control: crx,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{ConnectorId, LocalId, NodeEvent, NodeKey};

    fn app() -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: 1,
        }
    }

    #[test]
    fn push_reaches_the_ingress_receiver() {
        let ch = app_channel(app(), IngressConfig::default());
        ch.port.push(NodeEvent::FocusChanged {
            node: NodeKey {
                app: app(),
                local: LocalId(1),
            },
        });
        assert_eq!(ch.ingress.drain().events.len(), 1);
    }

    #[test]
    fn control_queue_is_bounded_and_reports_full() {
        let ch = app_channel(app(), IngressConfig::default());
        for _ in 0..CONTROL_QUEUE {
            assert_eq!(ch.port.lifecycle(AppLifecycle::Ready), Ok(()));
        }
        assert_eq!(ch.port.lifecycle(AppLifecycle::Ready), Err(PortError::Full));
    }

    #[test]
    fn dropped_core_half_reports_closed() {
        let AppChannel { port, control, .. } = app_channel(app(), IngressConfig::default());
        drop(control);
        assert_eq!(port.lifecycle(AppLifecycle::Ready), Err(PortError::Closed));
    }
}
