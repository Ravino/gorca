// SPDX-License-Identifier: GPL-3.0-or-later
//! One complete run: bus fixture, drain under test, per-app consumers, paced feeders, sampling
//! and ground-truth comparison. Everything the scenarios measure comes out of [`execute`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use gorca_dbus::drain::{DrainConfig, DrainEnd, DrainStats, spawn_drain};

use crate::bus::{Bus, BusKind};
use crate::clock::now_ns;
use crate::feeder::{AppPlan, FeedResult, MockSink, Sink, run_feeder, truth_for};
use crate::mockbus::Link;
use crate::model::{Verdict, compare};
use crate::procfs::{CpuSnapshot, reset_rss_peak, rss_bytes, rss_peak_bytes};
use crate::rig::{ConsumerCfg, ConsumerResult, HarnessRouter};

#[derive(Clone, Debug)]
pub struct AppSpec {
    pub plan: AppPlan,
    pub cons: ConsumerCfg,
}

#[derive(Clone, Debug)]
pub struct RunSpec {
    pub bus: BusKind,
    pub apps: Vec<AppSpec>,
    pub duration: Duration,
    /// How long to wait for the fences after the feeders stop.
    pub settle: Duration,
    pub rcvbuf: Option<usize>,
    pub max_connections: u8,
    /// Compare folds with the ground truth (needs `cons.fold` and `plan.fence`).
    pub verify: bool,
}

impl RunSpec {
    pub fn new(bus: BusKind, duration: Duration) -> Self {
        Self {
            bus,
            apps: Vec::new(),
            duration,
            settle: Duration::from_secs(10),
            rcvbuf: Some(4 * 1024 * 1024),
            max_connections: 1,
            verify: false,
        }
    }
}

#[derive(Debug)]
pub struct AppOutcome {
    pub role: String,
    pub plan: AppPlan,
    pub feed: FeedResult,
    pub cons: ConsumerResult,
    pub verdict: Option<Verdict>,
    pub fence_seen: bool,
    /// Probe latencies in nanoseconds (one per probe that was reflected).
    pub probe_lat_ns: Vec<f64>,
    pub probes_unobserved: u32,
}

#[derive(Clone, Debug, Default)]
pub struct LinkSnapshot {
    pub written: u64,
    pub queued_hwm: u64,
    pub outq_hwm: u64,
    pub disconnected: bool,
    pub disconnect_after_ns: Option<u64>,
    pub rules: Vec<String>,
}

#[derive(Debug)]
pub struct RunOutcome {
    pub bus: &'static str,
    pub elapsed_s: f64,
    pub drain_end: DrainEnd,
    pub drain: DrainStats,
    pub apps: Vec<AppOutcome>,
    /// CPU ns of non-harness threads while the feeders were running.
    pub cpu_ns: u64,
    /// Part of it spent in threads named `gorca-drain*`.
    pub drain_cpu_ns: u64,
    /// `(thread name, cpu ns)` of non-harness threads during the run.
    pub cpu_threads: Vec<(String, u64)>,
    pub rss_base: u64,
    pub rss_peak: u64,
    pub rss_samples: Vec<(f64, u64)>,
    pub backlog_samples: Vec<(f64, u64)>,
    pub link: Option<LinkSnapshot>,
    pub settled: bool,
    pub start_ns: u64,
}

impl RunOutcome {
    pub fn app(&self, role: &str) -> Option<&AppOutcome> {
        self.apps.iter().find(|a| a.role == role)
    }

    /// Data events sent by all feeders (excludes probes and fences).
    pub fn data_events(&self) -> u64 {
        self.apps.iter().map(|a| a.feed.sent).sum()
    }

    /// Everything that had to be pushed: data events that map to events, plus probes and fences.
    pub fn disconnected(&self) -> bool {
        matches!(self.drain_end, DrainEnd::Disconnected(_))
            || self.link.as_ref().is_some_and(|l| l.disconnected)
            || self.apps.iter().any(|a| a.feed.peer_gone)
    }
}

