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
use gorca_model::{AppId, ConnectorKind, NodeKey};
use gorca_rt::{
    BoxFuture, BudgetClass, CancellationToken, Clock, Deadline, FocusedApp, TaskSet, mpsc,
};
use tokio::task::JoinSet;
use zbus::Connection;
use zbus::zvariant::OwnedValue;

use crate::enablement::Enablement;
use crate::fetch::FetchCounters;
use crate::fetch::{AppServer, Caller};
use crate::paths::PathTable;
use crate::router::{AtspiRouter, Credentials, NewApp};
use crate::session::{
    REGISTERED_EVENTS, SessionError, a11y_bus_address, connect, read_status, register_events,
    write_status,
};
use crate::subscription::extra_subscription;

/// Applications announced but not yet picked up by the connector task.
const NEW_APP_QUEUE: usize = 1024;
/// Registrations the drain asked for, waiting for the connector task.
const REGISTER_QUEUE: usize = 256;
/// How often the connector checks that the drain is alive.
const DRAIN_POLL: Duration = Duration::from_millis(500);
const DRAIN_BACKOFF_MIN: Duration = Duration::from_millis(100);
const DRAIN_BACKOFF_MAX: Duration = Duration::from_secs(5);
/// Safety-net interval for re-resolving the shell's owner (changes also arrive as signals).
const SHELL_POLL: Duration = Duration::from_secs(5);
/// How often registered applications are reconciled with the bus.
const RECONCILE_EVERY: Duration = Duration::from_secs(5);
/// Failed reconciliations in a row after which all protection is dropped (fail closed).
const RECONCILE_FAILURES: u32 = 1;
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
    /// ID-5: set `IsEnabled` at startup and restore it at shutdown. `ScreenReaderEnabled` is
    /// never written: it flips the GSettings key that makes GNOME autostart Orca.
    pub enable_accessibility: bool,
    /// `Registry.RegisterEvent` for [`REGISTERED_EVENTS`].
    pub register_events: bool,
    /// The focused application, published by the Reader. The router never evicts or refuses it.
    pub focused: Option<Arc<FocusedApp>>,
    /// Shadow mode (B-1 PW-11): read-only beside the user's real screen reader. Forces
    /// `enable_accessibility` and `register_events` off and refuses every call outside the
    /// allowlist of `fetch::shadow`.
    pub shadow: bool,
}

impl Default for AtspiOptions {
    fn default() -> Self {
        Self {
            a11y_address: None,
            session_address: None,
            enable_accessibility: true,
            register_events: true,
            focused: None,
            shadow: false,
        }
    }
}

/// Bus names of the applications currently served, for diagnostics (the presentation log).
#[derive(Debug, Default)]
struct BusNames {
    map: std::sync::Mutex<HashMap<AppId, Arc<str>>>,
}

impl BusNames {
    fn set(&self, app: AppId, name: Arc<str>) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(app, name);
    }

    fn remove(&self, app: AppId) {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&app);
    }

    fn app_of(&self, bus: &str) -> Option<AppId> {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|(_, name)| &***name == bus)
            .map(|(app, _)| *app)
    }

    fn get(&self, app: AppId) -> Option<Arc<str>> {
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&app)
            .cloned()
    }
}

/// Resolves a `NodeKey` to `(bus name, object path)` from the path table. A diagnostics aid
/// (the presentation log, B-1 PW-10): the core never sees paths and nothing in the hot path
/// calls this.
#[derive(Clone, Debug)]
pub struct AtspiLabels {
    paths: Arc<PathTable>,
    buses: Arc<BusNames>,
}

impl AtspiLabels {
    /// The key of `path` in the application with unique name `bus`, if that application is
    /// currently served. A diagnostics aid (tests, the debug probe).
    pub fn key_of(&self, bus: &str, path: &str) -> Option<NodeKey> {
        let app = self.buses.app_of(bus)?;
        Some(NodeKey {
            app,
            local: gorca_dbus::drain::local_id(path),
        })
    }

    pub fn lookup(&self, key: NodeKey) -> Option<(Arc<str>, Arc<str>)> {
        Some((self.buses.get(key.app)?, self.paths.get(key)?))
    }

    /// The unique bus name of an application currently served.
    pub fn bus_of(&self, app: AppId) -> Option<Arc<str>> {
        self.buses.get(app)
    }

    /// The object path of a node the connector has recorded.
    pub fn path_of(&self, key: NodeKey) -> Option<Arc<str>> {
        self.paths.get(key)
    }
}

