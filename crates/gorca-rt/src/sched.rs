// SPDX-License-Identifier: GPL-3.0-or-later
//! Thread placement primitives for the runtime topology (bake-off B-5, P-12 report): scheduling
//! policy, CPU affinity, per-thread CPU time and the `/proc` thread table.
//!
//! Everything here acts on a *kernel thread id* (`gettid`), never on a `pthread_t`. Nothing in
//! this module needs a privilege except raising a thread to `SCHED_FIFO` (see
//! `gorca-daemon`'s `topology`, which asks rtkit first). Lowering is always allowed, so
//! [`demote_tid`] and the `SIGXCPU` handler are the safety net of an RT lane.
//!
//! `SCHED_RESET_ON_FORK` is set whenever this module sets a policy: rtkit sets it on the
//! threads it promotes, and the kernel then refuses a later policy change that clears it
//! (EPERM), so demotion must keep it.
#![allow(unsafe_code)]

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

/// `SCHED_RESET_ON_FORK` flag of `sched_setscheduler`.
const RESET_ON_FORK: libc::c_int = 0x4000_0000;

fn pid_t(tid: u32) -> io::Result<libc::pid_t> {
    libc::pid_t::try_from(tid).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

/// CPU time consumed so far by the calling thread (`CLOCK_THREAD_CPUTIME_ID`: on-CPU time,
/// unaffected by descheduling). Zero if the kernel refuses the clock, which cannot happen on
/// Linux; callers treat that as "no CPU time used", never as an error.
#[must_use]
pub fn thread_cpu_time() -> std::time::Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec and the clock id is a constant Linux supports.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) };
    if rc != 0 {
        return std::time::Duration::ZERO;
    }
    std::time::Duration::new(
        u64::try_from(ts.tv_sec).unwrap_or(0),
        u32::try_from(ts.tv_nsec).unwrap_or(0),
    )
}

/// Kernel thread id of the calling thread.
#[must_use]
pub fn current_tid() -> u32 {
    // SAFETY: `gettid` takes no arguments and cannot fail.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    u32::try_from(tid).unwrap_or(0)
}

/// Scheduling policy of a thread, as printed by `chrt` (`SCHED_OTHER` is `Other`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Policy {
    Other,
    Fifo(u8),
    RoundRobin(u8),
    Batch,
    Idle,
    Deadline,
    Unknown(i32),
}

impl Policy {
    #[must_use]
    pub fn is_realtime(self) -> bool {
        matches!(
            self,
            Policy::Fifo(_) | Policy::RoundRobin(_) | Policy::Deadline
        )
    }

    /// `other`, `fifo:15`, ...
    #[must_use]
    pub fn label(self) -> String {
        match self {
            Policy::Other => "other".to_owned(),
            Policy::Fifo(p) => format!("fifo:{p}"),
            Policy::RoundRobin(p) => format!("rr:{p}"),
            Policy::Batch => "batch".to_owned(),
            Policy::Idle => "idle".to_owned(),
            Policy::Deadline => "deadline".to_owned(),
            Policy::Unknown(n) => format!("policy-{n}"),
        }
    }

    fn from_raw(policy: i32, rt_prio: u8) -> Self {
        match policy & !RESET_ON_FORK {
            libc::SCHED_OTHER => Policy::Other,
            libc::SCHED_FIFO => Policy::Fifo(rt_prio),
            libc::SCHED_RR => Policy::RoundRobin(rt_prio),
            libc::SCHED_BATCH => Policy::Batch,
            libc::SCHED_IDLE => Policy::Idle,
            6 => Policy::Deadline,
            n => Policy::Unknown(n),
        }
    }
}

/// One row of `/proc/<pid>/task`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadInfo {
    pub tid: u32,
    pub name: String,
    pub policy: Policy,
    pub nice: i32,
    /// CPU the thread last ran on.
    pub cpu: u32,
    /// Cumulative on-CPU time (`schedstat`, first field), nanoseconds.
    pub cpu_ns: u64,
}

/// Parse the text of `/proc/<pid>/task/<tid>/stat`. The name sits in parentheses and may itself
/// contain spaces and parentheses, so the fields are counted from the last `)`.
#[must_use]
pub fn parse_stat(text: &str) -> Option<ThreadInfo> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let tid = text.get(..open)?.trim().parse().ok()?;
    let name = text.get(open + 1..close)?.to_owned();
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    // After the name, field 3 (state) is rest[0]; field N is rest[N - 3].
    let nice: i32 = rest.get(19 - 3)?.parse().ok()?;
    let cpu: u32 = rest.get(39 - 3)?.parse().ok()?;
    let rt_prio: u8 = rest.get(40 - 3)?.parse().ok()?;
    let policy: i32 = rest.get(41 - 3)?.parse().ok()?;
    Some(ThreadInfo {
        tid,
        name,
        policy: Policy::from_raw(policy, rt_prio),
        nice,
        cpu,
        cpu_ns: 0,
    })
}

