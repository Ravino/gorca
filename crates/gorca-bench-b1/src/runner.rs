//! H-6: interleaved repeats across arms, median gating, report rows.
//!
//! The runner never builds or starts anything itself. For every slot of the schedule it either
//! runs the walk driver (`--driver <cmd>`, one process per run, told where to write the run
//! directory through `B1_*` environment variables) or reads a pre-recorded run directory
//! (`--rundirs <root>`, layout `<root>/<arm>/<kind>/<repeat>/`). Both feed [`crate::rundir::evaluate`].
//! Gates apply to the **median over repeats** of the per-run value, never to a pooled number; counts
//! that must be zero look at the worst repeat.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use gorca_bench_b2::report::{Row, Verdict};

use crate::limits::{self, Cmp, L1C_VS_R0};
use crate::rundir::RunMetrics;

/// `--arm label:path[:rev]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arm {
    pub label: String,
    pub daemon: PathBuf,
    pub rev: String,
}

impl Arm {
    pub fn parse(s: &str) -> Result<Self> {
        let mut it = s.splitn(3, ':');
        let label = it.next().unwrap_or_default();
        let path = it.next().unwrap_or_default();
        if label.is_empty() || path.is_empty() {
            bail!("--arm must be `label:path-to-gorca-daemon[:git-rev]`, got `{s}`");
        }
        if !label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("arm label `{label}` must be [A-Za-z0-9_-]");
        }
        Ok(Self {
            label: label.to_owned(),
            daemon: PathBuf::from(path),
            rev: it.next().unwrap_or("unknown").to_owned(),
        })
    }
}

/// One run: arm index, run kind, 0-based repeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub arm: usize,
    pub kind: String,
    pub repeat: usize,
}

/// Round-robin over arms, rotated by one each round, so no arm always runs first (host drift and
/// thermal state hit every arm equally). `plan` is `(kind, repeats)` in execution order.
pub fn schedule(arms: usize, plan: &[(&str, usize)]) -> Vec<Slot> {
    let mut v = Vec::new();
    if arms == 0 {
        return v;
    }
    for (kind, repeats) in plan {
        for r in 0..*repeats {
            for i in 0..arms {
                v.push(Slot {
                    arm: (i + r) % arms,
                    kind: (*kind).to_owned(),
                    repeat: r,
                });
            }
        }
    }
    v
}

/// Default repeats of SPEC 8.3 (`conf` is cargo test, `soak` is separate).
pub fn default_plan() -> Vec<(&'static str, usize)> {
    vec![("step", 7), ("walk", 5), ("ff", 5), ("idle", 3), ("big", 3)]
}

/// `<root>/<arm>/<kind>/<repeat>/`.
pub fn run_dir(root: &Path, arm: &str, kind: &str, repeat: usize) -> PathBuf {
    root.join(arm).join(kind).join(repeat.to_string())
}

/// Run the external walk driver for one slot. The driver must exit 0 and leave the run directory
/// in the shape documented in [`crate::rundir`].
pub fn run_driver(driver: &str, arm: &Arm, slot: &Slot, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    let mut parts = driver.split_whitespace();
    let prog = parts.next().context("empty --driver")?;
    let status = Command::new(prog)
        .args(parts)
        .env("B1_ARM", &arm.label)
        .env("B1_DAEMON", &arm.daemon)
        .env("B1_RUN_KIND", &slot.kind)
        .env("B1_REPEAT", slot.repeat.to_string())
        .env("B1_RUN_DIR", dir)
        .status()
        .with_context(|| format!("spawning driver `{driver}`"))?;
    if !status.success() {
        bail!(
            "driver exited with {status} for {} {} #{}",
            arm.label,
            slot.kind,
            slot.repeat
        );
    }
    Ok(())
}

/// Per `(arm label, kind)` results of all repeats.
pub type Collected = BTreeMap<(String, String), Vec<RunMetrics>>;

