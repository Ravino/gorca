// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded-wait primitives for the plugin runtime (ADR-0026 §2.4.6, r4).
//!
//! A writer must wait for plugin actors for at most the shared deadline `D`, without a blocking
//! receive inside tokio and with sub-millisecond resolution. The mechanism is a futex word waited
//! on with an absolute `CLOCK_MONOTONIC` deadline, with the thread's timer slack set to 1 ns so
//! the wake is not late by the default 50 us.
#![allow(unsafe_code)]

use std::io;
use std::sync::atomic::AtomicU32;

/// `futex_wait(addr, expected)` until `deadline_ns` (absolute `CLOCK_MONOTONIC`, same clock as
/// [`crate::process::monotonic_ns`]). Returns `true` when woken, interrupted or the value
/// differed, `false` on timeout.
pub fn futex_wait_abs(addr: &AtomicU32, expected: u32, deadline_ns: u64) -> bool {
    futex_wait_bitset_abs(
        addr,
        expected,
        deadline_ns,
        libc::FUTEX_BITSET_MATCH_ANY.cast_unsigned(),
    )
}

/// As [`futex_wait_abs`], but only a [`futex_wake_bitset`] whose mask shares a bit with `mask`
/// wakes the waiter. Many threads can park on one word and be woken selectively by one call.
pub fn futex_wait_bitset_abs(addr: &AtomicU32, expected: u32, deadline_ns: u64, mask: u32) -> bool {
    let ts = libc::timespec {
        tv_sec: libc::time_t::try_from(deadline_ns / 1_000_000_000).unwrap_or(libc::time_t::MAX),
        tv_nsec: libc::c_long::try_from(deadline_ns % 1_000_000_000).unwrap_or(0),
    };
    // SAFETY: `addr` points at a live, 4-byte aligned AtomicU32 for the whole call; `ts` outlives
    // the call; FUTEX_WAIT_BITSET takes an absolute CLOCK_MONOTONIC timeout when
    // FUTEX_CLOCK_REALTIME is not set; the remaining arguments are unused for this operation.
    let r = unsafe {
        libc::syscall(
            libc::SYS_futex,
            std::ptr::from_ref(addr),
            libc::FUTEX_WAIT_BITSET | libc::FUTEX_PRIVATE_FLAG,
            expected,
            &raw const ts,
            std::ptr::null::<u32>(),
            mask,
        )
    };
    if r == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() != Some(libc::ETIMEDOUT)
}

/// Wake one waiter on `addr`.
pub fn futex_wake(addr: &AtomicU32) {
    // SAFETY: `addr` points at a live AtomicU32; FUTEX_WAKE reads no other memory and the
    // remaining arguments are unused.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            std::ptr::from_ref(addr),
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            1,
            0,
            0,
            0,
        );
    }
}

/// Wake up to `count` waiters on `addr` whose wait mask shares a bit with `mask`, with one system
/// call (ADR-0026 section 2.4.7.4 step 2: one wake for all the cold jobs of a handoff).
pub fn futex_wake_bitset(addr: &AtomicU32, count: i32, mask: u32) {
    // SAFETY: `addr` points at a live AtomicU32; FUTEX_WAKE_BITSET reads no other memory and the
    // remaining arguments are unused for this operation.
    unsafe {
        libc::syscall(
            libc::SYS_futex,
            std::ptr::from_ref(addr),
            libc::FUTEX_WAKE_BITSET | libc::FUTEX_PRIVATE_FLAG,
            count,
            0,
            0,
            mask,
        );
    }
}

/// Set the calling thread's timer slack in nanoseconds (`PR_SET_TIMERSLACK`). 0 would mean
/// "reset to the default", so callers pass 1 for the tightest setting.
pub fn set_timer_slack(ns: u64) -> io::Result<()> {
    // SAFETY: prctl with an integer argument; no memory is read or written.
    let rc = unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, ns as libc::c_ulong, 0, 0, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The calling thread's timer slack in nanoseconds.
pub fn timer_slack() -> io::Result<u64> {
    // SAFETY: prctl query; the return value is the slack, no memory is touched.
    let rc = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK, 0, 0, 0, 0) };
    u64::try_from(rc).map_err(|_| io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::monotonic_ns;
    use std::sync::atomic::Ordering;

    #[test]
    fn futex_wait_times_out_near_the_deadline() {
        set_timer_slack(1).unwrap();
        assert_eq!(timer_slack().unwrap(), 1);
        let w = AtomicU32::new(0);
        let t0 = monotonic_ns();
        let woke = futex_wait_abs(&w, 0, t0 + 300_000);
        let dt = monotonic_ns() - t0;
        assert!(!woke);
        assert!(dt >= 300_000, "woke early: {dt}");
        assert!(dt < 20_000_000, "woke very late: {dt}");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // a test thread, not a lane
    fn one_bitset_wake_wakes_exactly_the_matching_waiters() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU32;
        let w = Arc::new(AtomicU32::new(0));
        let woke = Arc::new(AtomicU32::new(0));
        let mut hs = Vec::new();
        for bit in 0..3u32 {
            let (w, woke) = (Arc::clone(&w), Arc::clone(&woke));
            hs.push(std::thread::spawn(move || {
                let t0 = monotonic_ns();
                // Each waiter has its own bit; the value never changes, only a wake ends it.
                let _ = futex_wait_bitset_abs(&w, 0, t0 + 400_000_000, 1 << bit);
                if monotonic_ns() - t0 < 300_000_000 {
                    woke.fetch_or(1 << bit, Ordering::SeqCst);
                }
            }));
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
        futex_wake_bitset(&w, i32::MAX, 0b101);
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(woke.load(Ordering::SeqCst), 0b101, "bit 1 stays asleep");
        for h in hs {
            h.join().unwrap();
        }
    }

    #[test]
    fn futex_returns_at_once_when_the_value_differs() {
        let w = AtomicU32::new(1);
        let t0 = monotonic_ns();
        assert!(futex_wait_abs(&w, 0, t0 + 1_000_000_000));
        assert!(monotonic_ns() - t0 < 100_000_000);
        w.store(2, Ordering::SeqCst);
    }
}
