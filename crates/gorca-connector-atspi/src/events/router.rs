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
use gorca_dbus::drain::{
    AppRoute, RouteDecision, Router, SignalView, map_signal, needs_hint_path, needs_path,
};
use gorca_model::{AppId, AppInfo, ConnectorKind, NodeEvent, NodeKey};
use gorca_rt::{BoxFuture, CancellationToken, Clock, Deadline, FocusedApp, mpsc};

use crate::paths::{MAX_PATH_BYTES, Owner, PathTable};
use crate::proc::{ProcInfo, read_proc_info_async};

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
/// Rounds of "pick a victim, re-validate" before the newcomer is refused instead.
const PICK_ATTEMPTS: usize = 3;
/// Attempts to learn a connection's owner before it is filed under "unknown".
const CREDENTIALS_TRIES: usize = 3;
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
    /// The registration matched the protected Shell identity (`State::is_shell`).
    pub verified_shell: bool,
    /// `(owner pid, process start time)` of the registration, to be checked against the Shell
    /// again when the protected process changes after publication (the first Shell signal can
    /// be routed before `org.gnome.Shell` has been resolved).
    pub identity: Option<(u32, u64)>,
    /// The protected Shell process as it changes.
    pub shell_rx: tokio::sync::watch::Receiver<Option<(u32, u64)>>,
    /// The route's hint flag: set when the registration is verified as the Shell later (N-4).
    pub hints: Arc<std::sync::atomic::AtomicBool>,
}

/// Who owns a bus connection (`org.freedesktop.DBus.GetConnectionCredentials`).
pub trait Credentials: Send + Sync + 'static {
    /// The process id of `name`, or `None` if the bus cannot or will not say (deadline inside).
    fn pid_of<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<u32>>;

    /// Process group, systemd unit and start time of `pid`, read when the application registers.
    /// The default reads `/proc` on a blocking thread under a deadline; tests supply their own.
    fn proc_info(&self, pid: u32) -> BoxFuture<'_, ProcInfo> {
        Box::pin(read_proc_info_async(pid))
    }
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

    fn proc_info(&self, _pid: u32) -> BoxFuture<'_, ProcInfo> {
        Box::pin(async { ProcInfo::default() })
    }
}

#[derive(Debug)]
struct Entry {
    route: AppRoute,
    gone: CancellationToken,
    /// The process (pid) the name limit counts against.
    group: Option<u32>,
    /// The process group the application cap counts against (a fork attack shares it).
    evg: Option<u64>,
    /// Start time of the process: with the pid, its identity (pids are reused).
    start: Option<u64>,
    /// Registration order; the highest is the newest.
    seq: u64,
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
    /// Registered names per process (`None`: credentials unknown, all such names together),
    /// **including names whose registration is in flight** (reserve, then commit).
    names_per_group: HashMap<Option<u32>, usize>,
    /// Registered names per process group (the unit of the application cap).
    names_per_evg: HashMap<Option<u64>, usize>,
    next_seq: u64,
    /// The one process that is never evicted or refused: the current owner of `org.gnome.Shell`,
    /// as `(pid, start time)`.
    protected_process: Option<(u32, u64)>,
    /// Set while protection cannot be verified: nothing is evicted.
    eviction_frozen: bool,
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

    /// Reserve a name for `group` if it has room (check and increment as one step).
    fn reserve_name(&mut self, group: Option<u32>) -> bool {
        let limit = if group.is_some() {
            MAX_NAMES_PER_OWNER
        } else {
            MAX_UNKNOWN_NAMES
        };
        let n = self.names_per_group.entry(group).or_insert(0);
        if *n >= limit {
            return false;
        }
        *n += 1;
        true
    }

    fn is_shell(&self, group: Option<u32>, start: Option<u64>) -> bool {
        match (self.protected_process, group, start) {
            (Some((pid, st)), Some(g), Some(s)) => pid == g && st == s,
            _ => false,
        }
    }

