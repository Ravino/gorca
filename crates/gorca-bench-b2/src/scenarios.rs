//! The benchmark scenarios T1..T6 and the fixture self-test (SPEC section 6).

use std::io::{Read, Write};
use std::time::Duration;

use anyhow::Result;

use crate::bus::{Bus, BusKind};
use crate::feeder::{AppPlan, ProbePlan};
use crate::limits as L;
use crate::mockbus::MockOpts;
use crate::report::Row;
use crate::rig::ConsumerCfg;
use crate::runner::{AppSpec, RunOutcome, RunSpec, execute};
use crate::stats::{spread, summarize};
use crate::workload::Mix;

#[derive(Clone, Debug)]
pub struct Ctx {
    pub quick: bool,
    pub repeats: usize,
    /// Real buses to include in the bus-sensitive scenarios: any of `daemon`, `broker`.
    pub real_buses: Vec<String>,
    /// An existing bus address (inside `run-isolated.sh`); used as the `existing` bus.
    pub existing: Option<String>,
}

impl Ctx {
    fn secs(&self, full: u64, quick: u64) -> Duration {
        Duration::from_secs(if self.quick { quick } else { full })
    }

    fn reps(&self) -> usize {
        if self.quick { 1 } else { self.repeats.max(1) }
    }

    fn mock(&self) -> BusKind {
        BusKind::Mock(MockOpts {
            quota: L::MOCK_QUOTA_BYTES,
            ..MockOpts::default()
        })
    }

    fn real(&self) -> Vec<BusKind> {
        let mut v = Vec::new();
        for b in &self.real_buses {
            match b.as_str() {
                "daemon" => v.push(BusKind::Daemon {
                    max_outgoing: 1_000_000_000,
                }),
                "broker" => v.push(BusKind::Broker {
                    pool_bytes: L::BROKER_POOL_BYTES,
                }),
                _ => {}
            }
        }
        if let Some(a) = &self.existing {
            v.push(BusKind::Existing(a.clone()));
        }
        v
    }
}

fn app(role: &str, seed: u64, mix: Mix, rate: f64) -> AppSpec {
    let mut plan = AppPlan::new(role, seed, mix, rate);
    plan.burst = if rate <= 0.0 {
        256
    } else {
        ((rate / 2000.0) as usize).clamp(16, 1024)
    };
    AppSpec {
        plan,
        cons: ConsumerCfg::default(),
    }
}

fn with_probes(mut a: AppSpec, period: Duration, total: Duration) -> AppSpec {
    a.plan.probe = Some(ProbePlan { period });
    a.cons.probes = (total.as_nanos() / period.as_nanos().max(1)) as u32 + 64;
    a
}

fn backlog_p99(o: &RunOutcome) -> f64 {
    let v: Vec<f64> = o
        .backlog_samples
        .iter()
        .filter(|(t, _)| *t > 1.0)
        .map(|(_, b)| *b as f64)
        .collect();
    summarize(&v).p99
}

fn lat_all(o: &RunOutcome, role: &str) -> Vec<f64> {
    o.app(role)
        .map(|a| a.probe_lat_ns.clone())
        .unwrap_or_default()
}

fn rss_delta_mib(o: &RunOutcome) -> f64 {
    o.rss_peak.saturating_sub(o.rss_base) as f64 / (1024.0 * 1024.0)
}

fn survived(o: &RunOutcome) -> bool {
    !o.disconnected() && o.settled
}

// ---- T1: sustained throughput ----------------------------------------------------------------

