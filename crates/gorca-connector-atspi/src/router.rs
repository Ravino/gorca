//! Sender -> application routing for the event drain.
//!
//! The drain asks [`AtspiRouter::resolve`] on its own thread and must never wait. The first event
//! of an unknown application therefore starts an **asynchronous** registration and answers
//! `Retry`; the drain does not cache that, and hands the event to [`AtspiRouter::park`], which
//! keeps a bounded number of them. The registration ([`AtspiRouter::complete`], run by the
//! connector task) asks the bus for the connection's credentials, announces the application to
//! the core, publishes the route and replays the parked events in order, all under the lock that
//! also protects `forget`, so an application that vanishes while it is being registered is never
//! published and leaks nothing.
//!
//! Failure never mutes an application for good: a failed registration sets a per-sender
//! exponential backoff (10 ms to 1 s) during which the drain just drops and counts, and after
//! which the next event starts a new attempt.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_connector_api::{AppPort, AppRegistrar, RegistrarError, ScopeKey};
use gorca_dbus::drain::{AppRoute, RouteDecision, Router, SignalView, map_signal};
use gorca_model::{AppId, AppInfo, ConnectorKind, NodeEvent, NodeKey};
use gorca_rt::{BoxFuture, CancellationToken, Clock, Deadline, mpsc};

use crate::paths::{MAX_PATH_BYTES, Owner, PathTable};

/// Deadline for the core to accept a new application.
pub const ANNOUNCE_WAIT: Duration = Duration::from_secs(1);
/// Registrations in flight at once. More senders than this answer `Retry` without parking.
pub const MAX_PENDING: usize = 64;
/// Non-focus events parked for one sender while it is being registered.
pub const MAX_PARKED_PER_SENDER: usize = 32;
/// Bytes of non-focus events parked for one sender. `MAX_PENDING` senders at this share exactly
/// fill [`PARK_BYTES_TOTAL`], so no set of senders can use up another sender's share.
pub const PARK_BYTES_PER_SENDER: usize = 4 * 1024;
/// Bytes of non-focus events parked in total.
pub const PARK_BYTES_TOTAL: usize = MAX_PENDING * PARK_BYTES_PER_SENDER;
/// Larger signal bodies are not parked (they are dropped and counted by the drain).
pub const MAX_PARKED_BODY: usize = 512;
/// D-Bus limit for interface and member names.
pub const MAX_NAME_BYTES: usize = 255;
/// Senders (not in registration) whose newest focus is kept for their next attempt.
pub const MAX_ORPHAN_FOCUS: usize = 256;
/// Accounting overhead of one parked event.
const PARKED_OVERHEAD: usize = 64;
/// Registered bus names one process may hold at once.
pub const MAX_NAMES_PER_OWNER: usize = 64;
/// Registered bus names of connections whose credentials are unknown, in total.
pub const MAX_UNKNOWN_NAMES: usize = 32;
/// Registered applications in total. A process that forks to multiply its per-pid caps meets this
/// one; the largest process group is the one that gives way.
pub const MAX_APPS_TOTAL: usize = 512;
const BACKOFF_MIN: Duration = Duration::from_millis(10);
const BACKOFF_MAX: Duration = Duration::from_secs(1);
/// Senders with a backoff or a tombstone are remembered this long.
const DEPARTED_TTL: Duration = Duration::from_secs(300);
/// Hard cap on remembered departed names.
const MAX_DEPARTED: usize = 65_536;
const MAX_BACKOFFS: usize = 4096;

/// A freshly announced application, handed to the connector task, which owns the task that
/// serves its requests.
#[derive(Debug)]
pub struct NewApp {
    pub port: AppPort,
    pub bus_name: Arc<str>,
    /// Cancelled when the application's bus name vanishes.
    pub gone: CancellationToken,
}

/// Who owns a bus connection (`org.freedesktop.DBus.GetConnectionCredentials`).
pub trait Credentials: Send + Sync + 'static {
    /// The process id of `name`, or `None` if the bus cannot or will not say (deadline inside).
    fn pid_of<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<u32>>;
}

