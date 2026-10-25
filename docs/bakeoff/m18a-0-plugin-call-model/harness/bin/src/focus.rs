// SPDX-License-Identifier: MIT OR Apache-2.0
//! The focus-change engine and the scenarios built on it: `focus` (benign, adversarial, smoke),
//! `flap`, `keypress` (M4, ABABAB), `k1k2` (M5), `a5` (cold load) and `pf1`.
//!
//! Open loop (SPEC 7 req. 1): focus changes happen at scheduled instants `start + i * interval`;
//! speech latency is measured from the scheduled instant, so a stalled writer cannot hide its stall.
//! The writer thread busy-polls its mailbox while idle (recorded in meta.json), so wake latency of
//! the writer itself does not pollute the plugin-related numbers.

use crate::arms::Factory;
use anyhow::{anyhow, Result};
use m18a0_common::fixture::*;
use m18a0_common::host::{set_writer_turn, BIG_ALLOC_MAX, WRITER_TURN_LOADS};
use m18a0_common::model::*;
use m18a0_common::runner::*;
use m18a0_common::scenario::Scenario;
use m18a0_common::summary::Summary;
use m18a0_common::timing::*;
use m18a0_common::writer::*;
use serde_json::{json, Value};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug)]
pub struct LegCfg {
    pub on: bool,
    pub samples: u64,
    pub warmup: u64,
    pub interval_us: u64,
    pub nodes: usize,
    pub chain: usize,
    pub seed: u64,
    pub timerslack_ns: Option<u64>,
    pub pin: Option<usize>,
    pub track_faults: bool,
    pub flap_ms: Option<u64>,
    /// Between warm-up and the measured part, run this on the writer thread (A5: start a loader).
    pub deterministic: bool,
}

impl LegCfg {
    pub fn from(sc: &Scenario, on: bool) -> LegCfg {
        LegCfg {
            on,
            samples: sc.samples,
            warmup: sc.warmup,
            interval_us: sc.interval_us,
            nodes: sc.nodes,
            chain: sc.chain,
            seed: sc.seed,
            timerslack_ns: sc.timerslack_ns,
            pin: sc.pin_writer,
            track_faults: sc.kind == "a5",
            // Spec clarification C-1: the N ms flapping cap exists only in the flap scenarios (default
            // 20 ms there). Every other scenario runs uncapped (0 = no cap), so each focus change owns
            // its bounded wait and M3/M4 measure the wait, not the cap.
            flap_ms: sc.flap_ms.or(Some(if sc.kind == "flap" { 20 } else { 0 })),
            deterministic: false,
        }
    }
}

#[derive(Default)]
pub struct LegOut {
    pub stall: Vec<u32>,
    pub lag: Vec<u32>,
    pub speech: Vec<u32>,
    pub woke_late: Vec<u32>,
    pub records: Vec<TurnRecord>,
    pub counters: Counters,
    pub plugins: Vec<(String, PluginCounters, bool, String)>,
    pub writer_slack: Option<u64>,
    pub pool_slacks: Vec<u64>,
    pub ctx_switches: (u64, u64),
    pub minor_faults: u64,
    pub major_faults: u64,
    pub turn_major_faults: u64,
    pub writer_turn_loads: u64,
    pub describe: Value,
    pub digests: Vec<u64>,
    pub pf1_timeouts: u64,
    pub elapsed_ns: u64,
    pub names: Vec<String>,
    pub big_alloc_max: usize,
}

/// Spin-poll the mailbox until `until`; send coalesced background work every 100 us.
fn idle(w: &mut StubWriter, r: &mut dyn WLaneRunner, until: u64) {
    let mut last = now_ns();
    loop {
        let n = now_ns();
        if n >= until {
            break;
        }
        w.drain();
        if n - last >= 100_000 {
            w.pump_background(r);
            last = n;
        }
        std::hint::spin_loop();
    }
}

