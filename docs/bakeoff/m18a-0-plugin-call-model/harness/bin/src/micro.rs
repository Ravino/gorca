// SPDX-License-Identifier: MIT OR Apache-2.0
//! Microbenchmarks and correctness scenarios: memory (M7), wq1, wq2, wq3, wq6-wake, wq6-rows,
//! mq2, pf6, and the harness self-test (SPEC 7 requirement 5).

use crate::arms::Factory;
use crate::focus::*;
use anyhow::{anyhow, Result};
use m18a0_common::budget::Grant;
use m18a0_common::corpora::*;
use m18a0_common::fixture::*;
use m18a0_common::host::*;
use m18a0_common::memstat::*;
use m18a0_common::model::*;
use m18a0_common::runner::*;
use m18a0_common::scenario::Scenario;
use m18a0_common::summary::*;
use m18a0_common::timing::*;
use m18a0_common::wake;
use serde_json::{json, Value};
use std::sync::Arc;

// ---- M7 memory ------------------------------------------------------------------------------------

/// Child process body: load `count` plugins in `mode` and print one JSON line of snapshots.
pub fn memory_child(factory: Factory, count: usize, mode: &str, kappa: f64) -> Result<Value> {
    let he = HostEngine::new(&EngineCfg {
        cache_dir: cache_dir(),
        pooling: true,
    })
    .map_err(|e| anyhow!("{e}"))?;
    let gd = guests_dir();
    let s0 = snap();
    let distinct = [
        "signal-p2",
        "dummy-1",
        "dummy-2",
        "dummy-3",
        "dummy-4",
        "dummy-5",
        "dummy-6",
        "dummy-7",
        "dummy-8",
        "dummy-9",
        "a5-cold-load",
        "a7-patch-match",
        "a8-refresh-each",
        "a12-self",
    ];
    let mut loaded: Vec<Arc<LoadedPlugin>> = Vec::new();
    if mode == "same" {
        let lp = Arc::new(
            he.load("dummy-2", &gd.join("dummy-2.wasm"))
                .map_err(|e| anyhow!("{e}"))?,
        );
        for _ in 0..count {
            loaded.push(lp.clone());
        }
    } else {
        for i in 0..count {
            let n = distinct[i % distinct.len()];
            loaded.push(Arc::new(
                he.load(n, &gd.join(format!("{n}.wasm")))
                    .map_err(|e| anyhow!("{e}"))?,
            ));
        }
    }
    let s_loaded = snap();
    let mut rts: Vec<PluginRuntime> = Vec::new();
    for (i, lp) in loaded.iter().enumerate() {
        let mut rt = PluginRuntime::new(i as PluginId, he.clone(), lp.clone());
        rt.warm().map_err(|e| anyhow!("{e}"))?;
        rts.push(rt);
    }
    let s_idle = snap();
    let table = NodeTable::build(512, 3);
    let chain: Vec<NodeId> = (2..6).collect();
    let rows = FrozenRowsFactory { table: &table }.build(&chain);
    let calls = (1000 / count.max(1)).max(1);
    for rt in rts.iter_mut() {
        for k in 0..calls {
            let b = PluginBatch {
                plugin: rt.plugin,
                job: k as u64,
                slot_epoch: 1,
                nodes: chain.clone(),
                interactive: true,
                grant: Grant {
                    fuel: (kappa * 150_000.0) as u64,
                    ns: 150_000,
                    capped: false,
                },
                created_ns: now_ns(),
            };
            let _ = rt.run(&b, rows.clone());
        }
    }
    let s_calls = snap();
    let runner = factory(rts)?;
    let slacks = runner.thread_timer_slacks();
    let s_runner = snap();
    let r16 = FrozenRowsFactory { table: &table }.build(&(2..18).collect::<Vec<_>>());
    let r64 = FrozenRowsFactory { table: &table }.build(&(2..66).collect::<Vec<_>>());
    let out = json!({"count": count, "mode": mode, "s0": s0, "loaded": s_loaded, "idle": s_idle, "after_calls": s_calls,
        "with_runner": s_runner, "runner": runner.describe(), "runner_threads_slack": slacks,
        "frozen_rows_bytes_16": r16.approx_bytes(), "frozen_rows_bytes_64": r64.approx_bytes(),
        "calls_per_plugin": calls});
    drop(runner);
    Ok(out)
}