pub fn t1(ctx: &Ctx) -> Result<Vec<Row>> {
    let ladder = [
        25_000.0,
        50_000.0,
        100_000.0,
        200_000.0,
        400_000.0,
        800_000.0,
        1_600_000.0,
    ];
    let step = ctx.secs(6, 2);
    let (mut best, mut backlog, mut lag, mut stops) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..ctx.reps() {
        let mut top = 0.0;
        let (mut bl, mut lg) = (0.0, 0.0);
        let mut why = String::from("ladder exhausted");
        for &rate in &ladder {
            let mut spec = RunSpec::new(ctx.mock(), step);
            spec.settle = Duration::from_secs(5);
            spec.apps.push(with_probes(
                app("hot", 1, Mix::Fakeapp, rate),
                Duration::from_millis(5),
                step,
            ));
            let o = execute(&spec)?;
            let a = &o.apps[0];
            let achieved = a.feed.achieved_rate();
            let b99 = backlog_p99(&o);
            let l99 = summarize(&a.probe_lat_ns).p99 / 1e3;
            let reason = if o.disconnected() {
                Some(format!("disconnected at {rate:.0}/s"))
            } else if achieved < 0.98 * rate && b99 < 256.0 * 1024.0 {
                Some(format!(
                    "generator-limited at {rate:.0}/s (achieved {achieved:.0})"
                ))
            } else if achieved < 0.98 * rate {
                Some(format!(
                    "fell behind at {rate:.0}/s (achieved {achieved:.0})"
                ))
            } else if b99 > L::T1_BACKLOG_P99_MAX_BYTES {
                Some(format!(
                    "backlog p99 {:.0} KiB at {rate:.0}/s",
                    b99 / 1024.0
                ))
            } else if l99 > L::T1_LAG_P99_MAX_US {
                Some(format!("lag p99 {:.1} ms at {rate:.0}/s", l99 / 1e3))
            } else if !o.settled {
                Some(format!("fence lost at {rate:.0}/s"))
            } else {
                None
            };
            match reason {
                None => {
                    top = rate;
                    bl = b99;
                    lg = l99;
                }
                Some(r) => {
                    why = r;
                    break;
                }
            }
        }
        best.push(top);
        backlog.push(bl / 1024.0);
        lag.push(lg);
        stops.push(why);
    }
    let note = stops.first().cloned().unwrap_or_default();
    let mut rows = vec![
        Row::info("T1.sustained_eps", "T1", "mock", "events/s", &best)
            .at_least(L::T1_SUSTAINED_EPS_MIN)
            .note(note),
        Row::info(
            "T1.backlog_p99_kib",
            "T1",
            "mock",
            "KiB at the sustained rate",
            &backlog,
        ),
        Row::info(
            "T1.lag_p99_us",
            "T1",
            "mock",
            "us at the sustained rate",
            &lag,
        ),
    ];
    // Saturation: closed loop, the feeder stays 8 MiB ahead of the drain.
    let mut sat = Vec::new();
    for _ in 0..ctx.reps() {
        let mut spec = RunSpec::new(ctx.mock(), ctx.secs(5, 2));
        let mut a = app("sat", 2, Mix::Fakeapp, 0.0);
        a.plan.backlog_cap = Some(8 * 1024 * 1024);
        spec.apps.push(a);
        let o = execute(&spec)?;
        sat.push(o.drain.messages as f64 / o.elapsed_s);
    }
    rows.push(Row::info(
        "T1.saturation_eps",
        "T1",
        "mock",
        "messages/s",
        &sat,
    ));
    Ok(rows)
}

// ---- T2: drain latency -----------------------------------------------------------------------

