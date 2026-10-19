// SPDX-License-Identifier: GPL-3.0-or-later
//! App announcement (`AppRegistrar`) and the scope table that enforces C-5.

use std::collections::HashMap;
use std::sync::Arc;

use gorca_ingress::{IngressConfig, SlotBudget};
use gorca_model::{AppId, AppInfo, ConnectorId};
use gorca_rt::{Deadline, DeadlineExceeded, mpsc, oneshot, with_deadline};

use crate::port::{AppChannel, AppPort, app_channel, app_channel_with_budget};

/// Connector-local scope identity (AT-SPI: the bus unique name, for example `:1.42`).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ScopeKey(pub Arc<str>);

#[derive(Debug)]
pub enum RegistrarRequest {
    Announce {
        scope: ScopeKey,
        info: AppInfo,
        reply: oneshot::Sender<Result<AppPort, RegistrarError>>,
    },
    Retire {
        app: AppId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RegistrarError {
    /// C-5: the scope is already owned (by this or another connector); views are never merged.
    #[error("scope is already registered")]
    Duplicate,
    #[error("registrar is closed")]
    Closed,
    #[error("registrar did not answer in time")]
    Timeout,
    #[error("registrar queue is full")]
    Full,
}

impl From<DeadlineExceeded> for RegistrarError {
    fn from(_: DeadlineExceeded) -> Self {
        RegistrarError::Timeout
    }
}

#[derive(Clone, Debug)]
pub struct AppRegistrar {
    connector: ConnectorId,
    tx: mpsc::Sender<RegistrarRequest>,
}

pub fn registrar_channel(
    connector: ConnectorId,
    capacity: usize,
) -> (AppRegistrar, mpsc::Receiver<RegistrarRequest>) {
    let (tx, rx) = mpsc::channel(capacity);
    (AppRegistrar { connector, tx }, rx)
}

impl AppRegistrar {
    pub fn connector(&self) -> ConnectorId {
        self.connector
    }

    /// Announce an app and receive its port. Bounded by `deadline`.
    pub async fn announce(
        &self,
        scope: ScopeKey,
        info: AppInfo,
        deadline: Deadline,
    ) -> Result<AppPort, RegistrarError> {
        let (reply, rx) = oneshot::channel();
        let send = self
            .tx
            .send(RegistrarRequest::Announce { scope, info, reply });
        with_deadline(deadline, send)
            .await?
            .map_err(|_| RegistrarError::Closed)?;
        with_deadline(deadline, rx)
            .await?
            .map_err(|_| RegistrarError::Closed)?
    }

    /// Retire an app. Never blocks.
    pub fn retire(&self, app: AppId) -> Result<(), RegistrarError> {
        self.tx
            .try_send(RegistrarRequest::Retire { app })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => RegistrarError::Full,
                mpsc::error::TrySendError::Closed(_) => RegistrarError::Closed,
            })
    }
}

/// Core-side table: assigns `AppId`s and enforces C-5 (one owner per scope, never reused serials).
#[derive(Debug)]
pub struct ScopeRegistry {
    next_serial: u32,
    by_scope: HashMap<ScopeKey, AppId>,
    budget: Option<Arc<SlotBudget>>,
}

impl ScopeRegistry {
    /// A registry whose apps all charge their ingress to `budget`.
    pub fn new(budget: Arc<SlotBudget>) -> Self {
        Self {
            next_serial: 0,
            by_scope: HashMap::new(),
            budget: Some(budget),
        }
    }

    /// A registry without a budget, for tests and tools. Never use it for a real desktop.
    pub fn unbounded() -> Self {
        Self {
            next_serial: 0,
            by_scope: HashMap::new(),
            budget: None,
        }
    }

    /// Register a scope and create its channel. A scope that is already registered by any
    /// connector is refused.
    pub fn register(
        &mut self,
        connector: ConnectorId,
        scope: ScopeKey,
        cfg: IngressConfig,
    ) -> Result<AppChannel, RegistrarError> {
        if self.by_scope.contains_key(&scope) {
            return Err(RegistrarError::Duplicate);
        }
        self.next_serial = self
            .next_serial
            .checked_add(1)
            .ok_or(RegistrarError::Full)?;
        let app = AppId {
            connector,
            serial: self.next_serial,
        };
        self.by_scope.insert(scope, app);
        Ok(match &self.budget {
            Some(b) => app_channel_with_budget(app, cfg, b),
            None => app_channel(app, cfg),
        })
    }

