// SPDX-License-Identifier: MIT OR Apache-2.0
//! `m18a0-report <results-dir> [--gates gates.toml] [--arm bprime] [--out report.md] [--code arms/]`
//!
//! Loads every `summary.json` under the results directory, adds the cross-run derived metrics
//! (M8 isolation, PF-1 differential), evaluates the gates from `gates.toml` (no hand-typed
//! verdicts) and writes `report.md` plus `verdicts.json`. With no `--arm` it writes one table per
//! arm and a cross-arm comparison. Exit code 1 when a judged decision-rule gate fails or is
//! NOT MEASURED for the selected arm.
#![allow(
    clippy::needless_range_loop,
    clippy::unusual_byte_groupings,
    clippy::should_implement_trait,
    clippy::if_same_then_else,
    clippy::chunks_exact_to_as_chunks,
    clippy::manual_is_multiple_of,
    clippy::disallowed_methods,
    clippy::too_many_arguments,
    clippy::type_complexity
)] // throwaway prototype harness: style lints only

use anyhow::Result;
use m18a0_common::summary::*;
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

fn arg(a: &[String], n: &str) -> Option<String> {
    a.iter()
        .position(|x| x == n)
        .and_then(|i| a.get(i + 1))
        .cloned()
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.is_empty() {
        f64::NAN
    } else {
        v[v.len() / 2]
    }
}

/// M8.3 isolation: max over benign plugins of |coverage under adversary - coverage in benign|, in pp.
fn add_isolation(runs: &mut [RunDir]) {
    let mut base: BTreeMap<(String, String), BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    for r in runs.iter().filter(|r| r.summary.scenario == "benign") {
        let e = base
            .entry((r.summary.arm.clone(), r.summary.condition.clone()))
            .or_default();
        for (p, v) in &r.summary.per_plugin {
            e.entry(p.clone()).or_default().push(*v);
        }
    }
    for r in runs
        .iter_mut()
        .filter(|r| r.summary.scenario.starts_with("adv-"))
    {
        let Some(b) = base.get(&(r.summary.arm.clone(), r.summary.condition.clone())) else {
            continue;
        };
        let mut worst = 0.0f64;
        let mut any = false;
        for (p, v) in &r.summary.per_plugin {
            if let Some(bv) = b.get(p) {
                let mut bv = bv.clone();
                worst = worst.max(((median(&mut bv) - v) * 100.0).abs());
                any = true;
            }
        }
        if any {
            r.summary.metrics.insert("m8_isolation_pp".into(), worst);
        }
    }
}

/// M10c: digests of pf1 runs of two arms must be equal line by line.
fn add_pf1(runs: &mut [RunDir]) {
    let mut by_arm: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for r in runs
        .iter()
        .filter(|r| r.summary.scenario == "pf1-differential" && r.summary.failed.is_none())
    {
        if let Ok(t) = std::fs::read_to_string(r.path.join("pf1.digests")) {
            by_arm.insert(r.summary.arm.clone(), t.lines().map(String::from).collect());
        }
    }
    let (Some(a), Some(b)) = (by_arm.get("a"), by_arm.get("bprime")) else {
        return;
    };
    let diffs = a.iter().zip(b.iter()).filter(|(x, y)| x != y).count() + a.len().abs_diff(b.len());
    for r in runs
        .iter_mut()
        .filter(|r| r.summary.scenario == "pf1-differential")
    {
        r.summary
            .metrics
            .insert("pf1_differences".into(), diffs as f64);
    }
}

/// M11: non-blank, non-comment lines, `unsafe` blocks and `Ordering::` uses of an arm crate.
fn code_stats(dir: &Path) -> (usize, usize, usize) {
    let (mut loc, mut uns, mut ord) = (0, 0, 0);
    fn walk(d: &Path, f: &mut dyn FnMut(&Path)) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n != "tests" && n != "target") {
                        walk(&p, f);
                    }
                } else if p.extension().is_some_and(|x| x == "rs") {
                    f(&p);
                }
            }
        }
    }
    walk(&dir.join("src"), &mut |p| {
        let t = std::fs::read_to_string(p).unwrap_or_default();
        let body = t.split("#[cfg(test)]").next().unwrap_or("");
        for l in body.lines() {
            let l = l.trim();
            if l.is_empty() || l.starts_with("//") {
                continue;
            }
            loc += 1;
            uns += l.matches("unsafe ").count();
            ord += l.matches("Ordering::").count();
        }
    });
    (loc, uns, ord)
}