/// No credentials available: every application is its own small owner.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct NoCredentials;

#[cfg(test)]
impl Credentials for NoCredentials {
    fn pid_of<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Option<u32>> {
        Box::pin(async { None })
    }
}

#[derive(Debug)]
struct Entry {
    route: AppRoute,
    gone: CancellationToken,
    group: Option<u32>,
}

#[derive(Debug)]
struct Parked {
    interface: String,
    member: String,
    path: String,
    body: Vec<u8>,
    big_endian: bool,
}

impl Parked {
    fn of(view: &SignalView<'_>) -> Self {
        Self {
            interface: view.interface.to_owned(),
            member: view.member.to_owned(),
            path: view.path.to_owned(),
            body: view.body.to_vec(),
            big_endian: view.big_endian,
        }
    }

    fn cost(&self) -> usize {
        self.interface.len()
            + self.member.len()
            + self.path.len()
            + self.body.len()
            + PARKED_OVERHEAD
    }
}

/// What arrives for a sender while it is being registered: the non-focus events, bounded by
/// count and bytes (oldest dropped first), and the **newest focus** in a slot of its own, outside
/// those bounds. Focus is lossless in the sense of ADR-0004: a newer one replaces an older one,
/// and no other sender can crowd it out.
#[derive(Debug, Default)]
struct Pending {
    events: VecDeque<Parked>,
    bytes: usize,
    focus: Option<Parked>,
}

/// An `AppId` for classifying a signal before the real one exists.
const PROBE_APP: AppId = AppId {
    connector: gorca_model::ConnectorId(0),
    serial: 0,
};

/// Does this signal carry a focus change?
fn is_focus(view: &SignalView<'_>) -> bool {
    let mut out = smallvec::SmallVec::<[NodeEvent; 2]>::new();
    map_signal(PROBE_APP, view, &mut out).is_ok()
        && out
            .iter()
            .any(|e| matches!(e, NodeEvent::FocusChanged { .. }))
}

#[derive(Copy, Clone, Debug)]
struct Backoff {
    not_before: Instant,
    delay: Duration,
}

#[derive(Debug, Default)]
struct State {
    apps: HashMap<Arc<str>, Entry>,
    ignored: HashSet<Arc<str>>,
    pending: HashMap<Arc<str>, Pending>,
    /// Bytes of non-focus events parked across all pending senders.
    parked_bytes: usize,
    /// Newest focus of senders that have no registration running (backoff, pending cap).
    orphan_focus: HashMap<Arc<str>, Parked>,
    backoff: HashMap<Arc<str>, Backoff>,
    /// Registered names per process (`None`: credentials unknown, all such names together).
    names_per_group: HashMap<Option<u32>, usize>,
    departed: HashSet<Arc<str>>,
    departed_order: VecDeque<(Instant, Arc<str>)>,
}

impl State {
    /// Unique names are never reused by the bus, so a name that left can never come back:
    /// remembering it closes the race between a registration in flight and the
    /// `NameOwnerChanged` that arrives while it is. Entries expire after [`DEPARTED_TTL`].
    fn mark_departed(&mut self, name: &str, now: Instant) {
        while let Some((at, old)) = self.departed_order.front() {
            if now.saturating_duration_since(*at) < DEPARTED_TTL
                && self.departed_order.len() < MAX_DEPARTED
            {
                break;
            }
            let old = Arc::clone(old);
            self.departed_order.pop_front();
            self.departed.remove(&old);
        }
        if self.departed.contains(name) {
            return;
        }
        let name: Arc<str> = Arc::from(name);
        self.departed.insert(Arc::clone(&name));
        self.departed_order.push_back((now, name));
    }

    /// Forget what was parked for `name`; the number of events lost with it.
    fn drop_parked(&mut self, name: &str) -> usize {
        let orphan = usize::from(self.orphan_focus.remove(name).is_some());
        let Some(p) = self.pending.remove(name) else {
            return orphan;
        };
        self.parked_bytes = self.parked_bytes.saturating_sub(p.bytes);
        orphan + p.events.len() + usize::from(p.focus.is_some())
    }

