// SPDX-License-Identifier: MIT OR Apache-2.0
//! `summary.json`: the machine-readable result of one run, and the gate evaluator that turns a set
//! of summaries into verdicts (thresholds come from one TOML file that mirrors SPEC section 4).
//! Verdicts are produced by code from the numbers, never typed by hand.

use crate::timing::Pcts;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Summary {
    pub scenario: String,
    pub kind: String,
    pub arm: String,
    pub condition: String,
    /// The run started or ended with loadavg or PSI above the quiet limits (kept, labelled).
    #[serde(default)]
    pub quiet_violated: bool,
    /// Set when the run crashed, hung or was killed by the watchdog (counted as failed).
    #[serde(default)]
    pub failed: Option<String>,
    /// Flat numeric metrics (ns unless the name says otherwise).
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub checks: BTreeMap<String, bool>,
    #[serde(default)]
    pub series: BTreeMap<String, Pcts>,
    /// M3 per plugin (fraction).
    #[serde(default)]
    pub per_plugin: BTreeMap<String, f64>,
    #[serde(default)]
    pub counters: serde_json::Value,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl Summary {
    pub fn new(scenario: &str, kind: &str, arm: &str, condition: &str) -> Summary {
        Summary {
            scenario: scenario.into(),
            kind: kind.into(),
            arm: arm.into(),
            condition: condition.into(),
            ..Summary::default()
        }
    }

    pub fn metric(&mut self, k: &str, v: f64) {
        self.metrics.insert(k.to_string(), v);
    }

    pub fn check(&mut self, k: &str, v: bool) {
        self.checks.insert(k.to_string(), v);
    }

    /// Insert a series' percentiles under `name` as flat metrics `<name>_p50_ns` ... `<name>_max_ns`.
    pub fn series_metrics(&mut self, name: &str, p: &Pcts) {
        self.metric(&format!("{name}_n"), p.n as f64);
        self.metric(&format!("{name}_min_ns"), p.min as f64);
        self.metric(&format!("{name}_p50_ns"), p.p50 as f64);
        self.metric(&format!("{name}_p90_ns"), p.p90 as f64);
        self.metric(&format!("{name}_p99_ns"), p.p99 as f64);
        self.metric(&format!("{name}_p999_ns"), p.p999 as f64);
        self.metric(&format!("{name}_max_ns"), p.max as f64);
        self.metric(&format!("{name}_mean_ns"), p.mean);
        self.metric(&format!("{name}_stddev_ns"), p.stddev);
        self.series.insert(name.to_string(), p.clone());
    }

    pub fn write(&self, dir: &Path) -> std::io::Result<()> {
        let s = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(dir.join("summary.json"), s)
    }
}

/// One loaded result directory.
#[derive(Clone, Debug)]
pub struct RunDir {
    pub path: PathBuf,
    pub summary: Summary,
}

pub fn load_runs(root: &Path) -> Vec<RunDir> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let p = e.path().join("summary.json");
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Ok(summary) = serde_json::from_str::<Summary>(&s) {
                    v.push(RunDir {
                        path: e.path(),
                        summary,
                    });
                }
            }
        }
    }
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

// ---- gates ------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Gate {
    pub id: String,
    pub title: String,
    /// Key in `Summary::metrics`, or `check:<name>` for a boolean check that must be true.
    pub metric: String,
    /// `<=`, `>=`, `==`.
    pub op: String,
    #[serde(default)]
    pub value: f64,
    /// Scenario name patterns (`*` suffix allowed).
    pub scenarios: Vec<String>,
    /// Arms the gate is *judged* for; other arms are reported but never fail.
    #[serde(default)]
    pub arms: Vec<String>,
    #[serde(default = "d_quiet")]
    pub condition: String,
    /// Part of the ADR-0026 confirmation rule (the decision rule), as opposed to a spec-only gate.
    #[serde(default)]
    pub decision_rule: bool,
    /// Informational gate: reported, never fails the arm.
    #[serde(default)]
    pub report_only: bool,
    #[serde(default)]
    pub unit: String,
    /// Checks that must be true in a run for the gate to be evaluated at all (the timer-slack
    /// proof for M1/M2). A run where one is false or absent makes the gate NOT MEASURED.
    #[serde(default)]
    pub requires: Vec<String>,
}

