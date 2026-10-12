//! The AT-SPI connector (ADR-0001 section 5.3).
//!
//! Topology, all owned by [`AtspiConnector::run`]:
//!
//! ```text
//! a11y bus --- event connection (raw drain thread, ADR-0004) --> per-app ingress (core)
//! a11y bus --- call connection (zbus) --> one AppServer task per application (requests)
//!                                     \-> NameOwnerChanged watcher, RegisterEvent
//! session bus --- org.a11y.Status (ID-5 enablement)
//! ```
//!
//! Every D-Bus call has a deadline; every task is owned by a `TaskSet`; a drain that the broker
//! closed is restarted with backoff while the rest of the connector keeps serving.

use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use gorca_connector_api::{Connector, ConnectorCtx, ConnectorError};
use gorca_dbus::call_method;
use gorca_dbus::drain::{DrainConfig, DrainEnd, DrainHandle, Router, spawn_drain};
use gorca_model::ConnectorKind;
use gorca_rt::{BoxFuture, BudgetClass, CancellationToken, Clock, Deadline, TaskSet, mpsc};
use zbus::Connection;

use crate::enablement::Enablement;
use crate::fetch::{AppServer, Caller};
use crate::paths::PathTable;
use crate::router::{AtspiRouter, NewApp};
use crate::session::{
    REGISTERED_EVENTS, SessionError, a11y_bus_address, connect, connect_session, read_status,
    register_events, write_status,
};

/// Applications announced but not yet picked up by the connector task.
const NEW_APP_QUEUE: usize = 1024;
/// How often the connector checks that the drain is alive.
const DRAIN_POLL: Duration = Duration::from_millis(500);
const DRAIN_BACKOFF_MIN: Duration = Duration::from_millis(100);
const DRAIN_BACKOFF_MAX: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct AtspiOptions {
    /// Address of the a11y bus. `None`: `GORCA_A11Y_BUS_ADDRESS`, `AT_SPI_BUS_ADDRESS`, then
    /// `org.a11y.Bus.GetAddress` on the session bus.
    pub a11y_address: Option<String>,
    /// Address of the session bus. `None`: `DBUS_SESSION_BUS_ADDRESS`.
    pub session_address: Option<String>,
    /// ID-5: set `IsEnabled` and `ScreenReaderEnabled` at startup and restore them at shutdown.
    pub enable_accessibility: bool,
    /// `Registry.RegisterEvent` for [`REGISTERED_EVENTS`].
    pub register_events: bool,
}

impl Default for AtspiOptions {
    fn default() -> Self {
        Self {
            a11y_address: None,
            session_address: None,
            enable_accessibility: true,
            register_events: true,
        }
    }
}

/// The AT-SPI connector.
#[derive(Debug, Default)]
pub struct AtspiConnector {
    opts: AtspiOptions,
}

impl AtspiConnector {
    pub fn new(opts: AtspiOptions) -> Self {
        Self { opts }
    }
}

impl Connector for AtspiConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Atspi
    }

    fn run(self: Box<Self>, ctx: ConnectorCtx) -> BoxFuture<'static, Result<(), ConnectorError>> {
        Box::pin(async move { run(self.opts, ctx).await })
    }
}

fn startup(e: &SessionError) -> ConnectorError {
    ConnectorError::Startup(e.to_string())
}

/// The address to use for the a11y bus, and the session connection if it was needed or used for
/// enablement.
async fn resolve_bus(
    opts: &AtspiOptions,
    clock: &Clock,
) -> Result<(String, Option<Connection>), ConnectorError> {
    let explicit = opts
        .a11y_address
        .clone()
        .or_else(|| std::env::var("GORCA_A11Y_BUS_ADDRESS").ok())
        .or_else(|| std::env::var("AT_SPI_BUS_ADDRESS").ok())
        .filter(|a| !a.is_empty());
    let deadline = Deadline::for_class(clock, BudgetClass::Startup);
    let session = connect_session(opts.session_address.as_deref(), deadline).await;
    match (explicit, session) {
        (Some(addr), Ok(s)) => Ok((addr, Some(s))),
        (Some(addr), Err(e)) => {
            tracing::warn!(error = %e, "no session bus; skipping accessibility enablement");
            Ok((addr, None))
        }
        (None, Ok(s)) => {
            let deadline = Deadline::for_class(clock, BudgetClass::Startup);
            let addr = a11y_bus_address(&s, deadline)
                .await
                .map_err(|e| startup(&e))?;
            Ok((addr, Some(s)))
        }
        (None, Err(e)) => Err(startup(&e)),
    }
}

