//! `b1`: B-1 harness runner.
//!
//! ```text
//! b1 run --label L --arm a:/path/gorca-daemon[:rev] [--arm ...] (--driver "CMD ARGS" | --rundirs ROOT)
//!        [--plan walk=5,ff=5,idle=3,big=3,step=7] [--baseline R0.json] [--r0 r0] [--out DIR] [--no-fail]
//! b1 analyze <walk|ff|idle|big|soak|step> <run-dir>     metrics of one run directory
//! b1 bus-probe [ADDRESS]                                Debug.Stats availability and counters
//! b1 limits-md                                          the SPEC section 9 table from limits.rs
//! b1 baseline-md <results.json> <r0-label>              SPEC section 12 text from an R0 result
//! ```
//!
//! Results: `bench/results/b1-mirror-fill/<label>/<UTC>.{json,txt}` (rows in the B-2/e2e format).
//! With `--driver` the command is run once per schedule slot with `B1_ARM`, `B1_DAEMON`,
//! `B1_RUN_KIND`, `B1_REPEAT`, `B1_RUN_DIR` set (see `gorca_bench_b1::rundir`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gorca_bench_b1::runner::{self, Arm, Collected};
use gorca_bench_b1::verdict::KeyMode;
use gorca_bench_b1::{host, limits, msgs, rundir};
use gorca_bench_b2::report::{Verdict, json, table};
use gorca_bench_b2::wire::BusConn;

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn opts(args: &[String], name: &str) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| *a == name)
        .filter_map(|(i, _)| args.get(i + 1).cloned())
        .collect()
}

fn parse_plan(s: Option<String>) -> Result<Vec<(String, usize)>> {
    let Some(s) = s else {
        return Ok(runner::default_plan()
            .into_iter()
            .map(|(k, n)| (k.to_owned(), n))
            .collect());
    };
    s.split(',')
        .map(|p| {
            let (k, n) = p
                .split_once('=')
                .with_context(|| format!("plan item `{p}` is not kind=N"))?;
            Ok((
                k.to_owned(),
                n.parse().with_context(|| format!("repeats in `{p}`"))?,
            ))
        })
        .collect()
}