    fn forget_evg(&mut self, evg: Option<u64>) {
        if let Some(n) = self.names_per_evg.get_mut(&evg) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.names_per_evg.remove(&evg);
            }
        }
    }

    /// Who gives way when the application cap is reached.
    ///
    /// Policy (documented because it is a security decision):
    ///
    /// 1. Never the focused application (and the previous one during its grace), and never the
    ///    shell's process (identified by pid *and* start time).
    /// 2. Only a process group **strictly larger than the newcomer's own** gives way, and
    ///    the biggest such group first (ties: the one that registered last); within it, its
    ///    newest name. A fork attack shares one process group and so is the largest; an
    ///    application the user just launched is a group of one and wins against it, and it gains
    ///    focus-based protection as soon as the Reader accepts its focus.
    /// 3. If no group is larger than the newcomer's (all singletons, or the newcomer's own group is
    ///    the biggest), **the newcomer is refused** (with a backoff). Evicting the newest
    ///    existing application instead would let an attacker, by registering one more name,
    ///    evict a legitimate application that had just launched.
    /// 4. The shell, as a newcomer, is never refused: it takes the biggest group above one name,
    ///    else the newest unprotected application, else is admitted over the cap.
    fn pick_victim(&self, newcomer: &Newcomer, focused: Option<&FocusedApp>) -> Victim {
        if self.eviction_frozen {
            // Protection is unverified: evict nobody. The shell is still never refused.
            return if newcomer.shell {
                Victim::AdmitOverCap
            } else {
                Victim::RefuseNewcomer
            };
        }
        let protected = |e: &Entry| {
            focused.is_some_and(|f| f.protects(e.route.app)) || self.is_shell(e.group, e.start)
        };
        let candidates: Vec<(&Arc<str>, &Entry)> =
            self.apps.iter().filter(|(_, e)| !protected(e)).collect();
        let count = |g: Option<u64>| self.names_per_evg.get(&g).copied().unwrap_or(0);
        // The newcomer joins its group: only a group bigger than that gives way.
        let floor = if newcomer.shell {
            1
        } else {
            count(newcomer.evg) + 1
        };
        let big = candidates
            .iter()
            .filter(|(_, e)| count(e.evg) > floor)
            .map(|(_, e)| e)
            .max_by_key(|e| (count(e.evg), e.seq))
            .map(|e| e.evg);
        let victim = match big {
            Some(g) => candidates
                .iter()
                .filter(|(_, e)| e.evg == g)
                .max_by_key(|(_, e)| e.seq),
            None if newcomer.shell => candidates.iter().max_by_key(|(_, e)| e.seq),
            None => return Victim::RefuseNewcomer,
        };
        match victim {
            Some((name, _)) => Victim::Evict(Arc::clone(name)),
            None if newcomer.shell => Victim::AdmitOverCap,
            None => Victim::RefuseNewcomer,
        }
    }
}

/// What `pick_victim` needs to know about the registration that triggered the decision.
#[derive(Copy, Clone, Debug)]
struct Newcomer {
    evg: Option<u64>,
    shell: bool,
}

/// Outcome of [`State::pick_victim`].
#[derive(Debug, Eq, PartialEq)]
enum Victim {
    Evict(Arc<str>),
    RefuseNewcomer,
    /// Only protected applications are registered and the newcomer is one: admit it anyway.
    AdmitOverCap,
}

/// Counters, for tests and diagnostics.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RouterStats {
    pub announced: u64,
    pub announce_failed: u64,
    pub ignored: u64,
    pub parked_replayed: u64,
    pub parked_dropped: u64,
    pub evicted: u64,
}

