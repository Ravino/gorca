// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::{Duration, Instant};

use gorca_ext_api::{DISABLE_AFTER_RESETS, DISABLE_WINDOW_SECS};
use gorca_rt::{RestartDecision, RestartPolicy, RestartTracker};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InstanceHealth {
    Active,
    /// Disabled for this app; the user is told once.
    Disabled,
}

/// Tracks instance resets for one (extension, app): a trap or an exceeded limit resets the
/// instance; three resets in 60 s disable it.
#[derive(Debug)]
pub struct InstanceGuard {
    tracker: RestartTracker,
    health: InstanceHealth,
}

impl Default for InstanceGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl InstanceGuard {
    pub fn new() -> Self {
        Self {
            tracker: RestartTracker::new(RestartPolicy {
                // `RestartTracker` quarantines when failures exceed this, so the third reset
                // is the one that disables.
                max_restarts: DISABLE_AFTER_RESETS - 1,
                window: Duration::from_secs(DISABLE_WINDOW_SECS),
            }),
            health: InstanceHealth::Active,
        }
    }

    pub fn health(&self) -> InstanceHealth {
        self.health
    }

    /// Record a reset. Returns the new health; `Disabled` is sticky.
    pub fn on_reset(&mut self, now: Instant) -> InstanceHealth {
        match self.health {
            InstanceHealth::Disabled => InstanceHealth::Disabled,
            InstanceHealth::Active => {
                if self.tracker.record(now) == RestartDecision::Quarantine {
                    self.health = InstanceHealth::Disabled;
                }
                self.health
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn third_reset_within_a_minute_disables() {
        let t = Instant::now();
        let mut g = InstanceGuard::new();
        assert_eq!(g.on_reset(t), InstanceHealth::Active);
        assert_eq!(
            g.on_reset(t + Duration::from_secs(10)),
            InstanceHealth::Active
        );
        assert_eq!(
            g.on_reset(t + Duration::from_secs(20)),
            InstanceHealth::Disabled
        );
        // Sticky, even long after.
        assert_eq!(
            g.on_reset(t + Duration::from_secs(600)),
            InstanceHealth::Disabled
        );
    }

    #[test]
    fn spread_out_resets_never_disable() {
        let t = Instant::now();
        let mut g = InstanceGuard::new();
        for i in 0..10 {
            assert_eq!(
                g.on_reset(t + Duration::from_secs(i * 40)),
                InstanceHealth::Active
            );
        }
    }
}