pub fn t2(ctx: &Ctx) -> Result<Vec<Row>> {
    let dur = ctx.secs(20, 4);
    let period = Duration::from_micros(500);
    let mut rows = Vec::new();
    for (load, p99, p999) in [
        (0.0, L::T2_IDLE_P99_MAX_US, None),
        (10_000.0, L::T2_10K_P99_MAX_US, Some(L::T2_10K_P999_MAX_US)),
        (
            100_000.0,
            L::T2_100K_P99_MAX_US,
            Some(L::T2_100K_P999_MAX_US),
        ),
    ] {
        let tag = match load as u64 {
            0 => "idle",
            10_000 => "10k",
            _ => "100k",
        };
        let (mut q50, mut q95, mut q99, mut q999, mut qmax, mut n) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        for _ in 0..ctx.reps() {
            let mut spec = RunSpec::new(ctx.mock(), dur);
            if load > 0.0 {
                spec.apps.push(app("load", 3, Mix::Fakeapp, load));
            }
            spec.apps.push(with_probes(
                app("probe", 4, Mix::Realistic, 200.0),
                period,
                dur,
            ));
            let o = execute(&spec)?;
            let s = summarize(&lat_all(&o, "probe"));
            q50.push(s.p50 / 1e3);
            q95.push(s.p95 / 1e3);
            q99.push(s.p99 / 1e3);
            q999.push(s.p999 / 1e3);
            qmax.push(s.max / 1e3);
            n.push(s.n as f64);
        }
        let id = |m: &str| format!("T2.{m}_us@{tag}");
        rows.push(
            Row::info(&id("p50"), "T2", "mock", "us", &q50)
                .at_most(L::T2_P50_MAX_US)
                .note(format!("n={:.0}", spread(&n).median)),
        );
        rows.push(Row::info(&id("p95"), "T2", "mock", "us", &q95));
        rows.push(Row::info(&id("p99"), "T2", "mock", "us", &q99).at_most(p99));
        let r = Row::info(&id("p999"), "T2", "mock", "us", &q999);
        rows.push(match p999 {
            Some(t) => r.at_most(t),
            None => r,
        });
        rows.push(Row::info(&id("max"), "T2", "mock", "us", &qmax));
    }
    Ok(rows)
}

// ---- T3: focus latency under a 40k/s storm ---------------------------------------------------

pub fn t3(ctx: &Ctx) -> Result<Vec<Row>> {
    let dur = ctx.secs(20, 4);
    let period = Duration::from_millis(2);
    let mut rows = Vec::new();
    let mut buses = vec![ctx.mock()];
    buses.extend(ctx.real());
    for bus in buses {
        let label = bus.label();
        let mock = label == "mock";
        let (mut q50, mut q99, mut q999, mut qmax, mut unobs, mut ok) =
            (vec![], vec![], vec![], vec![], vec![], vec![]);
        for _ in 0..ctx.reps() {
            let mut spec = RunSpec::new(bus.clone(), dur);
            spec.apps.push(with_probes(
                app("focus", 5, Mix::Fakeapp, 10_000.0),
                period,
                dur,
            ));
            for i in 1..=3u64 {
                spec.apps
                    .push(app(&format!("bg{i}"), 5 + i, Mix::Fakeapp, 10_000.0));
            }
            let o = execute(&spec)?;
            let s = summarize(&lat_all(&o, "focus"));
            q50.push(s.p50 / 1e3);
            q99.push(s.p99 / 1e3);
            q999.push(s.p999 / 1e3);
            qmax.push(s.max / 1e3);
            unobs.push(f64::from(o.app("focus").map_or(0, |a| a.probes_unobserved)));
            ok.push(f64::from(u8::from(survived(&o))));
        }
        let r50 = Row::info("T3.focus_p50_us", "T3", label, "us", &q50);
        let r99 = Row::info("T3.focus_p99_us", "T3", label, "us", &q99);
        let r999 = Row::info("T3.focus_p999_us", "T3", label, "us", &q999);
        // Real buses add their own forwarding latency; thresholds apply to the mock only.
        rows.push(if mock {
            r50.at_most(L::T2_P50_MAX_US * 2.0)
        } else {
            r50
        });
        rows.push(if mock {
            r99.at_most(L::T3_P99_MAX_US)
        } else {
            r99
        });
        rows.push(if mock {
            r999.at_most(L::T3_P999_MAX_US)
        } else {
            r999
        });
        rows.push(Row::info("T3.focus_max_us", "T3", label, "us", &qmax));
        rows.push(Row::info("T3.focus_unobserved", "T3", label, "probes", &unobs).at_most(0.0));
        rows.push(Row::info("T3.survived", "T3", label, "1=yes", &ok).at_least(1.0));
    }
    Ok(rows)
}

// ---- T4: CPU ---------------------------------------------------------------------------------