    /// Forget a scope. The serial is not reused.
    pub fn retire(&mut self, app: AppId) {
        self.by_scope.retain(|_, a| *a != app);
    }

    pub fn len(&self) -> usize {
        self.by_scope.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_scope.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::ConnectorKind;
    use gorca_rt::Clock;
    use std::time::Duration;

    fn scope(s: &str) -> ScopeKey {
        ScopeKey(Arc::from(s))
    }

    fn info() -> AppInfo {
        AppInfo {
            toolkit: Arc::from("gtk4"),
            app_name: Arc::from("test"),
            pid: None,
            connector_kind: ConnectorKind::Fake,
            verified_shell: false,
            dump_complete: false,
            text_unit: gorca_model::TextUnit::Scalar,
            text_insert_slack: 0,
            edit_quirks: gorca_model::EditQuirks::default(),
            page_events_settle_ms: 0,
            web_engine: gorca_model::WebEngine::None,
            root: None,
            sem_tables: None,
        }
    }

    #[test]
    fn scope_registry_never_merges_or_reuses() {
        let mut r = ScopeRegistry::unbounded();
        let a = r
            .register(ConnectorId(1), scope(":1.1"), IngressConfig::default())
            .unwrap();
        assert_eq!(
            r.register(ConnectorId(2), scope(":1.1"), IngressConfig::default())
                .unwrap_err(),
            RegistrarError::Duplicate
        );
        let first = a.port.app;
        r.retire(first);
        assert!(r.is_empty());
        let b = r
            .register(ConnectorId(1), scope(":1.1"), IngressConfig::default())
            .unwrap();
        assert_ne!(b.port.app, first, "serials are never reused");
    }

    #[test]
    fn registered_apps_charge_the_explicit_budget() {
        let budget = SlotBudget::new(1_000);
        let mut r = ScopeRegistry::new(Arc::clone(&budget));
        let ch = r
            .register(ConnectorId(1), scope(":1.7"), IngressConfig::default())
            .unwrap();
        ch.port.push(gorca_model::NodeEvent::NodeRemoved {
            node: gorca_model::NodeKey {
                app: ch.port.app,
                local: gorca_model::LocalId(1),
            },
        });
        assert!(
            (1..=16).contains(&budget.used()),
            "ring credit: {}",
            budget.used()
        );
        drop(ch);
        assert_eq!(budget.used(), 0);
    }

    #[tokio::test]
    async fn announce_round_trip() {
        let (reg, mut rx) = registrar_channel(ConnectorId(1), 4);
        let core = tokio::spawn(async move {
            let mut table = ScopeRegistry::unbounded();
            while let Some(req) = rx.recv().await {
                match req {
                    RegistrarRequest::Announce { scope, reply, .. } => {
                        let r = table
                            .register(ConnectorId(1), scope, IngressConfig::default())
                            .map(|c| c.port);
                        let _ = reply.send(r);
                    }
                    RegistrarRequest::Retire { app } => table.retire(app),
                }
            }
        });
        let d = Deadline::after(&Clock::System, Duration::from_secs(1));
        let port = reg.announce(scope(":1.9"), info(), d).await.unwrap();
        assert_eq!(port.app.connector, ConnectorId(1));
        assert_eq!(
            reg.announce(scope(":1.9"), info(), d).await.unwrap_err(),
            RegistrarError::Duplicate
        );
        drop(reg);
        core.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn announce_to_a_silent_core_times_out() {
        let (reg, _rx) = registrar_channel(ConnectorId(1), 4);
        let d = Deadline(std::time::Instant::now() + Duration::from_millis(100));
        let r = reg.announce(scope(":1.1"), info(), d).await;
        assert_eq!(r.unwrap_err(), RegistrarError::Timeout);
    }
}
