// SPDX-License-Identifier: MIT OR Apache-2.0
//! Wake-latency probes (WQ-6 / M9): how late does a bounded wait wake, with a futex (absolute
//! `CLOCK_MONOTONIC` deadline) or a `timerfd`, with timer slack 1 ns or the default, and what does
//! a writer -> actor -> reply round trip cost with spin 0 / 10 / 20 us. These are mechanism
//! probes, independent of any arm: candidates use [`futex_wait_abs`] / [`futex_wake`] too and
//! report the lateness of their own waits through `HandoffOutcome::woke_late`.

use crate::timing::{now_ns, sat32, set_timerslack};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// `futex_wait(addr, expected)` until `deadline_ns` (absolute CLOCK_MONOTONIC). Returns `true`
/// when woken or the value differed, `false` on timeout.
pub fn futex_wait_abs(addr: &AtomicU32, expected: u32, deadline_ns: u64) -> bool {
    let ts = libc::timespec {
        tv_sec: (deadline_ns / 1_000_000_000) as libc::time_t,
        tv_nsec: (deadline_ns % 1_000_000_000) as libc::c_long,
    };
    // SAFETY: `addr` is a live AtomicU32 (4-byte aligned); `ts` outlives the call; FUTEX_WAIT_BITSET
    // takes an absolute CLOCK_MONOTONIC timeout when FUTEX_CLOCK_REALTIME is not set.
    let r = unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr as *const AtomicU32,
            libc::FUTEX_WAIT_BITSET | libc::FUTEX_PRIVATE_FLAG,
            expected,
            &ts as *const libc::timespec,
            std::ptr::null::<u32>(),
            libc::FUTEX_BITSET_MATCH_ANY,
        )
    };
    if r == 0 {
        return true;
    }
    let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    e != libc::ETIMEDOUT
}

/// Wake one waiter on `addr`.
pub fn futex_wake(addr: &AtomicU32) {
    // SAFETY: `addr` is a live AtomicU32; FUTEX_WAKE reads no other memory.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            addr as *const AtomicU32,
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1,
            0,
            0,
            0,
        );
    }
}

/// A CLOCK_MONOTONIC `timerfd`, armed with absolute deadlines.
pub struct TimerFd(libc::c_int);

impl TimerFd {
    pub fn new() -> std::io::Result<TimerFd> {
        // SAFETY: plain syscall.
        let fd = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC) };
        if fd < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(TimerFd(fd))
        }
    }

    pub fn arm_abs(&self, deadline_ns: u64) {
        let spec = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: (deadline_ns / 1_000_000_000) as libc::time_t,
                tv_nsec: (deadline_ns % 1_000_000_000) as libc::c_long,
            },
        };
        // SAFETY: valid fd and itimerspec.
        unsafe {
            libc::timerfd_settime(self.0, libc::TFD_TIMER_ABSTIME, &spec, std::ptr::null_mut())
        };
    }

    /// Block until the timer fires (blocking read of the expiration count).
    pub fn wait(&self) {
        let mut buf = 0u64;
        // SAFETY: reads 8 bytes into a valid u64.
        unsafe { libc::read(self.0, &mut buf as *mut u64 as *mut libc::c_void, 8) };
    }
}

impl Drop for TimerFd {
    fn drop(&mut self) {
        // SAFETY: fd owned by this struct.
        unsafe { libc::close(self.0) };
    }
}

#[derive(Copy, Clone, Debug)]
pub enum Mechanism {
    Futex,
    TimerFd,
    NanoSleep,
}

impl Mechanism {
    pub fn name(self) -> &'static str {
        match self {
            Mechanism::Futex => "futex",
            Mechanism::TimerFd => "timerfd",
            Mechanism::NanoSleep => "clock_nanosleep",
        }
    }
}

