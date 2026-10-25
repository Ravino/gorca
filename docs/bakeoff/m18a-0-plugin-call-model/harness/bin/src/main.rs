// SPDX-License-Identifier: MIT OR Apache-2.0
//! `m18a0-run`: one binary, `--arm noop|ref|a|bprime`, scenario files, raw data + JSON summary.
//!
//! ```text
//! m18a0-run run --arm ref --scenario scenarios/smoke.toml --condition dev --out results
//! m18a0-run selftest [--out results]          # bad runners must fail, stale replies must be dropped
//! m18a0-run list
//! ```
//! Results go to `<out>/<UTC stamp>-<arm>-<condition>-<scenario>/` and are never overwritten.
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

mod arms;
mod focus;
mod meta;
mod micro;
mod storm;

use anyhow::{anyhow, Result};
use focus::RunEnv;
use m18a0_common::host::CountingAlloc;
use m18a0_common::scenario::Scenario;
use m18a0_common::summary::Summary;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(|s| s.as_str()) {
        Some("run") => cmd_run(&args[1..]),
        Some("selftest") => cmd_selftest(&args[1..]),
        Some("memory-child") => cmd_memory_child(&args[1..]),
        Some("list") => {
            println!("arms: {}", arms::ARMS.join(" "));
            Ok(true)
        }
        _ => {
            eprintln!("usage: m18a0-run run|selftest|list|memory-child ... (see src/main.rs)");
            Ok(false)
        }
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn cmd_memory_child(args: &[String]) -> Result<bool> {
    let arm = opt(args, "--arm").ok_or_else(|| anyhow!("--arm"))?;
    let f = arms::get(&arm).ok_or_else(|| anyhow!("unknown arm {arm}"))?;
    let count: usize = opt(args, "--count")
        .ok_or_else(|| anyhow!("--count"))?
        .parse()?;
    let mode = opt(args, "--mode").unwrap_or_else(|| "distinct".into());
    let kappa: f64 = opt(args, "--kappa")
        .and_then(|k| k.parse().ok())
        .unwrap_or(2.0);
    let v = micro::memory_child(f, count, &mode, kappa)?;
    println!("{v}");
    Ok(true)
}

fn cmd_selftest(args: &[String]) -> Result<bool> {
    let sc: Scenario = toml::from_str("name = \"selftest\"\nkind = \"selftest\"\n")?;
    let kappa = opt(args, "--kappa").and_then(|k| k.parse().ok());
    let res = micro::selftest(&sc, kappa)?;
    let mut ok = true;
    for (name, pass, detail) in &res {
        println!("{} {name} {detail}", if *pass { "PASS" } else { "FAIL" });
        ok &= *pass;
    }
    if let Some(out) = opt(args, "--out") {
        let dir = PathBuf::from(out);
        std::fs::create_dir_all(&dir)?;
        let p = dir.join(format!("{}-selftest.json", meta::utc_stamp()));
        std::fs::write(
            p,
            serde_json::to_string_pretty(&json!(res
                .iter()
                .map(|(n, p, d)| json!({"expect": n, "pass": p, "detail": d}))
                .collect::<Vec<_>>()))?,
        )?;
    }
    Ok(ok)
}

fn unique_dir(out: &Path, stem: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(out)?;
    let mut d = out.join(stem);
    let mut k = 1;
    while d.exists() {
        k += 1;
        d = out.join(format!("{stem}-{k}"));
    }
    std::fs::create_dir(&d)?;
    Ok(d)
}

fn cmd_run(args: &[String]) -> Result<bool> {
    let arm = opt(args, "--arm").ok_or_else(|| anyhow!("--arm required"))?;
    let factory = arms::get(&arm)
        .ok_or_else(|| anyhow!("unknown arm {arm} (known: {})", arms::ARMS.join(" ")))?;
    let sc_path =
        PathBuf::from(opt(args, "--scenario").ok_or_else(|| anyhow!("--scenario required"))?);
    let condition = opt(args, "--condition").unwrap_or_else(|| "dev".into());
    let out = PathBuf::from(opt(args, "--out").unwrap_or_else(|| "results".into()));
    let raw = std::fs::read_to_string(&sc_path)?;
    let scenario_hash = meta::sha256_hex(raw.as_bytes());
    let base = Scenario::load(&sc_path)?.with_overrides(
        opt(args, "--samples").and_then(|v| v.parse().ok()),
        opt(args, "--warmup").and_then(|v| v.parse().ok()),
        opt(args, "--leg-s").and_then(|v| v.parse().ok()),
        opt(args, "--duration-s").and_then(|v| v.parse().ok()),
    );
    let mut base = base;
    if let Some(r) = opt(args, "--runs").and_then(|v| v.parse().ok()) {
        base.runs = r;
    }
    if let Some(c) = opt(args, "--pin").and_then(|v| v.parse().ok()) {
        base.pin_writer = Some(c);
    }
    if let Some(s) = opt(args, "--timerslack-ns").and_then(|v| v.parse().ok()) {
        base.timerslack_ns = Some(s);
    }
    let kappa = opt(args, "--kappa").and_then(|k| k.parse().ok());
    let watchdog_cli: Option<u64> = opt(args, "--watchdog-s").and_then(|v| v.parse().ok());
    let label = opt(args, "--label");
    let mut all_ok = true;
    for sc in base.expand() {
        let stem = format!("{}-{}-{}-{}", meta::utc_stamp(), arm, condition, sc.name);
        let dir = unique_dir(&out, &stem)?;
        let watchdog_s = watchdog_cli.unwrap_or_else(|| sc.default_watchdog_s());
        all_ok &= run_one(
            &sc,
            &arm,
            factory,
            &condition,
            &dir,
            kappa,
            watchdog_s,
            &scenario_hash,
            label.as_deref(),
            flag(args, "--verbose"),
        )?;
        eprintln!("result: {}", dir.display());
    }
    Ok(all_ok)
}

#[allow(clippy::too_many_arguments)]
fn run_one(
    sc: &Scenario,
    arm: &str,
    factory: arms::Factory,
    condition: &str,
    dir: &Path,
    kappa: Option<f64>,
    watchdog_s: u64,
    scenario_hash: &str,
    label: Option<&str>,
    verbose: bool,
) -> Result<bool> {
    let before = meta::host_snap();
    let started = meta::utc_stamp();
    let mut sum = Summary::new(&sc.name, &sc.kind, arm, condition);
    // The 10-minute watchdog: a hung run is recorded as failed and kept, never retried silently.
    let done = Arc::new(AtomicBool::new(false));
    {
        let (done, dir, mut w) = (
            done.clone(),
            dir.to_path_buf(),
            Summary::new(&sc.name, &sc.kind, arm, condition),
        );
        std::thread::spawn(move || {
            let end = std::time::Instant::now() + std::time::Duration::from_secs(watchdog_s);
            while std::time::Instant::now() < end {
                if done.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            w.failed = Some(format!("killed by the {watchdog_s} s watchdog"));
            let _ = w.write(&dir);
            std::process::exit(124);
        });
    }
    let env = RunEnv {
        sc,
        arm,
        factory,
        dir,
        kappa,
    };
    if verbose {
        eprintln!("running {} on arm {arm} ({})", sc.name, sc.kind);
    }
    let r = match sc.kind.as_str() {
        "focus" | "smoke" | "flap" => focus::run_focus(&env, &mut sum),
        "keypress" => focus::run_keypress(&env, &mut sum),
        "k1k2" => focus::run_k1k2(&env, &mut sum),
        "a5" => focus::run_a5(&env, &mut sum),
        "pf1" => focus::run_pf1(&env, &mut sum),
        "storm" => storm::run_storm(&env, &mut sum),
        "memory" => micro::run_memory(&env, &mut sum),
        "wq1" => micro::run_wq1(&env, &mut sum),
        "wq2" => micro::run_wq2(&env, &mut sum),
        "wq3" => micro::run_wq3(&env, &mut sum),
        "wq6-wake" => micro::run_wq6_wake(&env, &mut sum),
        "wq6-rows" => micro::run_wq6_rows(&env, &mut sum),
        "mq2" => micro::run_mq2(&env, &mut sum),
        "pf6" => micro::run_pf6(&env, &mut sum),
        k => Err(anyhow!("unknown scenario kind {k}")),
    };
    done.store(true, Ordering::Release);
    let after = meta::host_snap();
    if condition == "quiet" && (!before.is_quiet() || !after.is_quiet()) {
        sum.quiet_violated = true;
        sum.notes.push("quiet-violated: loadavg1 >= 1.5, PSI cpu >= 1.0 or governor != performance at the start or the end of the run; kept and labelled".into());
    }
    let ok = r.is_ok();
    if let Err(e) = r {
        sum.failed = Some(format!("{e:#}"));
        eprintln!("run failed: {e:#}");
    }
    sum.notes.push("The writer thread busy-polls its mailbox while idle (harness choice); open-loop schedule, latency from the scheduled instant".into());
    if let Some(l) = label {
        sum.notes.push(format!("label: {l}"));
    }
    if sc.pin_writer.is_some() {
        sum.notes
            .push("DIAGNOSTIC variant: writer pinned (not a gating run)".into());
    }
    let mut meta = json!({
        "arm": arm, "condition": condition, "scenario": sc.name, "kind": sc.kind, "seed": sc.seed,
        "scenario_hash_sha256": scenario_hash, "started_utc": started, "label": label,
        "scenario": serde_json::to_value(sc)?,
        "host": meta::host_facts(), "versions": meta::versions(),
        "before": before.to_json(), "after": after.to_json(),
        "governors_before": before.governors, "governors_after": after.governors,
        "clock_read_overhead_ns": m18a0_common::timing::clock_overhead_ns(200_000),
        "wait_mechanism_note": "see config.describe of the arm in summary.counters",
        "tokio_runtime_threads": "n/a (harness writer and generators are std threads)",
        "main_thread_timerslack_ns": m18a0_common::timing::get_timerslack(),
        "writer_pinned": sc.pin_writer,
        "dir": dir.display().to_string(),
    });
    if let Some(f) = sum.counters.get("fixture") {
        meta["fixture"] = f.clone();
    }
    std::fs::write(dir.join("meta.json"), serde_json::to_string_pretty(&meta)?)?;
    sum.write(dir)?;
    Ok(ok)
}