/// Wait (bounded) until every plugin of the writer is idle and nothing is pending (PF-1 mode).
fn quiesce(w: &mut StubWriter, r: &mut dyn WLaneRunner, limit_ns: u64) -> bool {
    let end = now_ns() + limit_ns;
    loop {
        w.drain();
        w.pump_background(r);
        if w.plugins
            .iter()
            .all(|p| p.slot.busy.is_none() && p.pending.is_empty())
            && w.mailbox_depth() == 0
        {
            return true;
        }
        if now_ns() > end {
            return false;
        }
        std::hint::spin_loop();
    }
}

pub fn run_leg(fx: &Fixture, factory: Factory, cfg: &LegCfg) -> Result<LegOut> {
    let cap = cfg.samples as usize + 16;
    let mut w = if cfg.on {
        fx.writer(cfg.nodes, cfg.chain, cap)
    } else {
        fx.writer_off(cfg.nodes, cfg.chain, cap)
    };
    if let Some(f) = cfg.flap_ms {
        w.cfg.flap_ms = f;
    }
    let mut runner: Box<dyn WLaneRunner> = if cfg.on {
        factory(fx.runtimes()?)?
    } else {
        Box::new(NoopRunner)
    };
    let cfg = cfg.clone();
    let names = if cfg.on { fx.names.clone() } else { Vec::new() };
    let h = std::thread::Builder::new()
        .name("m18a0-writer".into())
        .spawn(move || -> Result<LegOut> {
            set_timerslack(cfg.timerslack_ns.unwrap_or(1))
                .map_err(|e| anyhow!("prctl timerslack: {e}"))?;
            if let Some(c) = cfg.pin {
                pin_to_cpu(c).map_err(|e| anyhow!("pin: {e}"))?;
            }
            let writer_slack = get_timerslack();
            let rows = Fixture::row_ids(&w.table);
            let mut rng = Rng(cfg.seed ^ 0xF0C5);
            let interval = cfg.interval_us * 1000;
            let total = cfg.warmup + cfg.samples;
            let start = now_ns() + 3_000_000;
            let mut out = LegOut::default();
            let (mut cs0, mut f0) = (thread_ctx_switches(), thread_faults());
            let loads0 = WRITER_TURN_LOADS.load(Ordering::Relaxed);
            let mut turn_major = 0u64;
            let t_begin = now_ns();
            BIG_ALLOC_MAX.store(0, Ordering::Relaxed);
            for i in 0..total {
                let t_sched = start + i * interval;
                idle(&mut w, &mut *runner, t_sched);
                if i == cfg.warmup {
                    w.reset_records();
                    cs0 = thread_ctx_switches();
                    f0 = thread_faults();
                    turn_major = 0;
                }
                let focus = rows[rng.below(rows.len() as u64) as usize];
                let fb = if cfg.track_faults {
                    thread_faults().1
                } else {
                    0
                };
                w.focus_change(
                    &mut *runner,
                    &FocusEvent {
                        focus,
                        t_sched_ns: t_sched,
                    },
                    None,
                );
                if cfg.track_faults {
                    turn_major += thread_faults().1.saturating_sub(fb);
                }
                if cfg.deterministic && !quiesce(&mut w, &mut *runner, 50_000_000) {
                    out.pf1_timeouts += 1;
                }
                if cfg.deterministic && i >= cfg.warmup {
                    let mut ch = Vec::new();
                    w.table.focus_chain(focus, cfg.chain, &mut ch);
                    out.digests.push(w.fused_digest(&ch));
                }
            }
            // Collect the stragglers (late replies) before reading counters.
            idle(&mut w, &mut *runner, now_ns() + 5_000_000);
            let cs1 = thread_ctx_switches();
            let f1 = thread_faults();
            out.big_alloc_max = BIG_ALLOC_MAX.load(Ordering::Relaxed);
            out.elapsed_ns = now_ns() - t_begin;
            out.ctx_switches = (cs1.0 - cs0.0, cs1.1 - cs0.1);
            out.minor_faults = f1.0 - f0.0;
            out.major_faults = f1.1 - f0.1;
            out.turn_major_faults = turn_major;
            out.writer_turn_loads = WRITER_TURN_LOADS.load(Ordering::Relaxed) - loads0;
            set_writer_turn(false);
            out.writer_slack = writer_slack;
            out.pool_slacks = runner.thread_timer_slacks();
            out.describe = runner.describe();
            runner.shutdown();
            for r in &w.records {
                out.stall.push(r.stall_ns);
                out.lag.push(r.lag_ns);
                out.speech.push(r.speech_ns);
                if r.waited {
                    out.woke_late.push(r.woke_late_ns);
                }
            }
            out.records = std::mem::take(&mut w.records);
            out.counters = w.counters.clone();
            out.plugins = w
                .plugins
                .iter()
                .map(|p| {
                    (
                        p.spec.name.clone(),
                        p.c.clone(),
                        p.spec.adversary,
                        p.slot.state.kind().to_string(),
                    )
                })
                .collect();
            out.names = names;
            Ok(out)
        })?;
    h.join().map_err(|_| anyhow!("writer thread panicked"))?
}

