// SPDX-License-Identifier: GPL-3.0-or-later
//! Baseline marks: steps that fail on `main` at the time M8 was prepared, with the root cause.
//!
//! The scenario table (`m8_steps.rs`) states what NVDA does. This table records, per step id,
//! why `main` does not do it yet, so that the gate is green at the baseline and goes red on a
//! regression. To promote a feature, delete the lines of the steps it fixes in the same commit
//! (an expected-fail step that starts to pass is reported as XPASS and must not stay marked).
//! Rows starting with `rig:` are not gorca faults but limits of the test rig; fix the rig and
//! delete them. Source: `docs/reports/m8-baseline.md` (run `m8-baseline-raw2`, 2026-10-15).

const RIG_KEYS: &str = "rig: a key typed into Firefox through the compositor lands on an unnamed browser 'panel', not in the page (probe m8-probe-tab, m8-probe-tab2); the page needs keyboard focus, which a click and AT-SPI GrabFocus did not give";

/// Step ids per root cause.
pub(super) const MARKS: &[(&[&str], &str)] = &[(&["FM.tab.real", "SP.back"], RIG_KEYS)];

/// Why this step is expected to fail today; `None` = it must pass.
pub(super) fn reason(id: &str) -> Option<&'static str> {
    MARKS
        .iter()
        .find(|(ids, _)| ids.contains(&id))
        .map(|(_, why)| *why)
}

#[cfg(test)]
mod tests {
    #[test]
    fn reasons_are_non_empty() {
        for (ids, why) in super::MARKS {
            assert!(!ids.is_empty() && !why.is_empty());
        }
        assert!(super::reason("SP.back").is_some());
        assert!(super::reason("QN.h.1").is_none());
    }
}
