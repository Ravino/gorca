//! Per-application request service: turns `BackendRequest`s into AT-SPI calls over the call
//! connection. Every call carries the request's deadline (R2, D-4); the number of requests in
//! flight is bounded per application (C-3), so a hung application costs its own actor only.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use gorca_connector_api::{
    AppPort, BackendError, BackendOp, BackendReply, BackendRequest, PortError,
};
use gorca_dbus::{DbusError, call_method};
use gorca_model::{AppId, AppInfo, AppLifecycle, ConnectorKind, NodeKey, Role, SnapshotBatch};
use gorca_rt::{CancellationToken, Clock, Deadline};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value as Zv};

use crate::decode::{RawNode, clip, snapshot};
use crate::paths::PathTable;
use crate::toolkit::Toolkit;

pub mod items;
pub mod shadow;
mod strategy;

/// Requests served concurrently per application; the rest is answered `Busy` (C-3).
pub const MAX_IN_FLIGHT: usize = 8;
/// Slots only Interactive requests may take: Prefetch and Background never wait an Interactive
/// request out (section 7, MA-8).
pub const INTERACTIVE_RESERVED: usize = 2;
/// Background requests (including `CacheDump`) served at once per application.
pub const BACKGROUND_MAX: usize = 2;
/// Hard bounds of the section 7 table, repeated here because the connector may not depend on
/// `gorca-mirror`.
pub const MAX_ANCESTRY_DEPTH: u8 = 64;
pub const MAX_SUBTREE_NODES: u32 = 4_096;
pub const MAX_CHILD_RANGE: u32 = 1_024;

const NULL_PATH: &str = "/org/a11y/atspi/null";
const CACHE_PATH: &str = "/org/a11y/atspi/cache";
const CACHE: &str = "org.a11y.atspi.Cache";

/// Object path of every application's root (`Registry.GetChildren` entries).
pub const ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const APPLICATION: &str = "org.a11y.atspi.Application";
const VALUE: &str = "org.a11y.atspi.Value";
const TEXT: &str = "org.a11y.atspi.Text";
const PROPS: &str = "org.freedesktop.DBus.Properties";
/// `ATSPI_RELATION_LABELLED_BY`.
const RELATION_LABELLED_BY: u32 = 2;
/// Characters of an entry's text that are fetched for the focus presentation.
const ENTRY_TEXT_CHARS: i32 = 1024;

/// Single-line entries present their text on focus. Password fields are no exception: gorca
/// reads what the application exposes (product decision), and never logs it.
pub fn wants_entry_text(role: Role, states: gorca_model::StateSet) -> bool {
    matches!(role, Role::TextEntry | Role::PasswordEntry)
        && !states.contains(gorca_model::StateSet::MULTILINE)
}

/// A `LabelledBy` target is read only when it lives in the application being presented: the
/// relation is app-supplied, and gorca must not let an application aim its calls (and the focus
/// deadline) at another peer.
pub fn label_may_be_read(target_bus: &str, app_bus: &str) -> bool {
    target_bus == app_bus
}

/// Map a D-Bus failure to the connector error vocabulary (C-2, C-6).
pub fn backend_error(e: &DbusError) -> BackendError {
    match e {
        DbusError::Timeout => BackendError::Timeout,
        DbusError::Zbus(zbus::Error::MethodError(name, detail, _)) => match name.as_str() {
            "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
            | "org.freedesktop.DBus.Error.UnknownObject" => BackendError::Gone,
            "org.freedesktop.DBus.Error.NoReply"
            | "org.freedesktop.DBus.Error.Timeout"
            | "org.freedesktop.DBus.Error.TimedOut" => BackendError::Timeout,
            "org.freedesktop.DBus.Error.UnknownMethod"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.UnknownProperty" => BackendError::NotSupported,
            other => {
                BackendError::Protocol(format!("{other}: {}", detail.as_deref().unwrap_or("")))
            }
        },
        DbusError::Zbus(other) => BackendError::Protocol(other.to_string()),
    }
}