/// Lateness (actual wake minus the absolute deadline) of `n` timed waits of `wait_us`, run on a
/// fresh thread whose timer slack is `slack_ns` (None = leave the default of 50 us).
pub fn lateness(mech: Mechanism, n: usize, wait_us: u64, slack_ns: Option<u64>) -> Vec<u32> {
    std::thread::spawn(move || {
        if let Some(s) = slack_ns {
            let _ = set_timerslack(s);
        }
        let tfd = TimerFd::new().ok();
        let word = AtomicU32::new(0);
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let deadline = now_ns() + wait_us * 1000;
            match mech {
                Mechanism::Futex => {
                    futex_wait_abs(&word, 0, deadline);
                }
                Mechanism::TimerFd => {
                    if let Some(t) = &tfd {
                        t.arm_abs(deadline);
                        t.wait();
                    }
                }
                Mechanism::NanoSleep => {
                    let ts = libc::timespec {
                        tv_sec: (deadline / 1_000_000_000) as libc::time_t,
                        tv_nsec: (deadline % 1_000_000_000) as libc::c_long,
                    };
                    // SAFETY: valid timespec, absolute sleep.
                    unsafe {
                        libc::clock_nanosleep(
                            libc::CLOCK_MONOTONIC,
                            libc::TIMER_ABSTIME,
                            &ts,
                            std::ptr::null_mut(),
                        )
                    };
                }
            }
            out.push(sat32(now_ns().saturating_sub(deadline)));
        }
        out
    })
    .join()
    .unwrap_or_default()
}

/// Round trip writer -> actor -> reply over two futex words, with the writer spinning `spin_ns`
/// before it parks and the actor spinning `spin_ns` before it parks. Returns ns per round trip.
pub fn roundtrip(n: usize, spin_ns: u64, slack_ns: Option<u64>, pause_us: u64) -> Vec<u32> {
    let req = Arc::new(AtomicU32::new(0));
    let resp = Arc::new(AtomicU32::new(0));
    let stop = Arc::new(AtomicU32::new(0));
    let (r2, p2, s2) = (req.clone(), resp.clone(), stop.clone());
    let actor = std::thread::spawn(move || {
        if let Some(s) = slack_ns {
            let _ = set_timerslack(s);
        }
        let mut seen = 0u32;
        loop {
            let spin_end = now_ns() + spin_ns;
            loop {
                let v = r2.load(Ordering::Acquire);
                if v != seen {
                    break;
                }
                if s2.load(Ordering::Acquire) != 0 {
                    return;
                }
                if now_ns() < spin_end {
                    std::hint::spin_loop();
                } else {
                    futex_wait_abs(&r2, seen, now_ns() + 1_000_000);
                }
            }
            seen = r2.load(Ordering::Acquire);
            p2.store(seen, Ordering::Release);
            futex_wake(&p2);
        }
    });
    if let Some(s) = slack_ns {
        let _ = set_timerslack(s);
    }
    let mut out = Vec::with_capacity(n);
    for i in 1..=n as u32 {
        if pause_us > 0 {
            crate::timing::wait_until(now_ns() + pause_us * 1000, 20_000);
        }
        let t = now_ns();
        req.store(i, Ordering::Release);
        futex_wake(&req);
        let spin_end = t + spin_ns;
        while resp.load(Ordering::Acquire) != i {
            if now_ns() < spin_end {
                std::hint::spin_loop();
            } else {
                futex_wait_abs(&resp, i - 1, now_ns() + 1_000_000);
            }
        }
        out.push(sat32(now_ns() - t));
    }
    stop.store(1, Ordering::Release);
    futex_wake(&req);
    let _ = actor.join();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lateness_is_bounded_with_slack_1ns() {
        let v = lateness(Mechanism::Futex, 200, 100, Some(1));
        assert_eq!(v.len(), 200);
        let mut s = v.clone();
        s.sort_unstable();
        // Not a latency gate (host dependent): a wake with 1 ns slack must not be tens of ms late.
        assert!(s[s.len() / 2] < 5_000_000, "median {}", s[s.len() / 2]);
    }

    #[test]
    fn roundtrip_completes() {
        let v = roundtrip(200, 10_000, Some(1), 20);
        assert_eq!(v.len(), 200);
    }

    #[test]
    fn timerfd_fires() {
        let v = lateness(Mechanism::TimerFd, 50, 100, Some(1));
        assert_eq!(v.len(), 50);
    }
}