fn cmd_run(args: &[String]) -> Result<bool> {
    let label = opt(args, "--label").context("--label is required")?;
    let arms: Vec<Arm> = opts(args, "--arm")
        .iter()
        .map(|s| Arm::parse(s))
        .collect::<Result<_>>()?;
    if arms.is_empty() {
        bail!("at least one --arm label:path is required (R0 is just another arm)");
    }
    let driver = opt(args, "--driver");
    let rundirs = opt(args, "--rundirs").map(PathBuf::from);
    if driver.is_some() == rundirs.is_some() {
        bail!("exactly one of --driver or --rundirs is required");
    }
    let plan = parse_plan(opt(args, "--plan"))?;
    let plan_ref: Vec<(&str, usize)> = plan.iter().map(|(k, n)| (k.as_str(), *n)).collect();
    let out_dir =
        PathBuf::from(opt(args, "--out").unwrap_or_else(|| "bench/results/b1-mirror-fill".into()))
            .join(&label);
    let work = rundirs.clone().unwrap_or_else(|| out_dir.join("runs"));
    let r0 = opt(args, "--r0").unwrap_or_else(|| "r0".into());
    let baseline = match opt(args, "--baseline") {
        Some(p) => rundir::load_baseline(Path::new(&p))?,
        None => BTreeMap::new(),
    };

    let load_start = host::loadavg();
    let slots = runner::schedule(arms.len(), &plan_ref);
    let mut collected = Collected::new();
    for (i, slot) in slots.iter().enumerate() {
        let arm = &arms[slot.arm];
        let dir = runner::run_dir(&work, &arm.label, &slot.kind, slot.repeat);
        eprintln!(
            "[{}/{}] {} {} #{} (load {})",
            i + 1,
            slots.len(),
            arm.label,
            slot.kind,
            slot.repeat,
            host::loadavg()
        );
        if let Some(d) = &driver {
            runner::run_driver(d, arm, slot, &dir)?;
        }
        let m = if dir.is_dir() {
            rundir::evaluate(&slot.kind, &dir, KeyMode::PathOnly)
                .with_context(|| format!("evaluating {}", dir.display()))?
        } else {
            let mut m = rundir::RunMetrics::default();
            m.invalid
                .push(format!("missing run directory {}", dir.display()));
            m
        };
        collected
            .entry((arm.label.clone(), slot.kind.clone()))
            .or_default()
            .push(m);
    }

    let rows = runner::gate_rows(&collected, &baseline, &r0);
    let arm_revs: Vec<(String, String)> = arms
        .iter()
        .map(|a| (a.label.clone(), a.rev.clone()))
        .collect();
    let mut meta = host::meta(
        &arm_revs,
        &[
            ("label", label.clone()),
            (
                "plan",
                plan.iter()
                    .map(|(k, n)| format!("{k}={n}"))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("loadavg_end", host::loadavg()),
            (
                "schedule",
                "round-robin over arms, rotated by one each round".into(),
            ),
        ],
    );
    meta.push(("loadavg_start_run".into(), load_start));
    // H-10(a): `run-all.sh` runs the strict (+-20 %) service-time fidelity test and passes the
    // result here. A failed fidelity check makes the whole run INVALID, never a silent score.
    let fidelity = std::env::var("B1_FIDELITY").unwrap_or_else(|_| "not-run".into());
    let fidelity_failed = fidelity.starts_with("fail");
    meta.push(("fidelity".into(), fidelity.clone()));
    meta.push((
        "run_status".into(),
        if fidelity_failed {
            format!("INVALID: service-time fidelity {fidelity}")
        } else {
            "valid".into()
        },
    ));
    let labels: Vec<String> = arms.iter().map(|a| a.label.clone()).collect();
    let mut text = format!("{}\n{}", table(&rows), runner::summary_md(&rows, &labels));
    if fidelity_failed {
        text = format!(
            "INVALID RUN: service-time fidelity check failed ({fidelity}); scores below are not authoritative\n\n{text}"
        );
    }
    print!("{text}");
    std::fs::create_dir_all(&out_dir)?;
    let stamp = host::stamp();
    std::fs::write(out_dir.join(format!("{stamp}.json")), json(&meta, &rows))?;
    std::fs::write(out_dir.join(format!("{stamp}.txt")), &text)?;
    eprintln!("wrote {}", out_dir.join(format!("{stamp}.json")).display());
    if fidelity_failed {
        bail!("INVALID RUN: service-time fidelity check failed ({fidelity})");
    }
    Ok(!rows.iter().any(|r| matches!(r.verdict, Verdict::Fail)))
}

fn cmd_analyze(args: &[String]) -> Result<()> {
    let kind = args.first().context("analyze <kind> <run-dir>")?;
    let dir = args.get(1).context("analyze <kind> <run-dir>")?;
    let m = rundir::evaluate(kind, Path::new(dir), KeyMode::PathOnly)?;
    for (k, v) in &m.metrics {
        println!("{k:<28} {v:>14.4}");
    }
    for (k, v) in &m.info {
        println!("  info {k:<26} {v:>12.4}");
    }
    for i in &m.invalid {
        println!("INVALID: {i}");
    }
    Ok(())
}

fn cmd_bus_probe(args: &[String]) -> Result<()> {
    let addr = args
        .first()
        .cloned()
        .or_else(|| std::env::var("DBUS_SESSION_BUS_ADDRESS").ok())
        .context("no address (pass one or run inside tests/harness/run-isolated.sh)")?;
    let mut c = BusConn::connect(&addr, Duration::from_secs(5))?;
    println!(
        "debug_stats_available: {}",
        msgs::stats_interface_available(&mut c, Duration::from_secs(2))
    );
    let me = c.unique.clone();
    println!(
        "{me} GetConnectionStats (gauges, NOT cumulative): {:?}",
        msgs::connection_stats_raw(&mut c, &me, Duration::from_secs(2))?
    );
    Ok(())
}

fn cmd_baseline_md(args: &[String]) -> Result<()> {
    let path = args
        .first()
        .context("baseline-md <results.json> <r0-label>")?;
    let label = args.get(1).map_or("r0", String::as_str);
    let base = rundir::load_baseline(Path::new(path))?;
    println!("| Metric | R0 median |\n|---|---|");
    for ((id, bus), v) in &base {
        if bus == label
            && limits::gate(id)
                .or_else(|| limits::gate(id.split('@').next().unwrap_or(id)))
                .is_some()
        {
            println!("| {id} | {v:.4} |");
        }
    }
    Ok(())
}

fn main() -> Result<std::process::ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("run") => {
            let ok = cmd_run(rest)?;
            Ok(if ok || args.iter().any(|a| a == "--no-fail") {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::from(1)
            })
        }
        Some("analyze") => cmd_analyze(rest).map(|()| std::process::ExitCode::SUCCESS),
        Some("bus-probe") => cmd_bus_probe(rest).map(|()| std::process::ExitCode::SUCCESS),
        Some("limits-md") => {
            print!("{}", limits::markdown());
            Ok(std::process::ExitCode::SUCCESS)
        }
        Some("baseline-md") => cmd_baseline_md(rest).map(|()| std::process::ExitCode::SUCCESS),
        _ => {
            eprintln!(
                "usage: b1 run|analyze|bus-probe|limits-md|baseline-md (see the file header)"
            );
            Ok(std::process::ExitCode::from(64))
        }
    }
}