// ---- metrics ------------------------------------------------------------------------------------

pub fn write_events(dir: &Path, out: &LegOut) -> Result<()> {
    let f = std::fs::File::create(dir.join("events.jsonl.zst"))?;
    let mut enc = zstd::Encoder::new(f, 3)?;
    for r in &out.records {
        let mut per: Vec<Value> = Vec::new();
        for (p, o) in r.outcomes.iter().enumerate() {
            if *o != 0 {
                per.push(json!([p, o, r.exec_ns[p], r.queued_ns[p], r.fuel[p]]));
            }
        }
        let line = json!({"turn": r.turn, "lag": r.lag_ns, "stall": r.stall_ns, "speech": r.speech_ns, "asked": r.asked,
            "arrived": r.arrived, "busy": r.busy_skips, "woke_late": r.woke_late_ns, "waited_ns": r.waited_ns,
            "waited": r.waited, "pairs": r.pairs, "ok": r.pairs_ok, "grant_sum": r.grant_sum_ns, "p": per});
        writeln!(enc, "{line}")?;
    }
    enc.finish()?;
    Ok(())
}

pub fn write_series(dir: &Path, stall: &[u32], latency: &[u32], lateness: &[u32]) -> Result<()> {
    write_u32_zst(&dir.join("stall.u32.zst"), stall)?;
    write_u32_zst(&dir.join("latency.u32.zst"), latency)?;
    write_u32_zst(&dir.join("lateness.u32.zst"), lateness)?;
    Ok(())
}

