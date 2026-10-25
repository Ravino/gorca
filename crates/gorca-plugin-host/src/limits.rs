// SPDX-License-Identifier: GPL-3.0-or-later
//! Budgets, fuel grants, size caps and the memory limiter (ADR-0026 §2.5).

use std::time::Duration;

use wasmtime::ResourceLimiter;

/// Fuel units per microsecond measured by the M18a-0 spike (one ALU loop). **Provisional**: the
/// normative rates (kappa_int, kappa_bg, per-plugin kappa_p) are frozen from WQ-1..WQ-3 and per
/// CPU fingerprint (ADR-0027 CD-W5); until then this is the conservative default.
pub const KAPPA_PROVISIONAL: u64 = 21_739;

/// B-1: background `derive`, `attach`, `removed`.
pub const B1_BACKGROUND: Duration = Duration::from_millis(1);
/// B-2: interactive `derive` cap per call.
pub const B2_INTERACTIVE: Duration = Duration::from_micros(200);
/// B-3: `present.edit`.
pub const B3_PRESENT_EDIT: Duration = Duration::from_micros(100);
/// B-4: `commands.run`.
pub const B4_COMMAND: Duration = Duration::from_millis(20);
/// B-5: `reactor.*`, `route-changed`, `settings-changed`.
pub const B5_REACTOR: Duration = Duration::from_millis(5);

/// A fuel grant for one guest call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuelGrant {
    /// Units given to the store.
    pub units: u64,
    /// The wall time the grant stands for (`units / kappa`).
    pub time: Duration,
}

impl FuelGrant {
    /// The grant for `time` at `kappa` units per microsecond.
    #[must_use]
    pub fn for_time(time: Duration, kappa: u64) -> Self {
        let us = u64::try_from(time.as_micros()).unwrap_or(u64::MAX);
        Self {
            units: us.saturating_mul(kappa).max(1),
            time,
        }
    }

    /// A wall overrun is a call that took more than `max(2 x grant_time, grant_time + 50 us)`
    /// (§2.5.2).
    #[must_use]
    pub fn is_wall_overrun(&self, wall: Duration) -> bool {
        let limit = (self.time * 2).max(self.time + Duration::from_micros(50));
        wall > limit
    }
}

/// Size caps of §2.5.3 and B-6 (bytes unless stated).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    /// `diag.log` message.
    LogMessage,
    /// A text value in facts, an output literal, settings words.
    Text,
    /// `attr` key, `settings` key, `cue` name.
    Key,
    /// `requests` text length.
    RequestText,
}

impl Cap {
    /// The cap in bytes.
    #[must_use]
    pub const fn bytes(self) -> usize {
        match self {
            Self::LogMessage => 256,
            Self::Text => 1024,
            Self::Key => 64,
            Self::RequestText => 64 << 10,
        }
    }
}

/// Facts per call (B-6).
pub const MAX_FACTS_PER_CALL: usize = 256;
/// Other effects per call (B-6).
pub const MAX_OTHER_EFFECTS_PER_CALL: usize = 64;
/// Effect text per call (B-6).
pub const MAX_EFFECT_TEXT_PER_CALL: usize = 32 << 10;
/// Forged handles tolerated in one call before the call is a malformed-effect fault (§2.3.3).
pub const MAX_FORGED_PER_CALL: u32 = 16;
/// The largest single value any host import may lift: the biggest cap, `RequestText`.
/// Wasmtime's per-call hostcall fuel is set to this so an oversize string or list is refused
/// before it is copied (`set_hostcall_fuel`).
pub const HOSTCALL_LIFT_CAP: usize = Cap::RequestText.bytes();

/// Check a guest-supplied length against `cap` without touching the data.
pub fn check_len(cap: Cap, len: usize) -> Result<(), usize> {
    if len > cap.bytes() { Err(len) } else { Ok(()) }
}

