// SPDX-License-Identifier: GPL-3.0-or-later
//! Watchdog heartbeat (ADR-0001 section 6.6). A lane records progress; an OS thread outside
//! tokio evaluates it. A stall over 250 ms with a non-empty queue is logged; over 2 s it
//! triggers a supervised restart.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const STALL_LOG_AFTER: Duration = Duration::from_millis(250);
pub const STALL_RESTART_AFTER: Duration = Duration::from_secs(2);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LaneVerdict {
    Healthy,
    /// Log with a backtrace.
    Stalled,
    /// Supervised restart.
    Restart,
}

/// Lock-free progress record written by a lane, read by the watchdog.
#[derive(Debug)]
pub struct Heartbeat {
    base: Instant,
    last_progress_ns: AtomicU64,
    queued: AtomicU32,
}

impl Heartbeat {
    pub fn new(now: Instant) -> Self {
        Self {
            base: now,
            last_progress_ns: AtomicU64::new(0),
            queued: AtomicU32::new(0),
        }
    }

    /// Called by the lane after each processed item.
    pub fn beat(&self, now: Instant) {
        let ns =
            u64::try_from(now.saturating_duration_since(self.base).as_nanos()).unwrap_or(u64::MAX);
        self.last_progress_ns.fetch_max(ns, Ordering::Release);
    }

    /// Called by the lane: items waiting in its queue.
    pub fn set_queued(&self, n: u32) {
        self.queued.store(n, Ordering::Release);
    }

    /// Time since the last processed item while the queue is non-empty; an idle lane is healthy.
    pub fn verdict(&self, now: Instant) -> LaneVerdict {
        if self.queued.load(Ordering::Acquire) == 0 {
            return LaneVerdict::Healthy;
        }
        let last = self.base + Duration::from_nanos(self.last_progress_ns.load(Ordering::Acquire));
        let since = now.saturating_duration_since(last);
        if since > STALL_RESTART_AFTER {
            LaneVerdict::Restart
        } else if since > STALL_LOG_AFTER {
            LaneVerdict::Stalled
        } else {
            LaneVerdict::Healthy
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_lane_is_healthy_however_long_it_waits() {
        let t0 = Instant::now();
        let h = Heartbeat::new(t0);
        assert_eq!(
            h.verdict(t0 + Duration::from_secs(60)),
            LaneVerdict::Healthy
        );
    }

    #[test]
    fn stall_thresholds() {
        let t0 = Instant::now();
        let h = Heartbeat::new(t0);
        h.set_queued(3);
        h.beat(t0 + Duration::from_millis(10));
        assert_eq!(
            h.verdict(t0 + Duration::from_millis(200)),
            LaneVerdict::Healthy
        );
        assert_eq!(
            h.verdict(t0 + Duration::from_millis(300)),
            LaneVerdict::Stalled
        );
        assert_eq!(
            h.verdict(t0 + Duration::from_millis(2100)),
            LaneVerdict::Restart
        );
        h.beat(t0 + Duration::from_millis(2100));
        assert_eq!(
            h.verdict(t0 + Duration::from_millis(2150)),
            LaneVerdict::Healthy
        );
    }
}
