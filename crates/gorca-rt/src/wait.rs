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
            libc::FUTEX_BITSET_MATCH_ANY,
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
    fn futex_returns_at_once_when_the_value_differs() {
        let w = AtomicU32::new(1);
        let t0 = monotonic_ns();
        assert!(futex_wait_abs(&w, 0, t0 + 1_000_000_000));
        assert!(monotonic_ns() - t0 < 100_000_000);
        w.store(2, Ordering::SeqCst);
    }
}