/// Every thread of this process with its policy and CPU time. Threads that exit while the table
/// is read are skipped.
#[must_use]
pub fn threads() -> Vec<ThreadInfo> {
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    let mut out: Vec<ThreadInfo> = dir
        .filter_map(Result::ok)
        .filter_map(|e| {
            let p = e.path();
            let mut t = parse_stat(&std::fs::read_to_string(p.join("stat")).ok()?)?;
            t.cpu_ns = thread_cpu_ns(t.tid).unwrap_or(0);
            Some(t)
        })
        .collect();
    out.sort_by_key(|t| t.tid);
    out
}

/// Cumulative on-CPU nanoseconds of a thread of this process (`schedstat`).
#[must_use]
pub fn thread_cpu_ns(tid: u32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/self/task/{tid}/schedstat")).ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// Policy of one thread of this process, read through the kernel (not `/proc`).
pub fn policy_of(tid: u32) -> io::Result<Policy> {
    let t = pid_t(tid)?;
    // SAFETY: plain syscall with a scalar argument.
    let policy = unsafe { libc::sched_getscheduler(t) };
    if policy < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut param = libc::sched_param { sched_priority: 0 };
    // SAFETY: `param` is a live local the kernel writes.
    if unsafe { libc::sched_getparam(t, &raw mut param) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Policy::from_raw(
        policy,
        u8::try_from(param.sched_priority).unwrap_or(u8::MAX),
    ))
}

/// Make `tid` `SCHED_FIFO` at `priority` (1..=99) without rtkit. Needs `RLIMIT_RTPRIO` or
/// `CAP_SYS_NICE`; the caller falls back to rtkit on `EPERM`.
pub fn set_fifo(tid: u32, priority: u8) -> io::Result<()> {
    set_policy(tid, libc::SCHED_FIFO, i32::from(priority))
}

/// Return `tid` to `SCHED_OTHER` (keeping `SCHED_RESET_ON_FORK`). Always permitted.
pub fn demote_tid(tid: u32) -> io::Result<()> {
    set_policy(tid, libc::SCHED_OTHER, 0)
}

fn set_policy(tid: u32, policy: libc::c_int, priority: i32) -> io::Result<()> {
    let t = pid_t(tid)?;
    let param = libc::sched_param {
        sched_priority: priority,
    };
    // SAFETY: `param` is a live local; the kernel only reads it.
    let rc = unsafe { libc::sched_setscheduler(t, policy | RESET_ON_FORK, &raw const param) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// CPUs the calling thread may run on (`sched_getaffinity`).
#[must_use]
pub fn allowed_cpus() -> Vec<usize> {
    // SAFETY: `set` is a zeroed `cpu_set_t` that the kernel fills; `CPU_ISSET` reads it.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, size_of::<libc::cpu_set_t>(), &raw mut set) != 0 {
            return Vec::new();
        }
        (0..libc::CPU_SETSIZE as usize)
            .filter(|&c| libc::CPU_ISSET(c, &set))
            .collect()
    }
}

/// Pin `tid` to `cpus` (`sched_setaffinity`). An empty list is rejected.
pub fn set_affinity(tid: u32, cpus: &[usize]) -> io::Result<()> {
    if cpus.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty cpu list",
        ));
    }
    let t = pid_t(tid)?;
    // SAFETY: `set` is a zeroed `cpu_set_t` filled with `CPU_SET` for in-range CPUs only; the
    // kernel reads it.
    let rc = unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for &c in cpus {
            if c >= libc::CPU_SETSIZE as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cpu out of range",
                ));
            }
            libc::CPU_SET(c, &mut set);
        }
        libc::sched_setaffinity(t, size_of::<libc::cpu_set_t>(), &raw const set)
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Set the process-wide `RLIMIT_RTTIME` (microseconds of RT CPU time without a blocking call):
/// `soft` raises `SIGXCPU` (see [`install_xcpu_demoter`]), `hard` is `SIGKILL`. rtkit refuses
/// a process whose limit is unset or above its `RTTimeUSecMax`.
pub fn set_rttime_limit(soft_us: u64, hard_us: u64) -> io::Result<()> {
    RTTIME_SOFT.store(soft_us, Ordering::Relaxed);
    RTTIME_HARD.store(hard_us, Ordering::Relaxed);
    let lim = libc::rlimit {
        rlim_cur: soft_us,
        rlim_max: hard_us,
    };
    // SAFETY: `lim` is a live local; the kernel only reads it.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &raw const lim) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Current process-wide `RLIMIT_RTTIME` as `(soft, hard)`; `u64::MAX` is unlimited.
pub fn rttime_limit() -> io::Result<(u64, u64)> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a live local the kernel writes.
    if unsafe { libc::getrlimit(libc::RLIMIT_RTTIME, &raw mut lim) } == 0 {
        Ok((lim.rlim_cur, lim.rlim_max))
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The limits last set by [`set_rttime_limit`], so the `SIGXCPU` handler can re-arm them.
static RTTIME_SOFT: AtomicU64 = AtomicU64::new(0);
static RTTIME_HARD: AtomicU64 = AtomicU64::new(0);
static XCPU_DEMOTIONS: AtomicU64 = AtomicU64::new(0);
static XCPU_REARM_FAILURES: AtomicU64 = AtomicU64::new(0);

/// How many times re-arming `RLIMIT_RTTIME` after a `SIGXCPU` failed (expected 0).
#[must_use]
pub fn xcpu_rearm_failures() -> u64 {
    XCPU_REARM_FAILURES.load(Ordering::Relaxed)
}

/// How many times the `SIGXCPU` handler demoted a thread.
#[must_use]
pub fn xcpu_demotions() -> u64 {
    XCPU_DEMOTIONS.load(Ordering::Relaxed)
}

extern "C" fn on_xcpu(_sig: libc::c_int) {
    // Async-signal-safe only: one syscall and an atomic. `sched_setscheduler(0, ..)` acts on the
    // calling thread, which is the one that ran past its RT budget (the kernel delivers
    // RLIMIT_RTTIME's SIGXCPU to it).
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: `param` is a live local; the call is async-signal-safe.
    unsafe {
        libc::sched_setscheduler(0, libc::SCHED_OTHER | RESET_ON_FORK, &raw const param);
    }
    // The kernel raises the soft limit by one second when it sends SIGXCPU
    // (`check_thread_timers`), which puts it above the hard limit: the next runaway would get
    // SIGKILL without a warning. Put the configured limits back; `setrlimit` is a plain syscall.
    let soft = RTTIME_SOFT.load(Ordering::Relaxed);
    let hard = RTTIME_HARD.load(Ordering::Relaxed);
    if hard != 0 {
        let lim = libc::rlimit {
            rlim_cur: soft,
            rlim_max: hard,
        };
        // SAFETY: `lim` is a live local; the kernel only reads it.
        if unsafe { libc::setrlimit(libc::RLIMIT_RTTIME, &raw const lim) } != 0 {
            XCPU_REARM_FAILURES.fetch_add(1, Ordering::Relaxed);
        }
    }
    XCPU_DEMOTIONS.fetch_add(1, Ordering::Relaxed);
}

/// Install the `SIGXCPU` handler that demotes the thread that exhausted `RLIMIT_RTTIME`
/// instead of letting the default action (terminate and dump core) kill the reader.
pub fn install_xcpu_demoter() -> io::Result<()> {
    // SAFETY: a zeroed `sigaction` with a handler of the right signature, installed with an
    // empty mask and `SA_RESTART`; no other code in this process handles SIGXCPU.
    let rc = unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_xcpu as extern "C" fn(libc::c_int) as usize;
        sa.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&raw mut sa.sa_mask);
        libc::sigaction(libc::SIGXCPU, &raw const sa, std::ptr::null_mut())
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// CPUs of the fastest class among `allowed`, for pinning hot lanes. Hybrid CPUs (P, E and LP-E
/// cores) differ in `cpuinfo_max_freq`; the top class is every CPU within 10 % of the highest.
/// Without cpufreq data every allowed CPU counts as top class.
#[must_use]
pub fn fast_cpus(allowed: &[usize]) -> Vec<usize> {
    let freq = |c: usize| -> Option<u64> {
        std::fs::read_to_string(format!(
            "/sys/devices/system/cpu/cpu{c}/cpufreq/cpuinfo_max_freq"
        ))
        .ok()?
        .trim()
        .parse()
        .ok()
    };
    let freqs: Vec<(usize, Option<u64>)> = allowed.iter().map(|&c| (c, freq(c))).collect();
    let Some(max) = freqs.iter().filter_map(|(_, f)| *f).max() else {
        return allowed.to_vec();
    };
    freqs
        .into_iter()
        .filter(|(_, f)| f.is_some_and(|f| f * 10 >= max * 9))
        .map(|(c, _)| c)
        .collect()
}

/// Detects a lane that runs flat out: on-CPU time over consecutive watchdog intervals.
#[derive(Debug, Clone, Default)]
pub struct DutyTracker {
    last: Option<(u64, u64)>,
    hot_streak: u32,
}

impl DutyTracker {
    /// A thread is runaway when its duty cycle exceeds [`Self::LIMIT`] for
    /// [`Self::INTERVALS`] consecutive intervals.
    pub const LIMIT: f64 = 0.8;
    pub const INTERVALS: u32 = 4;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed `(wall_ns, cpu_ns)` at the end of an interval; true when the thread must be demoted.
    #[allow(clippy::cast_precision_loss)]
    pub fn observe(&mut self, wall_ns: u64, cpu_ns: u64) -> bool {
        if let Some((w0, c0)) = self.last {
            let dw = wall_ns.saturating_sub(w0);
            let dc = cpu_ns.saturating_sub(c0);
            let duty = if dw == 0 { 0.0 } else { dc as f64 / dw as f64 };
            if duty > Self::LIMIT {
                self.hot_streak += 1;
            } else {
                self.hot_streak = 0;
            }
        }
        self.last = Some((wall_ns, cpu_ns));
        self.hot_streak >= Self::INTERVALS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_parses_names_with_spaces_and_parens() {
        // Real layout: 52 fields; only 19, 39, 40 and 41 matter here.
        let mut f: Vec<String> = (3..=52).map(|_| "0".to_owned()).collect();
        f[19 - 3] = "-5".into();
        f[39 - 3] = "7".into();
        f[40 - 3] = "15".into();
        f[41 - 3] = "1".into();
        let text = format!("4242 (gorca (in) put) {}", f.join(" "));
        let t = parse_stat(&text).unwrap();
        assert_eq!((t.tid, t.name.as_str()), (4242, "gorca (in) put"));
        assert_eq!((t.nice, t.cpu), (-5, 7));
        assert_eq!(t.policy, Policy::Fifo(15));
    }

    #[test]
    fn this_process_lists_itself_as_other() {
        let me = current_tid();
        let t = threads().into_iter().find(|t| t.tid == me).unwrap();
        assert_eq!(t.policy, Policy::Other);
        assert_eq!(policy_of(me).unwrap(), Policy::Other);
    }

    #[test]
    fn unprivileged_fifo_is_refused_and_demote_is_harmless() {
        // Hosts without RLIMIT_RTPRIO refuse; where it is granted the call may succeed, so only
        // the demotion postcondition is asserted. Runs on a throw-away thread.
        std::thread::spawn(|| {
            let _ = set_fifo(current_tid(), 1);
            demote_tid(current_tid()).unwrap();
            assert_eq!(policy_of(current_tid()).unwrap(), Policy::Other);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn affinity_round_trip() {
        std::thread::spawn(|| {
            let allowed = allowed_cpus();
            assert!(!allowed.is_empty());
            let tid = current_tid();
            set_affinity(tid, &allowed[..1]).unwrap();
            assert_eq!(allowed_cpus(), vec![allowed[0]]);
            assert!(set_affinity(tid, &[]).is_err());
        })
        .join()
        .unwrap();
    }

    #[test]
    fn duty_tracker_flags_only_sustained_flat_out_threads() {
        let ms = 1_000_000u64;
        // a busy burst of two intervals, then idle: not runaway
        let mut d = DutyTracker::new();
        assert!(!d.observe(0, 0));
        assert!(!d.observe(50 * ms, 49 * ms));
        assert!(!d.observe(100 * ms, 98 * ms));
        assert!(!d.observe(150 * ms, 100 * ms));
        // four flat-out intervals in a row: runaway on the fourth
        let mut d = DutyTracker::new();
        d.observe(0, 0);
        let verdicts: Vec<bool> = (1..=4u64)
            .map(|k| d.observe(50 * ms * k, 49 * ms * k))
            .collect();
        assert_eq!(verdicts, vec![false, false, false, true]);
    }

    #[test]
    fn fast_cpus_is_a_non_empty_subset() {
        let allowed = allowed_cpus();
        let fast = fast_cpus(&allowed);
        assert!(!fast.is_empty());
        assert!(fast.iter().all(|c| allowed.contains(c)));
    }
}