pub fn t4(ctx: &Ctx) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    // Idle: nothing may spin.
    let mut idle = Vec::new();
    for _ in 0..ctx.reps() {
        let mut spec = RunSpec::new(ctx.mock(), ctx.secs(5, 2));
        let mut a = app("idle", 9, Mix::Fakeapp, 1.0);
        a.plan.max_events = Some(0);
        spec.apps.push(a);
        let o = execute(&spec)?;
        idle.push(o.cpu_ns as f64 / 1e6 / o.elapsed_s);
    }
    rows.push(Row::info("T4.idle_cpu_ms_per_s", "T4", "mock", "ms/s", &idle).at_most(10.0));
    // 10k ev/s over 5 apps (M-3).
    let (mut per10k, mut total) = (Vec::new(), Vec::new());
    for _ in 0..ctx.reps() {
        let mut spec = RunSpec::new(ctx.mock(), ctx.secs(15, 4));
        for i in 0..5u64 {
            spec.apps
                .push(app(&format!("a{i}"), 20 + i, Mix::Fakeapp, 2_000.0));
        }
        let o = execute(&spec)?;
        let ev = o.drain.messages as f64;
        per10k.push(o.cpu_ns as f64 / 1e6 / (ev / 10_000.0));
        total.push(o.cpu_ns as f64 / 1e6 / o.elapsed_s);
    }
    rows.push(
        Row::info(
            "T4.cpu_ms_per_10k@10k",
            "T4",
            "mock",
            "ms per 10k events",
            &per10k,
        )
        .at_most(L::T4_CPU_MS_PER_10K_MAX),
    );
    rows.push(Row::info(
        "T4.cpu_ms_per_s@10k",
        "T4",
        "mock",
        "ms/s",
        &total,
    ));
    // 100k ev/s on one app: drain thread occupancy (M-11).
    let (mut core, mut per10k_hot, mut hot_total) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..ctx.reps() {
        let mut spec = RunSpec::new(ctx.mock(), ctx.secs(8, 3));
        spec.apps.push(app("hot", 30, Mix::Fakeapp, 100_000.0));
        let o = execute(&spec)?;
        core.push(o.drain_cpu_ns as f64 / 1e9 / o.elapsed_s * 100.0);
        per10k_hot.push(o.cpu_ns as f64 / 1e6 / (o.drain.messages as f64 / 10_000.0));
        hot_total.push(o.cpu_ns as f64 / 1e9 / o.elapsed_s * 100.0);
    }
    rows.push(
        Row::info(
            "T4.drain_core_pct@100k",
            "T4",
            "mock",
            "% of one core",
            &core,
        )
        .at_most(L::T4_DRAIN_CORE_PCT_MAX),
    );
    rows.push(
        Row::info(
            "T4.cpu_ms_per_10k@100k",
            "T4",
            "mock",
            "ms per 10k events",
            &per10k_hot,
        )
        .at_most(L::T4_CPU_MS_PER_10K_MAX),
    );
    rows.push(Row::info(
        "T4.all_threads_core_pct@100k",
        "T4",
        "mock",
        "% of one core",
        &hot_total,
    ));
    Ok(rows)
}

// ---- T5: memory ------------------------------------------------------------------------------

fn stalled(mut a: AppSpec, from_s: u64, to_s: u64) -> AppSpec {
    a.cons.stall = Some((from_s * 1_000_000_000, to_s * 1_000_000_000));
    a
}

