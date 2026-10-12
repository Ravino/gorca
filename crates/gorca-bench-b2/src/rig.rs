//! The consumer side of a run: a router that hands out per-app ingresses, and one consumer
//! thread per app that plays the role of the app actor (wake on `notified`, `drain`, fold).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gorca_dbus::drain::{AppRoute, Router};
use gorca_ingress::{IngressConfig, IngressReceiver, ingress};
use gorca_model::{AppId, ConnectorId};
use tokio::sync::Notify;

use crate::clock::now_ns;
use crate::gen::{FENCE_ID, canon, local_of, node_app, probe_id};
use crate::model::{Ev, Fold};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WakeMode {
    /// `notified().await` then `drain()`: the production path.
    Notify,
    /// Busy `drain()` loop: measures the data path without wake-up latency.
    Spin,
}

#[derive(Clone, Debug)]
pub struct ConsumerCfg {
    pub ingress: IngressConfig,
    pub mode: WakeMode,
    /// Fold every drained batch into a [`Fold`] (correctness runs).
    pub fold: bool,
    /// Check per-node payload monotonicity (ORD-1) while folding.
    pub check_order: bool,
    /// Probe ids `0..probes` are tracked.
    pub probes: u32,
    /// Do not drain between these `now_ns()` instants (hung app actor).
    pub stall: Option<(u64, u64)>,
}

impl Default for ConsumerCfg {
    fn default() -> Self {
        Self {
            ingress: IngressConfig::default(),
            mode: WakeMode::Notify,
            fold: false,
            check_order: false,
            probes: 0,
            stall: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Live {
    pub fence_ns: AtomicU64,
    pub drains: AtomicU64,
    pub events: AtomicU64,
    pub pending_hwm: AtomicUsize,
    pub last_probe: AtomicU64,
}

#[derive(Debug, Default)]
pub struct ConsumerResult {
    pub fold: Fold,
    /// `now_ns()` at which probe `k` was first reflected, or `u64::MAX`.
    pub probe_obs_ns: Vec<u64>,
    pub fence_ns: u64,
    pub drains: u64,
    pub events: u64,
    pub max_batch: usize,
    pub misrouted: u64,
    pub order_violations: u64,
    pub first_order_violation: Option<String>,
    pub flooded_batches: u64,
    pub max_drain_gap_ns: u64,
}

struct Slot {
    role: String,
    app: AppId,
    live: Arc<Live>,
    stop: Arc<AtomicBool>,
    wake: Arc<Notify>,
    thread: Mutex<Option<JoinHandle<ConsumerResult>>>,
}

/// Maps bus unique names to registered apps. Unknown senders are refused (counted by the drain).
pub struct HarnessRouter {
    routes: Mutex<HashMap<String, AppRoute>>,
    slots: Mutex<Vec<Arc<Slot>>>,
    next_serial: AtomicU64,
    pub route_calls: AtomicU64,
}

impl std::fmt::Debug for HarnessRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HarnessRouter").finish_non_exhaustive()
    }
}

impl Router for HarnessRouter {
    fn route(&self, sender: &str) -> Option<AppRoute> {
        self.route_calls.fetch_add(1, Ordering::Relaxed);
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(sender)
            .cloned()
    }
}

impl HarnessRouter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            routes: Mutex::new(HashMap::new()),
            slots: Mutex::new(Vec::new()),
            next_serial: AtomicU64::new(1),
            route_calls: AtomicU64::new(0),
        })
    }

    /// Create the ingress and the consumer thread for `sender`.
    pub fn register(&self, sender: &str, role: &str, cfg: &ConsumerCfg) -> AppId {
        let serial = u32::try_from(self.next_serial.fetch_add(1, Ordering::Relaxed)).unwrap_or(u32::MAX);
        let app = AppId {
            connector: ConnectorId(0),
            serial,
        };
        let (tx, rx) = ingress(cfg.ingress);
        let live = Arc::new(Live::default());
        let stop = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let thread = {
            let (cfg, live, stop, wake) = (cfg.clone(), Arc::clone(&live), Arc::clone(&stop), Arc::clone(&wake));
            std::thread::Builder::new()
                .name(format!("hx-cons-{role}").chars().take(15).collect::<String>())
                .spawn(move || consume(app, &rx, &cfg, &live, &stop, &wake))
                .ok()
        };
        self.slots.lock().unwrap_or_else(PoisonError::into_inner).push(Arc::new(Slot {
            role: role.to_owned(),
            app,
            live,
            stop,
            wake,
            thread: Mutex::new(thread),
        }));
        self.routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(sender.to_owned(), AppRoute { app, tx });
        app
    }

    fn slot(&self, role: &str) -> Option<Arc<Slot>> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|s| s.role == role)
            .cloned()
    }

    pub fn app_of(&self, role: &str) -> Option<AppId> {
        self.slot(role).map(|s| s.app)
    }

    pub fn live(&self, role: &str) -> Option<Arc<Live>> {
        self.slot(role).map(|s| Arc::clone(&s.live))
    }

    /// Wait until the consumer of `role` has seen its fence event.
    pub fn wait_fence(&self, role: &str, timeout: Duration) -> bool {
        let Some(live) = self.live(role) else { return false };
        let end = Instant::now() + timeout;
        while live.fence_ns.load(Ordering::Acquire) == 0 {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// Stop the consumer of `role` (after a final drain) and return its result.
    pub fn finish(&self, role: &str) -> Option<ConsumerResult> {
        let s = self.slot(role)?;
        s.stop.store(true, Ordering::Release);
        s.wake.notify_one();
        let t = s.thread.lock().unwrap_or_else(PoisonError::into_inner).take()?;
        t.join().ok()
    }

    pub fn roles(&self) -> Vec<String> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|s| s.role.clone())
            .collect()
    }
}

