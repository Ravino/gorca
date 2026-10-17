// SPDX-License-Identifier: GPL-3.0-or-later
//! `KeyInjector` over RemoteDesktop/EIS (libei) and the injection expectation table.
//!
//! Responsibility (ADR-0001 section 11): "`KeyInjector` over RemoteDesktop/EIS (libei);
//! expectation table".
//!
//! > "K-2: Re-injection is used only for keys that were swallowed but turn out to be meant for
//! > the app ... Injected keys go into an expectation table `(keycode, pressed) -> deadline
//! > 50 ms`. Virtual-device repeats are dropped by Mutter (`update_button_count`), so held-key
//! > repeat is not re-injected."
//! > "K-1: Prefer changing the grab set over re-injection."
//!
//! Phase 0 contains the expectation table (self-feedback suppression) and a stub injector that
//! reports `NotAvailable`. TBD: the RemoteDesktop portal session (needs user consent) and the
//! EIS/libei client, probably through C interop (`c-cpp-engineer`).
//!
//! Allowed dependencies: `gorca-input-api`, `gorca-rt`. `zbus` is permitted in this crate by
//! rule 2 of section 11 but is not used yet.

use std::time::{Duration, Instant};

use gorca_input_api::{InjectError, InjectKey, KeyInjector};
use gorca_rt::{BoxFuture, Deadline};

/// How long an injected key may take to come back as a watched `KeyEvent` (K-2).
pub const EXPECTATION_TTL: Duration = Duration::from_millis(50);
/// Bound of the table (PC-7). On overflow the oldest expectation is dropped.
pub const EXPECTATION_CAPACITY: usize = 64;

#[derive(Copy, Clone, Debug)]
struct Expectation {
    key: InjectKey,
    until: Instant,
}

/// Self-feedback suppression: injected keys come back through the keyboard monitor and must not
/// be treated as user input.
#[derive(Debug, Default)]
pub struct ExpectationTable {
    entries: Vec<Expectation>,
}

impl ExpectationTable {
    pub fn new() -> Self {
        Self {
            entries: Vec::with_capacity(EXPECTATION_CAPACITY),
        }
    }

    /// Record that `key` was injected at `now`.
    pub fn expect(&mut self, key: InjectKey, now: Instant) {
        self.entries.retain(|e| e.until > now);
        if self.entries.len() >= EXPECTATION_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(Expectation {
            key,
            until: now + EXPECTATION_TTL,
        });
    }

    /// True (and the expectation is consumed) if an observed event matches an injection.
    pub fn consume(&mut self, key: InjectKey, now: Instant) -> bool {
        self.entries.retain(|e| e.until > now);
        match self.entries.iter().position(|e| e.key == key) {
            Some(i) => {
                self.entries.remove(i);
                true
            }
            None => false,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Reports `NotAvailable` for every call, until the EIS session exists.
#[derive(Debug, Default)]
pub struct UnavailableInjector;

impl KeyInjector for UnavailableInjector {
    fn inject(
        &self,
        _key: InjectKey,
        _deadline: Deadline,
    ) -> BoxFuture<'_, Result<(), InjectError>> {
        Box::pin(async { Err(InjectError::NotAvailable) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_rt::Clock;

    fn k(code: u16, pressed: bool) -> InjectKey {
        InjectKey {
            keycode: code,
            pressed,
        }
    }

    #[test]
    fn matching_event_is_consumed_once() {
        let t = Instant::now();
        let mut table = ExpectationTable::new();
        table.expect(k(38, true), t);
        assert!(
            !table.consume(k(38, false), t),
            "release is a different expectation"
        );
        assert!(table.consume(k(38, true), t + Duration::from_millis(10)));
        assert!(!table.consume(k(38, true), t + Duration::from_millis(11)));
        assert!(table.is_empty());
    }

    #[test]
    fn expectations_expire_after_50ms() {
        let t = Instant::now();
        let mut table = ExpectationTable::new();
        table.expect(k(38, true), t);
        assert!(!table.consume(k(38, true), t + Duration::from_millis(51)));
        assert!(table.is_empty());
    }

    #[test]
    fn table_is_bounded() {
        let t = Instant::now();
        let mut table = ExpectationTable::new();
        let cap = u16::try_from(EXPECTATION_CAPACITY).unwrap();
        for i in 0..(cap + 10) {
            table.expect(k(i, true), t);
        }
        assert_eq!(table.len(), EXPECTATION_CAPACITY);
        assert!(!table.consume(k(0, true), t), "oldest was dropped");
        assert!(table.consume(k(cap + 9, true), t));
    }

    #[tokio::test]
    async fn stub_injector_is_not_available() {
        let d = Deadline::after(&Clock::System, Duration::from_millis(100));
        assert_eq!(
            UnavailableInjector.inject(k(1, true), d).await,
            Err(InjectError::NotAvailable)
        );
    }
}