/// Reason breakdown + per-plugin coverage + M8 checks into the summary.
pub fn focus_metrics(sum: &mut Summary, out: &LegOut, is_candidate: bool) {
    sum.series_metrics("stall", &percentiles(&out.stall));
    sum.series_metrics("lag", &percentiles(&out.lag));
    sum.series_metrics("speech", &percentiles(&out.speech));
    sum.series_metrics("woke_late", &percentiles(&out.woke_late));
    // M3
    let (mut pairs, mut ok) = (0u64, 0u64);
    for r in &out.records {
        pairs += r.pairs as u64;
        ok += r.pairs_ok as u64;
    }
    sum.metric("m3_pairs", pairs as f64);
    sum.metric("m3_pairs_ok", ok as f64);
    if pairs > 0 {
        sum.metric("m3_overall", ok as f64 / pairs as f64);
    }
    let mut min_benign = f64::INFINITY;
    let mut reasons: std::collections::BTreeMap<String, u64> = Default::default();
    for (name, c, adversary, _) in &out.plugins {
        if c.pairs > 0 {
            let f = c.pairs_ok as f64 / c.pairs as f64;
            sum.per_plugin.insert(name.clone(), f);
            if !adversary {
                min_benign = min_benign.min(f);
            }
        }
        for (k, v) in &c.reasons {
            *reasons.entry(k.clone()).or_insert(0) += v;
        }
    }
    if min_benign.is_finite() {
        sum.metric("m3_min_benign_plugin", min_benign);
    }
    let (mut bp, mut bo) = (0u64, 0u64);
    for (_, c, adversary, _) in &out.plugins {
        if !adversary {
            bp += c.pairs;
            bo += c.pairs_ok;
        }
    }
    if bp > 0 {
        sum.metric("m3_benign_overall", bo as f64 / bp as f64);
    }
    sum.metric("host_big_alloc_max_bytes", out.big_alloc_max as f64);
    sum.check("no_guest_sized_host_alloc", out.big_alloc_max < (8 << 20));
    sum.metric("busy_skips", out.counters.busy_skips as f64);
    sum.metric("waits", out.counters.waits as f64);
    sum.metric("turns", out.counters.turns as f64);
    sum.metric("ctx_switches_voluntary", out.ctx_switches.0 as f64);
    sum.metric("ctx_switches_involuntary", out.ctx_switches.1 as f64);
    sum.metric("writer_minor_faults", out.minor_faults as f64);
    sum.metric("writer_major_faults", out.major_faults as f64);
    sum.metric("writer_turn_loads", out.writer_turn_loads as f64);
    sum.metric("max_grant_sum_ns", out.counters.max_grant_sum_ns as f64);
    sum.metric(
        "grants_over_budget",
        out.counters.grants_sum_over_budget as f64,
    );
    // Checks (M8). A missing timer-slack proof makes M1 not evaluable for candidates (SPEC 7 req. 6).
    sum.check("writer_timerslack_1ns", out.writer_slack == Some(1));
    sum.check(
        "pool_timerslack_1ns",
        out.pool_slacks.iter().all(|s| *s == 1),
    );
    sum.check(
        "no_impossible_slot_cells",
        out.counters.impossible_cells == 0,
    );
    sum.check(
        "grant_sum_within_budget",
        out.counters.grants_sum_over_budget == 0,
    );
    sum.check("no_load_on_writer_turn", out.writer_turn_loads == 0);
    sum.check("process_survived", true);
    // Every adversary was contained: struck, over budget, overrun, demoted or no longer Ready.
    let mut contained = true;
    let mut demoted_after_overrun = true;
    for (_, c, adversary, state) in &out.plugins {
        if *adversary {
            let acted = c.strikes > 0
                || c.budget_misses > 0
                || c.overruns > 0
                || c.demoted_turns > 0
                || state != "Ready";
            contained &= acted || c.derive_calls == 0;
        }
        if c.overruns > 0 {
            demoted_after_overrun &= c.demoted_turns > 0 || state != "Ready";
        }
    }
    sum.check("adversaries_contained", contained);
    sum.check("demoted_after_first_overrun", demoted_after_overrun);
    let _ = is_candidate;
    let faults: std::collections::BTreeMap<String, Value> = out
        .plugins
        .iter()
        .map(|(n, c, _, st)| (n.clone(), json!({"faults": c.faults, "strikes": c.strikes, "overruns": c.overruns,
            "budget_misses": c.budget_misses, "stale_epoch_dropped": c.stale_epoch_dropped,
            "stale_token_dropped": c.stale_token_dropped, "late_admitted": c.late_admitted, "demoted_turns": c.demoted_turns,
            "refresh_requests": c.refresh_requests, "refresh_dropped": c.refresh_dropped, "preempted": c.preempted,
            "exec_ns_max": c.exec_ns_max, "queued_ns_max": c.queued_ns_max, "fuel_max": c.fuel_max, "state": st,
            "derive_calls": c.derive_calls})))
        .collect();
    sum.counters = json!({"writer": out.counters, "plugins": faults, "m3_reasons": reasons, "describe": out.describe});
    let stale: u64 = out.plugins.iter().map(|p| p.1.stale_epoch_dropped).sum();
    sum.metric("stale_epoch_dropped", stale as f64);
}

// ---- scenario drivers ----------------------------------------------------------------------------

pub struct RunEnv<'a> {
    pub sc: &'a Scenario,
    pub arm: &'a str,
    pub factory: Factory,
    pub dir: &'a Path,
    pub kappa: Option<f64>,
}