pub fn run_memory(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let counts: Vec<u32> = if env.sc.counts.is_empty() {
        vec![1, 10, 50]
    } else {
        env.sc.counts.clone()
    };
    let exe = std::env::current_exe()?;
    let kappa = {
        let fx = Fixture::build(&["dummy-1".to_string()], 1, env.kappa)?;
        fx.kappa
    };
    let mut rows = Vec::new();
    for mode in ["distinct", "same"] {
        let mut pts_idle = Vec::new();
        let mut pts_calls = Vec::new();
        let mut pts_runner = Vec::new();
        for &n in &counts {
            let o = std::process::Command::new(&exe)
                .args([
                    "memory-child",
                    "--arm",
                    env.arm,
                    "--count",
                    &n.to_string(),
                    "--mode",
                    mode,
                    "--kappa",
                    &kappa.to_string(),
                ])
                .output()?;
            if !o.status.success() {
                return Err(anyhow!(
                    "memory child failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                ));
            }
            let line = String::from_utf8_lossy(&o.stdout)
                .lines()
                .last()
                .unwrap_or("")
                .to_string();
            let v: Value = serde_json::from_str(&line)?;
            let g = |k: &str, f: &str| v[k][f].as_f64().unwrap_or(0.0);
            let base = g("s0", "rss_kb");
            let per = |k: &str| (g(k, "rss_kb") - base) / n as f64;
            sum.metric(
                &format!("m7_{mode}_{n}_rss_per_plugin_idle_kb"),
                per("idle"),
            );
            sum.metric(
                &format!("m7_{mode}_{n}_rss_per_plugin_after_calls_kb"),
                per("after_calls"),
            );
            sum.metric(
                &format!("m7_{mode}_{n}_rss_per_plugin_with_runner_kb"),
                per("with_runner"),
            );
            sum.metric(
                &format!("m7_{mode}_{n}_pss_total_kb"),
                g("with_runner", "pss_kb") - g("s0", "pss_kb"),
            );
            sum.metric(
                &format!("m7_{mode}_{n}_rss_total_kb"),
                g("with_runner", "rss_kb") - base,
            );
            sum.metric(
                &format!("m7_{mode}_{n}_vmhwm_kb"),
                g("with_runner", "vm_hwm_kb"),
            );
            sum.metric(
                &format!("m7_{mode}_{n}_vm_size_delta_kb"),
                g("with_runner", "vm_size_kb") - g("s0", "vm_size_kb"),
            );
            sum.metric(
                &format!("m7_{mode}_{n}_threads"),
                g("with_runner", "threads"),
            );
            pts_idle.push((n as f64, g("idle", "rss_kb") - base));
            pts_calls.push((n as f64, g("after_calls", "rss_kb") - base));
            pts_runner.push((n as f64, g("with_runner", "rss_kb") - base));
            sum.metric(
                "frozen_rows_bytes_16",
                v["frozen_rows_bytes_16"].as_f64().unwrap_or(0.0),
            );
            sum.metric(
                "frozen_rows_bytes_64",
                v["frozen_rows_bytes_64"].as_f64().unwrap_or(0.0),
            );
            rows.push(v);
        }
        sum.metric(
            &format!("m7_{mode}_fit_slope_idle_kb_per_plugin"),
            slope(&pts_idle),
        );
        sum.metric(
            &format!("m7_{mode}_fit_slope_after_calls_kb_per_plugin"),
            slope(&pts_calls),
        );
        sum.metric(
            &format!("m7_{mode}_fit_slope_with_runner_kb_per_plugin"),
            slope(&pts_runner),
        );
    }
    if let Some(n50) = sum.metrics.get("m7_distinct_50_rss_total_kb").copied() {
        sum.metric("m7_50_plugins_rss_mb", n50 / 1024.0);
    }
    sum.counters = json!({ "children": rows });
    sum.notes.push("RSS is process RSS minus the RSS before any plugin was loaded, one child process per (mode, N); 50 plugins x 2 lanes is read as 2 x the 50-plugin total".into());
    Ok(())
}

// ---- WQ-1 .. WQ-3, WQ-6 ----------------------------------------------------------------------------

pub fn run_wq1(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let names: Vec<String> = ["signal-p2", "dummy-2", "dummy-3", "dummy-4", "dummy-5"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let fx = Fixture::build(&names, 1, env.kappa)?;
    let table = NodeTable::build(4096, 1);
    let nodes: Vec<NodeId> = table
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.role == ROLE_ROW)
        .map(|(i, _)| i as NodeId)
        .take(64)
        .collect();
    let mut rts = fx.runtimes()?;
    let iters = env.sc.samples.max(500) as usize;
    for rt in rts.iter_mut() {
        let name = rt.name().to_string();
        let (mut exec, mut build, mut fuel) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..iters + 200 {
            let t = now_ns();
            let rows = FrozenRowsFactory { table: &table }.build(&nodes);
            let tb = now_ns() - t;
            let b = PluginBatch {
                plugin: rt.plugin,
                job: i as u64,
                slot_epoch: 1,
                nodes: nodes.clone(),
                interactive: true,
                grant: Grant {
                    fuel: 50_000_000,
                    ns: 25_000_000,
                    capped: false,
                },
                created_ns: now_ns(),
            };
            if let PluginReply::Done(d) = rt.run(&b, rows) {
                if i >= 200 {
                    exec.push(sat32(d.exec_ns));
                    fuel.push(sat32(d.fuel_used));
                    build.push(sat32(tb));
                }
            }
        }
        let (pe, pf, pb) = (percentiles(&exec), percentiles(&fuel), percentiles(&build));
        sum.metric(&format!("wq1_{name}_64nodes_exec_p50_ns"), pe.p50 as f64);
        sum.metric(&format!("wq1_{name}_64nodes_exec_p99_ns"), pe.p99 as f64);
        sum.metric(&format!("wq1_{name}_64nodes_exec_max_ns"), pe.max as f64);
        sum.metric(&format!("wq1_{name}_64nodes_fuel_p50"), pf.p50 as f64);
        sum.metric(&format!("wq1_{name}_64nodes_fuel_p99"), pf.p99 as f64);
        sum.metric(&format!("wq1_{name}_rows_build_64_p50_ns"), pb.p50 as f64);
        sum.series.insert(format!("wq1_{name}_exec"), pe);
    }
    Ok(())
}

fn cpu_list() -> Vec<usize> {
    let mut v = Vec::new();
    // SAFETY: sched_getaffinity into a zeroed cpu_set_t.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
            for c in 0..1024 {
                if libc::CPU_ISSET(c, &set) {
                    v.push(c);
                }
            }
        }
    }
    v
}

