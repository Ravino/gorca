//! Event-connection drain: the B-2 interface (ADR-0001 sections 5.3 and 6.3).
//!
//! A drain owns one or more D-Bus connections to the a11y bus, installs the match rules, reads
//! every broadcast AT-SPI signal as fast as the socket delivers it, maps it with [`map_signal`]
//! and calls `IngressSender::push` on the ingress of the sending app. It never awaits
//! downstream (D-1), never sends anything but `Hello`/`AddMatch` (D-3) and never blocks the
//! socket read on a slow consumer. The exact contract and the pass thresholds are in
//! `docs/bakeoff/b2-ingress/SPEC.md`.
//!
//! [`spawn_drain`] is implemented by B-2 candidate A: a raw-socket reader with its own D-Bus
//! framing (modules `raw` and `wire`) feeding the lock-free ring of `gorca-ingress`.

mod map;
mod raw;
mod wire;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use gorca_ingress::{
    GLOBAL_SLOT_BUDGET, IngressConfig, IngressReceiver, IngressSender, SlotBudget,
    ingress_with_budget,
};
use gorca_model::AppId;

pub use map::{MAX_ANNOUNCEMENT_BYTES, Malformed, SignalView, local_id, map_signal};

/// Largest per-connection sender cache; beyond it the cache is dropped and rebuilt (hostile
/// churn of bus names must not grow memory).
pub const MAX_ROUTES: usize = 4096;
pub use raw::spawn_drain;

/// Answer of [`Router::resolve`].
#[derive(Clone, Debug)]
pub enum RouteDecision {
    /// Events of this sender go here.
    Route(AppRoute),
    /// Not an application (the registry, a well-known name, an ignored peer): cache it.
    NotAnApp,
    /// Not decided yet; do not cache. The event is parked or dropped.
    Retry,
}

/// Where events of one sender go.
#[derive(Clone, Debug)]
pub struct AppRoute {
    pub app: AppId,
    pub tx: IngressSender,
}

/// Creates the per-app ingresses of one process. Every ingress made here is charged to the one
/// shared [`SlotBudget`] (SPEC 3.6, 3.7): a [`Router`] must create the ingress of each new app
/// through this factory, never with a bare `gorca_ingress::ingress`, so that a flood from many
/// applications is bounded as a whole and no application can starve another.
#[derive(Clone, Debug)]
pub struct IngressFactory {
    cfg: IngressConfig,
    budget: Arc<SlotBudget>,
}

impl IngressFactory {
    /// The production budget ([`GLOBAL_SLOT_BUDGET`] slots) and default per-app limits.
    pub fn new() -> Self {
        Self::with_budget(
            IngressConfig::default(),
            SlotBudget::new(GLOBAL_SLOT_BUDGET),
        )
    }

    /// A factory over an explicit budget (tests, or several routers sharing one budget).
    pub fn with_budget(cfg: IngressConfig, budget: Arc<SlotBudget>) -> Self {
        Self { cfg, budget }
    }

    /// The ingress of a newly seen app. Dropping the receiver returns its slots.
    pub fn new_app(&self) -> (IngressSender, IngressReceiver) {
        ingress_with_budget(self.cfg, &self.budget)
    }

    pub fn budget(&self) -> &Arc<SlotBudget> {
        &self.budget
    }
}

