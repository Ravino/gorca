// SPDX-License-Identifier: GPL-3.0-or-later
//! A lease on `/dev/cpu_dma_latency` (ADR-0026 section 2.4.7.10, DP-1 option (b), owner-approved
//! 2026-10-25).
//!
//! While plugins are active the process holds the device open with a latency target written to
//! it, which keeps every CPU out of the deep idle states (C2/C3, exit latencies of 100 us to 1 ms)
//! and so removes the wake tail of a parked plugin worker. The kernel drops the request when the
//! fd is closed or the process dies, so a crash cannot leave the host pinned.
//!
//! * [`PmQosLease::touch`] is the only call on the hot path: one relaxed store, plus a wake of the
//!   lease thread on the transition from idle to active. It never opens a file or blocks.
//! * The lease thread owns the fd. It takes the lease when touched, releases it
//!   [`HOLD_AFTER_LAST`] after the last touch (hysteresis: it does not flap per key) and, as a
//!   safety cap, drops and retakes it after [`MAX_HOLD`] of continuous holding.
//! * If the device cannot be opened, [`PmQosLease::open`] returns `None` after one info line; the
//!   caller then behaves as if there were no lease.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::process::monotonic_ns;

/// The device node.
pub const DEVICE: &str = "/dev/cpu_dma_latency";
/// The latency target in microseconds: forbids C2/C3, allows C1. Never lower: the floor keeps a
/// bug from pinning the CPUs in polling idle.
pub const TARGET_US: u32 = 20;
/// The lease is released this long after the last touch.
pub const HOLD_AFTER_LAST: Duration = Duration::from_secs(2);
/// Continuous holding is cut and retaken after this long.
pub const MAX_HOLD: Duration = Duration::from_secs(30);

static TAKES: AtomicU64 = AtomicU64::new(0);

/// How many times a lease was taken in this process (diagnostics).
#[must_use]
pub fn takes() -> u64 {
    TAKES.load(Ordering::Relaxed)
}

#[derive(Debug)]
struct Shared {
    last_ns: AtomicU64,
    wanted: AtomicBool,
    held: AtomicBool,
    stop: AtomicBool,
    gate: Mutex<()>,
    cv: Condvar,
}

