//! Harness self-tests (SPEC H-10(b)/(c)): the conformance suite must pass for a reader that
//! implements the contract and must fail, for the right case, for a reader with a known defect.

#![cfg(test)]

use super::ic::{self, CaseReport, Outcome};
use super::sim::{Fault, SimSut};
use super::sut::Sut;

fn run(fault: Fault) -> Vec<CaseReport> {
    ic::run_suite(&|| -> Box<dyn Sut> { Box::new(SimSut::with_fault(fault)) })
}

fn failed(r: &[CaseReport]) -> Vec<&'static str> {
    r.iter().filter(|c| c.result.is_err()).map(|c| c.id).collect()
}

#[test]
fn a_reader_that_implements_the_contract_passes_every_case() {
    let r = run(Fault::None);
    let bad: Vec<String> = r
        .iter()
        .filter_map(|c| c.result.as_ref().err().map(|e| format!("{}: {}", c.id, e.0)))
        .collect();
    assert!(bad.is_empty(), "{bad:#?}");
    let skipped: Vec<_> = r
        .iter()
        .filter(|c| matches!(c.result, Ok(Outcome::Skipped(_))))
        .map(|c| c.id)
        .collect();
    // Only the cases that need an eager reader or the malformed-ref mode may skip.
    assert_eq!(skipped, vec!["IC-12", "IC-16", "IC-22"], "{skipped:?}");
}

fn must_fail(fault: Fault, ids: &[&str]) {
    let f = failed(&run(fault));
    for id in ids {
        assert!(f.contains(id), "{fault:?}: {id} should fail, failed: {f:?}");
    }
}

#[test]
fn stale_probe_never_invalidating_fails_the_rename_and_state_cases() {
    must_fail(Fault::NeverInvalidate, &["IC-1", "IC-3"]);
}

#[test]
fn no_revalidation_fails_the_silent_rename_cases() {
    must_fail(Fault::NoRv1, &["IC-2"]);
}

#[test]
fn a_reader_that_never_caches_fails_every_vacuity_guard() {
    must_fail(Fault::NeverCache, &["IC-1", "IC-2", "IC-5", "IC-9", "IC-17"]);
}

#[test]
fn reviving_ghosts_fails_the_late_reply_case() {
    must_fail(Fault::ReviveGhosts, &["IC-10"]);
}

#[test]
fn ignoring_redirty_fails_the_race_case() {
    must_fail(Fault::IgnoreRedirty, &["IC-11"]);
}

#[test]
fn treating_gone_as_final_fails_the_reorder_case() {
    must_fail(Fault::GoneIsFinal, &["IC-8", "IC-10"]);
}

#[test]
fn keeping_the_mirror_of_a_dead_app_fails_the_restart_case() {
    must_fail(Fault::NoRetire, &["IC-6"]);
}

#[test]
fn ignoring_health_fails_the_hung_app_case() {
    must_fail(Fault::IgnoreHealth, &["IC-18"]);
}

#[test]
fn ignoring_floods_fails_the_flood_case() {
    must_fail(Fault::IgnoreFlood, &["IC-17"]);
}
