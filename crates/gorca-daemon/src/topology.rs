// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime topology (bake-off B-5, ADR-0001 M-14): where the lanes run, one codebase.
//!
//! The arms differ only in *placement and scheduling*. The lane code (key pump, speech lane,
//! Reader, drain) is the same function in every arm; what changes is the runtime or thread it is
//! spawned on and the thread's scheduling class and CPU mask.
//!
//! | arm | name | input + speech | Reader, drain | workers | bulk pool | real-time | pinning |
//! |---|---|---|---|---|---|---|---|
//! | (a) | `single` | tasks on the one multi-thread runtime | own threads (ADR-0001 3.2) | `ncpu` | none | no | no |
//! | (b) | `lanes` | one thread + current-thread runtime each | own threads | `ncpu - 4` (min 2) | rayon | no | no |
//! | (c) | `lanes-rt` | as (b) | as (b) | as (b) | as (b) | rtkit, bounded | fast cores, workers kept off them |
//!
//! Diagnostic arms for the report: `lanes-rt-nopin` (real-time only) and `lanes-pin` (pinning
//! only).
//!
//! The Reader and the drain thread are dedicated threads with their own current-thread runtime
//! in every arm (ADR-0001 3.2); they are the "presentation" and "ingress" hot lanes, so arm (a)
//! is today's behaviour exactly.
//!
//! # Safety net of an RT lane (P-12 report, section 5)
//!
//! 1. only the dedicated lane threads are ever real-time (never a tokio worker or a rayon
//!    thread; threads they spawn are demoted at start, because Linux threads inherit the policy
//!    of their creator);
//! 2. priorities 12 to 15, never above rtkit's `MaxRealtimePriority` and well below the
//!    threaded-IRQ priority 50;
//! 3. `RLIMIT_RTTIME` (soft: half of rtkit's `RTTimeUSecMax`, hard: all of it), with a `SIGXCPU`
//!    handler that demotes the offending thread instead of dying;
//! 4. a `SCHED_OTHER` watchdog thread that demotes a lane running flat out for 200 ms;
//! 5. rtkit's own canary, and the kernel's RT throttling (not touched);
//! 6. any refusal (no rtkit, polkit, rate limit) leaves the lane on the plain scheduler and is
//!    recorded in the report: the daemon never fails to start because of it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gorca_rt::sched::{self, DutyTracker, Policy};
use gorca_rt::{CancellationToken, Lane, LaneHandle, spawn_lane};

use crate::rtkit::Rtkit;

/// Threads that are hot lanes in the lane arms: input, speech, Reader, one drain.
const HOT_LANES: usize = 4;
const WATCHDOG_TICK: Duration = Duration::from_millis(50);
const REPORT_EVERY: u32 = 20;
const RTKIT_CONNECT: Duration = Duration::from_millis(1500);
const RTKIT_WAIT: Duration = Duration::from_millis(1200);
/// Nice level asked of rtkit when a real-time class is refused.
const FALLBACK_NICE: i32 = -11;

/// The topology arm.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Arm {
    /// The default (B-5 judgement, docs/reports/b5-judgement.md): `lanes-rt` when rtkit grants
    /// real-time scheduling, else `single`. Resolved once at startup by [`Arm::resolve`]; code
    /// that sees an unresolved `Auto` (library use, tests) behaves as `single`.
    #[default]
    Auto,
    /// (a) one multi-thread tokio runtime: the behaviour before the switch.
    Single,
    /// (b) dedicated hot-lane threads, workers, bulk pool.
    Lanes,
    /// (c) (b) plus real-time scheduling (rtkit) and pinning.
    LanesRt,
    /// diagnostic: (c) without pinning.
    LanesRtNoPin,
    /// diagnostic: (c) without real-time scheduling.
    LanesPin,
}

impl Arm {
    pub const EXPECTED: &'static str = "auto, single, lanes or lanes-rt";

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "auto" => Self::Auto,
            "single" | "a" => Self::Single,
            "lanes" | "b" => Self::Lanes,
            "lanes-rt" | "c" => Self::LanesRt,
            "lanes-rt-nopin" => Self::LanesRtNoPin,
            "lanes-pin" => Self::LanesPin,
            _ => return None,
        })
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Single => "single",
            Self::Lanes => "lanes",
            Self::LanesRt => "lanes-rt",
            Self::LanesRtNoPin => "lanes-rt-nopin",
            Self::LanesPin => "lanes-pin",
        }
    }

    /// Hot lanes on their own threads, workers and bulk pool apart.
    #[must_use]
    pub fn lanes(self) -> bool {
        !matches!(self, Self::Auto | Self::Single)
    }

    /// Resolve [`Arm::Auto`]: `lanes-rt` when the machine is big enough and rtkit really grants
    /// real-time scheduling, else `single` (the plain lanes were not better than `single` in the
    /// B-5 runs, so a refusal must not leave the daemon on them). Any other arm is returned
    /// unchanged.
    ///
    /// The grant is tested, not assumed: rtkit's limits are read and one throw-away thread is
    /// asked for real-time (polkit, the burst limit or the per-user cap may refuse it). Process
    /// side effect: `RLIMIT_RTTIME` is lowered (a hard limit cannot be raised again); it is inert
    /// while no thread is real-time.
    #[must_use]
    pub fn resolve(self, rtkit_address: Option<&str>) -> Self {
        if self != Self::Auto {
            return self;
        }
        let allowed = sched::allowed_cpus();
        let fast = sched::fast_cpus(&allowed);
        Self::resolve_on(rtkit_address, &allowed, &fast)
    }

    /// [`Arm::resolve`] with the CPU sets given (testable).
    #[must_use]
    pub fn resolve_on(rtkit_address: Option<&str>, allowed: &[usize], fast: &[usize]) -> Self {
        if !lanes_fit(allowed, fast) {
            return Self::Single;
        }
        match Rtkit::connect(rtkit_address, RTKIT_CONNECT) {
            Ok(k) if probe_grant(&k).is_ok() => Self::LanesRt,
            _ => Self::Single,
        }
    }

    #[must_use]
    pub fn rt(self) -> bool {
        matches!(self, Self::LanesRt | Self::LanesRtNoPin)
    }

    /// Workers and the bulk pool are kept off the hot cores. Only the diagnostic `lanes-pin` arm
    /// does it: in the B-5 verdict the restriction cost A1 p99 (the workers lost 4 of 6 P-cores).
    #[must_use]
    pub fn keep_workers_off(self) -> bool {
        matches!(self, Self::LanesPin)
    }

    #[must_use]
    pub fn pin(self) -> bool {
        matches!(self, Self::LanesRt | Self::LanesPin)
    }
}

