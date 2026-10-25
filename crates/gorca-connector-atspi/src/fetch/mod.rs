// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-application request service: turns `BackendRequest`s into AT-SPI calls over the call
//! connection. Every call carries the request's deadline (R2, D-4); the number of requests in
//! flight is bounded per application (C-3), so a hung application costs its own actor only.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use gorca_connector_api::{
    AppPort, BackendError, BackendOp, BackendReply, BackendRequest, PortError,
};
use gorca_dbus::DbusError;
use gorca_model::{
    AppId, AppInfo, AppLifecycle, ConnectorKind, NodeKey, PropMask, Role, SnapshotBatch,
};
use gorca_rt::{CancellationToken, Clock, Deadline};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::decode::{RawNode, clip, snapshot};
use crate::paths::PathTable;
use crate::toolkit::Toolkit;

mod caller;
pub mod items;
pub mod locate;
pub mod review;
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
/// `ATSPI_RELATION_ERROR_MESSAGE`.
const RELATION_ERROR_MESSAGE: u32 = 21;
/// Most characters read from an error message target.
const ERROR_TEXT_CHARS: i32 = 400;
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

pub use caller::Caller;

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
    /// `(owner pid, start time)` of the registration, for re-verification (F8).
    pub identity: Option<(u32, u64)>,
    pub shell_rx: Option<tokio::sync::watch::Receiver<Option<(u32, u64)>>>,
    /// The route's hint flag, raised with the path table's when a late verification succeeds.
    pub hints: Arc<std::sync::atomic::AtomicBool>,
    pub counters: Arc<FetchCounters>,
    pub shutdown: CancellationToken,
}

/// A registration verified as the Shell after it was published: its hint events record paths
/// from now on, in the drain's route flag and in the path table (N-4).
pub(crate) fn raise_hints(hints: &std::sync::atomic::AtomicBool, paths: &PathTable, app: AppId) {
    hints.store(true, std::sync::atomic::Ordering::Relaxed);
    paths.allow_hints(app);
}