    fn release_name(&mut self, group: Option<u32>) {
        if let Some(n) = self.names_per_group.get_mut(&group) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.names_per_group.remove(&group);
            }
        }
    }

    /// The process group holding the most registered names.
    fn largest_group(&self) -> Option<(Option<u32>, usize)> {
        self.names_per_group
            .iter()
            .map(|(g, n)| (*g, *n))
            .max_by_key(|(_, n)| *n)
    }

    /// Remove one application of `group` (the oldest by name order is as good as any: the group
    /// is the problem, not an individual).
    fn evict_from(&mut self, group: Option<u32>) -> Option<Entry> {
        let name = self
            .apps
            .iter()
            .find(|(_, e)| e.group == group)
            .map(|(n, _)| Arc::clone(n))?;
        let e = self.apps.remove(&name)?;
        self.release_name(group);
        Some(e)
    }
}

/// Counters, for tests and diagnostics.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RouterStats {
    pub announced: u64,
    pub announce_failed: u64,
    pub ignored: u64,
    pub parked_replayed: u64,
    pub parked_dropped: u64,
}

pub struct AtspiRouter {
    registrar: AppRegistrar,
    clock: Clock,
    paths: Arc<PathTable>,
    new_apps: mpsc::Sender<NewApp>,
    /// Senders whose registration the connector task must run.
    to_register: mpsc::Sender<Arc<str>>,
    credentials: Arc<dyn Credentials>,
    state: Mutex<State>,
    stats: Mutex<RouterStats>,
}

impl std::fmt::Debug for AtspiRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtspiRouter").finish_non_exhaustive()
    }
}

