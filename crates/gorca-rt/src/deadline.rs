// SPDX-License-Identifier: GPL-3.0-or-later
//! Deadlines and budget classes (ADR-0001 section 6.4). "Every await on IPC goes through a
//! timeout": [`with_deadline`] is the single wrapper.

use std::future::Future;
use std::time::{Duration, Instant};

use crate::clock::Clock;

/// Budget class of a request. Default deadlines from section 6.4.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum BudgetClass {
    /// 120 ms total per user action, split across calls.
    Interactive,
    /// Ancestry, siblings, document chunks.
    Prefetch,
    /// Full subtree sync, `GetItems`.
    Background,
    /// First contact with a new app, P2P connect.
    Startup,
}

impl BudgetClass {
    pub const fn default_timeout(self) -> Duration {
        match self {
            BudgetClass::Interactive => Duration::from_millis(120),
            BudgetClass::Prefetch => Duration::from_millis(300),
            BudgetClass::Background => Duration::from_secs(2),
            BudgetClass::Startup => Duration::from_secs(5),
        }
    }
}

/// An absolute point in time after which a request is abandoned.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Deadline(pub Instant);

impl Deadline {
    pub fn after(clock: &Clock, d: Duration) -> Self {
        Deadline(clock.now() + d)
    }

    pub fn for_class(clock: &Clock, class: BudgetClass) -> Self {
        Self::after(clock, class.default_timeout())
    }

    pub fn remaining(self, now: Instant) -> Duration {
        self.0.saturating_duration_since(now)
    }

    pub fn expired(self, now: Instant) -> bool {
        now >= self.0
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("deadline exceeded")]
pub struct DeadlineExceeded;

/// Await `fut`, but never past `deadline`. The future is dropped (cancelled) on expiry.
pub async fn with_deadline<F: Future>(
    deadline: Deadline,
    fut: F,
) -> Result<F::Output, DeadlineExceeded> {
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.0), fut)
        .await
        .map_err(|_| DeadlineExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_defaults_match_the_adr_table() {
        assert_eq!(
            BudgetClass::Interactive.default_timeout(),
            Duration::from_millis(120)
        );
        assert_eq!(
            BudgetClass::Prefetch.default_timeout(),
            Duration::from_millis(300)
        );
        assert_eq!(
            BudgetClass::Background.default_timeout(),
            Duration::from_secs(2)
        );
        assert_eq!(
            BudgetClass::Startup.default_timeout(),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn remaining_saturates() {
        let clock = Clock::System;
        let d = Deadline::after(&clock, Duration::from_millis(10));
        assert_eq!(d.remaining(d.0 + Duration::from_secs(1)), Duration::ZERO);
        assert!(d.expired(d.0));
    }

    #[tokio::test(start_paused = true)]
    async fn hung_future_is_cut_at_the_deadline() {
        let d = Deadline(Instant::now() + Duration::from_millis(50));
        let r = with_deadline(d, std::future::pending::<()>()).await;
        assert_eq!(r, Err(DeadlineExceeded));
    }

    #[tokio::test(start_paused = true)]
    async fn ready_future_passes_through() {
        let d = Deadline(Instant::now() + Duration::from_millis(50));
        assert_eq!(with_deadline(d, async { 7 }).await, Ok(7));
    }
}