pub fn t5(ctx: &Ctx) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    let dur = ctx.secs(8, 3);
    // (a) every event a new key, consumer healthy.
    // (b) the app actor hangs for most of the run.
    // (c) the app actor hangs while lossless classes flood.
    type Build = Box<dyn Fn() -> AppSpec>;
    let cases: Vec<(&str, f64, Build, bool)> = vec![
        (
            "distinct",
            50_000.0,
            Box::new(|| stalled(app("m", 40, Mix::Distinct, 50_000.0), 1, 5)),
            true,
        ),
        (
            "stall",
            100_000.0,
            Box::new(|| stalled(app("m", 41, Mix::Realistic, 100_000.0), 1, 6)),
            true,
        ),
        (
            "stall_lossless",
            100_000.0,
            Box::new(|| {
                let mut a = stalled(app("m", 42, Mix::Lossless, 100_000.0), 1, 6);
                a.plan.slots = 1_000_000;
                a
            }),
            true,
        ),
    ];
    for (tag, _rate, build, verify) in cases {
        let (mut rss, mut pend, mut ok, mut viol, mut flooded) =
            (vec![], vec![], vec![], vec![], vec![]);
        let mut first_viol = String::new();
        for _ in 0..ctx.reps() {
            let mut spec = RunSpec::new(ctx.mock(), dur);
            let mut a = build();
            if verify {
                a.cons.fold = true;
                spec.verify = true;
            }
            spec.apps.push(a);
            let o = execute(&spec)?;
            rss.push(rss_delta_mib(&o));
            pend.push(o.apps[0].cons.max_batch as f64);
            ok.push(f64::from(u8::from(survived(&o))));
            flooded.push(o.apps[0].cons.flooded_batches as f64);
            if let Some(f) = o.apps[0]
                .verdict
                .as_ref()
                .and_then(|v| v.violations.first())
            {
                first_viol.clone_from(f);
            }
            viol.push(
                o.apps[0]
                    .verdict
                    .as_ref()
                    .map_or(0.0, |v| v.violations.len() as f64),
            );
        }
        rows.push(
            Row::info(
                &format!("T5.rss_delta_mib@{tag}"),
                "T5",
                "mock",
                "MiB",
                &rss,
            )
            .at_most(L::T5_RSS_DELTA_MAX_MIB),
        );
        rows.push(
            Row::info(
                &format!("T5.pending_max@{tag}"),
                "T5",
                "mock",
                "events in one batch",
                &pend,
            )
            .at_most(L::T5_PENDING_MAX),
        );
        rows.push(
            Row::info(&format!("T5.survived@{tag}"), "T5", "mock", "1=yes", &ok).at_least(1.0),
        );
        rows.push(Row::info(
            &format!("T5.flooded_batches@{tag}"),
            "T5",
            "mock",
            "batches",
            &flooded,
        ));
        if verify {
            rows.push(
                Row::info(&format!("C.violations@{tag}"), "T5", "mock", "count", &viol)
                    .at_most(0.0)
                    .note(first_viol.clone()),
            );
        }
    }
    Ok(rows)
}

// ---- T6: correctness at benchmark scale ------------------------------------------------------