fn d_quiet() -> String {
    "quiet".into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GateFile {
    #[serde(default, rename = "gate")]
    pub gates: Vec<Gate>,
}

impl GateFile {
    pub fn load(path: &Path) -> anyhow::Result<GateFile> {
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }
}

pub fn pattern_match(pat: &str, s: &str) -> bool {
    match pat.strip_suffix('*') {
        Some(p) => s.starts_with(p),
        None => pat == s,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Verdict {
    pub gate: String,
    pub title: String,
    pub scenario: String,
    pub arm: String,
    pub condition: String,
    pub threshold: String,
    pub runs: usize,
    /// Value per run, in run order.
    pub values: Vec<f64>,
    /// Worst value against the operator.
    pub worst: f64,
    pub failed_runs: usize,
    pub quiet_violated_runs: usize,
    /// `None` = NOT MEASURED (a missing gate is a failed gate for the confirmation rule).
    pub pass: Option<bool>,
    pub decision_rule: bool,
    pub report_only: bool,
}

fn cmp(op: &str, v: f64, t: f64) -> bool {
    match op {
        "<=" => v <= t,
        ">=" => v >= t,
        "==" => (v - t).abs() < 1e-9,
        "<" => v < t,
        ">" => v > t,
        _ => false,
    }
}

/// Evaluate `gates` for every (scenario, arm, condition) present in `runs` that matches a gate.
/// A gate must hold in **every** run of the group (no median hides a bad run); a failed run
/// (crash, hang, watchdog) fails the group; a group whose metric is missing is NOT MEASURED.
pub fn evaluate(gates: &GateFile, runs: &[RunDir]) -> Vec<Verdict> {
    let mut out = Vec::new();
    let mut groups: BTreeMap<(String, String, String), Vec<&RunDir>> = BTreeMap::new();
    for r in runs {
        let s = &r.summary;
        groups
            .entry((s.scenario.clone(), s.arm.clone(), s.condition.clone()))
            .or_default()
            .push(r);
    }
    for g in &gates.gates {
        for ((scenario, arm, cond), rs) in &groups {
            if *cond != g.condition || !g.scenarios.iter().any(|p| pattern_match(p, scenario)) {
                continue;
            }
            let mut values = Vec::new();
            let mut failed_runs = 0;
            let mut qv = 0;
            let mut missing = false;
            for r in rs {
                let s = &r.summary;
                if s.failed.is_some() {
                    failed_runs += 1;
                }
                if s.quiet_violated {
                    qv += 1;
                }
                if g.requires.iter().any(|c| s.checks.get(c) != Some(&true)) {
                    missing = true;
                    continue;
                }
                let v = if let Some(name) = g.metric.strip_prefix("check:") {
                    s.checks.get(name).map(|b| if *b { 1.0 } else { 0.0 })
                } else {
                    s.metrics.get(&g.metric).copied()
                };
                match v {
                    Some(v) => values.push(v),
                    None => missing = true,
                }
            }
            let target = if g.metric.starts_with("check:") {
                1.0
            } else {
                g.value
            };
            let worst = if values.is_empty() {
                f64::NAN
            } else if g.op == ">=" || g.op == ">" {
                values.iter().cloned().fold(f64::INFINITY, f64::min)
            } else {
                values.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            };
            let judged = g.arms.is_empty() || g.arms.contains(arm);
            let pass = if missing || values.is_empty() {
                None
            } else {
                Some(failed_runs == 0 && values.iter().all(|v| cmp(&g.op, *v, target)))
            };
            out.push(Verdict {
                gate: g.id.clone(),
                title: g.title.clone(),
                scenario: scenario.clone(),
                arm: arm.clone(),
                condition: cond.clone(),
                threshold: format!(
                    "{} {} {}{}",
                    g.metric,
                    g.op,
                    target,
                    if g.unit.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", g.unit)
                    }
                ),
                runs: rs.len(),
                values,
                worst,
                failed_runs,
                quiet_violated_runs: qv,
                pass,
                decision_rule: g.decision_rule,
                report_only: g.report_only || !judged,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(stall_max: f64, arm: &str) -> RunDir {
        let mut s = Summary::new("benign", "focus", arm, "quiet");
        s.metric("stall_max_ns", stall_max);
        RunDir {
            path: PathBuf::new(),
            summary: s,
        }
    }

    fn gate() -> GateFile {
        GateFile {
            gates: vec![Gate {
                id: "M1".into(),
                title: "t".into(),
                metric: "stall_max_ns".into(),
                op: "<=".into(),
                value: 350_000.0,
                scenarios: vec!["benign".into()],
                arms: vec!["bprime".into()],
                condition: "quiet".into(),
                decision_rule: true,
                report_only: false,
                unit: "ns".into(),
                requires: vec![],
            }],
        }
    }

    #[test]
    fn every_run_must_pass() {
        let v = evaluate(
            &gate(),
            &[run(100_000.0, "bprime"), run(400_000.0, "bprime")],
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pass, Some(false));
        assert_eq!(v[0].worst, 400_000.0);
        let v = evaluate(
            &gate(),
            &[run(100_000.0, "bprime"), run(200_000.0, "bprime")],
        );
        assert_eq!(v[0].pass, Some(true));
    }

    #[test]
    fn missing_metric_is_not_measured_and_failed_run_fails() {
        let mut r = run(1.0, "bprime");
        r.summary.metrics.clear();
        assert_eq!(evaluate(&gate(), &[r])[0].pass, None);
        let mut r = run(1.0, "bprime");
        r.summary.failed = Some("killed by watchdog".into());
        assert_eq!(evaluate(&gate(), &[r])[0].pass, Some(false));
    }

    #[test]
    fn other_arms_are_reported_not_judged() {
        let v = evaluate(&gate(), &[run(900_000.0, "a")]);
        assert!(v[0].report_only);
        assert_eq!(v[0].pass, Some(false));
    }
}