impl Drop for HarnessRouter {
    fn drop(&mut self) {
        for s in self.slots.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            s.stop.store(true, Ordering::Release);
            s.wake.notify_one();
            if let Some(t) = s.thread.lock().unwrap_or_else(PoisonError::into_inner).take() {
                let _ = t.join();
            }
        }
    }
}

fn consume(
    app: AppId,
    rx: &IngressReceiver,
    cfg: &ConsumerCfg,
    live: &Live,
    stop: &AtomicBool,
    wake: &Notify,
) -> ConsumerResult {
    let probe_map: HashMap<u64, u32> = (0..cfg.probes).map(|k| (local_of(probe_id(k)), k)).collect();
    let fence_local = local_of(FENCE_ID);
    let mut res = ConsumerResult {
        probe_obs_ns: vec![u64::MAX; cfg.probes as usize],
        ..ConsumerResult::default()
    };
    let mut last_probe_seen: i64 = -1;
    let mut last_caret: HashMap<u64, u32> = HashMap::new();
    let mut last_text: HashMap<u64, u32> = HashMap::new();
    let mut last_drain_end = now_ns();
    let mut step = |res: &mut ConsumerResult| -> usize {
        let d = rx.drain();
        let t = now_ns();
        res.drains += 1;
        live.drains.fetch_add(1, Ordering::Relaxed);
        res.max_batch = res.max_batch.max(d.events.len());
        res.events += d.events.len() as u64;
        live.events.fetch_add(d.events.len() as u64, Ordering::Relaxed);
        if d.stats.flooded {
            res.flooded_batches += 1;
        }
        let mut canon_events: Vec<Ev> = Vec::new();
        for ev in &d.events {
            if let Some(a) = node_app(ev)
                && a != app
            {
                res.misrouted += 1;
            }
            let c = canon(ev);
            if let Ev::Focus(n) = &c {
                if *n == fence_local {
                    res.fence_ns = t;
                    live.fence_ns.store(t.max(1), Ordering::Release);
                } else if let Some(&k) = probe_map.get(n) {
                    let k = i64::from(k);
                    if k > last_probe_seen {
                        for j in (last_probe_seen + 1)..=k {
                            if let Some(slot) = res.probe_obs_ns.get_mut(j as usize)
                                && *slot == u64::MAX
                            {
                                *slot = t;
                            }
                        }
                        last_probe_seen = k;
                        live.last_probe.store(k as u64, Ordering::Relaxed);
                    }
                }
            }
            if cfg.check_order {
                let bad = match &c {
                    Ev::Caret(n, o) => last_caret.insert(*n, *o).filter(|p| o < p).map(|p| format!("caret {n:#x}: {o} after {p}")),
                    Ev::TextIns(n, o, _) | Ev::TextDel(n, o, _) => {
                        last_text.insert(*n, *o).filter(|p| o < p).map(|p| format!("text {n:#x}: {o} after {p}"))
                    }
                    Ev::TextReplaced(n) => {
                        last_text.remove(n);
                        None
                    }
                    _ => None,
                };
                if let Some(b) = bad {
                    res.order_violations += 1;
                    res.first_order_violation.get_or_insert(b);
                }
            }
            if cfg.fold {
                canon_events.push(c);
            }
        }
        if cfg.fold {
            let (all, nodes): (bool, Vec<u64>) = match &d.dirty {
                Some(ds) => (ds.is_all(), ds.iter().map(|k| k.local.0).collect()),
                None => (false, Vec::new()),
            };
            res.fold.apply_batch(
                &canon_events,
                all,
                nodes,
                d.stats.pushed,
                d.stats.announcements_dropped,
                d.stats.flooded,
            );
        }
        let p = rx.pending();
        live.pending_hwm.fetch_max(p, Ordering::Relaxed);
        res.max_drain_gap_ns = res.max_drain_gap_ns.max(t.saturating_sub(last_drain_end));
        last_drain_end = now_ns();
        d.events.len()
    };

    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_time().build() else {
        return res;
    };
    rt.block_on(async {
        loop {
            if let Some((from, to)) = cfg.stall {
                while {
                    let n = now_ns();
                    n >= from && n < to && !stop.load(Ordering::Acquire)
                } {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }
            match cfg.mode {
                WakeMode::Notify => {
                    tokio::select! {
                        biased;
                        () = wake.notified() => {}
                        () = rx.notified() => {}
                        () = tokio::time::sleep(Duration::from_millis(100)) => {}
                    }
                }
                WakeMode::Spin => {
                    if stop.load(Ordering::Acquire) {
                        // fall through to the final drain below
                    } else {
                        tokio::task::yield_now().await;
                        std::hint::spin_loop();
                    }
                }
            }
            step(&mut res);
            if stop.load(Ordering::Acquire) {
                while step(&mut res) > 0 {}
                break;
            }
        }
    });
    res.fence_ns = live.fence_ns.load(Ordering::Acquire);
    let _: HashSet<u8> = HashSet::new();
    res
}
