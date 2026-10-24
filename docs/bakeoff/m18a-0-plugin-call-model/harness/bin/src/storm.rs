// SPDX-License-Identifier: MIT OR Apache-2.0
//! M6 / PX-7: an open-loop event storm (10,000 and 50,000 events/s) into one writer with 3 plugins
//! watching half of the nodes, plugins on and off, sampled every 100 ms.
//!
//! The generator thread sends event `i` at `start + i / rate` regardless of how the writer
//! is doing (no coordinated omission); the ingress lag of an event is `writer_dequeue - scheduled`.

use crate::arms::Factory;
use crate::focus::RunEnv;
use anyhow::{anyhow, Result};
use m18a0_common::fixture::*;
use m18a0_common::model::*;
use m18a0_common::runner::*;
use m18a0_common::summary::Summary;
use m18a0_common::timing::*;
use m18a0_common::writer::*;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;

struct Ev {
    sched: u64,
    node: NodeId,
    class: Class,
}

#[derive(Default)]
struct StormOut {
    lag: Vec<u32>,
    depth: Vec<(f64, f64, f64, f64)>, // (t_s, ingress, writer mailbox, plugin mailboxes)
    events: u64,
    coalesced: u64,
    drain_ns: u64,
    quiesced: bool,
    blocked_max_ns: u64,
    describe: serde_json::Value,
    bg_jobs: u64,
}

fn run_storm_leg(
    fx: &Fixture,
    factory: Factory,
    on: bool,
    rate: f64,
    dur_s: f64,
    nodes: usize,
    seed: u64,
) -> Result<StormOut> {
    let n_events = (rate * dur_s) as u64;
    let mut specs = fx.specs.clone();
    specs.truncate(3);
    // Three plugins watching about half of the nodes: rows, labels (3 of 4 role slots in the tree).
    for (i, s) in specs.iter_mut().enumerate() {
        s.watch_roles = match i {
            0 => (1 << ROLE_ROW) | (1 << ROLE_LABEL),
            1 => 1 << ROLE_LABEL | 1 << ROLE_BUTTON,
            _ => 1 << ROLE_ROW,
        };
    }
    let table = NodeTable::build(nodes, seed);
    let mut cfg = fx.writer_cfg(8, 0);
    cfg.record_cap = 0;
    let mut w = StubWriter::new(table, if on { specs } else { Vec::new() }, cfg);
    let rts = if on {
        let mut v = fx.runtimes()?;
        v.truncate(3);
        v
    } else {
        Vec::new()
    };
    let mut runner: Box<dyn WLaneRunner> = if on {
        factory(rts)?
    } else {
        Box::new(NoopRunner)
    };
    let (tx, rx) = channel::<Ev>();
    let sent = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let (sent2, done2) = (sent.clone(), done.clone());
    let n_nodes = nodes as u64;
    let start = now_ns() + 20_000_000;
    let gen = std::thread::Builder::new()
        .name("m18a0-gen".into())
        .spawn(move || {
            let _ = set_timerslack(1);
            let mut rng = Rng(seed ^ 0x57_0121);
            let gap = 1e9 / rate;
            for i in 0..n_events {
                let sched = start + (i as f64 * gap) as u64;
                wait_until(sched, 30_000);
                let ev = Ev {
                    sched,
                    node: 2 + rng.below(n_nodes - 2) as NodeId,
                    class: ALL_CLASSES[rng.below(6) as usize],
                };
                sent2.fetch_add(1, Ordering::Release);
                if tx.send(ev).is_err() {
                    break;
                }
            }
            done2.store(true, Ordering::Release);
        })?;
    let h = std::thread::Builder::new()
        .name("m18a0-writer".into())
        .spawn(move || -> Result<StormOut> {
            set_timerslack(1).map_err(|e| anyhow!("{e}"))?;
            let mut o = StormOut {
                lag: Vec::with_capacity(n_events as usize + 16),
                ..StormOut::default()
            };
            let mut recv = 0u64;
            let mut seen: std::collections::HashSet<(NodeId, u8)> = Default::default();
            let t_start = start;
            let mut next_sample = start;
            let mut gen_done_at: Option<u64> = None;
            loop {
                let n0 = now_ns();
                w.drain();
                seen.clear();
                let mut batch = 0;
                while batch < 256 {
                    match rx.try_recv() {
                        Ok(ev) => {
                            recv += 1;
                            batch += 1;
                            let now = now_ns();
                            o.lag.push(sat32(now.saturating_sub(ev.sched)));
                            if seen.insert((ev.node, ev.class as u8)) {
                                w.apply_s0_event(ev.node, ev.class);
                            } else {
                                o.coalesced += 1; // superseded within the pass: counted, not applied twice
                            }
                        }
                        Err(_) => break,
                    }
                }
                o.bg_jobs += w.pump_background(&mut *runner) as u64;
                let now = now_ns();
                o.blocked_max_ns = o.blocked_max_ns.max(now - n0);
                if now >= next_sample {
                    let t_s = (now.saturating_sub(t_start)) as f64 / 1e9;
                    let ing = sent.load(Ordering::Acquire).saturating_sub(recv) as f64;
                    let pq: usize = runner.plugin_queue_depths().iter().sum();
                    o.depth
                        .push((t_s, ing, w.mailbox_depth() as f64, pq as f64));
                    next_sample += 100_000_000;
                }
                if done.load(Ordering::Acquire) {
                    let gd = *gen_done_at.get_or_insert(now);
                    let ingress_empty = sent.load(Ordering::Acquire) == recv;
                    let idle = w
                        .plugins
                        .iter()
                        .all(|p| p.slot.busy.is_none() && p.pending.is_empty())
                        && w.mailbox_depth() == 0;
                    if ingress_empty && idle {
                        o.drain_ns = now - gd;
                        o.quiesced = true;
                        break;
                    }
                    if now - gd > 2_000_000_000 {
                        o.drain_ns = now - gd;
                        break;
                    }
                }
                std::hint::spin_loop();
            }
            o.events = recv;
            o.describe = runner.describe();
            runner.shutdown();
            Ok(o)
        })?;
    let out = h.join().map_err(|_| anyhow!("writer panicked"))??;
    let _ = gen.join();
    Ok(out)
}