pub fn fixture_for(env: &RunEnv<'_>) -> Result<Fixture> {
    let names = plugin_names(&env.sc.fixture, &env.sc.adv);
    Fixture::build(&names, env.sc.seed, env.kappa)
}

fn note_fixture(sum: &mut Summary, fx: &Fixture) {
    sum.metric("kappa_int_fuel_per_ns", fx.kappa);
    sum.counters["fixture"] = json!({"plugins": fx.specs.iter().map(|s| json!({"name": s.name, "tier": s.tier, "watch_roles": s.watch_roles, "adversary": s.adversary})).collect::<Vec<_>>(),
        "kappa_rounds": fx.kappa_rounds, "guests": fx.guest_hashes(), "compat_hash": fx.he.compat_hash});
}

/// focus, flap, a5 (via `run_a5`), smoke.
pub fn run_focus(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let fx = fixture_for(env)?;
    note_fixture(sum, &fx);
    let on = run_leg(&fx, env.factory, &LegCfg::from(env.sc, true))?;
    write_series(env.dir, &on.stall, &on.speech, &on.woke_late)?;
    write_events(env.dir, &on)?;
    focus_metrics(sum, &on, crate::arms::is_candidate(env.arm));
    // The stub's own non-plugin work, measured with plugins off (budget <= 5 us); never subtracted.
    let mut offc = LegCfg::from(env.sc, false);
    offc.samples = offc.samples.min(50_000);
    let off = run_leg(&fx, env.factory, &offc)?;
    let p = percentiles(&off.stall);
    sum.metric("stub_stall_p50_ns", p.p50 as f64);
    sum.metric("stub_stall_p99_ns", p.p99 as f64);
    sum.metric("stub_stall_max_ns", p.max as f64);
    sum.metric("stub_speech_p99_ns", percentiles(&off.speech).p99 as f64);
    if env.sc.kind == "flap" || env.sc.flap_ms.is_some() {
        flap_metrics(sum, &on, env.sc.flap_ms.unwrap_or(20));
    }
    Ok(())
}

/// Max number of interactive waits any sliding window of `n_ms` contains (M5, flapping focus).
pub fn flap_metrics(sum: &mut Summary, out: &LegOut, n_ms: u64) {
    let times: Vec<u64> = out
        .records
        .iter()
        .filter(|r| r.waited)
        .map(|r| r.t0_ns)
        .collect();
    let mut worst = 0usize;
    let mut j = 0usize;
    // n_ms == 0 means "no cap": the window degenerates to 1 ns (and j never passes i).
    let win = (n_ms * 1_000_000).max(1);
    for i in 0..times.len() {
        while j < i && times[i] - times[j] >= win {
            j += 1;
        }
        worst = worst.max(i - j + 1);
    }
    sum.metric("m5_flap_max_waits_per_window", worst as f64);
    sum.metric("m5_flap_window_ms", n_ms as f64);
    sum.metric("m5_flap_waits", times.len() as f64);
    sum.metric("m5_flap_turns", out.records.len() as f64);
}

