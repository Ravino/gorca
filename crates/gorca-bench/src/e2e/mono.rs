// SPDX-License-Identifier: GPL-3.0-or-later
//! `CLOCK_MONOTONIC` in nanoseconds: the one clock shared by every process of a run.
#![allow(unsafe_code)]

/// Nanoseconds on `CLOCK_MONOTONIC` (the same clock as Python's `time.monotonic_ns()`).
#[inline]
pub fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a live local; clock_gettime only writes through the pointer.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_monotonic_and_ns() {
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
        assert!(a > 1_000_000);
    }
}
