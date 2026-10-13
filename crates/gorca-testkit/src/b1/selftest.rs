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
    r.iter()
        .filter(|c| c.result.is_err())
        .map(|c| c.id)
        .collect()
}

#[test]
fn a_reader_that_implements_the_contract_passes_every_case() {
    let r = run(Fault::None);
    let bad: Vec<String> = r
        .iter()
        .filter_map(|c| {
            c.result
                .as_ref()
                .err()
                .map(|e| format!("{}: {}", c.id, e.0))
        })
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
    must_fail(
        Fault::NeverCache,
        &["IC-1", "IC-2", "IC-5", "IC-9", "IC-17"],
    );
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

// ---- the walk, its run directory and the runner's gates (H-10(b), contract) ----------------

mod walk_gates {
    use std::path::PathBuf;

    use gorca_bench_b1::rundir::{RunMetrics, evaluate};
    use gorca_bench_b1::verdict::KeyMode;

    use super::{Fault, SimSut};
    use crate::b1::walk::{self, WalkSpec};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("b1-selftest-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn short() -> WalkSpec {
        WalkSpec {
            loops: 3,
            initial_tabs: 32,
            list_arrows: 70,
            panels: 3,
            l3_duration_ms: 3_000,
            ..WalkSpec::default()
        }
    }

    fn walk_metrics(fault: Fault, name: &str) -> RunMetrics {
        let mut sut = SimSut::with_fault(fault);
        let run = walk::execute(&mut sut, &short(), 5);
        let dir = scratch(name);
        walk::write_run_dir(&dir, &mut sut, &short(), &run).unwrap();
        let m = evaluate("walk", &dir, KeyMode::AppAndPath).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        m
    }

    #[test]
    fn a_correct_reader_passes_the_walk_gates_through_the_runner_code() {
        let m = walk_metrics(Fault::None, "ok");
        assert!(
            m.invalid.is_empty(),
            "the run directory contract is broken: {:?}",
            m.invalid
        );
        assert_eq!(m.metrics["B1-STALE@walk"], 0.0, "{:?}", m.info);
        assert_eq!(m.metrics["B1-LOST@walk"], 0.0);
        assert!(m.metrics["B1-HIT.revisit"] >= 0.95, "{:?}", m.metrics);
        assert_eq!(m.metrics["B1-L3.lost"], 0.0);
        assert!(m.info["focus_presentations"] > 1000.0);
    }

    #[test]
    fn the_stale_probe_fails_b1_stale() {
        let m = walk_metrics(Fault::NeverInvalidate, "stale");
        assert!(m.metrics["B1-STALE@walk"] > 0.0, "{:?}", m.metrics);
    }

    #[test]
    fn a_reader_that_never_caches_fails_b1_hit_revisit() {
        let m = walk_metrics(Fault::NeverCache, "nocache");
        assert!(m.metrics["B1-HIT.revisit"] < 0.95, "{:?}", m.metrics);
    }

    fn idle_metric(fault: Fault, name: &str) -> f64 {
        let mut sut = SimSut::with_fault(fault);
        let (w, delta) = walk::run_idle(&mut sut, 120).unwrap();
        let dir = scratch(name);
        walk::write_idle_dir(&dir, &mut sut, w, &delta).unwrap();
        let m = evaluate("idle", &dir, KeyMode::AppAndPath).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(m.invalid.is_empty(), "{:?}", m.invalid);
        m.metrics["B1-MSG.idle"]
    }

    #[test]
    fn a_polling_reader_fails_b1_msg_idle_and_a_quiet_one_passes() {
        assert!(idle_metric(Fault::None, "idle-ok") <= 0.5);
        assert!(idle_metric(Fault::DumpEvery100ms, "idle-dump") > 0.5);
    }

    #[test]
    fn the_ff_run_directory_is_accepted() {
        let mut sut = SimSut::with_fault(Fault::None);
        let ff = walk::run_ff(&mut sut, 3).unwrap();
        for m in ["shell", "settings", "files", "files-big", "gtk3"] {
            assert_eq!(ff[m].len(), 3, "{m}");
        }
        let dir = scratch("ff");
        let startup = std::collections::BTreeMap::new();
        walk::write_ff_dir(&dir, &mut sut, &ff, &startup).unwrap();
        let m = evaluate("ff", &dir, KeyMode::AppAndPath).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(m.invalid.is_empty(), "{:?}", m.invalid);
        assert!(m.metrics["B1-FF.p99@shell"] < 15.0);
    }
}