impl Default for IngressFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// Resolves a bus unique name (`:1.42`) to its ingress.
///
/// Called on a drain thread, **at most once per distinct sender per drain connection**: the
/// drain caches the answer, including `None`. It must not block for long. `None` means "not an
/// app we track": the signal is counted as unrouted and dropped.
pub trait Router: Send + Sync + 'static {
    /// Simple form: a route or "not an application". Routers that can fail transiently or
    /// register asynchronously implement [`Router::resolve`] instead.
    fn route(&self, sender: &str) -> Option<AppRoute>;

    /// The drain calls this. `Route` and `NotAnApp` are cached for the connection; `Retry` never
    /// is, and the drain asks again on the sender's next event, so a transient failure (core
    /// busy, registration in flight) cannot mute an application permanently. Must not block.
    fn resolve(&self, sender: &str) -> RouteDecision {
        match self.route(sender) {
            Some(r) => RouteDecision::Route(r),
            None => RouteDecision::NotAnApp,
        }
    }

    /// An event of a sender that answered `Retry`, handed over so the router can keep it until
    /// the registration completes (bounded). The default drops it; the drain counts it either way.
    fn park(&self, _sender: &str, _signal: &SignalView<'_>) {}

    /// Called on the drain thread, before the event is pushed, for every `FocusChanged` the
    /// drain maps. `path` is the object path of the signal, of which `node.local` is the hash
    /// ([`local_id`]): a connector that must call the object later records the reverse mapping
    /// here. Rare (focus only) and must be O(1) and non-blocking; the default does nothing.
    fn focus_seen(&self, _app: AppId, _node: gorca_model::NodeKey, _path: &str) {}
}

#[derive(Clone, Debug)]
pub struct DrainConfig {
    /// D-Bus address of the bus, e.g. `unix:path=/run/user/1000/at-spi/bus`.
    pub address: String,
    /// Match rules to install with `AddMatch`. Defaults to [`default_match_rules`].
    pub match_rules: Vec<String>,
    /// Minimum bytes asked of one `recv` (D-1).
    pub recv_bytes: usize,
    /// `SO_RCVBUF` to request, if any (PC-6).
    pub rcvbuf_bytes: Option<usize>,
    /// `SIOCINQ` backlog that raises a shed signal (D-2).
    pub shed_watermark: usize,
    /// Deadline for connect + authentication + `Hello` + all `AddMatch` calls (Startup class).
    pub connect_deadline: Duration,
    /// Upper bound on event connections a candidate may open (B-2 arm c). 1 = a single drain.
    pub max_event_connections: u8,
}

impl DrainConfig {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            match_rules: default_match_rules(),
            recv_bytes: crate::DRAIN_RECV_BYTES,
            rcvbuf_bytes: Some(4 * 1024 * 1024),
            shed_watermark: crate::SHED_WATERMARK_BYTES,
            connect_deadline: Duration::from_secs(5),
            max_event_connections: 1,
        }
    }
}

/// One rule per AT-SPI event interface gorca subscribes to.
pub fn default_match_rules() -> Vec<String> {
    ["Object", "Window", "Focus", "Document"]
        .iter()
        .map(|c| format!("type='signal',interface='org.a11y.atspi.Event.{c}'"))
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    #[error("connecting to the bus failed: {0}")]
    Connect(String),
    #[error("start-up exceeded its deadline")]
    Deadline,
    #[error("drain thread could not be spawned: {0}")]
    Spawn(#[from] std::io::Error),
}

/// How the drain ended, or `Running`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DrainEnd {
    Running,
    /// `DrainHandle::stop` was called.
    Stopped,
    /// EOF or a socket error: the broker closed the connection (F-3). Must never happen in B-2.
    Disconnected(String),
}

/// Counters every candidate maintains. All relaxed; read through [`DrainShared::snapshot`].
#[derive(Debug, Default)]
pub struct DrainCounters {
    /// Complete D-Bus messages framed.
    pub messages: AtomicU64,
    /// Bytes read from the socket(s).
    pub bytes: AtomicU64,
    /// `recv` calls (or batches handed to the framer).
    pub batches: AtomicU64,
    /// `IngressSender::push` calls made. Must equal the number of mapped events.
    pub pushed: AtomicU64,
    /// Signals whose sender had no route.
    pub unrouted: AtomicU64,
    /// Signals of senders the router answered `Retry` for (parked by the router or dropped).
    pub deferred: AtomicU64,
    /// Messages that were valid but not mapped (other interfaces, replies, unknown members).
    pub ignored: AtomicU64,
    /// Signals with a body that did not parse.
    pub malformed: AtomicU64,
    /// Largest `SIOCINQ` seen (D-2). 0 when the implementation does not sample.
    pub backlog_max: AtomicU64,
    /// Times the shed watermark was crossed upwards.
    pub shed_starts: AtomicU64,
}