/// Turn collected runs into report rows. `baseline` maps `(metric id, arm label)` to the R0
/// value used by the "1.2 x R0" cold-path gate.
pub fn gate_rows(
    collected: &Collected,
    baseline: &BTreeMap<(String, String), f64>,
    r0_label: &str,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for ((arm, kind), runs) in collected {
        let valid: Vec<&RunMetrics> = runs.iter().filter(|r| r.invalid.is_empty()).collect();
        let invalid = runs.len() - valid.len();
        let mut inv = Row::info("B1-RUNS.invalid", kind, arm, "runs", &[invalid as f64])
            .at_most_in_every_run(0.0);
        let reasons: Vec<&str> = runs
            .iter()
            .flat_map(|r| r.invalid.iter().map(String::as_str))
            .take(3)
            .collect();
        if !reasons.is_empty() {
            inv = inv.note(reasons.join(" | "));
        }
        rows.push(inv);

        let mut ids: Vec<&String> = valid.iter().flat_map(|r| r.metrics.keys()).collect();
        ids.sort();
        ids.dedup();
        for id in ids {
            let vals: Vec<f64> = valid
                .iter()
                .filter_map(|r| r.metrics.get(id))
                .copied()
                .collect();
            let base = id.split('@').next().unwrap_or(id);
            let g = limits::gate(id).or_else(|| limits::gate(base));
            let (unit, cmp, th) =
                g.map_or(("", Cmp::Rank, f64::NAN), |g| (g.unit, g.cmp, g.threshold));
            let mut row = Row::info(id, kind, arm, unit, &vals);
            row = match cmp {
                Cmp::MedianAtMost => row.at_most(th),
                Cmp::MedianAtLeast => row.at_least(th),
                Cmp::EveryRunAtMost => row.at_most_in_every_run(th),
                Cmp::Rank => row,
            };
            if vals.len() < runs.len() {
                row = row.note(format!("{} of {} runs produced it", vals.len(), runs.len()));
            }
            // Cold path unchanged: median <= 1.2 x R0's median.
            if id == "B1-L1c.p99"
                && arm != r0_label
                && let Some(r0) = baseline.get(&(id.clone(), r0_label.to_owned()))
            {
                let ratio = row.value / r0;
                rows.push(
                    Row::info("B1-L1c.vs_r0", kind, arm, "x", &[ratio])
                        .at_most(L1C_VS_R0)
                        .note(format!("R0 median {r0:.3} ms")),
                );
            }
            rows.push(row);
        }
        // A bold gate of this run kind with no value at all is a failure, not a silent pass.
        for g in limits::GATES
            .iter()
            .filter(|g| g.run == kind && g.cmp != Cmp::Rank)
        {
            let has = valid.iter().any(|r| {
                r.metrics
                    .keys()
                    .any(|k| k == g.id || k.split('@').next() == Some(g.id))
            });
            if !has {
                let mut r = Row::info(g.id, kind, arm, g.unit, &[f64::NAN]);
                r.verdict = Verdict::Fail;
                rows.push(r.note("no value produced by any valid run"));
            }
        }
    }
    rows.sort_by(|a, b| (&a.bus, &a.scenario, &a.id).cmp(&(&b.bus, &b.scenario, &b.id)));
    rows
}

/// An arm is rankable only if no bold row fails (SPEC 9, Gate).
pub fn rankable(rows: &[Row], arm: &str) -> bool {
    !rows
        .iter()
        .any(|r| r.bus == arm && matches!(r.verdict, Verdict::Fail))
}