pub struct AtspiRouter {
    registrar: AppRegistrar,
    clock: Clock,
    paths: Arc<PathTable>,
    new_apps: mpsc::Sender<NewApp>,
    /// Senders whose registration the connector task must run.
    to_register: mpsc::Sender<Arc<str>>,
    credentials: Arc<dyn Credentials>,
    /// The focused application, published by the Reader (lock-free).
    focused: Option<Arc<FocusedApp>>,
    /// The protected Shell process, published as it changes.
    shell_tx: tokio::sync::watch::Sender<Option<(u32, u64)>>,
    state: Mutex<State>,
    stats: Mutex<RouterStats>,
    /// Test hook run between choosing a victim and committing to it.
    #[cfg(test)]
    commit_hook: Mutex<Option<Box<dyn Fn() + Send>>>,
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
            focused: None,
            shell_tx: tokio::sync::watch::channel(None).0,
            state: Mutex::new(State::default()),
            stats: Mutex::new(RouterStats::default()),
            #[cfg(test)]
            commit_hook: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn run_commit_hook(&self) {
        if let Some(h) = self
            .commit_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            h();
        }
    }

    /// Read the focused application from `focused` when deciding who gives way at the cap.
    #[must_use]
    pub fn with_focused(mut self, focused: Arc<FocusedApp>) -> Self {
        self.focused = Some(focused);
        self
    }

    /// Protect the shell: the process `(pid, start time)` that currently owns
    /// `org.gnome.Shell`, or nothing. It replaces any earlier value, so a process that merely
    /// inherits a recycled pid is not protected (its start time differs).
    pub fn set_protected_process(&self, process: Option<(u32, u64)>) {
        self.state().protected_process = process;
        self.shell_tx.send_replace(process);
    }

    /// The protected Shell process as it changes (`None` until it is known).
    pub fn shell_watch(&self) -> tokio::sync::watch::Receiver<Option<(u32, u64)>> {
        self.shell_tx.subscribe()
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
                st.forget_evg(e.evg);
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
            st.names_per_evg.clear();
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

    /// Reconciliation against the bus (run every few seconds): every registered application
    /// whose bus name is not in `live` (the a11y bus's `ListNames`) is forgotten and retired. This
    /// catches a `NameOwnerChanged` that was never delivered, so no route, quota, name count or
    /// protection survives a name that is gone. Returns the retired applications.
    pub fn reconcile_with(&self, live: &HashSet<String>) -> Vec<AppId> {
        let dead: Vec<Arc<str>> = self
            .state()
            .apps
            .keys()
            .filter(|n| !live.contains(&***n))
            .cloned()
            .collect();
        let mut retired = Vec::new();
        for name in dead {
            if let Some(app) = self.forget(&name) {
                if let Some(f) = &self.focused {
                    f.forget(app);
                }
                let _ = self.registrar.retire(app);
                retired.push(app);
            }
        }
        retired
    }

    /// Protection could not be re-validated (the bus did not answer `ListNames`): **freeze
    /// eviction**. The last validated protection is kept, not cleared, because clearing it would
    /// make exactly the focused application and the shell evictable at the moment nobody can
    /// check anything. While frozen, a newcomer at the application cap is refused with a backoff
    /// and nobody is evicted (the shell, the one newcomer that is never refused, is admitted over
    /// the cap). Reading is unaffected: every registered application is still served.
    pub fn freeze_eviction(&self) {
        self.state().eviction_frozen = true;
    }

    /// The bus answered again: eviction may resume.
    pub fn resume_eviction(&self) {
        self.state().eviction_frozen = false;
    }

    pub fn eviction_frozen(&self) -> bool {
        self.state().eviction_frozen
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
        // The bus may be slow to answer under load; retry before treating the connection as one
        // of unknown ownership (which is a small, shared, capped group).
        let mut pid = None;
        for _ in 0..CREDENTIALS_TRIES {
            pid = self.credentials.pid_of(&sender).await;
            if pid.is_some() {
                break;
            }
        }
        let owner_known = pid.map(Owner::Pid);
        // `/proc` is read on a blocking thread, under a deadline.
        let info_of = match pid {
            Some(p) => self.credentials.proc_info(p).await,
            None => ProcInfo::default(),
        };
        // Reserve the name before the first await that matters; give it back on any failure.
        let reserved = self.state().reserve_name(pid);
        if !reserved {
            // One process may not hold arbitrarily many registered names. This is a temporary
            // verdict: back off and try again when a name has gone, never cache it.
            tracing::warn!(sender = &*sender, "too many bus names for one process");
            self.fail(&sender, false);
            return;
        }
        let info = AppInfo {
            // Replaced by `AppLifecycle::Appeared` once the app root has answered.
            toolkit: Arc::from("unknown"),
            app_name: Arc::from(&*sender),
            pid,
            connector_kind: ConnectorKind::Atspi,
            verified_shell: false,
            dump_complete: false,
            text_unit: gorca_model::TextUnit::Scalar,
            text_insert_slack: 0,
            root: None,
        };
        let deadline = Deadline::after(&self.clock, ANNOUNCE_WAIT);
        let announced = self
            .registrar
            .announce(ScopeKey(Arc::clone(&sender)), info, deadline)
            .await;
        match announced {
            Ok(port) => self.publish(&sender, port, owner_known, &info_of),
            Err(e) => {
                tracing::warn!(sender = &*sender, error = %e, "application announce failed");
                self.state().release_name(pid);
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
    fn publish(&self, sender: &Arc<str>, port: AppPort, owner: Option<Owner>, info: &ProcInfo) {
        let app = port.app;
        let group = match owner {
            Some(Owner::Pid(p)) => Some(p),
            Some(Owner::Unknown(_)) | None => None,
        };
        let owner = owner.unwrap_or(Owner::Unknown(app));
        let verified_shell = self.state().is_shell(group, info.start);
        let route = AppRoute {
            app,
            tx: port.ingress_sender(),
            hints: Arc::new(std::sync::atomic::AtomicBool::new(verified_shell)),
        };
        let gone = CancellationToken::new();
        let new = NewApp {
            port,
            bus_name: Arc::clone(sender),
            gone: gone.clone(),
            verified_shell,
            identity: group.zip(info.start),
            shell_rx: self.shell_tx.subscribe(),
            hints: Arc::clone(&route.hints),
        };
        let focused = self.focused.as_deref();
        // Fall back to the pid as the cap group when the process group is unknown.
        let evg = info.group_key().or(group.map(u64::from));
        let mut st = self.state();
        let newcomer = Newcomer {
            evg,
            shell: st.is_shell(group, info.start),
        };
        if st.departed.contains(&**sender) {
            st.release_name(group);
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
        // A global cap on applications: a process that forks to multiply its per-process caps
        // meets this one. The focused application and protected processes never give way.
        let mut victim: Option<Arc<str>> = None;
        if st.apps.len() >= MAX_APPS_TOTAL {
            // The decision is made on a snapshot of who is protected; the Reader can change that
            // at any moment. Re-validate at the moment of commit with a generation counter, and
            // decide again if anything moved (fail closed: still moving after a few rounds means
            // the newcomer is refused).
            let mut decision = Victim::RefuseNewcomer;
            for _ in 0..PICK_ATTEMPTS {
                let generation = focused.map(FocusedApp::generation);
                decision = st.pick_victim(&newcomer, focused);
                #[cfg(test)]
                self.run_commit_hook();
                let unchanged = generation == focused.map(FocusedApp::generation);
                let still_ok = match &decision {
                    Victim::Evict(name) => st
                        .apps
                        .get(name)
                        .is_none_or(|e| !focused.is_some_and(|f| f.protects(e.route.app))),
                    Victim::AdmitOverCap | Victim::RefuseNewcomer => true,
                };
                if unchanged && still_ok {
                    break;
                }
                decision = Victim::RefuseNewcomer;
            }
            match decision {
                Victim::Evict(name) => victim = Some(name),
                Victim::AdmitOverCap => {}
                Victim::RefuseNewcomer => {
                    st.release_name(group);
                    drop(st);
                    tracing::warn!(sender = &**sender, "application limit reached; backing off");
                    let _ = self.registrar.retire(app);
                    // Temporary: the sender is retried after a backoff, never cached as refused.
                    self.fail(sender, false);
                    return;
                }
            }
        }
        if self.new_apps.try_send(new).is_err() {
            st.release_name(group);
            drop(st);
            tracing::warn!(
                sender = &**sender,
                "connector is not accepting applications"
            );
            let _ = self.registrar.retire(app);
            self.fail(sender, false);
            return;
        }
        let evicted = victim.and_then(|v| {
            let e = st.apps.remove(&v)?;
            st.release_name(e.group);
            st.forget_evg(e.evg);
            Some((v, e))
        });
        self.paths.bind_app(app, owner);
        if verified_shell {
            self.paths.allow_hints(app);
        }
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
                if let Some(node) = needs_path(&ev) {
                    self.paths.insert(node, &p.path);
                } else if let Some(node) = needs_hint_path(&ev) {
                    self.paths.insert_hint(node, &p.path);
                }
                route.tx.push(ev);
                replayed += 1;
            }
        }
        st.backoff.remove(&**sender);
        // The name was reserved in `complete`; the entry commits it.
        st.next_seq += 1;
        let seq = st.next_seq;
        st.apps.insert(
            Arc::clone(sender),
            Entry {
                route: route.clone(),
                gone,
                group,
                evg,
                start: info.start,
                seq,
            },
        );
        *st.names_per_evg.entry(evg).or_insert(0) += 1;
        if let Some((name, _)) = &evicted {
            // The evicted application re-announces on its next event (the drain notices that its
            // ingress is closed), through the backoff path if the table is still full.
            let delay = st
                .backoff
                .get(&**name)
                .map_or(BACKOFF_MIN, |b| (b.delay * 2).min(BACKOFF_MAX));
            st.backoff.insert(
                Arc::clone(name),
                Backoff {
                    not_before: self.clock.now() + delay,
                    delay,
                },
            );
        }
        drop(st);
        if let Some((name, e)) = evicted {
            tracing::warn!(app = ?e.route.app, name = &*name, "application limit reached; evicting");
            e.gone.cancel();
            self.paths.forget_app(e.route.app);
            let _ = self.registrar.retire(e.route.app);
            self.bump(|s| s.evicted += 1);
        }
        tracing::debug!(sender = &**sender, ?app, replayed, "application announced");
        self.bump(|s| {
            s.announced += 1;
            s.parked_replayed += replayed;
        });
    }
}

impl AtspiRouter {
    /// Start registering `sender` (an application announced by the registry) without waiting for
    /// its first event. Idempotent: a known, pending, ignored or backing-off name is left alone.
    pub fn discover(&self, sender: &str) {
        let _ = self.resolve(sender);
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
                    if let Some(node) = needs_path(&ev) {
                        self.paths.insert(node, signal.path);
                    } else if let Some(node) = needs_hint_path(&ev) {
                        self.paths.insert_hint(node, signal.path);
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

    fn hint_seen(&self, _app: AppId, node: NodeKey, path: &str) {
        self.paths.insert_hint(node, path);
    }
}

#[cfg(test)]
mod tests;