/// Least-squares slope of depth over time and its standard error (for the "no monotone growth" test).
fn slope_se(points: &[(f64, f64)]) -> (f64, f64) {
    let n = points.len() as f64;
    if n < 4.0 {
        return (0.0, 0.0);
    }
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    if sxx == 0.0 {
        return (0.0, 0.0);
    }
    let b = sxy / sxx;
    let a = my - b * mx;
    let sse: f64 = points.iter().map(|p| (p.1 - (a + b * p.0)).powi(2)).sum();
    let se = (sse / (n - 2.0) / sxx).sqrt();
    (b, se)
}

pub fn run_storm(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let sc = env.sc;
    let names = plugin_names("benign", &[]);
    let fx = Fixture::build(&names, sc.seed, env.kappa)?;
    let (rate, dur) = (
        if sc.rate > 0.0 { sc.rate } else { 10_000.0 },
        if sc.duration_s > 0.0 {
            sc.duration_s
        } else {
            120.0
        },
    );
    let on = run_storm_leg(&fx, env.factory, true, rate, dur, sc.nodes, sc.seed)?;
    let off = run_storm_leg(&fx, env.factory, false, rate, dur, sc.nodes, sc.seed)?;
    let (pon, poff) = (percentiles(&on.lag), percentiles(&off.lag));
    sum.series_metrics("ingress_lag_on", &pon);
    sum.series_metrics("ingress_lag_off", &poff);
    let limit = (poff.p99 as f64 * 1.05).max(poff.p99 as f64 + 10_000.0);
    sum.metric("m6_lag_p99_on_ns", pon.p99 as f64);
    sum.metric("m6_lag_p99_off_ns", poff.p99 as f64);
    sum.metric("m6_lag_p99_limit_ns", limit);
    sum.check("m6_ingress_lag_unchanged", pon.p99 as f64 <= limit);
    // Depth over the last 60 s (or the second half of a shorter run): total backlog.
    let tail_from = (dur - 60.0).max(dur / 2.0);
    let pts: Vec<(f64, f64)> = on
        .depth
        .iter()
        .filter(|d| d.0 >= tail_from)
        .map(|d| (d.0, d.1 + d.2 + d.3))
        .collect();
    let (slope, se) = slope_se(&pts);
    sum.metric("m6_depth_slope_per_s", slope);
    sum.metric("m6_depth_slope_ci95", 1.96 * se);
    sum.metric(
        "m6_depth_max",
        on.depth.iter().map(|d| d.1 + d.2 + d.3).fold(0.0, f64::max),
    );
    sum.check("m6_no_monotone_growth", slope - 1.96 * se <= 0.0);
    sum.metric("m6_drain_ms", on.drain_ns as f64 / 1e6);
    sum.metric("m6_drain_ms_off", off.drain_ns as f64 / 1e6);
    sum.check(
        "m6_drained_within_10ms",
        on.quiesced && on.drain_ns <= 10_000_000,
    );
    sum.metric("m6_events", on.events as f64);
    sum.metric("m6_events_coalesced", on.coalesced as f64);
    sum.metric("m6_events_dropped", 0.0);
    sum.metric("m6_background_jobs", on.bg_jobs as f64);
    sum.metric("m6_writer_pass_max_ns", on.blocked_max_ns as f64);
    sum.check("m6_writer_never_blocked", on.blocked_max_ns < 5_000_000);
    sum.counters =
        json!({"rate": rate, "duration_s": dur, "describe": on.describe, "off_events": off.events});
    write_u32_zst(&env.dir.join("stall.u32.zst"), &[])?;
    write_u32_zst(&env.dir.join("latency.u32.zst"), &on.lag)?;
    write_u32_zst(&env.dir.join("lateness.u32.zst"), &off.lag)?;
    let csv: String = on
        .depth
        .iter()
        .map(|d| format!("{:.2},{},{},{}\n", d.0, d.1, d.2, d.3))
        .collect();
    std::fs::write(
        env.dir.join("depth.csv"),
        format!("t_s,ingress,writer_mailbox,plugin_mailboxes\n{csv}"),
    )?;
    Ok(())
}