/// WQ-2: kappa_int / kappa_bg per CPU and per core class (class = rank of `cpuinfo_max_freq`).
pub fn run_wq2(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let he = HostEngine::new(&EngineCfg {
        cache_dir: cache_dir(),
        pooling: true,
    })
    .map_err(|e| anyhow!("{e}"))?;
    let spin = guests_dir().join("a6-full-grant.wasm");
    let rounds = env.sc.runs.max(15) as u32;
    let mut table = Vec::new();
    let mut by_class: std::collections::BTreeMap<u64, Vec<f64>> = Default::default();
    for cpu in cpu_list() {
        let h = {
            let he = he.clone();
            let spin = spin.clone();
            std::thread::spawn(move || -> Result<Vec<f64>> {
                pin_to_cpu(cpu).map_err(|e| anyhow!("{e}"))?;
                calibrate_kappa(&he, &spin, rounds).map_err(|e| anyhow!("{e}"))
            })
        };
        let mut k = h.join().map_err(|_| anyhow!("calib panicked"))??;
        k.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let (int, bg_p10) = (k[k.len() / 2], k[k.len() / 10]);
        let maxf: u64 = read_trim(&format!(
            "/sys/devices/system/cpu/cpu{cpu}/cpufreq/cpuinfo_max_freq"
        ))
        .parse()
        .unwrap_or(0);
        sum.metric(&format!("wq2_cpu{cpu}_kappa_int"), int);
        sum.metric(&format!("wq2_cpu{cpu}_kappa_p10"), bg_p10);
        by_class.entry(maxf).or_default().push(int);
        table.push(json!({"cpu": cpu, "max_freq_khz": maxf, "kappa_int_median": int, "kappa_p10": bg_p10, "min": k[0], "max": k[k.len()-1]}));
    }
    let classes: Vec<Value> = by_class
        .iter()
        .rev()
        .enumerate()
        .map(|(i, (f, v))| {
            let mut s = v.clone();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            sum.metric(&format!("wq2_class{i}_kappa_int"), s[s.len() / 2]);
            json!({"class_rank": i, "max_freq_khz": f, "cpus": v.len(), "kappa_int_median": s[s.len() / 2], "kappa_min": s[0]})
        })
        .collect();
    sum.counters = json!({"per_cpu": table, "classes": classes});
    Ok(())
}