fn spawn_sampler(
    stop: Arc<AtomicBool>,
    link: Option<Arc<Link>>,
    t0: u64,
) -> std::thread::JoinHandle<(Vec<(f64, u64)>, Vec<(f64, u64)>)> {
    std::thread::Builder::new()
        .name("hx-sampler".to_owned())
        .spawn(move || {
            let (mut rss, mut backlog) = (Vec::new(), Vec::new());
            while !stop.load(Ordering::Acquire) {
                let t = (now_ns().saturating_sub(t0)) as f64 / 1e9;
                rss.push((t, rss_bytes()));
                if let Some(l) = &link {
                    backlog.push((t, l.backlog() as u64));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            (rss, backlog)
        })
        .unwrap_or_else(|_| std::thread::spawn(|| (Vec::new(), Vec::new())))
}

/// Run `spec` once.
pub fn execute(spec: &RunSpec) -> Result<RunOutcome> {
    crate::clock::init();
    let bus = Bus::start(spec.bus.clone())?;
    let router = HarnessRouter::new();
    let is_mock = bus.mock().is_some();

    // Real buses: connect the senders first (no traffic yet) so their unique names are known.
    let mut real_sinks: Vec<Option<Box<dyn Sink>>> = Vec::new();
    let mut senders: Vec<String> = Vec::new();
    for (i, a) in spec.apps.iter().enumerate() {
        if is_mock {
            let name = format!(":1.{}", 200 + i);
            router.register(&name, &a.plan.role, &a.cons);
            senders.push(name);
            real_sinks.push(None);
        } else {
            let (sink, unique) = bus.sender()?;
            router.register(&unique, &a.plan.role, &a.cons);
            senders.push(unique);
            real_sinks.push(Some(Box::new(sink)));
        }
    }

    reset_rss_peak();
    let rss_base = rss_bytes();
    let mut cfg = DrainConfig::new(bus.address.clone());
    cfg.rcvbuf_bytes = spec.rcvbuf;
    cfg.max_event_connections = spec.max_connections;
    let drain = spawn_drain(
        cfg,
        Arc::clone(&router) as Arc<dyn gorca_dbus::drain::Router>,
    )
    .map_err(|e| anyhow::anyhow!("spawn_drain: {e}"))?;
    let link = match bus.mock() {
        Some(m) => Some(m.next_link(Duration::from_secs(5))?),
        None => None,
    };
    if let Some(l) = &link {
        for (i, _) in spec.apps.iter().enumerate() {
            real_sinks[i] = Some(Box::new(MockSink {
                link: Arc::clone(l),
            }));
        }
    }

    let t0 = now_ns();
    let sampler_stop = Arc::new(AtomicBool::new(false));
    let sampler = spawn_sampler(Arc::clone(&sampler_stop), link.clone(), t0);
    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(spec.apps.len() + 1));
    let results: Arc<Mutex<Vec<Option<FeedResult>>>> =
        Arc::new(Mutex::new(vec![None; spec.apps.len()]));
    let mut threads = Vec::new();
    for (i, (a, sink)) in spec.apps.iter().zip(real_sinks).enumerate() {
        let Some(mut sink) = sink else {
            bail!("no sink for app {i}")
        };
        let (plan, sender, barrier, stop, results) = (
            a.plan.clone(),
            if is_mock {
                Some(senders[i].clone())
            } else {
                None
            },
            Arc::clone(&barrier),
            Arc::clone(&stop),
            Arc::clone(&results),
        );
        let dur = spec.duration;
        let name: String = format!("hx-feed-{}", plan.role).chars().take(15).collect();
        threads.push(std::thread::Builder::new().name(name).spawn(move || {
            barrier.wait();
            let r = run_feeder(&plan, sender.as_deref(), sink.as_mut(), dur, &stop);
            results.lock().unwrap_or_else(PoisonError::into_inner)[i] = Some(r);
        })?);
    }
    let cpu0 = CpuSnapshot::take();
    let start_ns = now_ns();
    for a in &spec.apps {
        if let Some(l) = router.live(&a.plan.role) {
            l.start_ns.store(start_ns.max(1), Ordering::Release);
        }
    }
    barrier.wait();
    for t in threads {
        let _ = t.join();
    }
    let cpu1 = CpuSnapshot::take();
    let elapsed_s = (now_ns() - start_ns) as f64 / 1e9;
    let cpu_ns = cpu1.candidate_ns_since(&cpu0);
    let drain_cpu_ns = cpu1.ns_since_by_prefix(&cpu0, "gorca-drain");
    let cpu_threads = cpu1.by_thread_since(&cpu0);

    if let Some(l) = &link {
        l.flush_all(Duration::from_secs(10));
    }
    let feeds: Vec<FeedResult> = results
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(|r| r.clone().unwrap_or_default())
        .collect();
    let mut settled = true;
    let mut fence_seen = Vec::new();
    for (a, f) in spec.apps.iter().zip(&feeds) {
        let ok = if a.plan.fence && !f.peer_gone {
            router.wait_fence(&a.plan.role, spec.settle)
        } else {
            false
        };
        if a.plan.fence && !ok && !f.peer_gone {
            settled = false;
        }
        fence_seen.push(ok);
    }
    sampler_stop.store(true, Ordering::Release);
    let (rss_samples, backlog_samples) = sampler.join().unwrap_or_default();
    let rss_peak = rss_peak_bytes().max(rss_samples.iter().map(|s| s.1).max().unwrap_or(0));
    let drain_end = drain.end();
    let drain_stats = drain.stop();
    let link_snap = link.as_ref().map(|l| LinkSnapshot {
        written: l.stats.written.load(Ordering::Relaxed),
        queued_hwm: l.stats.queued_hwm.load(Ordering::Relaxed),
        outq_hwm: l.stats.outq_hwm.load(Ordering::Relaxed),
        disconnected: l.is_disconnected(),
        disconnect_after_ns: l.is_disconnected().then(|| {
            l.stats
                .disconnect_ns
                .load(Ordering::Relaxed)
                .saturating_sub(start_ns)
        }),
        rules: l.rules(),
    });

    let mut apps = Vec::new();
    for ((a, feed), seen) in spec.apps.iter().zip(feeds).zip(fence_seen) {
        let cons = router.finish(&a.plan.role).unwrap_or_default();
        let app = router.app_of(&a.plan.role);
        let verdict = (spec.verify && a.cons.fold)
            .then(|| app.map(|app| compare(&truth_for(&a.plan, &feed, app), &cons.fold)))
            .flatten();
        let mut lat = Vec::new();
        let mut unobserved = 0;
        for k in 0..feed.probes_sent as usize {
            match (cons.probe_obs_ns.get(k), feed.probe_send_ns.get(k)) {
                (Some(&o), Some(&s)) if o != u64::MAX => lat.push(o.saturating_sub(s) as f64),
                _ => unobserved += 1,
            }
        }
        apps.push(AppOutcome {
            role: a.plan.role.clone(),
            plan: a.plan.clone(),
            feed,
            cons,
            verdict,
            fence_seen: seen,
            probe_lat_ns: lat,
            probes_unobserved: unobserved,
        });
    }
    let _ = Instant::now();
    Ok(RunOutcome {
        bus: spec.bus.label(),
        elapsed_s,
        drain_end,
        drain: drain_stats,
        apps,
        cpu_ns,
        drain_cpu_ns,
        cpu_threads,
        rss_base,
        rss_peak,
        rss_samples,
        backlog_samples,
        link: link_snap,
        settled,
        start_ns,
    })
}
