// SPDX-License-Identifier: GPL-3.0-or-later
//! Gate thresholds of SPEC section 9. The SPEC table is generated from [`GATES`]; do not
//! restate a number anywhere else in the harness.

/// What a gate compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    /// Median over repeats must be `<=` the threshold.
    MedianAtMost,
    /// Median over repeats must be `>=` the threshold.
    MedianAtLeast,
    /// The worst repeat must be `<=` the threshold (counts that must be exactly zero).
    EveryRunAtMost,
    /// Reported only.
    Rank,
}

/// One row of the section 9 table.
#[derive(Clone, Copy, Debug)]
pub struct Gate {
    pub id: &'static str,
    /// Run kind that produces it (`walk`, `ff`, `idle`, `big`, `soak`, `step`, `conf`).
    pub run: &'static str,
    pub unit: &'static str,
    pub cmp: Cmp,
    pub threshold: f64,
    pub what: &'static str,
}

const fn g(
    id: &'static str,
    run: &'static str,
    unit: &'static str,
    cmp: Cmp,
    threshold: f64,
    what: &'static str,
) -> Gate {
    Gate {
        id,
        run,
        unit,
        cmp,
        threshold,
        what,
    }
}

pub const MIB: f64 = 1024.0 * 1024.0;

/// Cold-path factor against R0 (B1-L1c.p99).
pub const L1C_VS_R0: f64 = 1.2;
/// A refine/followup must come within this long to count as `LATE` (SPEC 6.2).
pub const LATE_WINDOW_NS: u64 = 500_000_000;
/// A `Refined` correction that replaces the wrong utterance sooner than this after it was
/// handed to speech counts as inaudible: the speech server cannot have started the audio yet
/// (the Reader's own window for a replacing refine is 150 ms of speech start, ADR-0001 8.1; the
/// refine delays measured on R0 are 2-5 ms). `Late` follow-ups are always audible.
pub const AUDIBLE_AFTER_NS: u64 = 10_000_000;
/// Cross-check tolerance between counted and monitored signals (SPEC 6.1).
pub const MSG_CROSSCHECK_TOL: f64 = 0.01;
/// Truth and presentation clocks are cross-checked to this (SPEC 6.1); also the lower-bound slack
/// of the verdict window, so a value that was valid 1 ms before the stimulus is not called stale.
pub const CLOCK_SKEW_NS: u64 = 1_000_000;