/// Whether the CPU set can hold the hot lanes one per core: more than [`HOT_LANES`] CPUs, and at
/// least that many fast ones besides CPU 0. The duty watchdog is per thread, so two lanes sharing
/// a core could together starve it (B-5 verdict, small machines).
fn lanes_fit(allowed: &[usize], fast: &[usize]) -> bool {
    allowed.len() > HOT_LANES && fast.iter().filter(|&&c| c != 0).count() >= HOT_LANES
}

/// Ask rtkit for one real-time grant on a throw-away thread, to learn whether lanes would get
/// one. The thread is real-time for the length of the call only.
fn probe_grant(k: &Rtkit) -> Result<(), String> {
    let hard = u64::try_from(k.limits().rttime_usec_max).unwrap_or(0);
    if hard == 0 || k.limits().max_realtime_priority <= 0 {
        return Err("rtkit offers no real-time budget".to_owned());
    }
    // rtkit refuses a process whose RLIMIT_RTTIME is unset.
    sched::set_rttime_limit(hard / 2, hard).map_err(|e| format!("setting RLIMIT_RTTIME: {e}"))?;
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("gorca-rt-probe".to_owned())
            .spawn_scoped(s, || {
                let tid = sched::current_tid();
                let r = k.make_realtime(tid, 12, RTKIT_WAIT).map(drop);
                let _ = sched::demote_tid(tid);
                r
            })
            .map_err(|e| format!("spawning the probe thread: {e}"))?
            .join()
            .map_err(|_| "the probe thread panicked".to_owned())?
    })
}

/// Wanted real-time priority of a lane (P-12, section 5).
fn wanted_priority(lane: Lane) -> u8 {
    match lane {
        Lane::Input => 15,
        Lane::Reader => 13,
        Lane::Drain(_) | Lane::SpeechIo => 12,
        Lane::Watchdog => 0,
    }
}

/// How a lane's thread ended up scheduled.
#[derive(Clone, Debug)]
struct LaneGrant {
    lane: String,
    tid: u32,
    pinned_to: Option<usize>,
    pin_error: Option<String>,
    /// The class the kernel reports after the request: `rr:15`, `fifo:15`, `nice:-11` or `none`.
    granted: String,
    /// The thread is in a real-time class now.
    realtime: bool,
    /// `rlimit`, `rtkit` or `-`.
    via: &'static str,
    error: Option<String>,
    watchdog_demoted: bool,
}

#[derive(Debug)]
struct Watched {
    tid: u32,
    lane: String,
    duty: DutyTracker,
}

/// Process-wide placement state of one daemon run.
#[derive(Debug)]
pub struct Placement {
    arm: Arm,
    rtkit: Option<Rtkit>,
    rt_status: String,
    rttime: Option<(u64, u64)>,
    allowed: Vec<usize>,
    fast: Vec<usize>,
    hot: Vec<usize>,
    /// CPUs of workers, bulk threads and every non-lane thread.
    background: Vec<usize>,
    grants: Mutex<Vec<LaneGrant>>,
    watched: Mutex<Vec<Watched>>,
    watchdog_demotions: AtomicU64,
    /// `sched::xcpu_demotions()` as last seen by the watchdog, to log the new ones.
    xcpu_seen: AtomicU64,
    report_path: Option<PathBuf>,
    threads_json: Mutex<String>,
    stop: AtomicBool,
    started: Instant,
}

static CURRENT: Mutex<Option<Arc<Placement>>> = Mutex::new(None);
static BULK: Mutex<Option<Arc<rayon::ThreadPool>>> = Mutex::new(None);

