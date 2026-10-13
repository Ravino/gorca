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
    /// Bumped on every change of who is protected, so a decision made on a snapshot can be
    /// re-validated at the moment it commits.
    generation: AtomicU64,
    /// The application whose protection is suspended because it could not be re-validated.
    suspended: AtomicU64,
}

impl FocusedApp {
    pub const fn new() -> Self {
        Self {
            current: AtomicU64::new(0),
            since_ms: AtomicU64::new(0),
            previous: AtomicU64::new(0),
            previous_until_ms: AtomicU64::new(0),
            generation: AtomicU64::new(0),
            suspended: AtomicU64::new(0),
        }
    }

    /// Changes whenever the set of protected applications changes.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn bump(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Protection could not be re-validated: take it away now, but remember who had it, so that
    /// [`FocusedApp::resume`] can give it back once the bus has confirmed the application.
    pub fn suspend(&self) {
        let cur = self.current.swap(0, Ordering::AcqRel);
        if cur != 0 {
            self.suspended.store(cur, Ordering::Release);
        }
        self.previous.store(0, Ordering::Release);
        self.bump();
    }

    /// Re-validation succeeded: restore what [`FocusedApp::suspend`] took, unless the Reader has
    /// published something newer in the meantime.
    pub fn resume(&self) {
        let s = self.suspended.swap(0, Ordering::AcqRel);
        if s != 0
            && self
                .current
                .compare_exchange(0, s, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.since_ms.store(now_ms(), Ordering::Release);
            self.bump();
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
        self.suspended.store(0, Ordering::Release);
        self.bump();
    }

    /// The Reader's focus moved to something it could not validate (or to nothing): the current
    /// application stops being protected at once; it keeps the grace period only if it earned it.
    pub fn demote(&self) {
        self.demote_at(now_ms());
    }

    pub fn demote_at(&self, now: u64) {
        let cur = self.current.load(Ordering::Acquire);
        if cur == 0 {
            return;
        }
        let held = now.saturating_sub(self.since_ms.load(Ordering::Acquire));
        if held >= MIN_DWELL_MS {
            self.previous.store(cur, Ordering::Release);
            self.previous_until_ms
                .store(now.saturating_add(GRACE_MS), Ordering::Release);
        }
        self.current.store(0, Ordering::Release);
        self.bump();
    }

    /// `app` is gone (it left the bus, or reconciliation found it missing): no protection at all,
    /// not even a grace period.
    pub fn forget(&self, app: AppId) {
        let v = encode(app);
        if self.current.load(Ordering::Acquire) == v {
            self.current.store(0, Ordering::Release);
        }
        if self.previous.load(Ordering::Acquire) == v {
            self.previous.store(0, Ordering::Release);
        }
        if self.suspended.load(Ordering::Acquire) == v {
            self.suspended.store(0, Ordering::Release);
        }
        self.bump();
    }

    pub fn clear(&self) {
        self.current.store(0, Ordering::Release);
        self.previous.store(0, Ordering::Release);
        self.suspended.store(0, Ordering::Release);
        self.bump();
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
    /// Fail closed: when the Reader's focus moves to something unvalidated, the old application
    /// does not keep its immunity; a departed application loses it at once.
    #[test]
    fn protection_follows_reality_and_never_outlives_it() {
        let f = FocusedApp::new();
        f.publish_at(app(1), 0);
        f.demote_at(5_000); // focus moved to something unvalidated
        assert_eq!(f.get(), None, "no current application");
        assert!(f.protects_at(app(1), 5_001), "earned grace (held 5 s)");
        f.forget(app(1));
        assert!(
            !f.protects_at(app(1), 5_002),
            "gone means gone, grace or not"
        );

        let g = FocusedApp::new();
        g.publish_at(app(7), 0);
        g.demote_at(100); // held 100 ms: no grace
        assert!(!g.protects_at(app(7), 101));
        g.publish_at(app(8), 200);
        g.forget(app(8));
        assert_eq!(g.get(), None);
    }

    #[test]
    fn suspending_removes_protection_at_once_and_resuming_restores_it() {
        let f = FocusedApp::new();
        f.publish_at(app(1), 0);
        let g0 = f.generation();
        f.suspend();
        assert_ne!(
            f.generation(),
            g0,
            "a decision made before is detectably stale"
        );
        assert!(!f.protects_at(app(1), 1), "no protection while unverified");
        f.resume();
        assert!(f.protects_at(app(1), 2), "restored once verified");
        // A newer validated focus wins over a stale suspension.
        f.suspend();
        f.publish_at(app(2), 10);
        f.resume();
        assert_eq!(f.get(), Some(app(2)));
        // A suspended application that is gone is not restored.
        f.suspend();
        f.forget(app(2));
        f.resume();
        assert_eq!(f.get(), None);
    }

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