pub fn t6(ctx: &Ctx) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    let dur = ctx.secs(5, 2);
    let mut cases: Vec<(BusKind, Mix, u32, f64)> = Vec::new();
    for mix in [Mix::Fakeapp, Mix::Terminal, Mix::Realistic, Mix::Lossless] {
        cases.push((ctx.mock(), mix, 300, 50_000.0));
    }
    cases.push((ctx.mock(), Mix::Realistic, 20_000, 50_000.0)); // overflows max_keys: Flooded
    cases.push((ctx.mock(), Mix::Distinct, 100, 50_000.0));
    for b in ctx.real() {
        for mix in [Mix::Fakeapp, Mix::Realistic] {
            cases.push((b.clone(), mix, 300, 20_000.0));
        }
    }
    for (bus, mix, slots, rate) in cases {
        let flood = slots > 4000 || mix == Mix::Distinct;
        let tag = format!("{}{}", mix.name(), if flood { "+flood" } else { "" });
        let label = bus.label();
        let mut spec = RunSpec::new(bus, dur);
        spec.verify = true;
        for i in 0..3u64 {
            let mut a = app(&format!("c{i}"), 60 + i, mix, rate / 3.0);
            a.plan.slots = slots;
            a.cons.drain_delay = if flood {
                Duration::from_millis(500)
            } else {
                Duration::from_millis(i * 2)
            };
            a.cons.fold = true;
            a.cons.check_order = true;
            a = with_probes(a, Duration::from_millis(10), dur);
            spec.apps.push(a);
        }
        let o = execute(&spec)?;
        let viol: usize = o
            .apps
            .iter()
            .map(|a| a.verdict.as_ref().map_or(1, |v| v.violations.len()))
            .sum();
        let first = o
            .apps
            .iter()
            .find_map(|a| {
                a.verdict
                    .as_ref()
                    .and_then(|v| v.violations.first().cloned())
            })
            .unwrap_or_default();
        let compared: u64 = o
            .apps
            .iter()
            .filter_map(|a| a.verdict.as_ref())
            .map(|v| v.compared)
            .sum();
        let ord: u64 = o.apps.iter().map(|a| a.cons.order_violations).sum();
        let missed: u64 = o.apps.iter().map(|a| a.cons.missed_wakes).sum();
        let misrouted: u64 = o.apps.iter().map(|a| a.cons.misrouted).sum();
        let flooded: u64 = o.apps.iter().map(|a| a.cons.flooded_batches).sum();
        let unfenced = o.apps.iter().filter(|a| !a.fence_seen).count();
        // SPEC 5.2 item 6: a non-flood run must not flood, and must compare something.
        let vacuous = if flood {
            0
        } else {
            usize::try_from(flooded).unwrap_or(1) + usize::from(compared == 0)
        };
        let bad = viol
            + usize::try_from(ord + missed + misrouted).unwrap_or(usize::MAX)
            + unfenced
            + vacuous
            + usize::from(o.disconnected());
        let id = |m: &str| format!("C.{m}@{tag}");
        rows.push(
            Row::info(&id("failures"), "T6", label, "count", &[bad as f64])
                .at_most(0.0)
                .note(if first.is_empty() { format!("order={ord} missed_wakes={missed} misrouted={misrouted} unfenced={unfenced} vacuous={vacuous}") } else { first }),
        );
        rows.push(Row::info(
            &id("compared_nodes"),
            "T6",
            label,
            "nodes",
            &[compared as f64],
        ));
        rows.push(Row::info(
            &id("flooded_batches"),
            "T6",
            label,
            "batches",
            &[flooded as f64],
        ));
        rows.push(Row::info(
            &id("pushed_vs_sent"),
            "T6",
            label,
            "pushes",
            &[o.drain.pushed as f64],
        ));
    }
    Ok(rows)
}

// ---- fixture self-test: does dbus-broker really disconnect a slow reader (F-3)? ----------------

struct F3 {
    sent: u64,
    sender_failed: bool,
    sender_err: String,
    read: u64,
    reader_eof: bool,
}