/// Markdown summary: one line per arm with its failing rows.
pub fn summary_md(rows: &[Row], arms: &[String]) -> String {
    use std::fmt::Write as _;
    let mut s = String::from("| arm | rankable | failing gates |\n|---|---|---|\n");
    for a in arms {
        let bad: Vec<String> = rows
            .iter()
            .filter(|r| &r.bus == a && matches!(r.verdict, Verdict::Fail))
            .map(|r| r.id.clone())
            .collect();
        let _ = writeln!(
            s,
            "| {a} | {} | {} |",
            if bad.is_empty() { "yes" } else { "no" },
            if bad.is_empty() {
                "-".to_owned()
            } else {
                bad.join(", ")
            }
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, f64)]) -> RunMetrics {
        RunMetrics {
            metrics: pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
            ..RunMetrics::default()
        }
    }

    #[test]
    fn arm_parsing() {
        let a = Arm::parse("a:target/release/gorca-daemon:abc123").unwrap();
        assert_eq!((a.label.as_str(), a.rev.as_str()), ("a", "abc123"));
        assert!(Arm::parse("nolabel").is_err());
        assert!(Arm::parse("bad label:/x").is_err());
        assert_eq!(Arm::parse("r0:/x").unwrap().rev, "unknown");
    }

    #[test]
    fn schedule_rotates_and_balances_positions() {
        let s = schedule(3, &[("walk", 3)]);
        assert_eq!(s.len(), 9);
        let order: Vec<usize> = s.iter().map(|x| x.arm).collect();
        assert_eq!(order, vec![0, 1, 2, 1, 2, 0, 2, 0, 1]);
        // Every arm is first exactly once over three rounds.
        let firsts: Vec<usize> = s.chunks(3).map(|c| c[0].arm).collect();
        assert_eq!(firsts, vec![0, 1, 2]);
        assert!(schedule(0, &default_plan()).is_empty());
    }

    #[test]
    fn median_gate_ignores_one_bad_repeat_but_zero_gates_do_not() {
        let mut c = Collected::new();
        c.insert(
            ("a".into(), "walk".into()),
            vec![
                m(&[("B1-L1h.p99", 1.0), ("B1-STALE@walk", 0.0)]),
                m(&[("B1-L1h.p99", 1.5), ("B1-STALE@walk", 0.0)]),
                m(&[("B1-L1h.p99", 9.0), ("B1-STALE@walk", 1.0)]),
            ],
        );
        let rows = gate_rows(&c, &BTreeMap::new(), "r0");
        let l1h = rows.iter().find(|r| r.id == "B1-L1h.p99").unwrap();
        assert!(matches!(l1h.verdict, Verdict::Pass), "median 1.5 <= 2");
        assert_eq!((l1h.min, l1h.max), (1.0, 9.0));
        let stale = rows.iter().find(|r| r.id == "B1-STALE@walk").unwrap();
        assert!(
            matches!(stale.verdict, Verdict::Fail),
            "worst repeat counts"
        );
        assert!(!rankable(&rows, "a"));
    }

    #[test]
    fn invalid_runs_are_excluded_and_reported() {
        let mut bad = m(&[("B1-L1h.p99", 99.0)]);
        bad.invalid.push("scripted focus: 1 < 2".into());
        let mut c = Collected::new();
        c.insert(
            ("a".into(), "walk".into()),
            vec![m(&[("B1-L1h.p99", 1.0)]), bad],
        );
        let rows = gate_rows(&c, &BTreeMap::new(), "r0");
        let l1h = rows.iter().find(|r| r.id == "B1-L1h.p99").unwrap();
        assert_eq!(l1h.value, 1.0);
        let inv = rows.iter().find(|r| r.id == "B1-RUNS.invalid").unwrap();
        assert!(matches!(inv.verdict, Verdict::Fail));
    }

    #[test]
    fn cold_path_is_compared_with_r0() {
        let mut c = Collected::new();
        c.insert(("a".into(), "walk".into()), vec![m(&[("B1-L1c.p99", 6.0)])]);
        let mut base = BTreeMap::new();
        base.insert(("B1-L1c.p99".to_owned(), "r0".to_owned()), 4.0);
        let rows = gate_rows(&c, &base, "r0");
        let r = rows.iter().find(|r| r.id == "B1-L1c.vs_r0").unwrap();
        assert!(matches!(r.verdict, Verdict::Fail), "1.5 x > 1.2 x");
        base.insert(("B1-L1c.p99".to_owned(), "r0".to_owned()), 5.5);
        let rows = gate_rows(&c, &base, "r0");
        let r = rows.iter().find(|r| r.id == "B1-L1c.vs_r0").unwrap();
        assert!(matches!(r.verdict, Verdict::Pass));
    }

    #[test]
    fn a_missing_bold_gate_fails_instead_of_passing_silently() {
        let mut c = Collected::new();
        c.insert(("a".into(), "walk".into()), vec![m(&[("B1-L1h.p99", 1.0)])]);
        let rows = gate_rows(&c, &BTreeMap::new(), "r0");
        assert!(
            rows.iter()
                .any(|r| r.id == "B1-S1.p99" && matches!(r.verdict, Verdict::Fail))
        );
    }

    #[test]
    fn per_model_ff_ids_resolve_to_the_ff_gate() {
        let mut c = Collected::new();
        c.insert(
            ("a".into(), "ff".into()),
            vec![m(&[
                ("B1-FF.p99@files-big", 21.0),
                ("B1-FF.p99@shell", 3.0),
            ])],
        );
        let rows = gate_rows(&c, &BTreeMap::new(), "r0");
        let big = rows.iter().find(|r| r.id == "B1-FF.p99@files-big").unwrap();
        assert!(matches!(big.verdict, Verdict::Fail));
        let sh = rows.iter().find(|r| r.id == "B1-FF.p99@shell").unwrap();
        assert!(matches!(sh.verdict, Verdict::Pass));
    }
}
