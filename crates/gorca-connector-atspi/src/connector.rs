//! The AT-SPI connector (ADR-0001 section 5.3).
//!
//! Topology, all owned by [`AtspiConnector::run`]:
//!
//! ```text
//! a11y bus --- event connection (raw drain thread, ADR-0004) --> per-app ingress (core)
//!              supervised by its own task, restarted with backoff
//! a11y bus --- call connection (zbus) --> one AppServer task per application (requests),
//!                                         credentials lookups, RegisterEvent
//! a11y bus --- name connection (zbus) --> NameOwnerChanged only, so a flood of name changes
//!                                         can never stall the replies of the call connection
//! session bus --- org.a11y.Status (ID-5 enablement)
//! ```
//!
//! Every D-Bus call has a deadline. The connector reads no environment: addresses arrive fully
//! resolved in [`AtspiOptions`].

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use gorca_connector_api::{Connector, ConnectorCtx, ConnectorError};
use gorca_dbus::call_method;
use gorca_dbus::drain::{DrainConfig, DrainEnd, DrainHandle, Router, spawn_drain};
use gorca_model::ConnectorKind;
use gorca_rt::{
    BoxFuture, BudgetClass, CancellationToken, Clock, Deadline, FocusedApp, TaskSet, mpsc,
};
use tokio::task::JoinSet;
use zbus::Connection;
use zbus::zvariant::OwnedValue;

use crate::enablement::Enablement;
use crate::fetch::{AppServer, Caller};
use crate::paths::PathTable;
use crate::router::{AtspiRouter, Credentials, NewApp};
use crate::session::{
    REGISTERED_EVENTS, SessionError, a11y_bus_address, connect, read_status, register_events,
    write_status,
};

/// Applications announced but not yet picked up by the connector task.
const NEW_APP_QUEUE: usize = 1024;
/// Registrations the drain asked for, waiting for the connector task.
const REGISTER_QUEUE: usize = 256;
/// How often the connector checks that the drain is alive.
const DRAIN_POLL: Duration = Duration::from_millis(500);
const DRAIN_BACKOFF_MIN: Duration = Duration::from_millis(100);
const DRAIN_BACKOFF_MAX: Duration = Duration::from_secs(5);
/// How often the shell's owner is looked up (it may start after gorca).
const SHELL_POLL: Duration = Duration::from_secs(5);
/// Deadline of one `GetConnectionCredentials`.
const CREDENTIALS_DEADLINE: Duration = Duration::from_millis(250);

/// Fully resolved connection parameters. The connector never looks at the environment (only the
/// daemon's `main` does), so a test cannot reach the real desktop by leaving a field out.
#[derive(Clone, Debug)]
pub struct AtspiOptions {
    /// Address of the a11y bus. `None`: ask `org.a11y.Bus.GetAddress` on `session_address`.
    pub a11y_address: Option<String>,
    /// Address of the session bus (enablement, and discovery when `a11y_address` is `None`).
    /// `None`: no enablement, and `a11y_address` is required.
    pub session_address: Option<String>,
    /// ID-5: set `IsEnabled` and `ScreenReaderEnabled` at startup and restore them at shutdown.
    pub enable_accessibility: bool,
    /// `Registry.RegisterEvent` for [`REGISTERED_EVENTS`].
    pub register_events: bool,
    /// The focused application, published by the Reader. The router never evicts or refuses it.
    pub focused: Option<Arc<FocusedApp>>,
}