/// WQ-3: cost of import pre-charging, the 64th-import watchdog, Instant::now pairs, handle MAC.
pub fn run_wq3(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let names: Vec<String> = ["dummy-4", "a13-subtree-loop", "a6-full-grant"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let fx = Fixture::build(&names, 1, env.kappa)?;
    let table = NodeTable::build(512, 1);
    let nodes: Vec<NodeId> = (2..6).collect();
    let rows = FrozenRowsFactory { table: &table }.build(&nodes);
    let mut rts = fx.runtimes()?;
    // (a) cost per import through the real host path: dummy-4 does 20 model calls per node.
    let (mut per_import, mut execs) = (Vec::new(), Vec::new());
    for i in 0..3000u64 {
        let b = PluginBatch {
            plugin: 0,
            job: i,
            slot_epoch: 1,
            nodes: nodes.clone(),
            interactive: true,
            grant: Grant {
                fuel: 10_000_000,
                ns: 5_000_000,
                capped: false,
            },
            created_ns: now_ns(),
        };
        if let PluginReply::Done(d) = rts[0].run(&b, rows.clone()) {
            if i >= 300 && d.imports > 0 {
                per_import.push(sat32(d.exec_ns / d.imports as u64));
                execs.push(sat32(d.exec_ns));
            }
        }
    }
    sum.series_metrics("wq3_per_import", &percentiles(&per_import));
    sum.series_metrics("wq3_dummy4_exec", &percentiles(&execs));
    // (b) the 64th-import watchdog: a subtree loop with a huge fuel and pre-charge allowance and a
    // 100 us grant time: the call must be stopped at max(4 x grant time, 200 us).
    let b = PluginBatch {
        plugin: 1,
        job: 1,
        slot_epoch: 1,
        nodes: nodes.clone(),
        interactive: true,
        grant: Grant {
            fuel: u64::MAX / 4,
            ns: 100_000,
            capped: false,
        },
        created_ns: now_ns(),
    };
    let mut wd = Vec::new();
    for _ in 0..50 {
        if let PluginReply::Fault(f) = rts[1].run(&b, rows.clone()) {
            if f.kind == FaultKind::Watchdog {
                wd.push(sat32(f.exec_ns));
            }
        }
    }
    sum.metric("wq3_watchdog_trips_of_50", wd.len() as f64);
    sum.series_metrics("wq3_watchdog_exec", &percentiles(&wd));
    // (c) pre-charge: the same loop with the real small grant is stopped by the pre-charge itself.
    let b = PluginBatch {
        plugin: 1,
        job: 2,
        slot_epoch: 1,
        nodes: nodes.clone(),
        interactive: true,
        grant: Grant {
            fuel: 60_000,
            ns: 30_000,
            capped: false,
        },
        created_ns: now_ns(),
    };
    let mut pc = Vec::new();
    for _ in 0..50 {
        if let PluginReply::Fault(f) = rts[1].run(&b, rows.clone()) {
            pc.push((f.kind, sat32(f.exec_ns)));
        }
    }
    sum.metric(
        "wq3_precharge_stops_of_50",
        pc.iter()
            .filter(|(k, _)| *k == FaultKind::PrechargeExhausted)
            .count() as f64,
    );
    sum.series_metrics(
        "wq3_precharge_stop_exec",
        &percentiles(&pc.iter().map(|x| x.1).collect::<Vec<_>>()),
    );
    // (d) Instant::now pair, and handle MAC with a valid and a forged handle.
    sum.metric("wq3_clock_read_ns", clock_overhead_ns(1_000_000));
    let key = 0xA5A5_5A5A_1234_0001u64;
    let n = 5_000_000u64;
    let t = now_ns();
    let mut acc = 0u64;
    for i in 0..n {
        acc ^= mint_handle(key, 1, (i & 0xFFFF) as u32);
    }
    let valid = (now_ns() - t) as f64 / n as f64;
    let t = now_ns();
    for i in 0..n {
        acc ^= mint_handle(key ^ 1, 1, (i & 0xFFFF) as u32) ^ 1;
    }
    let forged = (now_ns() - t) as f64 / n as f64;
    std::hint::black_box(acc);
    sum.metric("wq3_mac_valid_ns", valid);
    sum.metric("wq3_mac_forged_ns", forged);
    Ok(())
}

/// WQ-6 mechanism probes: independent of any arm.
pub fn run_wq6_wake(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let n = env.sc.samples.max(1000) as usize;
    let mut table = Vec::new();
    for (mech, slack, wait_us) in [
        (wake::Mechanism::Futex, Some(1u64), 100u64),
        (wake::Mechanism::Futex, None, 100),
        (wake::Mechanism::TimerFd, Some(1), 100),
        (wake::Mechanism::TimerFd, None, 100),
        (wake::Mechanism::NanoSleep, Some(1), 100),
        (wake::Mechanism::NanoSleep, None, 100),
        (wake::Mechanism::Futex, Some(1), 300),
        (wake::Mechanism::Futex, None, 300),
        (wake::Mechanism::TimerFd, Some(1), 300),
        (wake::Mechanism::TimerFd, None, 300),
    ] {
        let v = wake::lateness(mech, n, wait_us, slack);
        let p = percentiles(&v);
        let key = format!(
            "wq6_late_{}_{}us_slack{}",
            mech.name(),
            wait_us,
            slack.map(|s| s.to_string()).unwrap_or("default".into())
        );
        sum.series_metrics(&key, &p);
        table.push(json!({"mech": mech.name(), "wait_us": wait_us, "slack_ns": slack, "p50": p.p50, "p99": p.p99, "max": p.max}));
    }
    for spin_us in [0u64, 10, 20] {
        for slack in [Some(1u64), None] {
            let v = wake::roundtrip(n, spin_us * 1000, slack, 50);
            let p = percentiles(&v);
            sum.series_metrics(
                &format!(
                    "wq6_roundtrip_spin{spin_us}us_slack{}",
                    slack.map(|s| s.to_string()).unwrap_or("default".into())
                ),
                &p,
            );
            table.push(json!({"roundtrip_spin_us": spin_us, "slack_ns": slack, "p50": p.p50, "p99": p.p99, "max": p.max}));
        }
    }
    sum.counters = json!({ "table": table });
    sum.notes.push("Mechanism probes on an unpinned thread; pool size, P/E core and loaded-host variants are the candidates' M9 runs.".into());
    Ok(())
}

/// WQ-6: FrozenRows build cost at 16 and 64 nodes.
pub fn run_wq6_rows(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let table = NodeTable::build(4096, env.sc.seed);
    for k in [16usize, 64] {
        let nodes: Vec<NodeId> = (2..(2 + k) as u32).collect();
        let mut v = Vec::new();
        let mut bytes = 0;
        for i in 0..(env.sc.samples.max(2000) as usize + 200) {
            let t = now_ns();
            let r = FrozenRowsFactory { table: &table }.build(&nodes);
            let d = now_ns() - t;
            if i >= 200 {
                v.push(sat32(d));
            }
            bytes = r.approx_bytes();
        }
        sum.series_metrics(&format!("wq6_rows_build_{k}"), &percentiles(&v));
        sum.metric(&format!("wq6_rows_bytes_{k}"), bytes as f64);
    }
    Ok(())
}

// ---- correctness corpora -----------------------------------------------------------------------------

pub fn run_mq2(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let runs = if env.sc.runs > 0 { env.sc.runs } else { 12_000 };
    let r = mq2_replay(runs, env.sc.seed);
    sum.metric("mq2_runs", r.runs as f64);
    sum.metric("mq2_false_accepts", r.false_accepts as f64);
    sum.metric("mq2_false_rejects", r.false_rejects as f64);
    sum.metric("mq2_accepted", r.accepted as f64);
    sum.metric("mq2_stale_dropped", r.stale_dropped as f64);
    sum.metric("mq2_scope_wide_runs", r.scope_wide_runs as f64);
    sum.metric("mq2_bind_gen_runs", r.bind_gen_runs as f64);
    sum.counters = serde_json::to_value(&r)?;
    sum.notes.push("Admission path is the shared StubWriter::admit; delivery delay is modelled as a random number of S0 mutations between snapshot and delivery.".into());
    Ok(())
}

pub fn run_pf6(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let runs = if env.sc.runs > 0 { env.sc.runs } else { 5_000 };
    let r = pf6_permutations(runs, env.sc.seed);
    sum.metric("pf6_runs", r.runs as f64);
    sum.metric("pf6_order_dependent", r.order_dependent as f64);
    sum.metric("pf6_permutations", r.permutations as f64);
    Ok(())
}

// ---- self-test ---------------------------------------------------------------------------------------

fn slow(r: Vec<PluginRuntime>) -> crate::arms::Built {
    Ok(Box::new(SlowRunner {
        inner: RefInlineRunner::new(r),
        delay_us: 1000,
    }))
}
fn ignore_deadline(r: Vec<PluginRuntime>) -> crate::arms::Built {
    Ok(Box::new(IgnoreDeadlineRunner {
        inner: RefInlineRunner::new(r),
        overshoot_us: 400,
    }))
}
fn stale(r: Vec<PluginRuntime>) -> crate::arms::Built {
    Ok(Box::new(StaleEpochInjector {
        inner: RefInlineRunner::new(r),
    }))
}

/// A harness that cannot fail a bad runner is not accepted: returns the list of expectation
/// results (name, ok, detail).
pub fn selftest(sc: &Scenario, kappa: Option<f64>) -> Result<Vec<(String, bool, String)>> {
    let gates: GateFile = toml::from_str(include_str!("../../report/gates.toml"))?;
    let names = plugin_names("benign", &[]);
    let fx = Fixture::build(&names, 1, kappa)?;
    let mut cfg = LegCfg::from(sc, true);
    cfg.samples = 2500;
    cfg.warmup = 300;
    cfg.interval_us = 400;
    cfg.nodes = 1024;
    let mut res: Vec<(String, bool, String)> = Vec::new();
    let mk = |name: &str, f: Factory, on: bool| -> Result<(Summary, LegOut)> {
        let mut c = cfg.clone();
        c.on = on;
        let out = run_leg(&fx, f, &c)?;
        let mut s = Summary::new("benign", "focus", "bprime", "quiet");
        focus_metrics(&mut s, &out, true);
        s.notes.push(format!("selftest runner {name}"));
        Ok((s, out))
    };
    let verdict = |s: Summary| -> Vec<Verdict> {
        evaluate(
            &gates,
            &[RunDir {
                path: Default::default(),
                summary: s,
            }],
        )
    };
    let m1 = |v: &[Verdict]| v.iter().find(|x| x.gate == "M1").and_then(|x| x.pass);

    // 1. NoopRunner: stall p99 under 10 us (plugins off in effect).
    let (s, _) = mk(
        "noop",
        crate::arms::get("noop").ok_or_else(|| anyhow!("noop"))?,
        true,
    )?;
    let p99 = s.metrics.get("stall_p99_ns").copied().unwrap_or(f64::NAN);
    res.push((
        "noop runner: stall p99 < 10 us".into(),
        p99 < 10_000.0,
        format!("p99 = {p99} ns"),
    ));
    // 2. A runner that sleeps 1 ms must fail M1.
    let (s, _) = mk("slow-1ms", slow, true)?;
    let v = verdict(s);
    res.push((
        "sleep(1 ms) runner fails M1".into(),
        m1(&v) == Some(false),
        format!("{:?}", m1(&v)),
    ));
    // 3. A runner that ignores the deadline must fail M1.
    let (s, _) = mk("ignore-deadline", ignore_deadline, true)?;
    let v = verdict(s);
    res.push((
        "deadline-ignoring runner fails M1".into(),
        m1(&v) == Some(false),
        format!("{:?}", m1(&v)),
    ));
    // 4. A stale-epoch reply injector must show the drop counter and never get a fact admitted.
    let (s, out) = mk("stale-epoch", stale, true)?;
    let dropped = s.metrics.get("stale_epoch_dropped").copied().unwrap_or(0.0);
    res.push((
        "stale-epoch injector shows the drop counter".into(),
        dropped > 1000.0,
        format!("dropped = {dropped}"),
    ));
    let any_666 = out.plugins.iter().any(|p| p.1.stale_epoch_dropped == 0);
    res.push((
        "every plugin saw stale replies dropped".into(),
        !any_666 || out.plugins.is_empty(),
        String::new(),
    ));
    // 5. Timer slack: M1 is refused (NOT MEASURED) when the slack check is false.
    let mut s = Summary::new("benign", "focus", "bprime", "quiet");
    s.metric("stall_max_ns", 1000.0);
    s.check("writer_timerslack_1ns", false);
    s.check("pool_timerslack_1ns", true);
    let v = verdict(s);
    res.push((
        "M1 is refused when timer slack is not 1 ns".into(),
        m1(&v).is_none(),
        format!("{:?}", m1(&v)),
    ));
    // 6. The reference runner is allowed to pass or fail M1 (host dependent); it must run end to end.
    let (s, _) = mk(
        "ref-inline",
        crate::arms::get("ref").ok_or_else(|| anyhow!("ref"))?,
        true,
    )?;
    let ok = s.metrics.get("m3_overall").copied().unwrap_or(0.0) > 0.9;
    res.push((
        "reference runner end to end: facts at first utterance > 90 %".into(),
        ok,
        format!("m3 = {:?}", s.metrics.get("m3_overall")),
    ));
    Ok(res)
}