fn fmt_val(v: f64, unit: &str) -> String {
    if v.is_nan() {
        return "n/a".into();
    }
    if unit.starts_with("ns") {
        format!("{:.1} us", v / 1000.0)
    } else {
        format!("{v}")
    }
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let root = PathBuf::from(a.first().cloned().unwrap_or_else(|| "results".into()));
    let gates_path = arg(&a, "--gates")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("gates.toml"));
    let only_arm = arg(&a, "--arm");
    let gates = GateFile::load(&gates_path)?;
    let mut runs = load_runs(&root);
    if let Some(arm) = &only_arm {
        runs.retain(|r| &r.summary.arm == arm);
    }
    add_isolation(&mut runs);
    add_pf1(&mut runs);
    let verdicts = evaluate(&gates, &runs);

    let mut md = String::new();
    writeln!(md, "# M18a-0 report\n")?;
    writeln!(md, "Generated by `m18a0-report` from `{}` ({} runs). Verdicts are computed from `summary.json` against `{}`; none is typed by hand.\n", root.display(), runs.len(), gates_path.display())?;
    let mut fail_decision = false;
    let arms: Vec<String> = {
        let mut s: Vec<String> = runs.iter().map(|r| r.summary.arm.clone()).collect();
        s.sort();
        s.dedup();
        s
    };
    for arm in &arms {
        writeln!(md, "## Arm `{arm}`\n")?;
        writeln!(
            md,
            "| Gate | Scenario | Cond | Threshold | Worst | Runs | Verdict | Rule |"
        )?;
        writeln!(md, "|---|---|---|---|---|---|---|---|")?;
        for v in verdicts.iter().filter(|v| &v.arm == arm) {
            let verdict = match (v.pass, v.report_only) {
                (None, _) => "NOT MEASURED".to_string(),
                (Some(true), _) => "PASS".to_string(),
                (Some(false), true) => "fail (reported, not judged)".to_string(),
                (Some(false), false) => "**FAIL**".to_string(),
            };
            if v.decision_rule && !v.report_only && v.pass != Some(true) {
                fail_decision = true;
            }
            let q = if v.quiet_violated_runs > 0 {
                format!(" ({} quiet-violated)", v.quiet_violated_runs)
            } else {
                String::new()
            };
            writeln!(
                md,
                "| {} {} | {} | {} | `{}` | {} | {}{} | {} | {} |",
                v.gate,
                v.title,
                v.scenario,
                v.condition,
                v.threshold,
                fmt_val(
                    v.worst,
                    if v.threshold.ends_with("ns") {
                        "ns"
                    } else {
                        ""
                    }
                ),
                v.runs,
                q,
                verdict,
                if v.decision_rule { "ADR" } else { "spec" }
            )?;
        }
        writeln!(md)?;
        writeln!(md, "### Raw data\n")?;
        for r in runs.iter().filter(|r| &r.summary.arm == arm) {
            let st = r
                .summary
                .metrics
                .get("stall_p99_ns")
                .map(|v| {
                    format!(
                        "stall p99 {:.1} us, max {:.1} us",
                        v / 1000.0,
                        r.summary
                            .metrics
                            .get("stall_max_ns")
                            .copied()
                            .unwrap_or(f64::NAN)
                            / 1000.0
                    )
                })
                .unwrap_or_default();
            writeln!(
                md,
                "- `{}` {} {}",
                r.path.display(),
                st,
                r.summary
                    .failed
                    .as_deref()
                    .map(|f| format!("FAILED: {f}"))
                    .unwrap_or_default()
            )?;
        }
        writeln!(md)?;
    }
    if let Some(code) = arg(&a, "--code") {
        writeln!(md, "## M11 cost and complexity (reported, no gate)\n\n| Crate | LOC (non-test, non-comment) | `unsafe` | `Ordering::` |\n|---|---|---|---|")?;
        for c in ["arm-a", "arm-bprime"] {
            let (l, u, o) = code_stats(&Path::new(&code).join(c));
            writeln!(md, "| {c} | {l} | {u} | {o} |")?;
        }
        writeln!(md)?;
    }
    writeln!(
        md,
        "## Deviations from the spec\n\nSee `harness/README.md`, section Deviations.\n"
    )?;
    let out = arg(&a, "--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("report.md"));
    std::fs::write(&out, md)?;
    std::fs::write(
        out.with_extension("verdicts.json"),
        serde_json::to_string_pretty(&verdicts)?,
    )?;
    println!("wrote {}", out.display());
    if fail_decision {
        std::process::exit(1);
    }
    Ok(())
}
