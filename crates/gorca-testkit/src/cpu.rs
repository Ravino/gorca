//! Preemption-proof timing for latency-bound tests.
//!
//! A bound such as "one push holds the lock for at most 250 us" is a statement about the work
//! the code does, not about how the scheduler treats the test thread. Wall-clock samples
//! (`Instant`) on a loaded host include time spent runnable-but-not-running, which turns a
//! correct implementation into a flaky test (load average 15 on 16 cores: one preemption of a
//! few ms in 20 samples is enough).
//!
//! Every such test measures per-thread CPU time instead (`CLOCK_THREAD_CPUTIME_ID`): the clock
//! only advances while this thread executes, so preemption and sleeping are excluded and real
//! work, including an injected spin, is included. The code under test must run on the measuring
//! thread. Cache and SMT interference still inflate CPU time a little; the bounds keep their
//! full margin for that and are never loosened.
//!
//! Tests that wait for other threads, sockets or timers are different: there the wall-clock
//! deadline is the property under test and these helpers do not apply.

// The one `unsafe` call is `clock_gettime` on a valid out-pointer.
#![allow(unsafe_code)]

use std::time::Duration;

/// CPU time consumed so far by the calling thread.
///
/// # Panics
/// If the kernel rejects `clock_gettime(CLOCK_THREAD_CPUTIME_ID)`, which cannot happen on Linux.
#[must_use]
pub fn thread_cpu_now() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec and the clock id is a constant Linux supports.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut ts) };
    assert_eq!(rc, 0, "clock_gettime(CLOCK_THREAD_CPUTIME_ID) failed");
    Duration::new(
        u64::try_from(ts.tv_sec).unwrap_or(0),
        u32::try_from(ts.tv_nsec).unwrap_or(0),
    )
}

/// Runs `f` and returns its result with the CPU time the calling thread spent in it.
pub fn cpu_time<R>(f: impl FnOnce() -> R) -> (R, Duration) {
    let t0 = thread_cpu_now();
    let r = f();
    let dt = thread_cpu_now().saturating_sub(t0);
    (r, dt)
}

/// Tracks the worst CPU time over a series of single-operation samples.
#[derive(Debug, Default, Clone, Copy)]
pub struct WorstCpu(Duration);

impl WorstCpu {
    #[must_use]
    pub fn new() -> Self {
        Self(Duration::ZERO)
    }

    /// Runs `f`, records its CPU time and returns its result.
    pub fn sample<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let (r, dt) = cpu_time(f);
        self.0 = self.0.max(dt);
        r
    }

    #[must_use]
    pub fn worst(self) -> Duration {
        self.0
    }
}

/// Attempts [`best_of`] makes by default.
pub const ATTEMPTS: u32 = 5;

/// Minimum-of-k over whole measurements, stopping early once one is within `bound`.
///
/// CPU time removes preemption, but not contention: when sibling hyperthreads and the shared
/// cache are saturated (this host runs at load average ~15 on 16 cores, and a test binary runs
/// its tests in parallel) the same instructions take 2-5x the CPU time in one attempt and not
/// in the next. A real defect (an O(n) step under a lock, a 1 ms spin) costs more than `bound`
/// in *every* attempt, so the minimum over `attempts` independent repeats of the complete
/// measurement still fails it, while a contention spike must hit all repeats to cause a false
/// failure. `run` returns the worst per-operation CPU time of one full repeat and must be
/// repeatable. The bound itself is never relaxed.
pub fn best_of(attempts: u32, bound: Duration, mut run: impl FnMut() -> Duration) -> Duration {
    let mut best = Duration::MAX;
    for _ in 0..attempts.max(1) {
        best = best.min(run());
        if best <= bound {
            break;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Real CPU work is counted.
    #[test]
    fn spin_is_measured() {
        let ((), dt) = cpu_time(|| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_millis(2) {
                std::hint::spin_loop();
            }
        });
        assert!(dt >= Duration::from_millis(1), "{dt:?}");
    }

    /// Sleeping (what preemption looks like to the thread) is not counted.
    #[test]
    #[allow(clippy::disallowed_methods)] // a sleeping thread is exactly what preemption looks like
    fn sleep_is_not_measured() {
        let ((), dt) = cpu_time(|| std::thread::sleep(Duration::from_millis(30)));
        assert!(dt < Duration::from_millis(5), "{dt:?}");
    }
}