/// One reader subscribes to every AT-SPI event; a sender floods `push_bytes`. The reader either
/// reads continuously (`active`) or does not read at all until the flood is over.
fn f3_run(kind: BusKind, push_bytes: u64, active: bool) -> Result<F3> {
    let bus = Bus::start(kind)?;
    let mut rd = crate::wire::BusConn::connect(&bus.address, Duration::from_secs(5))?;
    for r in gorca_dbus::drain::default_match_rules() {
        rd.add_match(&r, Duration::from_secs(5))?;
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let counted = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(rd.leftover.len() as u64));
    let reader = {
        let (stop, counted) = (
            std::sync::Arc::clone(&stop),
            std::sync::Arc::clone(&counted),
        );
        let mut stream = rd.stream.try_clone()?;
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        std::thread::Builder::new()
            .name("hx-f3-reader".to_owned())
            .spawn(move || {
                let mut buf = vec![0u8; 1 << 20];
                let mut eof = false;
                while !stop.load(std::sync::atomic::Ordering::Acquire) || !active {
                    if !active && !stop.load(std::sync::atomic::Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    match stream.read(&mut buf) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(n) => {
                            counted.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            if stop.load(std::sync::atomic::Ordering::Acquire) {
                                break;
                            }
                        }
                        Err(_) => {
                            eof = true;
                            break;
                        }
                    }
                }
                eof
            })?
    };
    let (mut tx, _) = bus.sender()?;
    tx.stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut sc = crate::workload::Scratch::default();
    let mut g = crate::workload::Gen::new(1, Mix::Fakeapp, 100);
    let mut out = Vec::new();
    let (mut sent, mut serial, mut sender_failed) = (0u64, 1u32, false);
    let mut sender_err = String::new();
    while sent < push_bytes {
        out.clear();
        for _ in 0..256 {
            serial += 1;
            g.next_event().encode(&mut out, &mut sc, serial, None);
        }
        if let Err(e) = tx.stream.write_all(&out) {
            sender_failed = true;
            sender_err = e.to_string();
            break;
        }
        sent += out.len() as u64;
    }
    std::thread::sleep(Duration::from_millis(300));
    let mut reader_eof = false;
    let mut read = counted.load(std::sync::atomic::Ordering::Relaxed);
    if !active {
        // Now drain what the broker still holds for the slow reader.
        rd.stream
            .set_read_timeout(Some(Duration::from_millis(500)))?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            match rd.stream.read(&mut buf) {
                Ok(0) => {
                    reader_eof = true;
                    break;
                }
                Ok(n) => read += n as u64,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(_) => {
                    reader_eof = true;
                    break;
                }
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = reader.join();
    } else {
        stop.store(true, std::sync::atomic::Ordering::Release);
        reader_eof = reader.join().unwrap_or(false);
        read = counted.load(std::sync::atomic::Ordering::Relaxed);
    }
    Ok(F3 {
        sent,
        sender_failed,
        sender_err,
        read,
        reader_eof,
    })
}

pub fn fixture(ctx: &Ctx) -> Result<Vec<Row>> {
    let mut rows = Vec::new();
    let pool = 8 * 1024 * 1024u64;
    let push = if ctx.quick { 48 } else { 96 } * 1024 * 1024u64;
    let runs: Vec<(&str, BusKind)> = vec![
        ("dbus-broker", BusKind::Broker { pool_bytes: pool }),
        ("dbus-daemon", BusKind::Daemon { max_outgoing: pool }),
    ];
    for (label, kind) in runs {
        for (tag, active) in [("slow", false), ("active", true)] {
            match f3_run(kind.clone(), push, active) {
                Ok(r) => {
                    let lost = r.sent.saturating_sub(r.read);
                    rows.push(
                        Row::info(&format!("F3.{tag}_reader_eof"), "fixture", label, "1=disconnected", &[f64::from(u8::from(r.reader_eof))]).note(format!(
                            "sent {:.1} MiB, reader got {:.1} MiB (missing {:.1} MiB), sender_failed={} {}",
                            r.sent as f64 / 1048576.0,
                            r.read as f64 / 1048576.0,
                            lost as f64 / 1048576.0,
                            r.sender_failed,
                            r.sender_err
                        )),
                    );
                }
                Err(e) => rows.push(
                    Row::info(
                        &format!("F3.{tag}_reader_eof"),
                        "fixture",
                        label,
                        "1=disconnected",
                        &[f64::NAN],
                    )
                    .note(format!("fixture failed: {e:#}")),
                ),
            }
        }
    }
    Ok(rows)
}

// ---- T7: push cost ---------------------------------------------------------------------------

/// Per-call cost of `IngressSender::push` (ADR: "p99 push cost"), uncontended and with four
/// producers on one app while a consumer drains. Nanoseconds, timer overhead subtracted.
pub fn t7(ctx: &Ctx) -> Result<Vec<Row>> {
    use crate::clock::{Rng, now_ns};
    use crate::model::Ev;
    use gorca_ingress::{IngressConfig, ingress};
    use gorca_model::{AppId, ConnectorId};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let app = AppId {
        connector: ConnectorId(0),
        serial: 1,
    };
    let n: usize = if ctx.quick { 300_000 } else { 2_000_000 };
    // Timer overhead: two clock reads around nothing.
    let mut over = Vec::with_capacity(100_000);
    for _ in 0..100_000 {
        let a = now_ns();
        let b = now_ns();
        over.push(b.saturating_sub(a) as f64);
    }
    let overhead = summarize(&over).p50;
    let script = |seed: u64, nodes: u64| -> Vec<gorca_model::NodeEvent> {
        let mut rng = Rng::new(seed);
        (0..n as u32)
            .map(|i| {
                let node = rng.below(nodes);
                match rng.below(6) {
                    0 => Ev::Props(node, 1 << rng.below(9)),
                    1 => Ev::Caret(node, i),
                    2 => Ev::TextIns(node, i * 3, 1),
                    3 => Ev::Children(node),
                    4 => Ev::Selection(node),
                    _ => Ev::Focus(node),
                }
                .to_node_event(app)
            })
            .collect()
    };
    let mut rows = Vec::new();
    let (mut p50, mut p99, mut p999, mut pmax) = (vec![], vec![], vec![], vec![]);
    for r in 0..ctx.reps() {
        let (tx, rx) = ingress(IngressConfig::default());
        let evs = script(1 + r as u64, 1500);
        let mut lat = Vec::with_capacity(n);
        for (i, e) in evs.into_iter().enumerate() {
            let a = now_ns();
            tx.push(e);
            let b = now_ns();
            lat.push((b.saturating_sub(a) as f64 - overhead).max(0.0));
            if i % 512 == 511 {
                let _ = rx.drain();
            }
        }
        let s = summarize(&lat);
        p50.push(s.p50);
        p99.push(s.p99);
        p999.push(s.p999);
        pmax.push(s.max);
    }
    rows.push(Row::info("T7.push_p50_ns", "T7", "-", "ns", &p50));
    rows.push(Row::info("T7.push_p99_ns", "T7", "-", "ns", &p99).at_most(L::T7_PUSH_P99_MAX_NS));
    rows.push(Row::info("T7.push_p999_ns", "T7", "-", "ns", &p999).at_most(L::T7_PUSH_P999_MAX_NS));
    rows.push(Row::info("T7.push_max_ns", "T7", "-", "ns", &pmax));

    let (mut c50, mut c99, mut c999) = (vec![], vec![], vec![]);
    for r in 0..ctx.reps() {
        let (tx, rx) = ingress(IngressConfig::default());
        let stop = Arc::new(AtomicBool::new(false));
        let consumer = {
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("hx-t7-cons".to_owned())
                .spawn(move || {
                    while !stop.load(Ordering::Acquire) {
                        let _ = rx.drain();
                        std::thread::sleep(Duration::from_micros(200));
                    }
                })?
        };
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let hs: Vec<_> = (0..4u64)
            .map(|t| {
                let (tx, barrier) = (tx.clone(), Arc::clone(&barrier));
                let evs = script(50 + t + r as u64 * 7, 1500);
                std::thread::Builder::new()
                    .name(format!("hx-t7-p{t}"))
                    .spawn(move || {
                        barrier.wait();
                        let mut lat = Vec::with_capacity(evs.len());
                        for e in evs {
                            let a = now_ns();
                            tx.push(e);
                            let b = now_ns();
                            lat.push((b.saturating_sub(a) as f64 - overhead).max(0.0));
                        }
                        lat
                    })
            })
            .collect::<std::io::Result<_>>()?;
        let mut all = Vec::new();
        for h in hs {
            all.extend(h.join().unwrap_or_default());
        }
        stop.store(true, Ordering::Release);
        let _ = consumer.join();
        let s = summarize(&all);
        c50.push(s.p50);
        c99.push(s.p99);
        c999.push(s.p999);
    }
    rows.push(Row::info(
        "T7.push4_p50_ns",
        "T7",
        "-",
        "ns (4 producers)",
        &c50,
    ));
    rows.push(
        Row::info("T7.push4_p99_ns", "T7", "-", "ns (4 producers)", &c99)
            .at_most(L::T7_PUSH4_P99_MAX_NS),
    );
    rows.push(
        Row::info("T7.push4_p999_ns", "T7", "-", "ns (4 producers)", &c999)
            .at_most(L::T7_PUSH4_P999_MAX_NS),
    );
    Ok(rows)
}
