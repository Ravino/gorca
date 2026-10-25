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
    /// The wall time the overrun rule and the host watchdog are measured against. Equal to
    /// `time` for a call that has its grant to itself; the per-call cap for an interactive call
    /// whose fuel is only its share of a crowded handoff (the serial-equivalent bound, ADR-0026
    /// section 2.4.6): the plugins run in parallel, so a small share of fuel is no reason to
    /// expect a call to finish in proportionally less wall time.
    pub wall: Duration,
}

impl FuelGrant {
    /// The grant for `time` at `kappa` units per microsecond.
    #[must_use]
    pub fn for_time(time: Duration, kappa: u64) -> Self {
        let us = u64::try_from(time.as_micros()).unwrap_or(u64::MAX);
        Self {
            units: us.saturating_mul(kappa).max(1),
            time,
            wall: time,
        }
    }

    /// Measure the wall rules against `wall` instead of the (smaller) fuel share.
    #[must_use]
    pub fn with_wall(mut self, wall: Duration) -> Self {
        self.wall = wall.max(self.time);
        self
    }

    /// A wall overrun (measured in CPU time of the plugin thread by the caller) is a call that took more than `max(2 x wall, wall + 50 us)` (`wall` is the grant time unless the grant is a crowded share)
    /// (§2.5.2).
    #[must_use]
    pub fn is_wall_overrun(&self, wall: Duration) -> bool {
        let limit = (self.wall * 2).max(self.wall + Duration::from_micros(50));
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

/// Table elements allowed over all tables of an instance.
pub const MAX_TABLE_ELEMENTS: usize = 10_000;

/// Host-call costs in fuel units, charged **before** the import does its work (ADR-0026 B-8).
/// Calibrated against `KAPPA_PROVISIONAL` = 21,739 units/us and release-build measurements of the
/// M18a-1 probe (loops over `text`/`children` must stay within grant + 50 us); provisional like
/// kappa itself.
pub mod cost {
    /// Every import: the host-call transition and argument handling (~90 ns).
    pub const BASE: u64 = 2_000;
    /// Per byte requested from or returned by `text`.
    pub const PER_TEXT_BYTE: u64 = 1;
    /// Charged up front for `name`/`attr` (size unknown before the call); the returned bytes are
    /// charged on top.
    pub const NAME_PRE: u64 = 1_024;
    /// Per element of `children` (view read, handle mint, lowering).
    pub const PER_CHILD: u64 = 800;
    /// Handle verification (one SipHash).
    pub const HANDLE_CHECK: u64 = 700;
}

/// The watchdog checks the wall clock on every Nth import and traps a call that exceeds
/// `WATCHDOG_FACTOR x` its grant time (ADR-0026 §2.5.2).
pub const WATCHDOG_EVERY: u32 = 64;
/// See [`WATCHDOG_EVERY`].
pub const WATCHDOG_FACTOR: u32 = 4;
/// Lifetime handle-forgery budget of one instance; exhausted = malformed fault and a rebuild with
/// a fresh key (review F-3).
pub const FORGERY_BUDGET: u32 = 256;

/// Largest read set a token records before it falls back to the scope-wide stamp (ADR-0026 2.6.1).
pub const MAX_READ_SET: usize = 256;
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
    /// Sum of all linear memories, bytes (wasmtime calls the limiter per memory).
    total: usize,
    total_at_call_start: usize,
    /// Sum of all table elements.
    table_total: usize,
    /// Set when a growth was denied during the last call.
    pub denied: bool,
}

impl PluginLimiter {
    /// A limiter for `limits`.
    #[must_use]
    pub fn new(limits: MemoryLimits) -> Self {
        Self {
            limits,
            total: 0,
            total_at_call_start: 0,
            table_total: 0,
            denied: false,
        }
    }

    /// Called before each guest call: growth is counted per call.
    pub fn begin_call(&mut self) {
        self.total_at_call_start = self.total;
        self.denied = false;
    }
}

impl ResourceLimiter for PluginLimiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        // `current`/`desired` are those of one memory: account the delta against the total of
        // all memories, so several memories cannot each reach the cap.
        let delta = desired.saturating_sub(current);
        let total = self.total.saturating_add(delta);
        if total > self.limits.max_bytes
            || total.saturating_sub(self.total_at_call_start) > self.limits.max_growth_per_call
        {
            self.denied = true;
            return Err(wasmtime::Error::msg("plugin memory limit"));
        }
        self.total = total;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let total = self
            .table_total
            .saturating_add(desired.saturating_sub(current));
        if total > MAX_TABLE_ELEMENTS {
            return Ok(false);
        }
        self.table_total = total;
        Ok(true)
    }

    // A component is several core instances (the guest module and the shims that glue it to the
    // canonical ABI); these bound a hostile component, not a normal one.
    fn instances(&self) -> usize {
        16
    }

    fn tables(&self) -> usize {
        8
    }

    // The canonical-ABI shims share the guest's single memory (verified by the test guest).
    fn memories(&self) -> usize {
        1
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
    fn a_crowded_share_is_judged_against_the_cap_not_the_share() {
        // 23 us of fuel (a tenth of a crowded handoff) must not make a 90 us call an overrun
        // (the plugins run in parallel): the wall rules follow the 200 us cap.
        let share = FuelGrant::for_time(Duration::from_micros(23), KAPPA_PROVISIONAL)
            .with_wall(B2_INTERACTIVE);
        assert_eq!(share.units, 23 * KAPPA_PROVISIONAL);
        assert!(!share.is_wall_overrun(Duration::from_micros(400)));
        assert!(share.is_wall_overrun(Duration::from_micros(401)));
        // The wall basis never drops below the grant itself.
        let big = FuelGrant::for_time(B1_BACKGROUND, KAPPA_PROVISIONAL).with_wall(B2_INTERACTIVE);
        assert_eq!(big.wall, B1_BACKGROUND);
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

    #[test]
    fn several_memories_share_one_total_and_one_growth_cap() {
        // wasmtime calls the limiter once per memory with that memory's (current, desired).
        let mut l = PluginLimiter::new(MemoryLimits::for_mib(16));
        l.begin_call();
        // Four memories growing 0 -> 4 MiB in one call: the second already breaks the 4 MiB cap.
        assert!(l.memory_growing(0, 4 << 20, None).expect("first"));
        assert!(l.memory_growing(0, 4 << 20, None).is_err());
        assert!(l.denied);
        // Across calls the total, not each memory, is capped at 16 MiB.
        let mut l = PluginLimiter::new(MemoryLimits::for_mib(16));
        let mut total = 0usize;
        'outer: for _ in 0..16 {
            l.begin_call();
            for _ in 0..4 {
                let cur = total / 4;
                if l.memory_growing(cur, cur + (1 << 20), None).is_err() {
                    break 'outer;
                }
                total += 1 << 20;
            }
        }
        assert!(total <= 16 << 20, "reached {} MiB", total >> 20);
    }
}