/// `GetNameOwner` on the a11y bus (best effort).
async fn name_owner(conn: &Connection, clock: &Clock, name: &str) -> Option<String> {
    let deadline = Deadline::for_class(clock, BudgetClass::Interactive);
    let msg = call_method(
        conn,
        deadline,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        &name,
    )
    .await
    .ok()?;
    msg.body().deserialize::<String>().ok()
}

async fn start_drain(address: &str, router: &Arc<AtspiRouter>) -> Result<DrainHandle, String> {
    let cfg = DrainConfig::new(address);
    let router: Arc<dyn Router> = Arc::clone(router) as Arc<dyn Router>;
    // `spawn_drain` waits for the match rules to be installed; keep that off the workers.
    let joined = tokio::task::spawn_blocking(move || spawn_drain(cfg, router)).await;
    match joined {
        Ok(Ok(h)) => Ok(h),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(format!("drain start task failed: {e}")),
    }
}

async fn stop_drain(handle: DrainHandle) {
    // Joins the drain thread; bounded by the socket shutdown the handle performs.
    let stop = tokio::task::spawn_blocking(move || handle.stop());
    if tokio::time::timeout(Duration::from_secs(2), stop)
        .await
        .is_err()
    {
        tracing::warn!("drain did not stop within 2 s; detaching it");
    }
}