/// `(so)` as a variant: `Some((bus, path))`, `None` for the null object or a wrong shape.
fn ref_of_value(v: &Zv<'_>) -> Option<(String, String)> {
    match v {
        Zv::Value(inner) => ref_of_value(inner),
        Zv::Structure(s) => match s.fields() {
            [Zv::Str(bus), Zv::ObjectPath(p)] if p.as_str() != NULL_PATH && !bus.is_empty() => {
                Some((bus.to_string(), p.as_str().to_owned()))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Counters of what the connector refused to believe (IC-16, MA-3). Shared by every app.
#[derive(Debug, Default)]
pub struct FetchCounters {
    foreign_refs: AtomicU64,
    dump_rejected: AtomicU64,
    dump_items_dropped: AtomicU64,
    dumps: AtomicU64,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FetchCountersSnapshot {
    /// Refs (children, parents, ancestors) that named another bus name and were dropped.
    pub foreign_refs: u64,
    /// Dump replies rejected whole (too large, malformed).
    pub dump_rejected: u64,
    /// Dump items or parent refs dropped for naming another bus name.
    pub dump_items_dropped: u64,
    /// Dumps decoded.
    pub dumps: u64,
}

impl FetchCounters {
    pub(crate) fn foreign_refs(&self) {
        self.foreign_refs.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn dump_rejected(&self) {
        self.dump_rejected.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn dump_items(&self, dropped: u32) {
        self.dumps.fetch_add(1, Ordering::Relaxed);
        self.dump_items_dropped
            .fetch_add(u64::from(dropped), Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> FetchCountersSnapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        FetchCountersSnapshot {
            foreign_refs: g(&self.foreign_refs),
            dump_rejected: g(&self.dump_rejected),
            dump_items_dropped: g(&self.dump_items_dropped),
            dumps: g(&self.dumps),
        }
    }
}

/// What a strategy needs for one app: its call connection, the path table (every ref seen in a
/// reply is recorded there) and the counters. Cheap to clone.
#[derive(Clone, Debug)]
pub(crate) struct CallCx {
    pub caller: Caller,
    pub paths: Arc<PathTable>,
    pub app: AppId,
    pub counters: Arc<FetchCounters>,
}

/// A node whose path is known.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub key: NodeKey,
    pub path: Arc<str>,
}

impl CallCx {
    /// A ref from a reply, resolved and recorded; `None` when it names another bus name (the app
    /// must not aim our calls or our mirror at a peer) or the path is unusable.
    pub(crate) fn resolve_ref(&self, bus: &str, path: &str) -> Option<Resolved> {
        if bus != self.caller.bus_name()
            || path.is_empty()
            || path.len() > crate::paths::MAX_PATH_BYTES
        {
            return None;
        }
        let key = NodeKey {
            app: self.app,
            local: gorca_dbus::drain::local_id(path),
        };
        self.paths.insert(key, path);
        Some(Resolved {
            key,
            path: Arc::from(path),
        })
    }

    pub(crate) fn key_of_ref(&self, bus: &str, path: &str) -> Option<NodeKey> {
        let r = self.resolve_ref(bus, path);
        if r.is_none() {
            self.counters.foreign_refs();
        }
        r.map(|r| r.key)
    }
}

/// The nodes a strategy learned, still raw (decoded and bounded, not yet snapshots).
#[derive(Debug, Default)]
pub(crate) struct RawBatch {
    pub nodes: Vec<(NodeKey, RawNode)>,
    pub truncated: bool,
}

impl RawBatch {
    fn into_batch(self, now: Instant) -> SnapshotBatch {
        SnapshotBatch {
            nodes: self
                .nodes
                .iter()
                .map(|(k, raw)| snapshot(*k, raw, now))
                .collect(),
            truncated: self.truncated,
        }
    }
}

/// One application on the call connection.
#[derive(Clone, Debug)]
pub struct Caller {
    conn: Connection,
    bus_name: Arc<str>,
    /// Shadow mode (PW-11): `Some` refuses every call outside [`shadow::allowed`] and counts it.
    shadow: Option<Arc<AtomicU64>>,
}

impl Caller {
    pub fn new(conn: Connection, bus_name: Arc<str>) -> Self {
        Self {
            conn,
            bus_name,
            shadow: None,
        }
    }

    /// Read-only: refuse (and count in `refused`) any call the shadow allowlist does not admit.
    #[must_use]
    pub fn shadowed(mut self, refused: Arc<AtomicU64>) -> Self {
        self.shadow = Some(refused);
        self
    }

    async fn call<B>(
        &self,
        dest: &str,
        deadline: Deadline,
        path: &str,
        iface: &str,
        member: &str,
        body: &B,
    ) -> Result<zbus::Message, DbusError>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        if let Some(refused) = &self.shadow
            && !shadow::allowed(iface, member)
        {
            refused.fetch_add(1, Ordering::Relaxed);
            return Err(DbusError::Zbus(zbus::Error::Failure(format!(
                "shadow mode refuses {iface}.{member}"
            ))));
        }
        call_method(&self.conn, deadline, dest, path, iface, member, body).await
    }

    pub async fn role(&self, path: &str, deadline: Deadline) -> Result<u32, DbusError> {
        let m = self
            .call(&self.bus_name, deadline, path, ACCESSIBLE, "GetRole", &())
            .await?;
        m.body().deserialize::<u32>().map_err(DbusError::Zbus)
    }

    pub async fn state(&self, path: &str, deadline: Deadline) -> Result<Vec<u32>, DbusError> {
        let m = self
            .call(&self.bus_name, deadline, path, ACCESSIBLE, "GetState", &())
            .await?;
        m.body().deserialize::<Vec<u32>>().map_err(DbusError::Zbus)
    }

    async fn prop(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        prop: &str,
        deadline: Deadline,
    ) -> Result<OwnedValue, DbusError> {
        let m = self
            .call(dest, deadline, path, PROPS, "Get", &(iface, prop))
            .await?;
        m.body()
            .deserialize::<OwnedValue>()
            .map_err(DbusError::Zbus)
    }

    async fn string_prop(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        prop: &str,
        deadline: Deadline,
    ) -> Result<String, DbusError> {
        let v = self.prop(dest, path, iface, prop, deadline).await?;
        String::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    async fn f64_prop(&self, path: &str, prop: &str, deadline: Deadline) -> Result<f64, DbusError> {
        let v = self
            .prop(&self.bus_name, path, VALUE, prop, deadline)
            .await?;
        f64::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    /// `Name` of an object, in this application or (for label relations) another peer.
    async fn name_of(
        &self,
        dest: &str,
        path: &str,
        deadline: Deadline,
    ) -> Result<String, DbusError> {
        self.string_prop(dest, path, ACCESSIBLE, "Name", deadline)
            .await
    }

    /// The first `LabelledBy` target: `(bus name, path)`.
    async fn label_target(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Option<(String, String)>, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetRelationSet",
                &(),
            )
            .await?;
        let sets = m
            .body()
            .deserialize::<Vec<(u32, Vec<(String, OwnedObjectPath)>)>>()
            .map_err(DbusError::Zbus)?;
        Ok(sets
            .into_iter()
            .find(|(kind, _)| *kind == RELATION_LABELLED_BY)
            .and_then(|(_, targets)| targets.into_iter().next())
            .map(|(bus, p)| (bus, p.as_str().to_owned())))
    }

    async fn text_prefix(&self, path: &str, deadline: Deadline) -> Result<String, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetText",
                &(0i32, ENTRY_TEXT_CHARS),
            )
            .await?;
        m.body().deserialize::<String>().map_err(DbusError::Zbus)
    }

    pub fn bus_name(&self) -> &str {
        &self.bus_name
    }

    async fn description_of(&self, path: &str, deadline: Deadline) -> Result<String, DbusError> {
        self.string_prop(&self.bus_name, path, ACCESSIBLE, "Description", deadline)
            .await
    }

    /// The `Parent` property: `Some((bus, path))`, or `None` for the root (null object).
    async fn parent_of(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Option<(String, String)>, DbusError> {
        let v = self
            .prop(&self.bus_name, path, ACCESSIBLE, "Parent", deadline)
            .await?;
        Ok(ref_of_value(&v))
    }

    async fn child_count_of(&self, path: &str, deadline: Deadline) -> Result<i32, DbusError> {
        let v = self
            .prop(&self.bus_name, path, ACCESSIBLE, "ChildCount", deadline)
            .await?;
        i32::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    /// `GetChildAtIndex`: `None` past the end.
    async fn child_at(
        &self,
        path: &str,
        index: u32,
        deadline: Deadline,
    ) -> Result<Option<(String, String)>, DbusError> {
        let i = i32::try_from(index).unwrap_or(i32::MAX);
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetChildAtIndex",
                &(i,),
            )
            .await?;
        let (bus, p) = m
            .body()
            .deserialize::<(String, OwnedObjectPath)>()
            .map_err(DbusError::Zbus)?;
        Ok((p.as_str() != NULL_PATH && !bus.is_empty()).then(|| (bus, p.as_str().to_owned())))
    }

    /// `GetChildren`, at most `MAX_SUBTREE_NODES` refs of a reply of at most 16 MiB.
    async fn children_of(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Vec<(String, String)>, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetChildren",
                &(),
            )
            .await?;
        if m.body().data().len() > items::MAX_DUMP_BYTES {
            return Err(DbusError::Zbus(zbus::Error::Failure(
                "GetChildren reply too large".to_owned(),
            )));
        }
        let kids = m
            .body()
            .deserialize::<Vec<(String, OwnedObjectPath)>>()
            .map_err(DbusError::Zbus)?;
        Ok(kids
            .into_iter()
            .take(MAX_SUBTREE_NODES as usize)
            .map(|(b, p)| (b, p.as_str().to_owned()))
            .collect())
    }

    /// `Cache.GetItems`; the reply is decoded by the caller (it can be large).
    async fn get_items(&self, deadline: Deadline) -> Result<zbus::Message, DbusError> {
        self.call(&self.bus_name, deadline, CACHE_PATH, CACHE, "GetItems", &())
            .await
    }

    /// Toolkit and application name of the app root, for `AppInfo` (best effort).
    pub async fn app_info(&self, deadline: Deadline) -> Option<(String, String)> {
        let (toolkit, name) = tokio::join!(
            self.string_prop(
                &self.bus_name,
                ROOT_PATH,
                APPLICATION,
                "ToolkitName",
                deadline
            ),
            self.name_of(&self.bus_name, ROOT_PATH, deadline)
        );
        Some((toolkit.ok()?, name.ok()?))
    }
}

/// Everything the per-app task needs.
#[derive(Debug)]
pub struct AppServer {
    pub caller: Caller,
    pub paths: Arc<PathTable>,
    pub clock: Clock,
    /// Cancelled when the application's bus name vanishes.
    pub gone: CancellationToken,
    /// Verified by the router against the protected Shell identity; never self-reported.
    pub verified_shell: bool,
    pub counters: Arc<FetchCounters>,
    pub shutdown: CancellationToken,
}

impl AppServer {
    /// Serve requests until the app is gone or the connector shuts down. Owns the port.
    pub async fn run(self, mut port: AppPort) {
        let mut requests = std::mem::replace(&mut port.requests, closed_receiver());
        let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
        let background = Arc::new(AtomicUsize::new(0));
        let cx = CallCx {
            caller: self.caller.clone(),
            paths: Arc::clone(&self.paths),
            app: port.app,
            counters: Arc::clone(&self.counters),
        };
        let mut inflight: JoinSet<()> = JoinSet::new();
        // Identifying the application is informational and may be slow: requests (the first
        // focus the user is waiting for) are served meanwhile.
        let (info_tx, mut info_rx) = tokio::sync::oneshot::channel::<(String, String)>();
        {
            let (caller, clock) = (self.caller.clone(), self.clock.clone());
            inflight.spawn(async move {
                let deadline = Deadline::for_class(&clock, gorca_rt::BudgetClass::Startup);
                if let Some(found) = caller.app_info(deadline).await {
                    let _ = info_tx.send(found);
                }
            });
        }
        let mut info_pending = true;
        loop {
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => break,
                () = self.gone.cancelled() => break,
                Some(req) = requests.recv() => self.accept(req, &cx, &permits, &background, &mut inflight),
                found = &mut info_rx, if info_pending => {
                    info_pending = false;
                    if let Ok((toolkit, app_name)) = found {
                        announce_info(&port, &toolkit, &app_name, self.verified_shell);
                    }
                }
                Some(_) = inflight.join_next() => {}
                else => break,
            }
        }
        inflight.abort_all();
        if self.gone.is_cancelled() {
            send_gone(&port).await;
        }
    }

    fn accept(
        &self,
        req: BackendRequest,
        cx: &CallCx,
        permits: &Arc<Semaphore>,
        background: &Arc<AtomicUsize>,
        inflight: &mut JoinSet<()>,
    ) {
        if req.deadline.expired(self.clock.now()) {
            // Too late to be useful; dropping the sender counts as Timeout (C-2).
            return;
        }
        // Interactive requests may use every slot; the others leave INTERACTIVE_RESERVED free,
        // and Background (dumps) at most BACKGROUND_MAX at a time (section 7).
        let is_background = matches!(
            req.class,
            gorca_rt::BudgetClass::Background | gorca_rt::BudgetClass::Startup
        );
        let class_ok = match req.class {
            gorca_rt::BudgetClass::Interactive => true,
            gorca_rt::BudgetClass::Prefetch => permits.available_permits() > INTERACTIVE_RESERVED,
            gorca_rt::BudgetClass::Background | gorca_rt::BudgetClass::Startup => {
                permits.available_permits() > INTERACTIVE_RESERVED
                    && background.load(Ordering::Acquire) < BACKGROUND_MAX
            }
        };
        let permit = if class_ok {
            Arc::clone(permits).try_acquire_owned().ok()
        } else {
            None
        };
        let Some(permit) = permit else {
            let _ = req.reply.send(Err(BackendError::Busy));
            return;
        };
        if is_background {
            background.fetch_add(1, Ordering::AcqRel);
        }
        let cx = cx.clone();
        let background = Arc::clone(background);
        inflight.spawn(async move {
            let result = serve_one(&cx, &req).await;
            let _ = req.reply.send(result);
            if is_background {
                background.fetch_sub(1, Ordering::AcqRel);
            }
            drop(permit);
        });
    }
}

fn announce_info(port: &AppPort, toolkit: &str, app_name: &str, verified_shell: bool) {
    let class = Toolkit::classify(toolkit, app_name);
    tracing::debug!(app = ?port.app, ?class, "application identified");
    let info = AppInfo {
        toolkit: Arc::from(clip(toolkit, 64)),
        app_name: Arc::from(clip(app_name, 128)),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
        verified_shell,
        // Shell is the only toolkit whose dump P-9 verified complete; the flag follows the
        // verified identity, never `app_name` (B-1 2.2).
        dump_complete: verified_shell,
    };
    // Informational: a full control queue is not worth retrying.
    let _ = port.lifecycle(AppLifecycle::Appeared(info));
}

/// A key the path table no longer holds is `Unresolved`, never `Gone`: the object may well
/// exist (PW-7).
fn resolve(cx: &CallCx, key: NodeKey) -> Result<Resolved, BackendError> {
    cx.paths
        .get(key)
        .map(|path| Resolved { key, path })
        .ok_or(BackendError::Unresolved)
}

async fn serve_one(cx: &CallCx, req: &BackendRequest) -> Result<BackendReply, BackendError> {
    let dl = req.deadline;
    let now = Instant::now();
    match &req.op {
        BackendOp::Fetch { node, want } => {
            let n = resolve(cx, *node)?;
            let raw = strategy::fetch(cx, &n, *want, dl).await?;
            Ok(BackendReply::Node(snapshot(n.key, &raw, now)))
        }
        BackendOp::ChildSnapshots { node, range, want } => {
            let n = resolve(cx, *node)?;
            let b = strategy::child_snapshots(cx, &n, range.clone(), *want, dl).await?;
            Ok(BackendReply::Subtree(b.into_batch(now)))
        }
        BackendOp::Ancestry {
            node,
            max_depth,
            want,
        } => {
            let n = resolve(cx, *node)?;
            let b = strategy::ancestry(cx, &n, *max_depth, *want, dl).await?;
            Ok(BackendReply::Subtree(b.into_batch(now)))
        }
        BackendOp::Subtree { root, max_nodes } => {
            let n = resolve(cx, *root)?;
            let b = strategy::subtree(cx, &n, *max_nodes, dl).await?;
            Ok(BackendReply::Subtree(b.into_batch(now)))
        }
        BackendOp::CacheDump { max_nodes } => {
            let b = strategy::cache_dump(cx, *max_nodes, dl).await?;
            Ok(BackendReply::Subtree(b.into_batch(now)))
        }
        BackendOp::Probe => cx
            .caller
            .role(ROOT_PATH, dl)
            .await
            .map(|_| BackendReply::ActionDone)
            .map_err(|e| backend_error(&e)),
        BackendOp::Children { .. }
        | BackendOp::Text(_)
        | BackendOp::Action(_)
        | BackendOp::HitTest { .. }
        | BackendOp::Collection(_) => Err(BackendError::NotSupported),
    }
}

/// A lost `Gone` would leave the app actor waiting (`AppPort::lifecycle` docs): retry briefly.
async fn send_gone(port: &AppPort) {
    for _ in 0..20 {
        match port.lifecycle(AppLifecycle::Gone) {
            Ok(()) | Err(PortError::Closed) => return,
            Err(PortError::Full) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
}

fn closed_receiver() -> gorca_rt::mpsc::Receiver<BackendRequest> {
    let (_tx, rx) = gorca_rt::mpsc::channel(1);
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_fields_are_read_like_any_other_entry() {
        use gorca_model::StateSet;
        assert!(wants_entry_text(Role::PasswordEntry, StateSet::empty()));
        assert!(wants_entry_text(Role::TextEntry, StateSet::empty()));
        assert!(!wants_entry_text(Role::PasswordEntry, StateSet::MULTILINE));
        assert!(!wants_entry_text(Role::Button, StateSet::empty()));
    }

    #[test]
    fn labels_are_read_only_from_the_same_application() {
        assert!(label_may_be_read(":1.5", ":1.5"));
        assert!(!label_may_be_read(":1.9", ":1.5"));
    }

    #[test]
    fn dbus_errors_map_to_backend_errors() {
        assert_eq!(backend_error(&DbusError::Timeout), BackendError::Timeout);
        let other = DbusError::Zbus(zbus::Error::InterfaceNotFound);
        assert!(matches!(backend_error(&other), BackendError::Protocol(_)));
    }
}