/// M4: interleaved ABABAB legs, same seeded focus stream. Delta p99 = p99(on) - p99(off), median over pairs.
pub fn run_keypress(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let fx = fixture_for(env)?;
    note_fixture(sum, &fx);
    let pairs = env.sc.pairs.max(1);
    let n = ((env.sc.leg_s * 1e6) / env.sc.interval_us as f64).max(200.0) as u64;
    let mut deltas: Vec<f64> = Vec::new();
    let (mut all_on, mut all_off, mut late) = (Vec::new(), Vec::new(), Vec::new());
    let mut last_on = None;
    for pair in 0..pairs {
        let mut c = LegCfg::from(env.sc, true);
        c.samples = n;
        let on = run_leg(&fx, env.factory, &c)?;
        c.on = false;
        let off = run_leg(&fx, env.factory, &c)?;
        let (po, pf) = (percentiles(&on.speech), percentiles(&off.speech));
        deltas.push(po.p99 as f64 - pf.p99 as f64);
        sum.metric(&format!("m4_pair{pair}_on_p99_ns"), po.p99 as f64);
        sum.metric(&format!("m4_pair{pair}_off_p99_ns"), pf.p99 as f64);
        all_on.extend_from_slice(&on.speech);
        all_off.extend_from_slice(&off.speech);
        late.extend_from_slice(&on.woke_late);
        last_on = Some(on);
    }
    let mut s = deltas.clone();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    sum.metric("m4_delta_p99_ns", s[s.len() / 2]);
    sum.metric("m4_delta_min_ns", s[0]);
    sum.metric("m4_delta_max_ns", s[s.len() - 1]);
    sum.metric("m4_pairs", pairs as f64);
    sum.series_metrics("speech_on", &percentiles(&all_on));
    sum.series_metrics("speech_off", &percentiles(&all_off));
    write_series(env.dir, &[], &all_on, &late)?;
    write_u32_zst(&env.dir.join("latency_off.u32.zst"), &all_off)?;
    if let Some(on) = last_on {
        focus_metrics(sum, &on, crate::arms::is_candidate(env.arm));
    }
    Ok(())
}

/// M5: K1 and K2 writers on one keypress path; only the first takes the bounded wait.
pub fn run_k1k2(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let fx = fixture_for(env)?;
    note_fixture(sum, &fx);
    let sc = env.sc;
    let mut w1 = fx.writer(sc.nodes, sc.chain, sc.samples as usize + 16);
    let mut w2 = {
        let table = NodeTable::build(sc.nodes, sc.seed + 77);
        StubWriter::new(
            table,
            fx.specs.clone(),
            fx.writer_cfg(sc.chain, sc.samples as usize + 16),
        )
    };
    let mut r1 = (env.factory)(fx.runtimes()?)?;
    let mut r2 = (env.factory)(fx.runtimes()?)?;
    let (interval, samples, warm, seed) = (sc.interval_us * 1000, sc.samples, sc.warmup, sc.seed);
    let h = std::thread::Builder::new()
        .name("m18a0-writer".into())
        .spawn(
            move || -> Result<(Vec<u32>, Vec<u32>, u32, u64, Option<u64>)> {
                set_timerslack(1).map_err(|e| anyhow!("{e}"))?;
                let slack = get_timerslack();
                let rows1 = Fixture::row_ids(&w1.table);
                let rows2 = Fixture::row_ids(&w2.table);
                let mut rng = Rng(seed ^ 0x4B31);
                let start = now_ns() + 3_000_000;
                let (mut combined, mut speech) = (Vec::new(), Vec::new());
                let (mut max_waits, mut paths) = (0u32, 0u64);
                for i in 0..warm + samples {
                    let t_sched = start + i * interval;
                    let mut last = now_ns();
                    loop {
                        let n = now_ns();
                        if n >= t_sched {
                            break;
                        }
                        w1.drain();
                        w2.drain();
                        if n - last > 100_000 {
                            w1.pump_background(&mut *r1);
                            w2.pump_background(&mut *r2);
                            last = n;
                        }
                        std::hint::spin_loop();
                    }
                    let path = PathToken::default();
                    let f1 = rows1[rng.below(rows1.len() as u64) as usize];
                    let f2 = rows2[rng.below(rows2.len() as u64) as usize];
                    let a = w1.focus_change(
                        &mut *r1,
                        &FocusEvent {
                            focus: f1,
                            t_sched_ns: t_sched,
                        },
                        Some(&path),
                    );
                    let b = w2.focus_change(
                        &mut *r2,
                        &FocusEvent {
                            focus: f2,
                            t_sched_ns: t_sched,
                        },
                        Some(&path),
                    );
                    if i >= warm {
                        combined.push(sat32(b.t_publish_ns - a.t0_ns));
                        speech.push(sat32(b.t_speech_ns.max(a.t_speech_ns) - t_sched));
                        max_waits = max_waits.max(path.waits.load(Ordering::Acquire));
                        paths += 1;
                    }
                }
                r1.shutdown();
                r2.shutdown();
                Ok((combined, speech, max_waits, paths, slack))
            },
        )?;
    let (combined, speech, max_waits, paths, slack) =
        h.join().map_err(|_| anyhow!("writer panicked"))??;
    sum.series_metrics("m5_combined", &percentiles(&combined));
    sum.series_metrics("speech", &percentiles(&speech));
    sum.metric("m5_waits_max_per_path", max_waits as f64);
    sum.metric("m5_paths", paths as f64);
    sum.check("writer_timerslack_1ns", slack == Some(1));
    write_series(env.dir, &combined, &speech, &[])?;
    Ok(())
}