/// The lease and its thread. Dropping it releases the lease and joins the thread.
#[derive(Debug)]
pub struct PmQosLease {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

fn take(path: &Path, target_us: u32) -> std::io::Result<File> {
    let mut f = OpenOptions::new().write(true).open(path)?;
    // The kernel accepts the raw native-endian 32-bit value.
    f.write_all(&target_us.to_ne_bytes())?;
    Ok(f)
}

impl PmQosLease {
    /// Probe `path` for write access and start the lease thread. `None` (after one info line) if
    /// the node cannot be opened. The probe holds no lease: nothing is written to the device.
    #[must_use]
    pub fn open(path: &Path, target_us: u32) -> Option<Self> {
        let target_us = target_us.max(TARGET_US);
        if let Err(e) = OpenOptions::new().write(true).open(path) {
            tracing::info!(
                path = %path.display(),
                errno = e.raw_os_error().unwrap_or(0),
                error = %e,
                "pm-qos-unavailable: no idle-state lease (install packaging/udev/70-gorca-cpu-dma-latency.rules to enable it)"
            );
            return None;
        }
        let shared = Arc::new(Shared {
            last_ns: AtomicU64::new(0),
            wanted: AtomicBool::new(false),
            held: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            gate: Mutex::new(()),
            cv: Condvar::new(),
        });
        let s = Arc::clone(&shared);
        let p = path.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("gorca-pmqos".into())
            .spawn(move || run(&s, &p, target_us))
            .ok()?;
        tracing::info!(
            target_us,
            "idle-state lease available (held only while plugins are active)"
        );
        Some(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Plugins are active at `now_ns` (`CLOCK_MONOTONIC`): take the lease or renew it. Never blocks
    /// beyond a short uncontended mutex.
    pub fn touch(&self, now_ns: u64) {
        self.shared.last_ns.store(now_ns, Ordering::Relaxed);
        if !self.shared.wanted.swap(true, Ordering::AcqRel) {
            let _g = self
                .shared
                .gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            self.shared.cv.notify_one();
        }
    }

    /// The lease is held right now.
    #[must_use]
    pub fn held(&self) -> bool {
        self.shared.held.load(Ordering::Acquire)
    }
}

impl Drop for PmQosLease {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        {
            let _g = self
                .shared
                .gate
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            self.shared.cv.notify_all();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn ns(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

fn run(s: &Shared, path: &Path, target_us: u32) {
    run_with(s, path, target_us, HOLD_AFTER_LAST, MAX_HOLD);
}

fn run_with(s: &Shared, path: &Path, target_us: u32, after_last: Duration, max_hold: Duration) {
    let tick = Duration::from_millis(20).min(after_last);
    let mut fd: Option<File> = None;
    let mut taken_at = 0u64;
    loop {
        if s.stop.load(Ordering::Acquire) {
            break;
        }
        let now = monotonic_ns();
        if fd.is_some() {
            let idle = now.saturating_sub(s.last_ns.load(Ordering::Relaxed)) > ns(after_last);
            let old = now.saturating_sub(taken_at) > ns(max_hold);
            if idle || old {
                fd = None;
                s.held.store(false, Ordering::Release);
                if idle {
                    s.wanted.store(false, Ordering::Release);
                    // A touch that raced with the release must not be lost.
                    if monotonic_ns().saturating_sub(s.last_ns.load(Ordering::Relaxed))
                        <= ns(after_last)
                    {
                        s.wanted.store(true, Ordering::Release);
                    }
                }
            }
        }
        if fd.is_none() && s.wanted.load(Ordering::Acquire) {
            match take(path, target_us) {
                Ok(f) => {
                    TAKES.fetch_add(1, Ordering::Relaxed);
                    taken_at = monotonic_ns();
                    fd = Some(f);
                    s.held.store(true, Ordering::Release);
                }
                Err(e) => {
                    tracing::info!(error = %e, "pm-qos-unavailable: the lease could not be taken");
                    s.wanted.store(false, Ordering::Release);
                }
            }
        }
        let g = s.gate.lock().unwrap_or_else(PoisonError::into_inner);
        if s.stop.load(Ordering::Acquire) {
            break;
        }
        // Idle and not wanted: sleep until touched (bounded, so stop is always noticed).
        let wait = if fd.is_none() && !s.wanted.load(Ordering::Acquire) {
            Duration::from_millis(200)
        } else {
            tick
        };
        let _ = s.cv.wait_timeout(g, wait);
    }
    drop(fd);
    s.held.store(false, Ordering::Release);
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn wait_for(f: impl Fn() -> bool) -> bool {
        let end = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        f()
    }

    fn fake(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("gorca-pmqos-{tag}-{}", std::process::id()));
        std::fs::write(&p, b"").unwrap();
        p
    }

    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            last_ns: AtomicU64::new(0),
            wanted: AtomicBool::new(false),
            held: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            gate: Mutex::new(()),
            cv: Condvar::new(),
        })
    }

    #[test]
    fn a_missing_device_is_a_quiet_none() {
        assert!(PmQosLease::open(Path::new("/nonexistent/cpu_dma_latency"), 20).is_none());
    }

    #[test]
    fn takes_on_touch_writes_the_target_and_releases_after_the_hold() {
        let p = fake("hold");
        let s = shared();
        let s2 = Arc::clone(&s);
        let p2 = p.clone();
        let t = std::thread::spawn(move || {
            run_with(
                &s2,
                &p2,
                20,
                Duration::from_millis(150),
                Duration::from_secs(30),
            );
        });
        assert!(!s.held.load(Ordering::Acquire));
        s.last_ns.store(monotonic_ns(), Ordering::Relaxed);
        s.wanted.store(true, Ordering::Release);
        assert!(wait_for(|| s.held.load(Ordering::Acquire)), "taken");
        assert_eq!(std::fs::read(&p).unwrap(), 20u32.to_ne_bytes());
        // Renewing keeps it past the hold time.
        for _ in 0..8 {
            s.last_ns.store(monotonic_ns(), Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(40));
        }
        assert!(s.held.load(Ordering::Acquire), "renewed");
        // No touch: released, and wanted is cleared.
        assert!(wait_for(|| !s.held.load(Ordering::Acquire)), "released");
        assert!(!s.wanted.load(Ordering::Acquire));
        s.stop.store(true, Ordering::Release);
        s.cv.notify_all();
        t.join().unwrap();
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn continuous_holding_is_cut_and_retaken() {
        let p = fake("cap");
        let s = shared();
        let s2 = Arc::clone(&s);
        let p2 = p.clone();
        let before = takes();
        let t = std::thread::spawn(move || {
            run_with(
                &s2,
                &p2,
                20,
                Duration::from_secs(5),
                Duration::from_millis(100),
            );
        });
        s.last_ns.store(monotonic_ns(), Ordering::Relaxed);
        s.wanted.store(true, Ordering::Release);
        assert!(wait_for(|| takes() >= before + 2), "retaken after the cap");
        s.stop.store(true, Ordering::Release);
        s.cv.notify_all();
        t.join().unwrap();
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn open_and_drop_join_the_thread() {
        let p = fake("drop");
        let l = PmQosLease::open(&p, 5).unwrap();
        l.touch(monotonic_ns());
        assert!(wait_for(|| l.held()));
        drop(l);
        let _ = std::fs::remove_file(p);
    }
}