/// All gates. Bold rows of the SPEC are `Median*`/`EveryRun*`; ranking rows are `Rank`.
pub const GATES: &[Gate] = &[
    g(
        "B1-STALE@walk",
        "walk",
        "count",
        Cmp::EveryRunAtMost,
        0.0,
        "uncorrected wrong presentations",
    ),
    g(
        "B1-STALE@soak",
        "soak",
        "count",
        Cmp::EveryRunAtMost,
        0.0,
        "uncorrected wrong presentations",
    ),
    g(
        "B1-LOST@walk",
        "walk",
        "count",
        Cmp::EveryRunAtMost,
        0.0,
        "focus stimuli with no SPEAK",
    ),
    g(
        "B1-L1h.p99",
        "walk",
        "ms",
        Cmp::MedianAtMost,
        2.0,
        "focus to SPEAK served from the mirror",
    ),
    g(
        "B1-HIT.all",
        "walk",
        "ratio",
        Cmp::Rank,
        f64::NAN,
        "hit / focus presentations (higher is better)",
    ),
    g(
        "B1-HIT.revisit",
        "walk",
        "ratio",
        Cmp::MedianAtLeast,
        0.95,
        "hit rate on revisits of unmutated nodes",
    ),
    g(
        "B1-HIT.ancestry",
        "walk",
        "ratio",
        Cmp::Rank,
        f64::NAN,
        "where-am-I demands answered without IPC",
    ),
    g(
        "B1-L1c.p99",
        "walk",
        "ms",
        Cmp::MedianAtMost,
        15.0,
        "focus to SPEAK on a miss (also <= 1.2 x R0 median)",
    ),
    g(
        "B1-FF.p99",
        "ff",
        "ms",
        Cmp::MedianAtMost,
        15.0,
        "first focus of a newly appeared app (per model)",
    ),
    g(
        "B1-S1.p99",
        "walk",
        "ms",
        Cmp::MedianAtMost,
        5.0,
        "state change on focused node to SPEAK",
    ),
    g(
        "B1-M6",
        "walk",
        "ratio",
        Cmp::Rank,
        f64::NAN,
        "all corrections (audible or not) / focus presentations; split evented / silent in info",
    ),
    g(
        "B1-M6.audible",
        "walk",
        "ratio",
        Cmp::MedianAtMost,
        0.01 - f64::EPSILON,
        "audible corrections (Late, or a refine later than 10 ms) / focus presentations (< 1 %)",
    ),
    g(
        "B1-MSG.idle",
        "idle",
        "calls/s/app",
        Cmp::MedianAtMost,
        0.5,
        "gorca-originated calls per second per app, 60 s after settle",
    ),
    g(
        "B1-MSG.walk",
        "walk",
        "msgs/s",
        Cmp::Rank,
        f64::NAN,
        "msgs/s and calls per focus presentation (lower is better)",
    ),
    g(
        "B1-MSG.l3",
        "walk",
        "calls/s",
        Cmp::MedianAtMost,
        500.0,
        "calls/s to the storm app during L3-50k",
    ),
    g(
        "B1-L3.lost",
        "walk",
        "count",
        Cmp::EveryRunAtMost,
        0.0,
        "focus presentations lost under L3-50k",
    ),
    g(
        "B1-MSG.startup",
        "ff",
        "calls",
        Cmp::Rank,
        f64::NAN,
        "calls and bytes in the first 2 s after an app appears",
    ),
    g(
        "B1-RSS.soak",
        "soak",
        "MiB",
        Cmp::MedianAtMost,
        8.0,
        "daemon RSS growth from minute 10 to minute 60",
    ),
    g(
        "B1-RSS.big",
        "big",
        "MiB",
        Cmp::MedianAtMost,
        128.0,
        "RSS peak minus RSS before the apps appeared",
    ),
    g(
        "B1-L4.p99",
        "walk",
        "ms",
        Cmp::MedianAtMost,
        15.0,
        "hung model: L1 p99 of the other apps",
    ),
    g(
        "B1-L4.announce",
        "walk",
        "ms",
        Cmp::MedianAtMost,
        500.0,
        "hung model: 'not responding' spoken within",
    ),
    g(
        "B1-STEP.max_us",
        "step",
        "us",
        Cmp::MedianAtMost,
        250.0,
        "worst policy step() at the caps (MA-6)",
    ),
    g(
        "B1-CPU.focus_us",
        "walk",
        "us",
        Cmp::Rank,
        f64::NAN,
        "daemon CPU per focus presentation",
    ),
];

pub fn gate(id: &str) -> Option<&'static Gate> {
    GATES.iter().find(|g| g.id == id)
}

/// Markdown table for the SPEC (kept in sync by `b1 limits-md`).
pub fn markdown() -> String {
    use std::fmt::Write as _;
    let mut s = String::from("| Id | Run | Pass | Unit | What |\n|---|---|---|---|---|\n");
    for g in GATES {
        let pass = match g.cmp {
            Cmp::MedianAtMost => format!("median <= {}", g.threshold),
            Cmp::MedianAtLeast => format!("median >= {}", g.threshold),
            Cmp::EveryRunAtMost => format!("every run <= {}", g.threshold),
            Cmp::Rank => "rank".to_owned(),
        };
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} |",
            g.id, g.run, pass, g.unit, g.what
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_gates_have_thresholds() {
        let mut seen = std::collections::BTreeSet::new();
        for g in GATES {
            assert!(seen.insert(g.id), "duplicate {}", g.id);
            assert_eq!(g.cmp == Cmp::Rank, g.threshold.is_nan(), "{}", g.id);
        }
    }

    #[test]
    fn spec_numbers() {
        assert_eq!(gate("B1-L1h.p99").map(|g| g.threshold), Some(2.0));
        assert_eq!(gate("B1-S1.p99").map(|g| g.threshold), Some(5.0));
        assert_eq!(gate("B1-STEP.max_us").map(|g| g.threshold), Some(250.0));
        assert_eq!(gate("B1-RSS.big").map(|g| g.threshold), Some(128.0));
        assert!(gate("B1-M6.audible").is_some_and(|g| g.threshold < 0.01));
        assert_eq!(gate("B1-M6").map(|g| g.cmp), Some(Cmp::Rank));
    }
}