/// A5: the cache file is evicted with `posix_fadvise(DONTNEED)` (that file only, never
/// `drop_caches`), the plugin is then loaded on a loader thread while the writer keeps running
/// focus changes. Pass = no major fault and no file read / deserialize on a writer turn.
pub fn run_a5(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let fx = fixture_for(env)?;
    note_fixture(sum, &fx);
    let p = fx.he.cache_path(
        &fx.loaded
            .iter()
            .find(|l| l.name == "a5-cold-load")
            .map(|l| l.sha256.clone())
            .ok_or_else(|| {
                anyhow!("fixture lacks a5-cold-load; scenario must set adv = [\"a5\"]")
            })?,
    );
    let evict = |path: &Path| -> Result<()> {
        use std::os::fd::AsRawFd;
        let f = std::fs::File::open(path)?;
        // SAFETY: valid fd; advice DONTNEED on this file only.
        let r = unsafe { libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        if r != 0 {
            return Err(anyhow!("posix_fadvise failed: {r}"));
        }
        Ok(())
    };
    evict(&p)?;
    let he = fx.he.clone();
    let wasm = guests_dir().join("a5-cold-load.wasm");
    let loader = std::thread::Builder::new()
        .name("m18a0-loader".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let t = now_ns();
            let r = he.load("a5-cold-load", &wasm);
            (
                r.map(|l| (l.load_ns, l.cache_hit))
                    .map_err(|e| e.to_string()),
                now_ns() - t,
            )
        })?;
    let on = run_leg(&fx, env.factory, &LegCfg::from(env.sc, true))?;
    let (lr, _) = loader.join().map_err(|_| anyhow!("loader panicked"))?;
    write_series(env.dir, &on.stall, &on.speech, &on.woke_late)?;
    focus_metrics(sum, &on, crate::arms::is_candidate(env.arm));
    sum.metric("a5_writer_turn_major_faults", on.turn_major_faults as f64);
    sum.metric(
        "a5_loader_load_ns",
        lr.as_ref().map(|x| x.0 as f64).unwrap_or(-1.0),
    );
    sum.check(
        "a5_no_major_fault_on_writer_turn",
        on.turn_major_faults == 0,
    );
    sum.check("a5_loader_completed", lr.is_ok());
    sum.notes.push("A5 pass = no major fault and no file read/deserialize on a writer turn; strace proof: host/strace-writer.sh".into());
    Ok(())
}

/// PF-1: facts of a deterministic view corpus, written as digests for the cross-arm differential.
pub fn run_pf1(env: &RunEnv<'_>, sum: &mut Summary) -> Result<()> {
    let fx = fixture_for(env)?;
    note_fixture(sum, &fx);
    let mut c = LegCfg::from(env.sc, true);
    c.deterministic = true;
    c.samples = if env.sc.runs > 0 { env.sc.runs } else { 2000 };
    c.warmup = 200;
    c.interval_us = 100;
    let on = run_leg(&fx, env.factory, &c)?;
    let txt: String = on.digests.iter().map(|d| format!("{d:016x}\n")).collect();
    std::fs::write(env.dir.join("pf1.digests"), txt)?;
    sum.metric("pf1_views", on.digests.len() as f64);
    sum.metric("pf1_quiesce_timeouts", on.pf1_timeouts as f64);
    sum.check("pf1_all_views_quiesced", on.pf1_timeouts == 0);
    focus_metrics(sum, &on, crate::arms::is_candidate(env.arm));
    Ok(())
}
