//! The applications protected from eviction, published lock-free.
//!
//! The Reader publishes an application only after it **accepted and presented** that application's
//! focus (the focus state machine passed it and a state fetch showed the node `FOCUSED`); the
//! connector's router reads it when it must decide who gives way at the application cap.
//!
//! Protection must not be cheap to win or to steal, so it is deliberately narrow:
//!
//! - at most **two** applications are protected: the current one, and the previous one for a
//!   short grace period ([`GRACE_MS`]);
//! - the previous slot is only taken by an application that held focus for at least
//!   [`MIN_DWELL_MS`]. A hostile application that flips focus quickly replaces only the
//!   *current* slot; the real focused application it displaced does not lose its grace.
//!
//! Identity is the `AppId`, which is minted per bus unique name and never reused.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use gorca_model::{AppId, ConnectorId};

/// How long the previously focused application stays protected after focus moved on.
pub const GRACE_MS: u64 = 5_000;
/// Focus must have been held this long for the application to earn a grace period.
pub const MIN_DWELL_MS: u64 = 1_000;

/// Milliseconds on a monotonic clock shared by all callers of this module.
pub fn now_ms() -> u64 {
    static BASE: OnceLock<Instant> = OnceLock::new();
    let base = BASE.get_or_init(Instant::now);
    u64::try_from(base.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn encode(app: AppId) -> u64 {
    (u64::from(app.connector.0) + 1) << 32 | u64::from(app.serial)
}

fn decode(v: u64) -> Option<AppId> {
    if v == 0 {
        return None;
    }
    Some(AppId {
        connector: ConnectorId(u16::try_from((v >> 32) - 1).ok()?),
        serial: u32::try_from(v & 0xffff_ffff).ok()?,
    })
}

/// `0` in an app slot means "none".
#[derive(Debug, Default)]
pub struct FocusedApp {
    current: AtomicU64,
    since_ms: AtomicU64,
    previous: AtomicU64,
    previous_until_ms: AtomicU64,
}

impl FocusedApp {
    pub const fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            since_ms: AtomicU64::new(0),
            previous: AtomicU64::new(0),
            previous_until_ms: AtomicU64::new(0),
        }
    }

    /// `app` has been accepted as the focused application.
    pub fn publish(&self, app: AppId) {
        self.publish_at(app, now_ms());
    }

    /// [`FocusedApp::publish`] with an explicit clock (tests).
    pub fn publish_at(&self, app: AppId, now: u64) {
        let new = encode(app);
        let cur = self.current.load(Ordering::Acquire);
        if cur == new {
            return;
        }
        let held = now.saturating_sub(self.since_ms.load(Ordering::Acquire));
        if cur != 0 && held >= MIN_DWELL_MS {
            self.previous.store(cur, Ordering::Release);
            self.previous_until_ms
                .store(now.saturating_add(GRACE_MS), Ordering::Release);
        }
        self.since_ms.store(now, Ordering::Release);
        self.current.store(new, Ordering::Release);
    }

    pub fn clear(&self) {
        self.current.store(0, Ordering::Release);
    }

    /// The current focused application.
    pub fn get(&self) -> Option<AppId> {
        decode(self.current.load(Ordering::Acquire))
    }

    /// Is `app` protected right now (current, or previous within its grace)?
    pub fn protects(&self, app: AppId) -> bool {
        self.protects_at(app, now_ms())
    }

    pub fn protects_at(&self, app: AppId, now: u64) -> bool {
        let v = encode(app);
        self.current.load(Ordering::Acquire) == v
            || (self.previous.load(Ordering::Acquire) == v
                && now < self.previous_until_ms.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(n: u32) -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: n,
        }
    }

    #[test]
    fn round_trips_every_app_id_including_zero() {
        let f = FocusedApp::new();
        assert_eq!(f.get(), None);
        for (c, n) in [(0u16, 0u32), (1, 7), (u16::MAX, u32::MAX)] {
            let a = AppId {
                connector: ConnectorId(c),
                serial: n,
            };
            f.publish_at(a, 10_000);
            assert_eq!(f.get(), Some(a));
        }
        f.clear();
        assert_eq!(f.get(), None);
    }

    #[test]
    fn the_previous_app_keeps_protection_for_the_grace_period_only() {
        let f = FocusedApp::new();
        f.publish_at(app(1), 0);
        f.publish_at(app(2), 10_000); // app 1 held focus long enough
        assert!(f.protects_at(app(2), 10_001));
        assert!(f.protects_at(app(1), 10_000 + GRACE_MS - 1));
        assert!(!f.protects_at(app(1), 10_000 + GRACE_MS));
    }

    /// A hostile app that flips focus quickly cannot push the real focused app out of its
    /// protection: only apps that held focus for the minimum dwell earn the grace slot.
    #[test]
    fn rapid_flipping_does_not_displace_the_real_apps_grace() {
        let f = FocusedApp::new();
        f.publish_at(app(1), 0);
        f.publish_at(app(2), 10_000); // the user works in app 2 ...
        f.publish_at(app(66), 12_000); // ... a hostile app grabs focus (app 2 held it 2 s)
        f.publish_at(app(67), 12_100); // ... and another one right after
        assert!(
            f.protects_at(app(2), 12_200),
            "the real app keeps its grace"
        );
        assert!(
            f.protects_at(app(67), 12_200),
            "the current one is protected"
        );
        assert!(!f.protects_at(app(66), 12_200), "a flipper earns nothing");
        assert!(!f.protects_at(app(1), 12_200), "never more than two");
    }
}
