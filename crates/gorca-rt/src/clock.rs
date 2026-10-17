// SPDX-License-Identifier: GPL-3.0-or-later
//! Clock abstraction; virtual in tests (ADR-0001 section 5.1 `ConnectorCtx::clock`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A manually advanced clock. Cloning shares the same time line.
#[derive(Clone, Debug)]
pub struct VirtualClock {
    base: Instant,
    offset_ns: Arc<AtomicU64>,
}

impl VirtualClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
            offset_ns: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn advance(&self, by: Duration) {
        let ns = u64::try_from(by.as_nanos()).unwrap_or(u64::MAX);
        self.offset_ns.fetch_add(ns, Ordering::AcqRel);
    }

    pub fn now(&self) -> Instant {
        self.base + Duration::from_nanos(self.offset_ns.load(Ordering::Acquire))
    }
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default)]
pub enum Clock {
    #[default]
    System,
    Virtual(VirtualClock),
}

impl Clock {
    pub fn now(&self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            Clock::Virtual(v) => v.now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_only_moves_when_advanced() {
        let v = VirtualClock::new();
        let clock = Clock::Virtual(v.clone());
        let t0 = clock.now();
        assert_eq!(clock.now(), t0);
        v.advance(Duration::from_millis(30));
        assert_eq!(clock.now() - t0, Duration::from_millis(30));
    }

    #[test]
    fn system_clock_is_monotonic() {
        let c = Clock::System;
        let a = c.now();
        assert!(c.now() >= a);
    }
}