/// Does the registration with `identity` belong to the verified Shell process?
pub(crate) fn is_the_shell(shell: Option<(u32, u64)>, identity: Option<(u32, u64)>) -> bool {
    matches!((shell, identity), (Some(s), Some(i)) if s == i)
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
        // What the pointer resolver needs to describe a window to the compositor (ADR-0025).
        self.caller.set_pid(self.identity.map(|i| i.0));
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
        // The Shell can be routed before `org.gnome.Shell` was resolved: verification is
        // re-evaluated whenever the protected process changes, not once at publication.
        let mut verified = self.verified_shell;
        let mut shell_rx = self.shell_rx.clone();
        let mut known: Option<(String, String)> = None;
        let mut settle_ms = 0u32;
        let mut exe: Option<String> = None;
        if !verified && let Some(rx) = &shell_rx {
            verified = is_the_shell(*rx.borrow(), self.identity);
        }
        loop {
            let shell_changed = async {
                match shell_rx.as_mut() {
                    Some(rx) if !verified => rx.changed().await.is_ok(),
                    Some(_) | None => std::future::pending().await,
                }
            };
            tokio::select! {
                biased;
                () = self.shutdown.cancelled() => break,
                () = self.gone.cancelled() => break,
                Some(req) = requests.recv() => self.accept(req, &cx, &permits, &background, &mut inflight),
                found = &mut info_rx, if info_pending => {
                    info_pending = false;
                    // An application that cannot be identified is still announced (with no
                    // toolkit or name): the reader and the policies know it exists.
                    let (toolkit, app_name) = found.unwrap_or_default();
                    settle_ms = settle_window_ms(&toolkit, &app_name, self.identity).await;
                    exe = match self.identity {
                        Some((pid, start)) => crate::proc::exe_of_async(pid, start).await,
                        None => None,
                    };
                    announce_info(&port, &toolkit, &app_name, exe.as_deref(), verified, settle_ms);
                    self.caller.set_text_screen_coords(
                        Toolkit::classify(&toolkit, &app_name).text_extents_use_screen_coords(),
                    );
                    self.caller.set_children_in_device_pixels(
                        Toolkit::classify(&toolkit, &app_name).children_in_device_pixels(),
                    );
                    // Quirk row `blink-wake-call`: once, at first sight (a re-announcement
                    // after Shell verification does not repeat it).
                    if Toolkit::classify(&toolkit, &app_name).needs_wake_call() {
                        let (caller, clock) = (self.caller.clone(), self.clock.clone());
                        inflight.spawn(async move {
                            let deadline =
                                Deadline::for_class(&clock, gorca_rt::BudgetClass::Startup);
                            let answered = caller.wake(deadline).await;
                            tracing::debug!(answered, "wake call sent to an application whose tree needs it");
                        });
                    }
                    // Announced and served: policies that fill at readiness start here (once;
                    // a later announcement only refines the identity).
                    let _ = port.lifecycle(AppLifecycle::Ready);
                    known = Some((toolkit, app_name));
                }
                ok = shell_changed => {
                    if !ok {
                        shell_rx = None;
                    } else if let Some(rx) = &shell_rx
                        && is_the_shell(*rx.borrow(), self.identity)
                    {
                        verified = true;
                        // The Shell's hint events record paths from now on (N-4).
                        raise_hints(&self.hints, &self.paths, port.app);
                        // Announce again: the profile, the dump eligibility and the protection
                        // follow the verified identity.
                        if let Some((t, n)) = &known {
                            announce_info(&port, t, n, exe.as_deref(), true, settle_ms);
                        }
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
        let class_ok = class_admitted(
            req.class,
            permits.available_permits(),
            background.load(Ordering::Acquire),
        );
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

fn announce_info(
    port: &AppPort,
    toolkit: &str,
    app_name: &str,
    exe: Option<&str>,
    verified_shell: bool,
    settle_ms: u32,
) {
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
        exe: exe.map(|e| Arc::from(clip(e, 64))),
        text_unit: class.text_unit(),
        text_insert_slack: class.text_insert_slack(),
        edit_quirks: class.edit_quirks(),
        page_events_settle_ms: settle_ms,
        web_engine: class.web_engine(),
        root: Some(NodeKey {
            app: port.app,
            local: gorca_dbus::drain::local_id(ROOT_PATH),
        }),
        sem_tables: crate::sem_profile::tables_for(toolkit, app_name, class.web_engine()),
    };
    // Informational: a full control queue is not worth retrying.
    let _ = port.lifecycle(AppLifecycle::Appeared(info));
}

/// The quirk row `blink-tree-load-complete` for an application that just appeared, except that
/// an application that was started by the user while the reader ran, moments ago (inside the
/// window itself), has no tree built for our wake-up call from an earlier page: the first page
/// it opens is the user's, and its `load-complete` is a real load that gets the auto-read (M9
/// review L-2).
async fn settle_window_ms(toolkit: &str, app_name: &str, identity: Option<(u32, u64)>) -> u32 {
    let ms = Toolkit::classify(toolkit, app_name).page_events_settle_ms();
    if ms == 0 {
        return 0;
    }
    let Some((_, start)) = identity else {
        return ms;
    };
    let own = crate::proc::own_start_ticks();
    match crate::proc::age_ms_async(start).await {
        Some(age) if is_fresh_launch(start, own, age, ms) => {
            tracing::debug!(age, "freshly launched application: no tree-creation window");
            0
        }
        Some(_) | None => ms,
    }
}

/// An application that started while we were running (an application that was already there
/// when the reader started, even a moment before, shows a page the user opened earlier) and
/// less than `window_ms` ago.
fn is_fresh_launch(start: u64, own_start: Option<u64>, age_ms: u64, window_ms: u32) -> bool {
    own_start.is_some_and(|own| start >= own) && age_ms < u64::from(window_ms)
}

/// Admission by class (section 7, MA-8): Interactive may take any free slot; Prefetch and
/// Background leave [`INTERACTIVE_RESERVED`] free, and Background (dumps) at most
/// [`BACKGROUND_MAX`] at a time.
pub(crate) fn class_admitted(
    class: gorca_rt::BudgetClass,
    available: usize,
    background_in_flight: usize,
) -> bool {
    match class {
        gorca_rt::BudgetClass::Interactive => available > 0,
        gorca_rt::BudgetClass::Prefetch => available > INTERACTIVE_RESERVED,
        gorca_rt::BudgetClass::Background | gorca_rt::BudgetClass::Startup => {
            available > INTERACTIVE_RESERVED && background_in_flight < BACKGROUND_MAX
        }
    }
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
            let mut snap = [snapshot(n.key, &raw, now)];
            if want.contains(PropMask::BOUNDS) {
                review::add_bounds(cx, &mut snap, dl).await;
            }
            let [snap] = snap;
            Ok(BackendReply::Node(snap))
        }
        BackendOp::ChildSnapshots { node, range, want } => {
            let n = resolve(cx, *node)?;
            let b = strategy::child_snapshots(cx, &n, range.clone(), *want, dl).await?;
            let mut batch = b.into_batch(now);
            if want.contains(PropMask::BOUNDS) {
                review::add_bounds(cx, &mut batch.nodes, dl).await;
            }
            Ok(BackendReply::Subtree(batch))
        }
        BackendOp::Siblings {
            parent,
            node,
            before,
            after,
            indexed,
            want,
        } => {
            let p = resolve(cx, *parent)?;
            let n = resolve(cx, *node)?;
            let b = strategy::siblings(cx, &p, &n, (*before, *after), *indexed, *want, dl).await?;
            Ok(BackendReply::Subtree(b.into_batch(now)))
        }
        BackendOp::Ancestry {
            node,
            max_depth,
            want,
        } => {
            let n = resolve(cx, *node)?;
            let b = strategy::ancestry(cx, &n, *max_depth, *want, dl).await?;
            let mut batch = b.into_batch(now);
            if want.contains(PropMask::BOUNDS) {
                review::add_bounds(cx, &mut batch.nodes, dl).await;
            }
            Ok(BackendReply::Subtree(batch))
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
        BackendOp::Text(q) => review::text(cx, q, dl).await,
        BackendOp::Action(a) => review::action(cx, a, dl).await,
        BackendOp::Locate { node, offset } => locate::locate(cx, *node, *offset, dl).await,
        BackendOp::Collection(rule) => review::collection(cx, rule, dl).await,
        BackendOp::Children { .. } | BackendOp::HitTest { .. } => Err(BackendError::NotSupported),
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
    #[test]
    fn a_late_verified_shell_gets_its_hint_paths() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let app = AppId {
            connector: gorca_model::ConnectorId(1),
            serial: 7,
        };
        let key = NodeKey {
            app,
            local: gorca_model::LocalId(3),
        };
        let paths = PathTable::new();
        paths.bind_app(app, crate::paths::Owner::Pid(10));
        let hints = AtomicBool::new(false);
        paths.insert_hint(key, "/s/3");
        assert!(paths.get(key).is_none(), "not verified yet: no hint paths");
        raise_hints(&hints, &paths, app);
        assert!(hints.load(Ordering::Relaxed));
        paths.insert_hint(key, "/s/3");
        assert_eq!(paths.get(key).as_deref(), Some("/s/3"));
    }

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

#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod strategy_ref_tests;

#[cfg(test)]
mod admission_tests {
    use super::*;
    use gorca_rt::BudgetClass as C;

    #[test]
    fn interactive_keeps_two_slots_nobody_else_may_take() {
        // 8 slots: with 2 or fewer free only Interactive is admitted.
        for free in 0..=MAX_IN_FLIGHT {
            assert_eq!(class_admitted(C::Interactive, free, 0), free > 0);
            assert_eq!(
                class_admitted(C::Prefetch, free, 0),
                free > INTERACTIVE_RESERVED
            );
            assert_eq!(
                class_admitted(C::Background, free, 0),
                free > INTERACTIVE_RESERVED
            );
        }
    }

    #[test]
    fn background_is_capped_whatever_is_free() {
        assert!(class_admitted(C::Background, 8, BACKGROUND_MAX - 1));
        assert!(!class_admitted(C::Background, 8, BACKGROUND_MAX));
        assert!(!class_admitted(C::Startup, 8, BACKGROUND_MAX));
        assert!(
            class_admitted(C::Prefetch, 8, BACKGROUND_MAX),
            "Prefetch is not Background"
        );
        assert!(class_admitted(C::Interactive, 1, BACKGROUND_MAX));
    }
}

#[cfg(test)]
mod settle_window_tests {
    use super::is_fresh_launch;

    #[test]
    fn only_an_app_that_started_after_the_reader_and_moments_ago_has_no_window() {
        assert!(is_fresh_launch(500, Some(400), 1000, 3000));
        assert!(
            is_fresh_launch(400, Some(400), 0, 3000),
            "same tick counts as after"
        );
        assert!(
            !is_fresh_launch(399, Some(400), 1000, 3000),
            "older than the reader"
        );
        assert!(
            !is_fresh_launch(500, Some(400), 3000, 3000),
            "outside the window"
        );
        assert!(!is_fresh_launch(500, None, 10, 3000), "own start unknown");
    }
}