/// Memory limits of one instance (B-7).
#[derive(Debug, Clone, Copy)]
pub struct MemoryLimits {
    /// Linear memory ceiling in bytes.
    pub max_bytes: usize,
    /// Growth allowed within one call, in bytes (growth costs host time that fuel does not
    /// count, ADR host-limits notes).
    pub max_growth_per_call: usize,
}

impl MemoryLimits {
    /// Limits for a manifest request in MiB.
    #[must_use]
    pub fn for_mib(mib: u32) -> Self {
        Self {
            max_bytes: (mib as usize) << 20,
            max_growth_per_call: 4 << 20,
        }
    }
}

/// The store's `ResourceLimiter`. A denied growth returns an error, which traps the guest.
#[derive(Debug)]
pub struct PluginLimiter {
    limits: MemoryLimits,
    start_of_call: usize,
    current: usize,
    /// Set when a growth was denied during the last call.
    pub denied: bool,
}

impl PluginLimiter {
    /// A limiter for `limits`.
    #[must_use]
    pub fn new(limits: MemoryLimits) -> Self {
        Self {
            limits,
            start_of_call: 0,
            current: 0,
            denied: false,
        }
    }

    /// Called before each guest call: growth is counted per call.
    pub fn begin_call(&mut self) {
        self.start_of_call = self.current;
        self.denied = false;
    }
}

impl ResourceLimiter for PluginLimiter {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > self.limits.max_bytes
            || desired.saturating_sub(self.start_of_call) > self.limits.max_growth_per_call
        {
            self.denied = true;
            return Err(wasmtime::Error::msg("plugin memory limit"));
        }
        self.current = desired;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= 10_000)
    }

    // A component is several core instances (the guest module and the shims that glue it to the
    // canonical ABI); these bound a hostile component, not a normal one.
    fn instances(&self) -> usize {
        16
    }

    fn tables(&self) -> usize {
        8
    }

    fn memories(&self) -> usize {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_matches_the_adr_examples() {
        // 0.2 ms ~ 4.35 M units, 1 ms ~ 21.7 M units.
        assert_eq!(
            FuelGrant::for_time(Duration::from_micros(200), KAPPA_PROVISIONAL).units,
            4_347_800
        );
        assert_eq!(
            FuelGrant::for_time(Duration::from_millis(1), KAPPA_PROVISIONAL).units,
            21_739_000
        );
    }

    #[test]
    fn wall_overrun_rule() {
        let g = FuelGrant::for_time(Duration::from_micros(200), KAPPA_PROVISIONAL);
        assert!(!g.is_wall_overrun(Duration::from_micros(400)));
        assert!(g.is_wall_overrun(Duration::from_micros(401)));
        let tiny = FuelGrant::for_time(Duration::from_micros(10), KAPPA_PROVISIONAL);
        assert!(!tiny.is_wall_overrun(Duration::from_micros(60)));
        assert!(tiny.is_wall_overrun(Duration::from_micros(61)));
    }

    #[test]
    fn len_checks_do_not_need_the_data() {
        assert!(check_len(Cap::LogMessage, 256).is_ok());
        assert_eq!(check_len(Cap::LogMessage, 257), Err(257));
    }

    #[test]
    fn limiter_denies_over_ceiling_and_over_per_call_growth() {
        let mut l = PluginLimiter::new(MemoryLimits::for_mib(16));
        l.begin_call();
        assert!(l.memory_growing(0, 1 << 20, None).expect("ok"));
        assert!(l.memory_growing(1 << 20, 17 << 20, None).is_err());
        assert!(l.denied);
        let mut l = PluginLimiter::new(MemoryLimits::for_mib(16));
        l.begin_call();
        assert!(
            l.memory_growing(0, 5 << 20, None).is_err(),
            "5 MiB in one call"
        );
        l.begin_call();
        assert!(l.memory_growing(0, 4 << 20, None).expect("ok"));
        l.begin_call();
        assert!(l.memory_growing(4 << 20, 8 << 20, None).expect("ok"));
    }
}