async fn run(opts: AtspiOptions, ctx: ConnectorCtx) -> Result<(), ConnectorError> {
    let ConnectorCtx {
        registry,
        shutdown,
        clock,
        ..
    } = ctx;
    let (address, session) = resolve_bus(&opts, &clock).await?;
    tracing::info!(%address, "connecting to the accessibility bus");

    // ID-5.
    let mut enablement: Option<(Connection, Enablement)> = None;
    if opts.enable_accessibility
        && let Some(s) = session
    {
        let deadline = Deadline::for_class(&clock, BudgetClass::Startup);
        match read_status(&s, deadline).await {
            Ok(prev) => {
                let plan = Enablement::new(prev);
                if !plan.startup_is_noop() {
                    let deadline = Deadline::for_class(&clock, BudgetClass::Startup);
                    if let Err(e) = write_status(&s, deadline, prev, plan.on_startup()).await {
                        tracing::warn!(error = %e, "could not enable accessibility");
                    }
                }
                enablement = Some((s, plan));
            }
            Err(e) => tracing::warn!(error = %e, "could not read org.a11y.Status"),
        }
    }

    let deadline = Deadline::for_class(&clock, BudgetClass::Startup);
    let conn = connect(&address, deadline).await.map_err(|e| startup(&e))?;

    if opts.register_events {
        match register_events(&conn, &clock, REGISTERED_EVENTS).await {
            Ok(n) => tracing::debug!(registered = n, "event listeners registered"),
            // Not fatal: always-on events (state changes) still arrive.
            Err(e) => tracing::warn!(error = %e, "RegisterEvent failed"),
        }
    }

    let mut tasks = TaskSet::new(shutdown.child_token());
    let paths = Arc::new(PathTable::new());
    let (new_tx, mut new_rx) = mpsc::channel::<NewApp>(NEW_APP_QUEUE);
    let router = Arc::new(AtspiRouter::new(
        tokio::runtime::Handle::current(),
        registry.clone(),
        clock.clone(),
        Arc::clone(&paths),
        new_tx,
    ));
    if let Some(owner) = name_owner(&conn, &clock, "org.a11y.atspi.Registry").await {
        router.ignore(&owner);
    }
    if let Some(me) = conn.unique_name() {
        router.ignore(me.as_str());
    }

    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")
        .and_then(|b| b.interface("org.freedesktop.DBus"))
        .and_then(|b| b.member("NameOwnerChanged"))
        .map(zbus::match_rule::Builder::build)
        .map_err(|e| ConnectorError::Startup(e.to_string()))?;
    let deadline = Deadline::for_class(&clock, BudgetClass::Startup);
    let mut owner_changes = gorca_dbus::with_deadline(
        deadline,
        zbus::MessageStream::for_match_rule(rule, &conn, Some(256)),
    )
    .await
    .map_err(|e| ConnectorError::Startup(e.to_string()))?;

    let mut drain = Some(
        start_drain(&address, &router)
            .await
            .map_err(ConnectorError::Startup)?,
    );
    let mut backoff = DRAIN_BACKOFF_MIN;
    let mut health = tokio::time::interval(DRAIN_POLL);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!("atspi connector running");

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            Some(new) = new_rx.recv() => {
                spawn_app(&mut tasks, new, &conn, &paths, &clock, &shutdown);
            }
            Some(msg) = owner_changes.next() => {
                let Ok(msg) = msg else { continue };
                let Ok((name, _old, new)) = msg.body().deserialize::<(String, String, String)>() else {
                    continue;
                };
                if new.is_empty()
                    && name.starts_with(':')
                    && let Some(app) = router.forget(&name)
                {
                    tracing::debug!(%name, ?app, "application left the bus");
                    let _ = registry.retire(app);
                }
            }
            _ = health.tick() => {
                let dead = match &drain {
                    Some(d) => match d.end() {
                        DrainEnd::Running | DrainEnd::Stopped => false,
                        DrainEnd::Disconnected(why) => {
                            tracing::warn!(%why, "event drain lost; restarting");
                            true
                        }
                    },
                    None => true,
                };
                if dead {
                    if let Some(old) = drain.take() { stop_drain(old).await; }
                    match start_drain(&address, &router).await {
                        Ok(h) => { drain = Some(h); backoff = DRAIN_BACKOFF_MIN; }
                        Err(e) => {
                            tracing::warn!(error = %e, ?backoff, "drain restart failed");
                            tokio::select! {
                                () = shutdown.cancelled() => break,
                                () = tokio::time::sleep(backoff) => {}
                            }
                            backoff = (backoff * 2).min(DRAIN_BACKOFF_MAX);
                        }
                    }
                }
            }
        }
    }

    tracing::info!("atspi connector stopping");
    if let Some(d) = drain.take() {
        stop_drain(d).await;
    }
    tasks.shutdown(Duration::from_secs(1)).await;
    for app in router.take_all() {
        let _ = registry.retire(app);
    }
    if let Some((s, plan)) = enablement {
        let deadline = Deadline::after(&clock, Duration::from_secs(1));
        if let Err(e) = write_status(&s, deadline, plan.on_startup(), plan.on_shutdown()).await {
            tracing::warn!(error = %e, "could not restore org.a11y.Status");
        }
    }
    Ok(())
}

fn spawn_app(
    tasks: &mut TaskSet,
    new: NewApp,
    conn: &Connection,
    paths: &Arc<PathTable>,
    clock: &Clock,
    shutdown: &CancellationToken,
) {
    let NewApp {
        port,
        bus_name,
        gone,
    } = new;
    let server = AppServer {
        caller: Caller::new(conn.clone(), bus_name),
        paths: Arc::clone(paths),
        clock: clock.clone(),
        gone,
        shutdown: shutdown.child_token(),
    };
    tasks.spawn("atspi-app", server.run(port));
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_connector_api::registrar_channel;
    use gorca_model::ConnectorId;
    use std::time::Duration;

    #[tokio::test]
    async fn run_fails_fast_when_the_bus_is_missing() {
        let (registry, _rx) = registrar_channel(ConnectorId(1), 4);
        let shutdown = CancellationToken::new();
        let ctx = ConnectorCtx {
            id: ConnectorId(1),
            registry,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        };
        let c = Box::new(AtspiConnector::new(AtspiOptions {
            a11y_address: Some("unix:path=/nonexistent/gorca-a11y".to_owned()),
            session_address: Some("unix:path=/nonexistent/gorca-session".to_owned()),
            ..AtspiOptions::default()
        }));
        assert_eq!(c.kind(), ConnectorKind::Atspi);
        let r = tokio::time::timeout(Duration::from_secs(10), c.run(ctx)).await;
        assert!(matches!(r, Ok(Err(ConnectorError::Startup(_)))), "{r:?}");
    }
}
