//! Event-connection drain: the B-2 interface (ADR-0001 sections 5.3 and 6.3).
//!
//! A drain owns one or more D-Bus connections to the a11y bus, installs the match rules, reads
//! every broadcast AT-SPI signal as fast as the socket delivers it, maps it with [`map_signal`]
//! and calls `IngressSender::push` on the ingress of the sending app. It never awaits
//! downstream (D-1), never sends anything but `Hello`/`AddMatch` (D-3) and never blocks the
//! socket read on a slow consumer. The exact contract and the pass thresholds are in
//! `docs/bakeoff/b2-ingress/SPEC.md`.
//!
//! [`spawn_drain`] is implemented by the **reference** arm (a): zbus `MessageStream` on a
//! dedicated thread plus the `Mutex` coalescer of `gorca-ingress`. Bake-off candidates replace
//! the internals of this module and of `gorca-ingress` and keep every public item below.

mod map;
mod reference;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use gorca_ingress::IngressSender;
use gorca_model::AppId;

pub use map::{MAX_ANNOUNCEMENT_BYTES, Malformed, SignalView, local_id, map_signal};

/// Largest per-connection sender cache; beyond it the cache is dropped and rebuilt (hostile
/// churn of bus names must not grow memory).
pub const MAX_ROUTES: usize = 4096;
pub use reference::spawn_drain;

/// Where events of one sender go.
#[derive(Clone, Debug)]
pub struct AppRoute {
    pub app: AppId,
    pub tx: IngressSender,
}

/// Resolves a bus unique name (`:1.42`) to its ingress.
///
/// Called on a drain thread, **at most once per distinct sender per drain connection**: the
/// drain caches the answer, including `None`. It must not block for long. `None` means "not an
/// app we track": the signal is counted as unrouted and dropped.
pub trait Router: Send + Sync + 'static {
    fn route(&self, sender: &str) -> Option<AppRoute>;
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