/// State that outlives one run of the connector (the daemon restarts it with backoff): the path
/// table, bus names and counters. Cloning shares it.
#[derive(Clone, Debug, Default)]
pub struct AtspiState {
    paths: Arc<PathTable>,
    buses: Arc<BusNames>,
    counters: Arc<FetchCounters>,
    shadow_refused: Arc<std::sync::atomic::AtomicU64>,
    /// The event drain is connected and its match rules are installed.
    drain_ready: Arc<std::sync::atomic::AtomicBool>,
    /// The a11y bus address the running connector resolved (browse mode's document sources
    /// open their own connection to it).
    a11y_address: Arc<std::sync::Mutex<Option<String>>>,
}

impl AtspiState {
    /// The a11y bus address, once the connector has resolved it.
    pub fn a11y_address(&self) -> Option<String> {
        self.a11y_address
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Events are being received (the drain installed its match rules). Tests wait for it before
    /// starting applications, because events sent earlier are lost.
    pub fn drain_ready(&self) -> bool {
        self.drain_ready.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Calls shadow mode refused (should stay zero: the connector only reads).
    pub fn shadow_refused(&self) -> u64 {
        self.shadow_refused
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Counters of refs and dumps the connector refused to believe.
    pub fn fetch_counters(&self) -> Arc<FetchCounters> {
        Arc::clone(&self.counters)
    }

    /// The `NodeKey` to `(bus, path)` resolver for logs.
    pub fn labels(&self) -> AtspiLabels {
        AtspiLabels {
            paths: Arc::clone(&self.paths),
            buses: Arc::clone(&self.buses),
        }
    }

    /// The reverse path table (tests and tools).
    pub fn path_table(&self) -> Arc<PathTable> {
        Arc::clone(&self.paths)
    }
}

/// The AT-SPI connector.
#[derive(Debug, Default)]
pub struct AtspiConnector {
    opts: AtspiOptions,
    shared: AtspiState,
}

impl AtspiConnector {
    pub fn new(opts: AtspiOptions) -> Self {
        Self {
            opts,
            shared: AtspiState::default(),
        }
    }

    /// A connector whose paths, bus names and counters live in `state`, so they survive a
    /// restart of the connector.
    pub fn with_state(opts: AtspiOptions, state: AtspiState) -> Self {
        Self {
            opts,
            shared: state,
        }
    }

    pub fn state(&self) -> AtspiState {
        self.shared.clone()
    }
}

impl Connector for AtspiConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Atspi
    }

    fn run(self: Box<Self>, ctx: ConnectorCtx) -> BoxFuture<'static, Result<(), ConnectorError>> {
        Box::pin(async move { run(self.opts, self.shared, ctx).await })
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

/// The `(pid, process start time)` of the current owner of `org.gnome.Shell`, if it can be told.
async fn resolve_shell(
    session: &Connection,
    creds: &BusCredentials,
    clock: &Clock,
) -> Option<(u32, u64)> {
    let owner = name_owner(session, clock, "org.gnome.Shell").await?;
    let pid = creds.pid_of(&owner).await?;
    crate::proc::read_proc_info_async(pid)
        .await
        .start
        .map(|st| (pid, st))
}

/// Keeps the router's one protected process equal to the **current** owner of
/// `org.gnome.Shell` on the session bus, as `(pid, process start time)`. The owner is re-resolved
/// whenever the name changes hands (`NameOwnerChanged`, with a slow poll as a safety net), and the
/// protection replaces the previous value, so a recycled pid or a name handover never leaves
/// someone else protected: the start time of an impostor differs.
async fn watch_shell(session: Connection, router: Arc<AtspiRouter>, clock: Clock) {
    let creds = BusCredentials {
        conn: session.clone(),
        clock: clock.clone(),
    };
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")
        .and_then(|b| b.interface("org.freedesktop.DBus"))
        .and_then(|b| b.member("NameOwnerChanged"))
        .and_then(|b| b.arg(0, "org.gnome.Shell"))
        .map(zbus::match_rule::Builder::build);
    let deadline = Deadline::for_class(&clock, BudgetClass::Startup);
    let mut stream = match rule {
        Ok(rule) => gorca_dbus::with_deadline(
            deadline,
            zbus::MessageStream::for_match_rule(rule, &session, Some(16)),
        )
        .await
        .ok(),
        Err(_) => None,
    };
    let mut protected: Option<(u32, u64)> = None;
    loop {
        let process = resolve_shell(&session, &creds, &clock).await;
        if process != protected {
            tracing::debug!(?process, "the shell process changed");
            protected = process;
        }
        // Only a definite answer replaces the protected process. A failed lookup keeps the last
        // validated identity (`(pid, start time)` cannot match an impostor), it never clears it:
        // clearing would make the shell evictable exactly when nobody can check.
        if let Some(p) = process {
            router.set_protected_process(Some(p));
        }
        let changed = async {
            match stream.as_mut() {
                Some(s) => s.next().await.is_some(),
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = changed => {}
            () = tokio::time::sleep(SHELL_POLL) => {}
        }
    }
}

/// `ListNames` on the a11y bus.
async fn live_names(conn: &Connection, clock: &Clock) -> Option<std::collections::HashSet<String>> {
    let deadline = Deadline::for_class(clock, BudgetClass::Prefetch);
    let msg = call_method(
        conn,
        deadline,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "ListNames",
        &(),
    )
    .await
    .ok()?;
    let names: Vec<String> = msg.body().deserialize().ok()?;
    Some(names.into_iter().collect())
}

const REGISTRY_NAME: &str = "org.a11y.atspi.Registry";
const REGISTRY_ROOT: &str = "/org/a11y/atspi/accessible/root";

/// Ask the registry for the applications it knows and start registering every one that is not
/// yet routed, so the reader knows an application when it is announced, not when it first
/// speaks. Cheap when nothing is new.
async fn discover_apps(conn: &Connection, router: &AtspiRouter, clock: &Clock) {
    let deadline = Deadline::for_class(clock, BudgetClass::Prefetch);
    let Ok(msg) = call_method(
        conn,
        deadline,
        REGISTRY_NAME,
        REGISTRY_ROOT,
        "org.a11y.atspi.Accessible",
        "GetChildren",
        &(),
    )
    .await
    else {
        return;
    };
    let Ok(children) = msg
        .body()
        .deserialize::<Vec<(String, zbus::zvariant::OwnedObjectPath)>>()
    else {
        return;
    };
    for (name, _) in children {
        if name.starts_with(':') {
            router.discover(&name);
        }
    }
}

/// Periodic reconciliation of identity state with the bus. A missed `NameOwnerChanged` must not
/// leave a route, a quota or a protection behind for a name that is gone; and when the bus cannot
/// be asked, eviction is frozen (the last validated protection stays, nobody is evicted, and
/// applications are still read).
async fn reconcile_loop(conn: Connection, router: Arc<AtspiRouter>, clock: Clock) {
    let mut failures = 0u32;
    loop {
        tokio::time::sleep(RECONCILE_EVERY).await;
        // A missed announcement is picked up here.
        discover_apps(&conn, &router, &clock).await;
        if let Some(live) = live_names(&conn, &clock).await {
            failures = 0;
            let gone = router.reconcile_with(&live);
            // The bus answered: protection is verified again, eviction may resume.
            router.resume_eviction();
            if !gone.is_empty() {
                tracing::warn!(
                    count = gone.len(),
                    "reconciliation retired vanished applications"
                );
            }
        } else {
            failures += 1;
            if failures >= RECONCILE_FAILURES {
                tracing::warn!("cannot reconcile with the bus; eviction is frozen");
                router.freeze_eviction();
            }
        }
    }
}

async fn start_drain(address: &str, router: &Arc<AtspiRouter>) -> Result<DrainHandle, String> {
    let mut cfg = DrainConfig::new(address);
    cfg.match_rules = extra_subscription().match_rules();
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
async fn supervise_drain(
    address: String,
    router: Arc<AtspiRouter>,
    cancel: CancellationToken,
    ready: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut backoff = DRAIN_BACKOFF_MIN;
    let mut drain: Option<DrainHandle> = None;
    let mut health = tokio::time::interval(DRAIN_POLL);
    health.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if drain.is_none() {
            match start_drain(&address, &router).await {
                Ok(h) => {
                    drain = Some(h);
                    ready.store(true, std::sync::atomic::Ordering::Release);
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
            ready.store(false, std::sync::atomic::Ordering::Release);
            if let Some(old) = drain.take() {
                stop_drain(old).await;
            }
        }
    }
    if let Some(d) = drain.take() {
        stop_drain(d).await;
    }
}

async fn run(
    opts: AtspiOptions,
    shared: AtspiState,
    ctx: ConnectorCtx,
) -> Result<(), ConnectorError> {
    let ConnectorCtx {
        registry,
        shutdown,
        clock,
        ..
    } = ctx;
    let (address, session) = resolve_bus(&opts, &clock).await?;
    *shared
        .a11y_address
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(address.clone());
    tracing::info!("connecting to the accessibility bus");

    // The shell is identified through the session bus: its well-known name lives there.
    let shell_conn = session.clone();

    // ID-5.
    let mut enablement: Option<(Connection, Enablement)> = None;
    if opts.enable_accessibility
        && !opts.shadow
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

    if opts.register_events && !opts.shadow {
        match register_events(&conn, &clock, REGISTERED_EVENTS).await {
            Ok(n) => tracing::debug!(registered = n, "event listeners registered"),
            // Not fatal: always-on events (state changes) still arrive.
            Err(e) => tracing::warn!(error = %e, "RegisterEvent failed"),
        }
    }

    let mut tasks = TaskSet::new(shutdown.child_token());
    let paths = Arc::clone(&shared.paths);
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
    tasks.spawn(
        "reconcile",
        reconcile_loop(conn.clone(), Arc::clone(&router), clock.clone()),
    );
    if let Some(sc) = shell_conn {
        // Known before the first application is announced: the Shell's identity is decided when
        // it registers, so a Shell found by the first discovery must already be recognisable
        // (a race here left it unverified, and without its extension, under load).
        let creds = BusCredentials {
            conn: sc.clone(),
            clock: clock.clone(),
        };
        if let Some(p) = resolve_shell(&sc, &creds, &clock).await {
            router.set_protected_process(Some(p));
        }
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

    // The registry announces an application by a `ChildrenChanged` on its root when it embeds.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .and_then(|b| b.member("ChildrenChanged"))
        .and_then(|b| b.path(REGISTRY_ROOT))
        .map(zbus::match_rule::Builder::build)
        .map_err(|e| ConnectorError::Startup(e.to_string()))?;
    let deadline = Deadline::for_class(&clock, BudgetClass::Startup);
    let mut announcements = gorca_dbus::with_deadline(
        deadline,
        zbus::MessageStream::for_match_rule(rule, &name_conn, Some(256)),
    )
    .await
    .map_err(|e| ConnectorError::Startup(e.to_string()))?;
    // Applications that were already running.
    discover_apps(&conn, &router, &clock).await;

    let drain_cancel = CancellationToken::new();
    let drain_task = tokio::spawn(supervise_drain(
        address.clone(),
        Arc::clone(&router),
        drain_cancel.clone(),
        Arc::clone(&shared.drain_ready),
    ));
    let mut registrations: JoinSet<()> = JoinSet::new();
    tracing::info!("atspi connector running");

    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            Some(new) = new_rx.recv() => {
                spawn_app(&mut tasks, new, &conn, &shared, opts.shadow, &clock, &shutdown);
            }
            Some(name) = reg_rx.recv() => {
                // The drain asked for this sender to be registered; it never waits for it.
                let router = Arc::clone(&router);
                registrations.spawn(async move { router.complete(name).await });
            }
            Some(_) = registrations.join_next() => {}
            Some(_) = announcements.next() => {
                // Something embedded or left: ask who is there, off the loop (the call has its
                // own deadline, and the loop must keep serving the other tasks meanwhile).
                let (conn, router, clock) = (conn.clone(), Arc::clone(&router), clock.clone());
                registrations.spawn(async move { discover_apps(&conn, &router, &clock).await });
            }
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
    // First, before the slow teardown below can run into the daemon's shutdown grace: the one
    // thing the user can see is `IsEnabled` left on after gorca is gone.
    if let Some((s, plan)) = enablement {
        let deadline = Deadline::after(&clock, Duration::from_secs(1));
        match read_status(&s, deadline).await {
            Ok(now) => {
                let deadline = Deadline::after(&clock, Duration::from_secs(1));
                if let Err(e) = write_status(&s, deadline, now, plan.on_shutdown(now)).await {
                    tracing::warn!(error = %e, "could not restore org.a11y.Status");
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not read org.a11y.Status to restore it"),
        }
    }
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
    Ok(())
}

fn spawn_app(
    tasks: &mut TaskSet,
    new: NewApp,
    conn: &Connection,
    shared: &AtspiState,
    shadow: bool,
    clock: &Clock,
    shutdown: &CancellationToken,
) {
    let NewApp {
        port,
        bus_name,
        gone,
        verified_shell,
        identity,
        shell_rx,
        hints,
    } = new;
    let caller = Caller::new(conn.clone(), Arc::clone(&bus_name));
    let caller = if shadow {
        caller.shadowed(Arc::clone(&shared.shadow_refused))
    } else {
        caller
    };
    let app = port.app;
    shared.buses.set(app, Arc::clone(&bus_name));
    let server = AppServer {
        caller,
        paths: Arc::clone(&shared.paths),
        clock: clock.clone(),
        gone,
        verified_shell,
        identity,
        shell_rx: Some(shell_rx),
        hints,
        counters: Arc::clone(&shared.counters),
        shutdown: shutdown.child_token(),
    };
    let buses = Arc::clone(&shared.buses);
    tasks.spawn("atspi-app", async move {
        server.run(port).await;
        buses.remove(app);
    });
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