/// Plain copy of the counters plus the connection state.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DrainStats {
    pub messages: u64,
    pub bytes: u64,
    pub batches: u64,
    pub pushed: u64,
    pub unrouted: u64,
    pub deferred: u64,
    pub ignored: u64,
    pub malformed: u64,
    pub backlog_max: u64,
    pub shed_starts: u64,
}

/// State shared between the drain threads and the [`DrainHandle`].
#[derive(Debug)]
pub struct DrainShared {
    pub counters: DrainCounters,
    end: Mutex<DrainEnd>,
    stopping: AtomicBool,
}

impl DrainShared {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            counters: DrainCounters::default(),
            end: Mutex::new(DrainEnd::Running),
            stopping: AtomicBool::new(false),
        })
    }

    pub fn snapshot(&self) -> DrainStats {
        let c = &self.counters;
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        DrainStats {
            messages: g(&c.messages),
            bytes: g(&c.bytes),
            batches: g(&c.batches),
            pushed: g(&c.pushed),
            unrouted: g(&c.unrouted),
            deferred: g(&c.deferred),
            ignored: g(&c.ignored),
            malformed: g(&c.malformed),
            backlog_max: g(&c.backlog_max),
            shed_starts: g(&c.shed_starts),
        }
    }

    /// Record why a drain thread ended. The first non-`Running` value wins.
    pub fn set_end(&self, end: DrainEnd) {
        let mut g = self.end.lock().unwrap_or_else(PoisonError::into_inner);
        if *g == DrainEnd::Running {
            *g = end;
        }
    }

    pub fn end(&self) -> DrainEnd {
        self.end
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub fn raise_backlog(&self, backlog: usize) {
        self.counters
            .backlog_max
            .fetch_max(backlog as u64, Ordering::Relaxed);
    }
}

/// Owner of a running drain.
pub struct DrainHandle {
    shared: Arc<DrainShared>,
    wake: Box<dyn Fn() + Send + Sync>,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for DrainHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DrainHandle")
            .field("end", &self.shared.end())
            .field("threads", &self.threads.len())
            .finish_non_exhaustive()
    }
}

impl DrainHandle {
    /// `wake` must make every drain thread notice `shared.is_stopping()` promptly even when the
    /// socket is idle (shutdown of the fd, an eventfd, a `Notify`, ...).
    pub fn new(
        shared: Arc<DrainShared>,
        wake: Box<dyn Fn() + Send + Sync>,
        threads: Vec<JoinHandle<()>>,
    ) -> Self {
        Self {
            shared,
            wake,
            threads,
        }
    }

    pub fn stats(&self) -> DrainStats {
        self.shared.snapshot()
    }

    pub fn end(&self) -> DrainEnd {
        self.shared.end()
    }

    /// Stop and join. Returns within 1 s on an idle connection (SPEC 6, C-stop).
    pub fn stop(self) -> DrainStats {
        self.shared.stopping.store(true, Ordering::Release);
        self.shared.set_end(DrainEnd::Stopped);
        (self.wake)();
        for t in self.threads {
            let _ = t.join();
        }
        self.shared.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, WindowKind};

    fn window(n: u64) -> NodeEvent {
        NodeEvent::WindowEvent {
            node: NodeKey {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                local: LocalId(n),
            },
            kind: WindowKind::Create,
        }
    }

    /// Every app created by the factory is charged to the one shared budget, and teardown
    /// returns the slots.
    #[test]
    fn apps_of_one_factory_share_one_budget() {
        let f = IngressFactory::with_budget(IngressConfig::default(), SlotBudget::new(1000));
        let (tx1, rx1) = f.new_app();
        let (tx2, rx2) = f.new_app();
        for n in 0..10 {
            tx1.push(window(n));
            tx2.push(window(n));
        }
        assert_eq!(rx1.pending() + rx2.pending(), 20);
        assert_eq!(f.budget().used(), 20);
        drop(rx1);
        assert_eq!(f.budget().used(), 10);
        drop(rx2);
        assert_eq!(f.budget().used(), 0);
    }
}
