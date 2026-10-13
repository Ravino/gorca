//! Per-application request service: turns `BackendRequest`s into AT-SPI calls over the call
//! connection. Every call carries the request's deadline (R2, D-4); the number of requests in
//! flight is bounded per application (C-3), so a hung application costs its own actor only.

use std::sync::Arc;
use std::time::Instant;

use gorca_connector_api::{
    AppPort, BackendError, BackendOp, BackendReply, BackendRequest, PortError,
};
use gorca_dbus::{DbusError, call_method};
use gorca_model::{AppInfo, AppLifecycle, ConnectorKind, NodeKey, PropMask, Role};
use gorca_rt::{CancellationToken, Clock, Deadline};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::decode::{MAX_TEXT_BYTES, RawNode, RawValue, clip, snapshot};
use crate::paths::PathTable;
use crate::toolkit::Toolkit;

/// Requests served concurrently per application; the rest is answered `Busy` (C-3).
pub const MAX_IN_FLIGHT: usize = 8;

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

/// One application on the call connection.
#[derive(Clone, Debug)]
pub struct Caller {
    conn: Connection,
    bus_name: Arc<str>,
}

impl Caller {
    pub fn new(conn: Connection, bus_name: Arc<str>) -> Self {
        Self { conn, bus_name }
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

    /// Name, role, state (and what the role needs on top) of one object. The three base calls run
    /// concurrently; all share one deadline. Failures of the optional parts are not errors: the
    /// utterance is simply poorer.
    pub async fn fetch(
        &self,
        key: NodeKey,
        path: &str,
        want: PropMask,
        deadline: Deadline,
    ) -> Result<Arc<gorca_model::NodeSnapshot>, BackendError> {
        let name_fut = async {
            if want.contains(PropMask::NAME) {
                self.name_of(&self.bus_name, path, deadline).await.map(Some)
            } else {
                Ok(None)
            }
        };
        let (role, state, name) = tokio::join!(
            self.role(path, deadline),
            self.state(path, deadline),
            name_fut
        );
        // Role and state are required; a failed name only costs the name.
        let role = role.map_err(|e| backend_error(&e))?;
        let state_words = state.map_err(|e| backend_error(&e))?;
        let mut raw = RawNode {
            role,
            state_words,
            name: name.ok().flatten(),
            ..RawNode::default()
        };
        let core_role = crate::decode::role_from_raw(role);
        let states = crate::decode::states_from_raw(&raw.state_words);

        if want.contains(PropMask::VALUE) {
            raw.value = self.value_for(core_role, states, path, deadline).await;
        }
        if raw.name.as_deref().is_none_or(str::is_empty)
            && let Ok(Some((bus, p))) = self.label_target(path, deadline).await
            && label_may_be_read(&bus, &self.bus_name)
            && let Ok(label) = self.name_of(&bus, &p, deadline).await
        {
            raw.name = Some(label);
        }
        Ok(snapshot(key, &raw, Instant::now()))
    }

    async fn value_for(
        &self,
        role: Role,
        states: gorca_model::StateSet,
        path: &str,
        deadline: Deadline,
    ) -> Option<RawValue> {
        match role {
            Role::Slider | Role::ProgressBar | Role::SpinButton => {
                let (cur, min, max) = tokio::join!(
                    self.f64_prop(path, "CurrentValue", deadline),
                    self.f64_prop(path, "MinimumValue", deadline),
                    self.f64_prop(path, "MaximumValue", deadline)
                );
                Some(RawValue::Number {
                    current: cur.ok()?,
                    min: min.unwrap_or(f64::NAN),
                    max: max.unwrap_or(f64::NAN),
                })
            }
            // Product decision: gorca reads whatever the application exposes, password fields
            // included. The text is never logged.
            Role::TextEntry | Role::PasswordEntry if wants_entry_text(role, states) => {
                let t = self.text_prefix(path, deadline).await.ok()?;
                Some(RawValue::Text(clip(&t, MAX_TEXT_BYTES).to_owned()))
            }
            // Everything else has no value on focus.
            Role::PasswordEntry
            | Role::TextEntry
            | Role::Unknown
            | Role::Application
            | Role::Window
            | Role::Dialog
            | Role::Panel
            | Role::Document
            | Role::Heading
            | Role::Paragraph
            | Role::Link
            | Role::Image
            | Role::List
            | Role::ListItem
            | Role::Table
            | Role::Row
            | Role::Cell
            | Role::ColumnHeader
            | Role::RowHeader
            | Role::Button
            | Role::ToggleButton
            | Role::CheckBox
            | Role::RadioButton
            | Role::ComboBox
            | Role::Menu
            | Role::MenuBar
            | Role::MenuItem
            | Role::Popover
            | Role::Tab
            | Role::TabList
            | Role::Tree
            | Role::TreeItem
            | Role::Toolbar
            | Role::StatusBar
            | Role::Separator
            | Role::Label
            | Role::StaticText
            | Role::Terminal
            | Role::Landmark
            | Role::Alert => None,
        }
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
    pub shutdown: CancellationToken,
}

impl AppServer {
    /// Serve requests until the app is gone or the connector shuts down. Owns the port.
    pub async fn run(self, mut port: AppPort) {
        let mut requests = std::mem::replace(&mut port.requests, closed_receiver());
        let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
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
                Some(req) = requests.recv() => self.accept(req, &permits, &mut inflight),
                found = &mut info_rx, if info_pending => {
                    info_pending = false;
                    if let Ok((toolkit, app_name)) = found {
                        announce_info(&port, &toolkit, &app_name);
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

    fn accept(&self, req: BackendRequest, permits: &Arc<Semaphore>, inflight: &mut JoinSet<()>) {
        if req.deadline.expired(self.clock.now()) {
            // Too late to be useful; dropping the sender counts as Timeout (C-2).
            return;
        }
        let Ok(permit) = Arc::clone(permits).try_acquire_owned() else {
            let _ = req.reply.send(Err(BackendError::Busy));
            return;
        };
        let caller = self.caller.clone();
        let paths = Arc::clone(&self.paths);
        inflight.spawn(async move {
            let result = serve_one(&caller, &paths, &req).await;
            let _ = req.reply.send(result);
            drop(permit);
        });
    }
}

fn announce_info(port: &AppPort, toolkit: &str, app_name: &str) {
    let class = Toolkit::classify(toolkit, app_name);
    tracing::debug!(app = ?port.app, ?class, "application identified");
    let info = AppInfo {
        toolkit: Arc::from(clip(toolkit, 64)),
        app_name: Arc::from(clip(app_name, 128)),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
    };
    // Informational: a full control queue is not worth retrying.
    let _ = port.lifecycle(AppLifecycle::Appeared(info));
}

async fn serve_one(
    caller: &Caller,
    paths: &PathTable,
    req: &BackendRequest,
) -> Result<BackendReply, BackendError> {
    match &req.op {
        BackendOp::Fetch { node, want } => {
            let Some(path) = paths.get(*node) else {
                return Err(BackendError::Gone);
            };
            caller
                .fetch(*node, &path, *want, req.deadline)
                .await
                .map(BackendReply::Node)
        }
        BackendOp::Probe => caller
            .role(ROOT_PATH, req.deadline)
            .await
            .map(|_| BackendReply::ActionDone)
            .map_err(|e| backend_error(&e)),
        BackendOp::Children { .. }
        | BackendOp::Subtree { .. }
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