impl AtspiRouter {
    pub fn new(
        registrar: AppRegistrar,
        clock: Clock,
        paths: Arc<PathTable>,
        new_apps: mpsc::Sender<NewApp>,
        to_register: mpsc::Sender<Arc<str>>,
        credentials: Arc<dyn Credentials>,
    ) -> Self {
        Self {
            registrar,
            clock,
            paths,
            new_apps,
            to_register,
            credentials,
            state: Mutex::new(State::default()),
            stats: Mutex::new(RouterStats::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self, f: impl FnOnce(&mut RouterStats)) {
        f(&mut self.stats.lock().unwrap_or_else(PoisonError::into_inner));
    }

    pub fn stats(&self) -> RouterStats {
        *self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Bus names that emit AT-SPI signals but are not applications (the registry daemon, our
    /// own connections).
    pub fn ignore(&self, name: &str) {
        self.state().ignored.insert(Arc::from(name));
    }

    /// The application owning `name` left the bus. Returns its id so the caller can retire it.
    ///
    /// Atomic with respect to registration: the name is marked departed under the same lock
    /// that [`AtspiRouter::complete`] takes to publish a route, so an application that dies
    /// while its registration is in flight is never published (the registration retires it), and
    /// one that is already published is removed here. Either way nothing is left behind.
    pub fn forget(&self, name: &str) -> Option<AppId> {
        let (entry, dropped) = {
            let mut st = self.state();
            st.mark_departed(name, self.clock.now());
            st.backoff.remove(name);
            // The name is gone for good; it must not stay in the ignored set (departed covers it).
            st.ignored.remove(name);
            let dropped = st.drop_parked(name);
            let e = st.apps.remove(name);
            if let Some(e) = &e {
                st.release_name(e.group);
            }
            (e, dropped)
        };
        if dropped > 0 {
            self.bump(|s| s.parked_dropped += dropped as u64);
        }
        let entry = entry?;
        entry.gone.cancel();
        self.paths.forget_app(entry.route.app);
        Some(entry.route.app)
    }

    /// Forget every application (connector shutdown or restart) and return their ids, so the
    /// caller can retire them in the core.
    pub fn take_all(&self) -> Vec<AppId> {
        let apps: Vec<Entry> = {
            let mut st = self.state();
            st.pending.clear();
            st.orphan_focus.clear();
            st.parked_bytes = 0;
            st.names_per_group.clear();
            st.apps.drain().map(|(_, e)| e).collect()
        };
        apps.into_iter()
            .map(|e| {
                e.gone.cancel();
                self.paths.forget_app(e.route.app);
                e.route.app
            })
            .collect()
    }

    pub fn known_apps(&self) -> usize {
        self.state().apps.len()
    }

    pub fn pending_registrations(&self) -> usize {
        self.state().pending.len()
    }

    /// Run the registration the drain started for `sender`. Never blocks a thread; bounded by
    /// [`ANNOUNCE_WAIT`] plus the credentials deadline.
    pub async fn complete(&self, sender: Arc<str>) {
        let pid = self.credentials.pid_of(&sender).await;
        let owner_known = pid.map(Owner::Pid);
        let limit = if pid.is_some() {
            MAX_NAMES_PER_OWNER
        } else {
            MAX_UNKNOWN_NAMES
        };
        if self.state().names_per_group.get(&pid).copied().unwrap_or(0) >= limit {
            // One process may not hold arbitrarily many registered names: this is not served.
            tracing::warn!(
                sender = &*sender,
                "too many bus names for one process; ignoring"
            );
            let mut st = self.state();
            let dropped = st.drop_parked(&sender);
            st.ignored.insert(Arc::clone(&sender));
            drop(st);
            self.bump(|s| {
                s.ignored += 1;
                s.parked_dropped += dropped as u64;
            });
            return;
        }
        let info = AppInfo {
            // Replaced by `AppLifecycle::Appeared` once the app root has answered.
            toolkit: Arc::from("unknown"),
            app_name: Arc::from(&*sender),
            pid,
            connector_kind: ConnectorKind::Atspi,
        };
        let deadline = Deadline::after(&self.clock, ANNOUNCE_WAIT);
        let announced = self
            .registrar
            .announce(ScopeKey(Arc::clone(&sender)), info, deadline)
            .await;
        match announced {
            Ok(port) => self.publish(&sender, port, owner_known),
            Err(e) => {
                tracing::warn!(sender = &*sender, error = %e, "application announce failed");
                self.fail(&sender, e == RegistrarError::Duplicate);
            }
        }
    }

    /// Registration failed: drop the parked events and back off before the next attempt.
    fn fail(&self, sender: &str, _duplicate: bool) {
        let now = self.clock.now();
        let mut st = self.state();
        let dropped = st.drop_parked(sender);
        let delay = st
            .backoff
            .get(sender)
            .map_or(BACKOFF_MIN, |b| (b.delay * 2).min(BACKOFF_MAX));
        if st.backoff.len() >= MAX_BACKOFFS {
            st.backoff.clear();
        }
        st.backoff.insert(
            Arc::from(sender),
            Backoff {
                not_before: now + delay,
                delay,
            },
        );
        drop(st);
        self.bump(|s| {
            s.announce_failed += 1;
            s.parked_dropped += dropped as u64;
        });
    }

    /// Publish under the lock `forget` takes: check "departed", hand the port over, replay the
    /// parked events in order and insert the route as one step.
    fn publish(&self, sender: &Arc<str>, port: AppPort, owner: Option<Owner>) {
        let app = port.app;
        let group = match owner {
            Some(Owner::Pid(p)) => Some(p),
            Some(Owner::Unknown(_)) | None => None,
        };
        let owner = owner.unwrap_or(Owner::Unknown(app));
        let route = AppRoute {
            app,
            tx: port.ingress_sender(),
        };
        let gone = CancellationToken::new();
        let new = NewApp {
            port,
            bus_name: Arc::clone(sender),
            gone: gone.clone(),
        };
        let mut st = self.state();
        if st.departed.contains(&**sender) {
            let dropped = st.drop_parked(sender);
            drop(st);
            // The app left the bus while the core was accepting it. Retiring drops the core half
            // (ingress, budget); dropping `new` closes the port.
            tracing::debug!(
                sender = &**sender,
                ?app,
                "application vanished during registration"
            );
            let _ = self.registrar.retire(app);
            self.bump(|s| {
                s.announce_failed += 1;
                s.parked_dropped += dropped as u64;
            });
            return;
        }
        if self.new_apps.try_send(new).is_err() {
            drop(st);
            tracing::warn!(
                sender = &**sender,
                "connector is not accepting applications"
            );
            let _ = self.registrar.retire(app);
            self.fail(sender, false);
            return;
        }
        // A global cap on applications: a process that forks to multiply its per-process caps
        // meets this one, and the largest group gives way, not the newcomer's.
        let mut evicted: Option<Entry> = None;
        if st.apps.len() >= MAX_APPS_TOTAL {
            match st.largest_group() {
                Some((big, _)) if big == group => {
                    // The newcomer belongs to the largest group: it is the one refused.
                    let dropped = st.drop_parked(sender);
                    st.ignored.insert(Arc::clone(sender));
                    drop(st);
                    tracing::warn!(sender = &**sender, "application limit reached; refusing");
                    let _ = self.registrar.retire(app);
                    self.bump(|s| {
                        s.ignored += 1;
                        s.parked_dropped += dropped as u64;
                    });
                    return;
                }
                Some((big, _)) => evicted = st.evict_from(big),
                None => {}
            }
        }
        self.paths.bind_app(app, owner);
        // Replay what arrived while the registration ran, in arrival order, before any later
        // event can be routed (the lock is held).
        let pending = st.pending.remove(&**sender).unwrap_or_default();
        st.parked_bytes = st.parked_bytes.saturating_sub(pending.bytes);
        // Non-focus events first, in arrival order, and the newest focus last.
        let parked: Vec<Parked> = pending.events.into_iter().chain(pending.focus).collect();
        let mut replayed = 0u64;
        let mut mapped = smallvec::SmallVec::<[NodeEvent; 2]>::new();
        for p in &parked {
            let view = SignalView {
                interface: &p.interface,
                member: &p.member,
                path: &p.path,
                body: &p.body,
                big_endian: p.big_endian,
            };
            mapped.clear();
            if map_signal(app, &view, &mut mapped).is_err() {
                continue;
            }
            for ev in mapped.drain(..) {
                if let NodeEvent::FocusChanged { node } = &ev {
                    self.paths.insert(*node, &p.path);
                }
                route.tx.push(ev);
                replayed += 1;
            }
        }
        st.backoff.remove(&**sender);
        *st.names_per_group.entry(group).or_insert(0) += 1;
        st.apps.insert(
            Arc::clone(sender),
            Entry {
                route: route.clone(),
                gone,
                group,
            },
        );
        drop(st);
        if let Some(e) = evicted {
            tracing::warn!(app = ?e.route.app, "application limit reached; evicting the largest group");
            e.gone.cancel();
            self.paths.forget_app(e.route.app);
            let _ = self.registrar.retire(e.route.app);
        }
        tracing::debug!(sender = &**sender, ?app, replayed, "application announced");
        self.bump(|s| {
            s.announced += 1;
            s.parked_replayed += replayed;
        });
    }
}

impl Router for AtspiRouter {
    fn route(&self, sender: &str) -> Option<AppRoute> {
        match self.resolve(sender) {
            RouteDecision::Route(r) => Some(r),
            RouteDecision::NotAnApp | RouteDecision::Retry => None,
        }
    }

    fn resolve(&self, sender: &str) -> RouteDecision {
        let now = self.clock.now();
        let mut st = self.state();
        if st.ignored.contains(sender) {
            drop(st);
            self.bump(|s| s.ignored += 1);
            return RouteDecision::NotAnApp;
        }
        if st.departed.contains(sender) {
            return RouteDecision::NotAnApp;
        }
        if let Some(e) = st.apps.get(sender) {
            return RouteDecision::Route(e.route.clone());
        }
        // Bus unique names start with ':'; a well-known name never emits signals.
        if !sender.starts_with(':') {
            return RouteDecision::NotAnApp;
        }
        if st.pending.contains_key(sender) {
            return RouteDecision::Retry;
        }
        if st.backoff.get(sender).is_some_and(|b| now < b.not_before) {
            return RouteDecision::Retry;
        }
        if st.pending.len() >= MAX_PENDING {
            return RouteDecision::Retry;
        }
        let name: Arc<str> = Arc::from(sender);
        if self.to_register.try_send(Arc::clone(&name)).is_err() {
            // The connector task is not taking registrations: back off like a failure.
            let delay = st
                .backoff
                .get(sender)
                .map_or(BACKOFF_MIN, |b| (b.delay * 2).min(BACKOFF_MAX));
            st.backoff.insert(
                name,
                Backoff {
                    not_before: now + delay,
                    delay,
                },
            );
            return RouteDecision::Retry;
        }
        let pending = Pending {
            focus: st.orphan_focus.remove(sender),
            ..Pending::default()
        };
        st.pending.insert(name, pending);
        RouteDecision::Retry
    }

    fn park(&self, sender: &str, signal: &SignalView<'_>) {
        // Ingress limits: oversized strings are refused before anything is copied.
        if signal.body.len() > MAX_PARKED_BODY
            || signal.path.len() > MAX_PATH_BYTES
            || signal.interface.len() > MAX_NAME_BYTES
            || signal.member.len() > MAX_NAME_BYTES
        {
            self.bump(|s| s.parked_dropped += 1);
            return;
        }
        let focus = is_focus(signal);
        let mut st = self.state();
        // The registration may have been published between the drain's `resolve` (Retry) and
        // this call: deliver straight through the route instead of losing the event.
        if let Some(e) = st.apps.get(sender) {
            let route = e.route.clone();
            let mut mapped = smallvec::SmallVec::<[NodeEvent; 2]>::new();
            if map_signal(route.app, signal, &mut mapped).is_ok() {
                for ev in mapped.drain(..) {
                    if let NodeEvent::FocusChanged { node } = &ev {
                        self.paths.insert(*node, signal.path);
                    }
                    route.tx.push(ev);
                }
            }
            return;
        }
        let entry = Parked::of(signal);
        let parked_now = st.parked_bytes;
        let Some(p) = st.pending.get_mut(sender) else {
            // No registration is running (backoff, pending cap): keep the newest focus for the
            // next attempt, drop the rest.
            let keep = focus
                && sender.starts_with(':')
                && !st.departed.contains(sender)
                && !st.ignored.contains(sender);
            if keep {
                if st.orphan_focus.len() >= MAX_ORPHAN_FOCUS
                    && !st.orphan_focus.contains_key(sender)
                {
                    st.orphan_focus.clear();
                }
                st.orphan_focus.insert(Arc::from(sender), entry);
            } else {
                drop(st);
                self.bump(|s| s.parked_dropped += 1);
            }
            return;
        };
        if focus {
            // The newest focus replaces the older one and costs nothing against any cap.
            p.focus = Some(entry);
            return;
        }
        let cost = entry.cost();
        if cost > PARK_BYTES_PER_SENDER || parked_now + cost > PARK_BYTES_TOTAL {
            drop(st);
            self.bump(|s| s.parked_dropped += 1);
            return;
        }
        p.bytes += cost;
        p.events.push_back(entry);
        let mut dropped = 0u64;
        let mut freed = 0usize;
        while p.events.len() > MAX_PARKED_PER_SENDER || p.bytes > PARK_BYTES_PER_SENDER {
            let Some(old) = p.events.pop_front() else {
                break;
            };
            p.bytes = p.bytes.saturating_sub(old.cost());
            freed += old.cost();
            dropped += 1;
        }
        st.parked_bytes = (parked_now + cost).saturating_sub(freed);
        drop(st);
        if dropped > 0 {
            self.bump(|s| s.parked_dropped += dropped);
        }
    }

    fn focus_seen(&self, _app: AppId, node: NodeKey, path: &str) {
        self.paths.insert(node, path);
    }
}

#[cfg(test)]
mod tests;