/// The rayon pool for bulk work (parallel tree builds, say-all chunks): present in the lane
/// arms, `None` in arm (a), where bulk work uses the worker runtime. Its threads are never
/// real-time.
#[must_use]
pub fn bulk_pool() -> Option<Arc<rayon::ThreadPool>> {
    BULK.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn current() -> Option<Arc<Placement>> {
    CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// The worker runtime for `arm`; arm (a) is exactly the pre-switch runtime. Installs the
/// placement (start hook, rlimit, watchdog) for the other arms.
pub fn build_runtime_for(
    arm: Arm,
    report: Option<&str>,
    rtkit_address: Option<&str>,
) -> Result<tokio::runtime::Runtime> {
    let ncpu = std::thread::available_parallelism().map_or(1, usize::from);
    let placement = Arc::new(Placement::plan(arm, report, rtkit_address, ncpu));
    let workers = if arm.lanes() {
        ncpu.saturating_sub(HOT_LANES).max(2)
    } else {
        ncpu
    };
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder
        .worker_threads(workers)
        .thread_name("gorca-worker")
        .enable_all();
    if arm.lanes() {
        let p = Arc::clone(&placement);
        builder.on_thread_start(move || p.place_background());
    }
    let rt = builder.build().context("building the tokio runtime")?;
    if arm.lanes() {
        let p = Arc::clone(&placement);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|i| format!("gorca-bulk-{i}"))
            .start_handler(move |_| p.place_background())
            .build()
            .context("building the bulk pool")?;
        *BULK.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(pool));
        let p = Arc::clone(&placement);
        gorca_rt::set_start_hook(Some(Arc::new(move |lane| p.place_lane(lane))));
    } else {
        gorca_rt::set_start_hook(None);
        *BULK.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
    Placement::start_watch(&placement)?;
    *CURRENT.lock().unwrap_or_else(PoisonError::into_inner) = Some(placement);
    Ok(rt)
}

/// Write the final report and stop the watchdog. Idempotent.
pub fn finish() {
    if let Some(p) = current() {
        p.stop.store(true, Ordering::Release);
        p.write_report(true);
    }
}

impl Placement {
    fn plan(arm: Arm, report: Option<&str>, rtkit_address: Option<&str>, ncpu: usize) -> Self {
        let allowed = sched::allowed_cpus();
        let fast = sched::fast_cpus(&allowed);
        // CPU 0 is where unrouted interrupts tend to land: keep it for the background when there
        // are enough other fast CPUs.
        let pool: Vec<usize> = if fast.iter().filter(|&&c| c != 0).count() >= HOT_LANES {
            fast.iter().copied().filter(|&c| c != 0).collect()
        } else {
            fast.clone()
        };
        let hot: Vec<usize> = pool.iter().copied().take(HOT_LANES).collect();
        let background = if arm.keep_workers_off() {
            let rest: Vec<usize> = allowed
                .iter()
                .copied()
                .filter(|c| !hot.contains(c))
                .collect();
            if rest.is_empty() {
                allowed.clone()
            } else {
                rest
            }
        } else {
            allowed.clone()
        };
        let mut rtkit = None;
        let mut rttime = None;
        let rt_status = if arm.rt() {
            match Self::enable_rt(rtkit_address) {
                Ok((k, lim)) => {
                    rtkit = Some(k);
                    rttime = Some(lim);
                    "rtkit available".to_owned()
                }
                Err(e) => format!("unavailable: {e}"),
            }
        } else {
            "not requested".to_owned()
        };
        let _ = ncpu;
        Self {
            arm,
            rtkit,
            rt_status,
            rttime,
            allowed,
            fast,
            hot,
            background,
            grants: Mutex::new(Vec::new()),
            watched: Mutex::new(Vec::new()),
            watchdog_demotions: AtomicU64::new(0),
            xcpu_seen: AtomicU64::new(0),
            report_path: report
                .map(|r| PathBuf::from(r.replace("%p", &std::process::id().to_string()))),
            threads_json: Mutex::new("[]".to_owned()),
            stop: AtomicBool::new(false),
            started: Instant::now(),
        }
    }

    /// rlimit and `SIGXCPU` handler first, then the rtkit connection (rtkit refuses an
    /// unlimited process, so the limit must be in place before the first request).
    fn enable_rt(address: Option<&str>) -> Result<(Rtkit, (u64, u64)), String> {
        // Probe rtkit before changing process-wide limits: nothing is touched when it is absent.
        let probe = Rtkit::connect(address, RTKIT_CONNECT)?;
        let hard = u64::try_from(probe.limits().rttime_usec_max)
            .map_err(|_| "rtkit RTTimeUSecMax is negative".to_owned())?;
        if hard == 0 {
            return Err("rtkit RTTimeUSecMax is 0".to_owned());
        }
        let soft = hard / 2;
        sched::set_rttime_limit(soft, hard).map_err(|e| format!("setting RLIMIT_RTTIME: {e}"))?;
        sched::install_xcpu_demoter()
            .map_err(|e| format!("installing the SIGXCPU handler: {e}"))?;
        Ok((probe, (soft, hard)))
    }

    /// A plain thread of the daemon (worker, bulk, blocking-pool): never real-time, never on a
    /// hot lane's CPU. Needed because new threads inherit the creator's class and mask.
    fn place_background(&self) {
        let tid = sched::current_tid();
        if self.arm.rt() && sched::policy_of(tid).is_ok_and(Policy::is_realtime) {
            let _ = sched::demote_tid(tid);
        }
        if self.arm.keep_workers_off() {
            let _ = sched::set_affinity(tid, &self.background);
        }
    }

    fn core_for(&self, lane: Lane) -> Option<usize> {
        if self.hot.is_empty() {
            return None;
        }
        let slot = match lane {
            Lane::Input => 0,
            Lane::Reader => 1,
            Lane::SpeechIo => 2,
            Lane::Drain(n) => 3 + usize::from(n),
            Lane::Watchdog => return None,
        };
        self.hot.get(slot % self.hot.len()).copied()
    }

    /// Runs on the lane's own thread as its first act.
    fn place_lane(&self, lane: Lane) {
        let tid = sched::current_tid();
        let mut g = LaneGrant {
            lane: lane.thread_name(),
            tid,
            pinned_to: None,
            pin_error: None,
            granted: "none".to_owned(),
            realtime: false,
            via: "-",
            error: None,
            watchdog_demoted: false,
        };
        if self.arm.rt() && wanted_priority(lane) > 0 {
            self.request_rt(lane, tid, &mut g);
        }
        // Pin only after real-time was granted: a refused lane stays unpinned, so a refusal
        // never leaves the daemon in the pinned-without-RT shape (B-5 verdict F-2).
        if self.arm.pin()
            && (!self.arm.rt() || g.realtime)
            && let Some(cpu) = self.core_for(lane)
        {
            match sched::set_affinity(tid, &[cpu]) {
                Ok(()) => g.pinned_to = Some(cpu),
                Err(e) => g.pin_error = Some(e.to_string()),
            }
        }
        if g.realtime {
            self.watched
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Watched {
                    tid,
                    lane: g.lane.clone(),
                    duty: DutyTracker::new(),
                });
        }
        tracing::info!(
            lane = %g.lane, tid, granted = %g.granted, via = g.via, pinned = ?g.pinned_to,
            error = ?g.error, "lane placed"
        );
        self.grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(g);
        self.write_report(false);
    }

    /// RLIMIT path (`SCHED_FIFO`), then rtkit (it grants `SCHED_RR`), then rtkit nice, then nothing.
    fn request_rt(&self, lane: Lane, tid: u32, g: &mut LaneGrant) {
        let want = wanted_priority(lane);
        if sched::set_fifo(tid, want).is_ok() {
            g.granted = granted_label(tid, "fifo", want);
            g.realtime = true;
            g.via = "rlimit";
            return;
        }
        let Some(rtkit) = &self.rtkit else {
            g.error = Some(format!("no rtkit: {}", self.rt_status));
            return;
        };
        match rtkit.make_realtime(tid, want, RTKIT_WAIT) {
            Ok(p) => {
                g.granted = granted_label(tid, "rt", p);
                g.realtime = true;
                g.via = "rtkit";
            }
            Err(e) => {
                g.error = Some(e);
                match rtkit.make_high_priority(tid, FALLBACK_NICE, RTKIT_WAIT) {
                    Ok(n) => {
                        g.granted = format!("nice:{n}");
                        g.via = "rtkit";
                    }
                    Err(e2) => {
                        g.error = Some(format!(
                            "{}; nice: {e2}",
                            g.error.clone().unwrap_or_default()
                        ));
                    }
                }
            }
        }
    }

    /// The watchdog and report thread (normal priority): demotes a lane that runs flat out,
    /// rewrites the report about once a second.
    fn start_watch(this: &Arc<Self>) -> Result<()> {
        if !this.arm.rt() && this.report_path.is_none() {
            return Ok(());
        }
        let p = Arc::clone(this);
        std::thread::Builder::new()
            .name("gorca-rt-watch".to_owned())
            .spawn(move || {
                let mut tick = 0u32;
                while !p.stop.load(Ordering::Acquire) {
                    // A dedicated OS thread outside tokio: sleeping is its whole job.
                    #[allow(clippy::disallowed_methods)]
                    std::thread::sleep(WATCHDOG_TICK);
                    p.watch_once();
                    tick += 1;
                    if tick.is_multiple_of(REPORT_EVERY) {
                        p.write_report(false);
                    }
                }
            })
            .context("spawning the topology watchdog")?;
        Ok(())
    }

    fn watch_once(&self) {
        let xcpu = sched::xcpu_demotions();
        let seen = self.xcpu_seen.swap(xcpu, Ordering::Relaxed);
        if xcpu > seen {
            tracing::warn!(
                new = xcpu - seen,
                total = xcpu,
                rearm_failures = sched::xcpu_rearm_failures(),
                "RLIMIT_RTTIME SIGXCPU: a real-time lane ran 100 ms without blocking and was demoted to SCHED_OTHER"
            );
        }
        let wall = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let mut doomed = Vec::new();
        {
            let mut w = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
            w.retain_mut(|t| {
                let Some(cpu) = sched::thread_cpu_ns(t.tid) else {
                    return false; // the thread is gone
                };
                if t.duty.observe(wall, cpu) {
                    doomed.push((t.tid, t.lane.clone()));
                    false
                } else {
                    true
                }
            });
        }
        for (tid, lane) in doomed {
            let r = sched::demote_tid(tid);
            self.watchdog_demotions.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(lane = %lane, tid, ok = r.is_ok(), "lane ran flat out: demoted to SCHED_OTHER");
            let mut g = self.grants.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(x) = g.iter_mut().find(|x| x.tid == tid) {
                x.watchdog_demoted = true;
            }
        }
    }

    fn write_report(&self, last: bool) {
        let Some(path) = &self.report_path else {
            return;
        };
        if !last {
            *self
                .threads_json
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = threads_json();
        }
        let grants = self
            .grants
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let lanes: Vec<String> = grants.iter().map(grant_json).collect();
        let rt_wanted = grants.iter().filter(|g| wanted_rt(&g.lane)).count();
        let rt_granted = grants
            .iter()
            .filter(|g| wanted_rt(&g.lane) && g.realtime)
            .count();
        let limits = self.rtkit.as_ref().map_or_else(
            || "null".to_owned(),
            |k| {
                let l = k.limits();
                format!(
                    "{{\"max_realtime_priority\":{},\"min_nice_level\":{},\"rttime_usec_max\":{}}}",
                    l.max_realtime_priority, l.min_nice_level, l.rttime_usec_max
                )
            },
        );
        let text = format!(
            "{{\"arm\":\"{}\",\"pid\":{},\"final\":{},\"uptime_ms\":{},\"rt_requested\":{},\"rt_status\":{},\
\"rtkit_limits\":{},\"rttime_soft_hard_us\":{},\"rt_lanes_wanted\":{},\"rt_lanes_granted\":{},\
\"rt_granted_all\":{},\"pin_requested\":{},\"cpus_allowed\":{:?},\"fast_cpus\":{:?},\"hot_cpus\":{:?},\
\"background_cpus\":{:?},\"watchdog_demotions\":{},\"xcpu_demotions\":{},\"xcpu_rearm_failures\":{},\"lanes\":[{}],\"threads\":{}}}\n",
            self.arm.name(),
            std::process::id(),
            last,
            self.started.elapsed().as_millis(),
            self.arm.rt(),
            json_str(&self.rt_status),
            limits,
            self.rttime
                .map_or_else(|| "null".to_owned(), |(s, h)| format!("[{s},{h}]")),
            rt_wanted,
            rt_granted,
            self.arm.rt() && rt_wanted > 0 && rt_wanted == rt_granted,
            self.arm.pin(),
            self.allowed,
            self.fast,
            self.hot,
            self.background,
            self.watchdog_demotions.load(Ordering::Relaxed),
            sched::xcpu_demotions(),
            sched::xcpu_rearm_failures(),
            lanes.join(","),
            self.threads_json
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// What the kernel says the thread is now (rtkit grants `SCHED_RR`, the rlimit path `SCHED_FIFO`).
fn granted_label(tid: u32, fallback_kind: &str, fallback_prio: u8) -> String {
    sched::policy_of(tid).map_or_else(
        |_| format!("{fallback_kind}:{fallback_prio}"),
        Policy::label,
    )
}

fn wanted_rt(lane_name: &str) -> bool {
    lane_name != "gorca-watchdog"
}

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn opt_str(s: Option<&String>) -> String {
    s.map_or_else(|| "null".to_owned(), |s| json_str(s))
}

fn grant_json(g: &LaneGrant) -> String {
    format!(
        "{{\"lane\":{},\"tid\":{},\"pinned_to\":{},\"pin_error\":{},\"granted\":{},\"via\":\"{}\",\"error\":{},\"watchdog_demoted\":{}}}",
        json_str(&g.lane),
        g.tid,
        g.pinned_to
            .map_or_else(|| "null".to_owned(), |c| c.to_string()),
        opt_str(g.pin_error.as_ref()),
        json_str(&g.granted),
        g.via,
        opt_str(g.error.as_ref()),
        g.watchdog_demoted,
    )
}

fn threads_json() -> String {
    let rows: Vec<String> = sched::threads()
        .iter()
        .map(|t| {
            format!(
                "{{\"tid\":{},\"name\":{},\"policy\":\"{}\",\"nice\":{},\"cpu\":{},\"cpu_ms\":{}}}",
                t.tid,
                json_str(&t.name),
                t.policy.label(),
                t.nice,
                t.cpu,
                t.cpu_ns / 1_000_000
            )
        })
        .collect();
    format!("[{}]", rows.join(","))
}

/// Runtimes of the hot lanes that live on tasks in arm (a) (key input and speech). In arm (a)
/// the handles are `None` and the tasks stay on the ambient runtime.
#[derive(Debug, Default)]
pub struct LaneRuntimes {
    input: Option<tokio::runtime::Handle>,
    speech: Option<tokio::runtime::Handle>,
    threads: Vec<LaneHandle>,
    stop: CancellationToken,
}

impl LaneRuntimes {
    /// Start the lane threads of `arm` (none for arm (a)).
    pub fn start(arm: Arm) -> Result<Self> {
        let mut me = Self::default();
        if !arm.lanes() {
            return Ok(me);
        }
        me.input = Some(me.spawn(Lane::Input)?);
        me.speech = Some(me.spawn(Lane::SpeechIo)?);
        Ok(me)
    }

    /// One lane thread with a current-thread runtime. The thread is placed by the start hook
    /// before the runtime exists, so everything the runtime does on it runs in the lane's class.
    fn spawn(&mut self, lane: Lane) -> Result<tokio::runtime::Handle> {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let stop = self.stop.clone();
        let placement = current();
        let handle = spawn_lane(lane, move || {
            let mut b = tokio::runtime::Builder::new_current_thread();
            b.enable_all().thread_name("gorca-lane-blocking");
            if let Some(p) = placement {
                // blocking-pool threads are created by this thread and would inherit its class
                b.on_thread_start(move || p.place_background());
            }
            match b.build() {
                Ok(rt) => {
                    let _ = tx.send(Ok(rt.handle().clone()));
                    rt.block_on(stop.cancelled());
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                }
            }
        })
        .with_context(|| format!("spawning lane {}", lane.thread_name()))?;
        self.threads.push(handle);
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow::anyhow!("lane {} did not start", lane.thread_name()))?
            .map_err(|e| anyhow::anyhow!("lane {} runtime: {e}", lane.thread_name()))
    }

    #[must_use]
    pub fn input(&self) -> Option<&tokio::runtime::Handle> {
        self.input.as_ref()
    }

    #[must_use]
    pub fn speech(&self) -> Option<&tokio::runtime::Handle> {
        self.speech.as_ref()
    }

    /// End the lane threads (call after the tasks on them have ended). Blocks briefly.
    pub fn stop(&mut self) {
        self.stop.cancel();
        for t in std::mem::take(&mut self.threads) {
            let _ = t.join();
        }
    }
}

impl Drop for LaneRuntimes {
    fn drop(&mut self) {
        // Never leave a lane thread running when the daemon's run ends early.
        self.stop.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_names_round_trip_and_default_is_single() {
        for a in [
            Arm::Auto,
            Arm::Single,
            Arm::Lanes,
            Arm::LanesRt,
            Arm::LanesRtNoPin,
            Arm::LanesPin,
        ] {
            assert_eq!(Arm::parse(a.name()), Some(a));
        }
        assert_eq!(Arm::default(), Arm::Auto);
        assert_eq!(Arm::parse("auto"), Some(Arm::Auto));
        // Without rtkit (no bus at that address) auto falls back to the single runtime and an
        // explicit arm is never changed.
        assert_eq!(
            Arm::Auto.resolve(Some("unix:path=/nonexistent/gorca-rtkit")),
            Arm::Single
        );
        assert_eq!(
            Arm::Lanes.resolve(Some("unix:path=/nonexistent/gorca-rtkit")),
            Arm::Lanes
        );
        assert_eq!(Arm::parse("c"), Some(Arm::LanesRt));
        assert_eq!(Arm::parse("nope"), None);
    }

    #[test]
    fn arm_flags_match_the_table() {
        assert!(!Arm::Single.lanes() && !Arm::Single.rt() && !Arm::Single.pin());
        assert!(Arm::Lanes.lanes() && !Arm::Lanes.rt() && !Arm::Lanes.pin());
        assert!(Arm::LanesRt.lanes() && Arm::LanesRt.rt() && Arm::LanesRt.pin());
        assert!(Arm::LanesRtNoPin.rt() && !Arm::LanesRtNoPin.pin());
        assert!(!Arm::LanesPin.rt() && Arm::LanesPin.pin());
    }

    #[test]
    fn priorities_stay_below_the_irq_threads_and_rtkit_default() {
        for l in [Lane::Input, Lane::Reader, Lane::Drain(0), Lane::SpeechIo] {
            let p = wanted_priority(l);
            assert!((1..=20).contains(&p), "{l:?} {p}");
        }
        assert_eq!(wanted_priority(Lane::Watchdog), 0);
    }

    #[test]
    fn plan_keeps_workers_off_the_hot_cpus() {
        let p = Placement::plan(Arm::LanesPin, None, None, 16);
        assert!(p.hot.len() <= HOT_LANES);
        assert!(p.hot.iter().all(|c| p.allowed.contains(c)));
        if p.allowed.len() > p.hot.len() {
            assert!(p.hot.iter().all(|c| !p.background.contains(c)));
        }
        // distinct lanes get distinct cores while there are enough
        if p.hot.len() == HOT_LANES {
            let cores: Vec<_> = [Lane::Input, Lane::Reader, Lane::SpeechIo, Lane::Drain(0)]
                .iter()
                .map(|l| p.core_for(*l))
                .collect();
            let mut d = cores.clone();
            d.sort_unstable();
            d.dedup();
            assert_eq!(d.len(), HOT_LANES);
        }
    }

    #[test]
    fn lanes_rt_does_not_restrict_the_workers() {
        let p = Placement::plan(Arm::LanesRt, None, Some("unix:path=/nonexistent/x"), 16);
        assert_eq!(p.background, p.allowed);
    }

    #[test]
    fn small_machines_never_resolve_to_lanes() {
        let cpus: Vec<usize> = (0..4).collect();
        assert!(!lanes_fit(&cpus, &cpus));
        let five: Vec<usize> = (0..5).collect();
        assert!(lanes_fit(&five, &five));
        // many CPUs but too few fast ones besides CPU 0
        let eight: Vec<usize> = (0..8).collect();
        assert!(!lanes_fit(&eight, &[0, 1, 2]));
        // the guard runs before rtkit is asked: a 2-CPU machine is single even with rtkit
        assert_eq!(
            Arm::resolve_on(Some("unix:path=/nonexistent/x"), &[0, 1], &[0, 1]),
            Arm::Single
        );
    }

    /// A private bus with a fake rtkit that grants or refuses `MakeThreadRealtime`.
    struct FakeRtkit {
        dir: std::path::PathBuf,
        bus: std::process::Child,
        stop: Arc<AtomicBool>,
        server: Option<std::thread::JoinHandle<()>>,
        address: String,
    }

    struct FakeIface {
        grant: bool,
    }

    #[allow(clippy::unused_self, clippy::used_underscore_binding)]
    #[zbus::interface(name = "org.freedesktop.RealtimeKit1")]
    impl FakeIface {
        #[zbus(property)]
        fn max_realtime_priority(&self) -> i32 {
            20
        }
        #[zbus(property)]
        fn min_nice_level(&self) -> i32 {
            -15
        }
        #[zbus(property, name = "RTTimeUSecMax")]
        fn rttime_usec_max(&self) -> i64 {
            200_000
        }
        fn make_thread_realtime(&self, tid: u64, priority: u32) -> zbus::fdo::Result<()> {
            let _ = (tid, priority);
            if self.grant {
                Ok(())
            } else {
                Err(zbus::fdo::Error::AccessDenied("refused by the fake".into()))
            }
        }
        fn make_thread_high_priority(&self, tid: u64, nice: i32) -> zbus::fdo::Result<()> {
            let _ = (tid, nice);
            if self.grant {
                Ok(())
            } else {
                Err(zbus::fdo::Error::AccessDenied("refused by the fake".into()))
            }
        }
    }

    impl FakeRtkit {
        fn start(grant: bool) -> Option<Self> {
            use std::sync::atomic::AtomicU32;
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "gorca-fake-rtkit-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).ok()?;
            let address = format!("unix:path={}/bus", dir.display());
            let bus = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--address", &address])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            let mut me = Self {
                dir,
                bus,
                stop: Arc::new(AtomicBool::new(false)),
                server: None,
                address: address.clone(),
            };
            let sock = me.dir.join("bus");
            for _ in 0..100 {
                if sock.exists() {
                    break;
                }
                // A test helper waiting for a socket on its own thread.
                #[allow(clippy::disallowed_methods)]
                std::thread::sleep(Duration::from_millis(50));
            }
            let stop = Arc::clone(&me.stop);
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            me.server = Some(std::thread::spawn(move || {
                let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let conn = rt.block_on(async {
                    zbus::connection::Builder::address(address.as_str())?
                        .name("org.freedesktop.RealtimeKit1")?
                        .serve_at("/org/freedesktop/RealtimeKit1", FakeIface { grant })?
                        .build()
                        .await
                });
                let _ = ready_tx.send(conn.is_ok());
                rt.block_on(async {
                    while !stop.load(Ordering::Acquire) {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                });
                drop(conn);
            }));
            ready_rx
                .recv_timeout(Duration::from_secs(5))
                .ok()
                .filter(|ok| *ok)?;
            Some(me)
        }
    }

    impl Drop for FakeRtkit {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(s) = self.server.take() {
                let _ = s.join();
            }
            let _ = self.bus.kill();
            let _ = self.bus.wait();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn eight() -> Vec<usize> {
        (0..8).collect()
    }

    #[test]
    fn auto_resolves_to_lanes_rt_only_after_a_real_grant() {
        let Some(granting) = FakeRtkit::start(true) else {
            eprintln!("SKIP: dbus-daemon unavailable");
            return;
        };
        assert_eq!(
            Arm::resolve_on(Some(&granting.address), &eight(), &eight()),
            Arm::LanesRt
        );
        let Some(refusing) = FakeRtkit::start(false) else {
            return;
        };
        // rtkit answers and reports a budget, but refuses the grant: single, not lanes-rt
        assert_eq!(
            Arm::resolve_on(Some(&refusing.address), &eight(), &eight()),
            Arm::Single
        );
    }

    #[test]
    fn a_refused_lane_is_not_pinned() {
        let Some(refusing) = FakeRtkit::start(false) else {
            eprintln!("SKIP: dbus-daemon unavailable");
            return;
        };
        let addr = refusing.address.clone();
        std::thread::spawn(move || {
            let p = Placement::plan(Arm::LanesRt, None, Some(&addr), 16);
            let before = sched::allowed_cpus();
            p.place_lane(Lane::Input);
            let g = p.grants.lock().unwrap();
            // a host that lets this user go real-time without rtkit would grant it
            if !g[0].realtime {
                assert_eq!(g[0].pinned_to, None);
                assert_eq!(sched::allowed_cpus(), before);
            }
        })
        .join()
        .unwrap();
    }

    /// Child half of [`a_second_runaway_is_demoted_not_killed`]: runs only when re-executed.
    #[test]
    fn runaway_child() {
        if std::env::var_os("GORCA_TEST_RUNAWAY_CHILD").is_none() {
            return;
        }
        let verdict = runaway_scenario();
        println!("RUNAWAY-RESULT: {verdict}");
        let ok = verdict.starts_with("OK") || verdict.starts_with("SKIP");
        std::process::exit(i32::from(!ok));
    }

    fn runaway_scenario() -> String {
        let addr = std::env::var("GORCA_RTKIT_BUS_ADDRESS").ok();
        let Ok(k) = Rtkit::connect(addr.as_deref(), Duration::from_millis(1500)) else {
            return "SKIP: no rtkit".to_owned();
        };
        // The same shape as gorca: soft = hard / 2.
        let (soft, hard) = (100_000, 200_000);
        if sched::set_rttime_limit(soft, hard).is_err() || sched::install_xcpu_demoter().is_err() {
            return "FAIL: limits".to_owned();
        }
        let spin = |k: &Rtkit, n: u64| -> String {
            let k: &Rtkit = k;
            std::thread::scope(|s| {
                s.spawn(|| {
                    let tid = sched::current_tid();
                    if let Some(&c) = sched::allowed_cpus().last() {
                        let _ = sched::set_affinity(tid, &[c]);
                    }
                    if let Err(e) = k.make_realtime(tid, 12, Duration::from_millis(1200)) {
                        return format!("SKIP: refused: {e}");
                    }
                    let t0 = Instant::now();
                    while sched::xcpu_demotions() < n && t0.elapsed() < Duration::from_secs(2) {
                        std::hint::spin_loop();
                    }
                    if sched::xcpu_demotions() < n {
                        return format!("FAIL: spin {n} was not demoted");
                    }
                    "OK".to_owned()
                })
                .join()
                .unwrap_or_else(|_| "FAIL: panic".to_owned())
            })
        };
        for n in 1..=3 {
            let r = spin(&k, n);
            if r != "OK" {
                return r;
            }
            if sched::rttime_limit().ok() != Some((soft, hard)) {
                return format!("FAIL: limit not re-armed after runaway {n}");
            }
        }
        format!("OK: {} demotions, limit re-armed", sched::xcpu_demotions())
    }

    /// Regression for B-5 verdict F-1: after the first SIGXCPU the kernel raises the soft limit
    /// above the hard one, so the second runaway used to get SIGKILL. The scenario runs in a
    /// child process so that a regression fails the test instead of killing the test runner.
    #[test]
    fn a_second_runaway_is_demoted_not_killed() {
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "topology::tests::runaway_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("GORCA_TEST_RUNAWAY_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("RUNAWAY-RESULT: "),
            "child status {:?}: {stdout}",
            out.status
        );
        eprintln!(
            "{}",
            stdout
                .lines()
                .find(|l| l.contains("RUNAWAY-RESULT"))
                .unwrap_or("")
        );
    }

    #[test]
    fn json_strings_are_escaped() {
        assert_eq!(json_str("a\"b\\c\nd"), "\"a\\\"b\\\\c d\"");
    }

    /// A host that grants `RLIMIT_RTPRIO` (a `realtime` group, for example) takes the rlimit
    /// path and never reaches the no-rtkit fallback, and the limit is process-wide, so the
    /// scenario runs in a child process that lowers it to 0 first.
    #[test]
    fn a_missing_rtkit_does_not_stop_the_arm() {
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "topology::tests::no_rtkit_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("GORCA_TEST_NO_RTKIT_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("NO-RTKIT-RESULT: "),
            "child status {:?}: {stdout}",
            out.status
        );
        eprintln!(
            "{}",
            stdout
                .lines()
                .find(|l| l.contains("NO-RTKIT-RESULT"))
                .unwrap_or("")
        );
    }

    /// Child half of [`a_missing_rtkit_does_not_stop_the_arm`]: runs only when re-executed.
    #[test]
    fn no_rtkit_child() {
        if std::env::var_os("GORCA_TEST_NO_RTKIT_CHILD").is_none() {
            return;
        }
        let verdict = no_rtkit_scenario();
        println!("NO-RTKIT-RESULT: {verdict}");
        let ok = verdict.starts_with("OK") || verdict.starts_with("SKIP");
        std::process::exit(i32::from(!ok));
    }

    fn no_rtkit_scenario() -> String {
        if let Err(e) = sched::set_rtprio_soft_limit(0) {
            return format!("FAIL: lowering RLIMIT_RTPRIO: {e}");
        }
        // CAP_SYS_NICE ignores the limit, so such a host cannot reach the fallback either.
        let fifo_still_allowed = std::thread::spawn(|| {
            let tid = sched::current_tid();
            let ok = sched::set_fifo(tid, 1).is_ok();
            let _ = sched::demote_tid(tid);
            ok
        })
        .join()
        .unwrap_or(true);
        if fifo_still_allowed {
            return "SKIP: SCHED_FIFO is still allowed (CAP_SYS_NICE)".to_owned();
        }
        let p = Placement::plan(
            Arm::LanesRt,
            None,
            Some("unix:path=/nonexistent/system_bus_socket"),
            16,
        );
        if p.rtkit.is_some() {
            return "FAIL: rtkit connected to a nonexistent bus".to_owned();
        }
        if !p.rt_status.starts_with("unavailable") {
            return format!("FAIL: rt_status {}", p.rt_status);
        }
        // placing a lane without rtkit leaves it plain and records why
        std::thread::spawn(move || {
            p.place_lane(Lane::Input);
            let g = p.grants.lock().unwrap();
            if g[0].granted != "none" {
                return format!("FAIL: granted {} via {}", g[0].granted, g[0].via);
            }
            if g[0].error.is_none() {
                return "FAIL: no error recorded".to_owned();
            }
            "OK: plain lane, error recorded".to_owned()
        })
        .join()
        .unwrap_or_else(|_| "FAIL: the lane thread panicked".to_owned())
    }

    /// The lane arms start their lane threads and run tasks placed on them.
    #[test]
    fn lane_runtimes_run_placed_tasks_on_their_own_threads() {
        let mut lanes = LaneRuntimes::start(Arm::Lanes).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let tx2 = tx.clone();
        lanes.input().unwrap().spawn(async move {
            let _ = tx.send(std::thread::current().name().map(str::to_owned));
        });
        lanes.speech().unwrap().spawn(async move {
            let _ = tx2.send(std::thread::current().name().map(str::to_owned));
        });
        let mut names: Vec<_> = (0..2)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, vec!["gorca-input", "gorca-speech-io"]);
        lanes.stop();
    }

    #[test]
    fn arm_a_has_no_lane_runtimes() {
        let mut lanes = LaneRuntimes::start(Arm::Single).unwrap();
        assert!(lanes.input().is_none() && lanes.speech().is_none());
        lanes.stop();
    }
}
