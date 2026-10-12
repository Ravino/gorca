//! `b2`: bake-off B-2 benchmark runner. Prints a result table and writes JSON under
//! `bench/results/b2-ingress/<label>/`.
//!
//! ```text
//! b2 bench   [--label L] [--only t1,t2,t3,t4,t5,t6,t7,fixture] [--quick] [--repeats N]
//!            [--bus daemon,broker] [--out DIR]
//! b2 fixture                       # does dbus-broker really disconnect a slow reader (F-3)?
//! ```
//! Build with `--release`; a debug build is refused unless `--allow-debug` is given.

#![allow(clippy::map_unwrap_or, clippy::cast_precision_loss)]

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use gorca_bench_b2::report::{Row, Verdict, json, table};
use gorca_bench_b2::scenarios::{Ctx, fixture, t1, t2, t3, t4, t5, t6, t7};

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else {
        bail!("usage: b2 bench|fixture [options]; see the module docs");
    };
    if cfg!(debug_assertions) && !args.iter().any(|a| a == "--allow-debug") {
        bail!("debug build: numbers would be meaningless; use --release (or pass --allow-debug)");
    }
    gorca_bench_b2::clock::init();
    let quick = args.iter().any(|a| a == "--quick");
    let existing = std::env::var("B2_EXISTING_BUS").ok();
    let real_buses = arg_value(&args, "--bus")
        .map(|s| s.split(',').map(str::to_owned).collect())
        .unwrap_or_else(|| {
            if quick {
                Vec::new()
            } else {
                vec!["daemon".to_owned(), "broker".to_owned()]
            }
        });
    let ctx = Ctx {
        quick,
        repeats: arg_value(&args, "--repeats")
            .and_then(|s| s.parse().ok())
            .unwrap_or(3),
        real_buses,
        existing,
    };
    if cmd == "threads" {
        return threads(&args);
    }
    if cmd == "fakeapp" {
        return fakeapp(&args);
    }
    let only: Vec<String> = match cmd {
        "fixture" => vec!["fixture".to_owned()],
        "bench" => arg_value(&args, "--only")
            .map(|s| s.split(',').map(str::to_owned).collect())
            .unwrap_or_else(|| {
                ["t7", "t1", "t2", "t3", "t4", "t5", "t6", "fixture"]
                    .map(str::to_owned)
                    .to_vec()
            }),
        other => bail!("unknown command {other}"),
    };
    let load_start = std::fs::read_to_string("/proc/loadavg")
        .unwrap_or_default()
        .trim()
        .to_owned();
    let label = arg_value(&args, "--label").unwrap_or_else(|| "reference".to_owned());
    let mut rows: Vec<Row> = Vec::new();
    for name in &only {
        eprintln!("== {name} ==");
        let part = match name.as_str() {
            "t1" => t1(&ctx),
            "t2" => t2(&ctx),
            "t3" => t3(&ctx),
            "t4" => t4(&ctx),
            "t5" => t5(&ctx),
            "t6" => t6(&ctx),
            "t7" => t7(&ctx),
            "fixture" => fixture(&ctx),
            other => bail!("unknown scenario {other}"),
        }
        .with_context(|| format!("scenario {name}"))?;
        if args.iter().any(|a| a == "-v") {
            eprint!("{}", table(&part));
        }
        rows.extend(part);
    }
    let fails = rows
        .iter()
        .filter(|r| matches!(r.verdict, Verdict::Fail))
        .count();
    let meta = vec![
        ("label".to_owned(), label.clone()),
        (
            "git".to_owned(),
            sh("git", &["rev-parse", "--short", "HEAD"]),
        ),
        (
            "dirty".to_owned(),
            (!sh("git", &["status", "--porcelain", "crates"]).is_empty()).to_string(),
        ),
        (
            "date".to_owned(),
            sh("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        ),
        ("kernel".to_owned(), sh("uname", &["-r"])),
        ("cpus".to_owned(), gorca_bench_b2::sys::cpus().to_string()),
        (
            "loadavg_at_end".to_owned(),
            std::fs::read_to_string("/proc/loadavg")
                .unwrap_or_default()
                .trim()
                .to_owned(),
        ),
        ("loadavg_at_start".to_owned(), load_start),
        (
            "cpu_governor".to_owned(),
            std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
                .map_or_else(|_| "unknown".to_owned(), |g| g.trim().to_owned()),
        ),
        (
            "cpus_allowed".to_owned(),
            std::fs::read_to_string("/proc/self/status")
                .unwrap_or_default()
                .lines()
                .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
                .map_or_else(|| "unknown".to_owned(), |v| v.trim().to_owned()),
        ),
        (
            "profile".to_owned(),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
        ),
        ("quick".to_owned(), quick.to_string()),
        ("repeats".to_owned(), ctx.repeats.to_string()),
        ("scenarios".to_owned(), only.join(",")),
    ];
    let out_dir: PathBuf = arg_value(&args, "--out").map_or_else(
        || PathBuf::from("bench/results/b2-ingress").join(&label),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out_dir)?;
    let stamp = sh("date", &["-u", "+%Y%m%dT%H%M%SZ"]);
    let base = out_dir.join(format!("{stamp}{}", if quick { "-quick" } else { "" }));
    std::fs::write(base.with_extension("json"), json(&meta, &rows))?;
    std::fs::write(base.with_extension("txt"), table(&rows))?;
    println!("{}", table(&rows));
    println!(
        "results: {}.{{json,txt}}   failing metrics: {fails}",
        base.display()
    );
    Ok(())
}

/// Diagnostic: where does the candidate spend CPU? `b2 threads [--rate N] [--secs S] [--mix M]`.
fn threads(args: &[String]) -> Result<()> {
    use gorca_bench_b2::bus::BusKind;
    use gorca_bench_b2::feeder::AppPlan;
    use gorca_bench_b2::mockbus::MockOpts;
    use gorca_bench_b2::runner::{AppSpec, RunSpec, execute};
    use gorca_bench_b2::workload::Mix;
    let rate: f64 = arg_value(args, "--rate")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let secs: u64 = arg_value(args, "--secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    let mix = arg_value(args, "--mix")
        .and_then(|s| Mix::parse(&s))
        .unwrap_or(Mix::Fakeapp);
    let mut spec = RunSpec::new(
        BusKind::Mock(MockOpts::default()),
        std::time::Duration::from_secs(secs),
    );
    let mut plan = AppPlan::new("t", 1, mix, rate.max(1.0));
    if rate <= 0.0 {
        plan.max_events = Some(0);
    }
    spec.apps.push(AppSpec {
        plan,
        cons: gorca_bench_b2::rig::ConsumerCfg::default(),
    });
    let o = execute(&spec)?;
    println!(
        "elapsed {:.2}s, messages {}, pushed {}",
        o.elapsed_s, o.drain.messages, o.drain.pushed
    );
    for (name, ns) in &o.cpu_threads {
        println!(
            "{name:<20} {:>9.1} ms  ({:.1} % of one core)",
            *ns as f64 / 1e6,
            *ns as f64 / 1e9 / o.elapsed_s * 100.0
        );
    }
    Ok(())
}

/// `b2 fakeapp storm|mixed|hung [--secs N] [--label L]`, run inside `tests/harness/run-isolated.sh`.
fn fakeapp(args: &[String]) -> Result<()> {
    use gorca_bench_b2::fakeapp::{FaScenario, default_fakeapp, run};
    let scn = args
        .get(1)
        .and_then(|s| FaScenario::parse(s))
        .context("usage: b2 fakeapp storm|mixed|hung")?;
    let addr = std::env::var("AT_SPI_BUS_ADDRESS")
        .context("AT_SPI_BUS_ADDRESS not set: run inside tests/harness/run-isolated.sh")?;
    let secs = arg_value(args, "--secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(15);
    let label = arg_value(args, "--label").unwrap_or_else(|| "reference".to_owned());
    let rows = run(scn, &addr, secs, &default_fakeapp())?;
    let meta = vec![
        ("label".to_owned(), label.clone()),
        ("scenarios".to_owned(), format!("fakeapp-{scn:?}")),
    ];
    let out_dir = arg_value(args, "--out").map_or_else(
        || PathBuf::from("bench/results/b2-ingress").join(&label),
        PathBuf::from,
    );
    std::fs::create_dir_all(&out_dir)?;
    let stamp = sh("date", &["-u", "+%Y%m%dT%H%M%SZ"]);
    let base = out_dir.join(format!(
        "{stamp}-fakeapp-{}",
        format!("{scn:?}").to_lowercase()
    ));
    std::fs::write(base.with_extension("json"), json(&meta, &rows))?;
    println!("{}", table(&rows));
    println!("results: {}.json", base.display());
    Ok(())
}