impl Default for AtspiOptions {
    fn default() -> Self {
        Self {
            a11y_address: None,
            session_address: None,
            enable_accessibility: true,
            register_events: true,
            focused: None,
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

/// The a11y bus address, and the session connection if there is one.
async fn resolve_bus(
    opts: &AtspiOptions,
    clock: &Clock,
) -> Result<(String, Option<Connection>), ConnectorError> {
    let deadline = Deadline::for_class(clock, BudgetClass::Startup);
    let session = match &opts.session_address {
        Some(a) => Some(connect(a, deadline).await),
        None => None,
    };
    match (opts.a11y_address.clone().filter(|a| !a.is_empty()), session) {
        (Some(addr), Some(Ok(s))) => Ok((addr, Some(s))),
        (Some(addr), Some(Err(e))) => {
            tracing::warn!(error = %e, "no session bus; skipping accessibility enablement");
            Ok((addr, None))
        }
        (Some(addr), None) => Ok((addr, None)),
        (None, Some(Ok(s))) => {
            let deadline = Deadline::for_class(clock, BudgetClass::Startup);
            let addr = a11y_bus_address(&s, deadline)
                .await
                .map_err(|e| startup(&e))?;
            Ok((addr, Some(s)))
        }
        (None, Some(Err(e))) => Err(startup(&e)),
        (None, None) => Err(ConnectorError::Startup(
            "neither an a11y bus address nor a session bus address was given".to_owned(),
        )),
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

/// `GetConnectionCredentials` on the a11y bus: who owns a connection.
struct BusCredentials {
    conn: Connection,
    clock: Clock,
}

impl Credentials for BusCredentials {
    fn pid_of<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<u32>> {
        Box::pin(async move {
            let deadline = Deadline::after(&self.clock, CREDENTIALS_DEADLINE);
            let msg = call_method(
                &self.conn,
                deadline,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetConnectionCredentials",
                &name,
            )
            .await
            .ok()?;
            let creds: HashMap<String, OwnedValue> = msg.body().deserialize().ok()?;
            let pid = creds.get("ProcessID")?;
            u32::try_from(pid.clone()).ok()
        })
    }
}

/// Keeps the GNOME Shell process in the router's protected set. The shell owns
/// `org.gnome.Shell` on the session bus; its a11y connection belongs to the same process.
async fn watch_shell(session: Connection, router: Arc<AtspiRouter>, clock: Clock) {
    let creds = BusCredentials {
        conn: session.clone(),
        clock: clock.clone(),
    };
    let mut known: Option<u32> = None;
    loop {
        if let Some(owner) = name_owner(&session, &clock, "org.gnome.Shell").await
            && let Some(pid) = creds.pid_of(&owner).await
            && known != Some(pid)
        {
            tracing::debug!(pid, "the shell is protected from eviction");
            router.protect_pid(pid);
            known = Some(pid);
        }
        tokio::time::sleep(SHELL_POLL).await;
    }
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

/// Keeps one drain alive: restarts it with backoff when the broker closed it. Runs in its own
/// task so that a slow stop, start or backoff never delays the connector's event loop. Stops the
/// drain itself on cancellation (it is awaited, never dropped mid-flight).
async fn supervise_drain(address: String, router: Arc<AtspiRouter>, cancel: CancellationToken) {
    let mut backoff = DRAIN_BACKOFF_MIN;
    let mut drain: Option<DrainHandle> = None;
    let mut health = tokio::time::interval(DRAIN_POLL);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if drain.is_none() {
            match start_drain(&address, &router).await {
                Ok(h) => {
                    drain = Some(h);
                    backoff = DRAIN_BACKOFF_MIN;
                }
                Err(e) => {
                    tracing::warn!(error = %e, ?backoff, "drain start failed");
                    tokio::select! {
                        () = cancel.cancelled() => break,
                        () = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(DRAIN_BACKOFF_MAX);
                    continue;
                }
            }
        }
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = health.tick() => {}
        }
        let lost = match drain.as_ref().map(DrainHandle::end) {
            Some(DrainEnd::Disconnected(why)) => Some(why),
            Some(DrainEnd::Running | DrainEnd::Stopped) | None => None,
        };
        if let Some(why) = lost {
            tracing::warn!(%why, "event drain lost; restarting");
            if let Some(old) = drain.take() {
                stop_drain(old).await;
            }
        }
    }
    if let Some(d) = drain.take() {
        stop_drain(d).await;
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
    tracing::info!("connecting to the accessibility bus");

    // The shell is identified through the session bus: its well-known name lives there.
    let shell_conn = session.clone();

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
    // A connection of its own for NameOwnerChanged: zbus stops delivering to a connection whose
    // subscription queue is full, and that must never be the connection that carries replies.
    let deadline = Deadline::for_class(&clock, BudgetClass::Startup);
    let name_conn = connect(&address, deadline).await.map_err(|e| startup(&e))?;

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
    let (reg_tx, mut reg_rx) = mpsc::channel::<Arc<str>>(REGISTER_QUEUE);
    let credentials = Arc::new(BusCredentials {
        conn: conn.clone(),
        clock: clock.clone(),
    });
    let mut router = AtspiRouter::new(
        registry.clone(),
        clock.clone(),
        Arc::clone(&paths),
        new_tx,
        reg_tx,
        credentials,
    );
    if let Some(f) = &opts.focused {
        router = router.with_focused(Arc::clone(f));
    }
    let router = Arc::new(router);
    if let Some(sc) = shell_conn {
        tasks.spawn(
            "shell-watch",
            watch_shell(sc, Arc::clone(&router), clock.clone()),
        );
    }
    if let Some(owner) = name_owner(&conn, &clock, "org.a11y.atspi.Registry").await {
        router.ignore(&owner);
    }
    for c in [&conn, &name_conn] {
        if let Some(me) = c.unique_name() {
            router.ignore(me.as_str());
        }
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
        zbus::MessageStream::for_match_rule(rule, &name_conn, Some(4096)),
    )
    .await
    .map_err(|e| ConnectorError::Startup(e.to_string()))?;

    let drain_cancel = CancellationToken::new();
    let drain_task = tokio::spawn(supervise_drain(
        address.clone(),
        Arc::clone(&router),
        drain_cancel.clone(),
    ));
    let mut registrations: JoinSet<()> = JoinSet::new();
    tracing::info!("atspi connector running");

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            Some(new) = new_rx.recv() => {
                spawn_app(&mut tasks, new, &conn, &paths, &clock, &shutdown);
            }
            Some(name) = reg_rx.recv() => {
                // The drain asked for this sender to be registered; it never waits for it.
                let router = Arc::clone(&router);
                registrations.spawn(async move { router.complete(name).await });
            }
            Some(_) = registrations.join_next() => {}
            Some(msg) = owner_changes.next() => {
                let Ok(msg) = msg else { continue };
                let Ok((name, _old, new)) = msg.body().deserialize::<(String, String, String)>() else {
                    continue;
                };
                if new.is_empty() && name.starts_with(':') {
                    // Also marks a name that is still being registered as departed.
                    if let Some(app) = router.forget(&name) {
                        tracing::debug!(%name, ?app, "application left the bus");
                        let _ = registry.retire(app);
                    }
                }
            }
        }
    }

    tracing::info!("atspi connector stopping");
    drain_cancel.cancel();
    if tokio::time::timeout(Duration::from_secs(4), drain_task)
        .await
        .is_err()
    {
        tracing::warn!("drain supervisor did not stop in time");
    }
    registrations.abort_all();
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

    fn ctx(shutdown: &CancellationToken) -> ConnectorCtx {
        let (registry, _rx) = registrar_channel(ConnectorId(1), 4);
        ConnectorCtx {
            id: ConnectorId(1),
            registry,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        }
    }

    #[tokio::test]
    async fn run_fails_fast_when_the_bus_is_missing() {
        let shutdown = CancellationToken::new();
        let c = Box::new(AtspiConnector::new(AtspiOptions {
            a11y_address: Some("unix:path=/nonexistent/gorca-a11y".to_owned()),
            session_address: Some("unix:path=/nonexistent/gorca-session".to_owned()),
            ..AtspiOptions::default()
        }));
        assert_eq!(c.kind(), ConnectorKind::Atspi);
        let r = tokio::time::timeout(Duration::from_secs(10), c.run(ctx(&shutdown))).await;
        assert!(matches!(r, Ok(Err(ConnectorError::Startup(_)))), "{r:?}");
    }

    /// The connector reads no environment: with nothing given it fails, it does not look around.
    #[tokio::test]
    async fn missing_addresses_are_an_error_not_an_environment_lookup() {
        let shutdown = CancellationToken::new();
        let c = Box::new(AtspiConnector::new(AtspiOptions::default()));
        let r = tokio::time::timeout(Duration::from_secs(5), c.run(ctx(&shutdown))).await;
        assert!(matches!(r, Ok(Err(ConnectorError::Startup(_)))), "{r:?}");
    }
}
