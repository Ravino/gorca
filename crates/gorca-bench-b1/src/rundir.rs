// SPDX-License-Identifier: GPL-3.0-or-later
//! The run-directory contract between the walk driver (H-3, test-engineer), the daemon (PW-10) and
//! this harness, and the evaluation of one run into gate metrics.
//!
//! One run (`walk`, `ff`, `idle`, `big`, `soak`, `step`) of one arm writes a directory:
//!
//! | file | content |
//! |---|---|
//! | `truth.jsonl` | fake-app truth log (SPEC 6.1), see [`crate::verdict`] |
//! | `present.jsonl` | daemon presentation log (PW-10) |
//! | `counters.json` | scalars below; every key optional unless the run kind needs it |
//! | `samples.json` | optional tap-to-SPEAK latency samples in ms (override the log-derived ones) |
//! | `step.json` | `step` runs: output of the step bench |
//!
//! `counters.json` keys: `duration_s`; `calls` = `{app: {member: n}}` (fake-app counters);
//! `signals_received` (drain), `signals_monitored` (monitor under gorca's rules); `storm_app`,
//! `l3_window_s`, `l3_calls_to_storm`, `l3_lost`; `ancestry_demands`, `ancestry_no_ipc`;
//! `idle_window_s`, `idle_apps` (list); `rss_before_kib`, `rss_peak_kib`, `rss_series_kib` =
//! `[[t_s, kib], ...]`; `cpu_ns`; `l4_announce_ms`; `startup` = `{model: {calls, bytes}}`;
//! `scripted` / `actual` = `{counter: n}` (a run with `actual < scripted` for any key is invalid).
//!
//! `samples.json` keys: `L1h_ms`, `L1c_ms`, `S1_ms`, `L4_ms` (arrays), `FF_ms` = `{model: [..]}`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use gorca_bench_b2::stats::summarize;
use serde_json::Value;

use crate::limits::MIB;
use crate::msgs::Accounting;
use crate::verdict::{self, KeyMode, Kind, Source};

/// Metrics of one run plus why it might be invalid.
#[derive(Clone, Debug, Default)]
pub struct RunMetrics {
    /// Gate id (with `@model` suffix where per-model) -> value in the gate's unit.
    pub metrics: BTreeMap<String, f64>,
    /// Non-empty = the run is invalid and excluded from medians (and fails `B1-RUNS.invalid`).
    pub invalid: Vec<String>,
    /// Informational values that have no gate (corrections split, superseded focus, ...).
    pub info: BTreeMap<String, f64>,
}

fn read_opt(dir: &Path, name: &str) -> Result<Option<String>> {
    match std::fs::read_to_string(dir.join(name)) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {name}")),
    }
}

fn json_opt(dir: &Path, name: &str) -> Result<Value> {
    match read_opt(dir, name)? {
        Some(s) => serde_json::from_str(&s).with_context(|| format!("{name}: not JSON")),
        None => Ok(Value::Null),
    }
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64)
}

fn floats(v: Option<&Value>) -> Vec<f64> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn p99(samples: &[f64]) -> Option<f64> {
    (!samples.is_empty()).then(|| summarize(samples).p99)
}

fn accounting(c: &Value) -> Accounting {
    let mut a = Accounting {
        signals_received: num(c, "signals_received").unwrap_or(0.0) as u64,
        signals_monitored: num(c, "signals_monitored").unwrap_or(0.0) as u64,
        window_s: num(c, "duration_s").unwrap_or(0.0),
        ..Accounting::default()
    };
    if let Some(apps) = c.get("calls").and_then(Value::as_object) {
        for (app, members) in apps {
            for (m, n) in members.as_object().into_iter().flatten() {
                a.calls
                    .insert((app.clone(), m.clone()), n.as_u64().unwrap_or(0));
            }
        }
    }
    a
}

fn check_scripted(c: &Value, invalid: &mut Vec<String>) {
    let (Some(s), Some(a)) = (
        c.get("scripted").and_then(Value::as_object),
        c.get("actual").and_then(Value::as_object),
    ) else {
        return;
    };
    for (k, want) in s {
        let want = want.as_f64().unwrap_or(0.0);
        let got = a.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        if got < want {
            invalid.push(format!("scripted {k}: {got} < {want}"));
        }
    }
}

/// Evaluate the run directory `dir` of kind `kind`.
pub fn evaluate(kind: &str, dir: &Path, mode: KeyMode) -> Result<RunMetrics> {
    let c = json_opt(dir, "counters.json")?;
    let samples = json_opt(dir, "samples.json")?;
    let mut out = RunMetrics::default();
    check_scripted(&c, &mut out.invalid);
    let acct = accounting(&c);
    if (acct.signals_received > 0 || acct.signals_monitored > 0)
        && let Err(e) = acct.cross_check()
    {
        out.invalid.push(e.to_string());
    }

    match kind {
        "walk" | "soak" | "ff" => {
            let mut truth =
                verdict::parse_truth(&read_opt(dir, "truth.jsonl")?.unwrap_or_default())
                    .context("truth.jsonl")?;
            let present = read_opt(dir, "present.jsonl")?.unwrap_or_default();
            if let Some(key) = verdict::redaction_key(&present) {
                truth.redact(&key);
            }
            let pres = verdict::parse_presentations(&present).context("present.jsonl")?;
            let r = verdict::classify(&truth, &pres, mode);
            let stale = r.count(verdict::Verdict::Stale) as f64;
            match kind {
                "soak" => out.metrics.insert("B1-STALE@soak".into(), stale),
                "walk" => out.metrics.insert("B1-STALE@walk".into(), stale),
                _ => out.info.insert("stale_at_ff".into(), stale),
            };
            out.info
                .insert("correct".into(), r.count(verdict::Verdict::Correct) as f64);
            out.info
                .insert("refined".into(), r.count(verdict::Verdict::Refined) as f64);
            out.info
                .insert("late".into(), r.count(verdict::Verdict::Late) as f64);
            out.info
                .insert("focus_presentations".into(), r.focus_presentations() as f64);
            out.info
                .insert("focus_superseded".into(), r.superseded as f64);
            out.info.insert(
                "refine_epoch_mismatch".into(),
                r.refine_epoch_mismatch as f64,
            );
            out.info
                .insert("orphan_followups".into(), r.orphan_followups as f64);
            if kind == "ff" {
                ff_metrics(&c, &samples, &mut out);
            }
            if kind == "walk" {
                walk_metrics(&c, &samples, &r, &acct, &mut out);
            }
            if kind != "ff" && r.focus_presentations() == 0 {
                out.invalid.push("no focus presentation in the log".into());
            }
            if kind == "walk" {
                out.metrics.insert("B1-LOST@walk".into(), r.lost as f64);
            }
        }
        "idle" => {
            let win = num(&c, "idle_window_s").unwrap_or(0.0);
            let apps: Vec<String> = c
                .get("idle_apps")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if win <= 0.0 || apps.is_empty() {
                out.invalid
                    .push("idle run needs idle_window_s and idle_apps".into());
            } else {
                // Worst app, not the mean: the gate is per app.
                let worst = apps
                    .iter()
                    .map(|a| acct.calls_of(a) as f64 / win)
                    .fold(0.0, f64::max);
                out.metrics.insert("B1-MSG.idle".into(), worst);
                out.info.insert(
                    "idle_signals_per_s".into(),
                    acct.signals_received as f64 / win,
                );
            }
        }
        "big" => {
            if let (Some(b), Some(p)) = (num(&c, "rss_before_kib"), num(&c, "rss_peak_kib")) {
                out.metrics
                    .insert("B1-RSS.big".into(), (p - b) * 1024.0 / MIB);
            } else {
                out.invalid
                    .push("big run needs rss_before_kib and rss_peak_kib".into());
            }
        }
        _ => {}
    }
    if kind == "soak" {
        match rss_growth_mib(&c) {
            Some(g) => {
                out.metrics.insert("B1-RSS.soak".into(), g);
            }
            None => out
                .invalid
                .push("soak run needs rss_series_kib covering minutes 10..60".into()),
        }
    }
    if kind == "step" {
        let s = json_opt(dir, "step.json")?;
        let rounds = s.get("rounds").and_then(Value::as_array);
        let maxes: Vec<f64> = rounds
            .into_iter()
            .flatten()
            .filter_map(|r| num(r, "max_us"))
            .collect();
        if maxes.is_empty() {
            out.invalid.push("step.json has no rounds".into());
        } else {
            // Median over rounds of the per-round worst step (SPEC 8.3: 7 rounds, median).
            out.metrics.insert(
                "B1-STEP.max_us".into(),
                gorca_bench_b2::stats::spread(&maxes).median,
            );
        }
    }
    Ok(out)
}

fn walk_metrics(
    c: &Value,
    samples: &Value,
    r: &verdict::Report,
    acct: &Accounting,
    out: &mut RunMetrics,
) {
    let derived_h = r.latencies_ms(|j| j.kind == Kind::Focus && j.source == Source::Hit);
    let derived_c = r.latencies_ms(|j| j.kind == Kind::Focus && j.source == Source::MissReply);
    let derived_s1 = r.latencies_ms(|j| j.kind == Kind::StateChange);
    let pick = |key: &str, derived: Vec<f64>| {
        let s = floats(samples.get(key));
        if s.is_empty() { derived } else { s }
    };
    // Tap-to-SPEAK samples win; the log-derived ones (fake-app emit to SPEAK) include bus transit.
    out.info.insert(
        "latency_source_tap".into(),
        f64::from(samples.get("L1h_ms").is_some()),
    );
    for (id, key, derived) in [
        ("B1-L1h.p99", "L1h_ms", derived_h),
        ("B1-L1c.p99", "L1c_ms", derived_c),
        ("B1-S1.p99", "S1_ms", derived_s1),
    ] {
        let s = pick(key, derived);
        out.info.insert(format!("{id}.n"), s.len() as f64);
        if let Some(v) = p99(&s) {
            out.metrics.insert(id.into(), v);
            out.info.insert(id.replace("p99", "p50"), summarize(&s).p50);
        }
    }
    out.metrics.insert("B1-HIT.all".into(), r.hit_all());
    let (hr, n) = r.hit_revisit();
    out.info.insert("revisits".into(), n as f64);
    if n > 0 {
        out.metrics.insert("B1-HIT.revisit".into(), hr);
    }
    let (m6, ev, sil) = r.m6();
    out.metrics.insert("B1-M6".into(), m6);
    let (aud, n_aud) = r.m6_audible();
    out.metrics.insert("B1-M6.audible".into(), aud);
    out.info.insert("m6_audible".into(), n_aud as f64);
    let mut delays = r.refine_delays_ms();
    delays.sort_by(f64::total_cmp);
    if let Some(p99) = delays.get(delays.len().saturating_sub(1) * 99 / 100) {
        out.info.insert("refine_delay_p99_ms".into(), *p99);
        out.info
            .insert("refine_delay_max_ms".into(), delays[delays.len() - 1]);
    }
    out.info.insert("m6_evented".into(), ev as f64);
    out.info.insert("m6_silent".into(), sil as f64);
    if let (Some(d), Some(n)) = (num(c, "ancestry_demands"), num(c, "ancestry_no_ipc"))
        && d > 0.0
    {
        out.metrics.insert("B1-HIT.ancestry".into(), n / d);
    }
    if acct.window_s > 0.0 {
        out.metrics.insert("B1-MSG.walk".into(), acct.msgs_per_s());
        let f = r.focus_presentations().max(1) as f64;
        out.info
            .insert("calls_per_focus".into(), acct.total_calls() as f64 / f);
    }
    if let (Some(cpu), n) = (num(c, "cpu_ns"), r.focus_presentations())
        && n > 0
    {
        out.metrics
            .insert("B1-CPU.focus_us".into(), cpu / 1e3 / n as f64);
    }
    // L3: calls/s the daemon sent to the storm app inside the L3 window.
    if let (Some(calls), Some(win)) = (num(c, "l3_calls_to_storm"), num(c, "l3_window_s"))
        && win > 0.0
    {
        out.metrics.insert("B1-MSG.l3".into(), calls / win);
    }
    if let Some(l) = num(c, "l3_lost") {
        out.metrics.insert("B1-L3.lost".into(), l);
    }
    let l4 = floats(samples.get("L4_ms"));
    if let Some(v) = p99(&l4) {
        out.metrics.insert("B1-L4.p99".into(), v);
    }
    if let Some(v) = num(c, "l4_announce_ms") {
        out.metrics.insert("B1-L4.announce".into(), v);
    }
}

fn ff_metrics(c: &Value, samples: &Value, out: &mut RunMetrics) {
    if let Some(models) = samples.get("FF_ms").and_then(Value::as_object) {
        for (m, v) in models {
            let s = floats(Some(v));
            if let Some(x) = p99(&s) {
                out.metrics.insert(format!("B1-FF.p99@{m}"), x);
                out.info.insert(format!("B1-FF.p50@{m}"), summarize(&s).p50);
            }
        }
    }
    if let Some(st) = c.get("startup").and_then(Value::as_object) {
        for (m, v) in st {
            if let Some(x) = num(v, "calls") {
                out.metrics.insert(format!("B1-MSG.startup@{m}"), x);
            }
            if let Some(x) = num(v, "bytes") {
                out.info.insert(format!("startup_bytes@{m}"), x);
            }
        }
    }
}

/// RSS growth in MiB from minute 10 to the last sample at or after minute 60 (or the last one).
pub fn rss_growth_mib(c: &Value) -> Option<f64> {
    let series: Vec<(f64, f64)> = c
        .get("rss_series_kib")?
        .as_array()?
        .iter()
        .filter_map(|p| {
            let a = p.as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
        })
        .collect();
    let at = |t: f64| series.iter().find(|(x, _)| *x >= t).copied();
    let (t0, r0) = at(600.0)?;
    let (t1, r1) = series.last().copied()?;
    if t1 < t0 + 1800.0 {
        return None;
    }
    Some((r1 - r0) * 1024.0 / MIB)
}

/// Read `bench/results/.../*.json` rows into `{(id, bus): value}` for the R0 comparison.
pub fn load_baseline(path: &Path) -> Result<BTreeMap<(String, String), f64>> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let rows = v.get("rows").and_then(Value::as_array);
    let Some(rows) = rows else {
        bail!("{}: no `rows`", path.display());
    };
    let mut m = BTreeMap::new();
    for r in rows {
        if let (Some(id), Some(bus), Some(val)) = (
            r.get("id").and_then(Value::as_str),
            r.get("bus").and_then(Value::as_str),
            r.get("value").and_then(Value::as_f64),
        ) {
            m.insert((id.to_owned(), bus.to_owned()), val);
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("b1-rundir-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn idle_reports_the_worst_app() {
        let d = tmp("idle");
        std::fs::write(
            d.join("counters.json"),
            r#"{"idle_window_s": 60, "idle_apps": ["shell","settings"],
                "calls": {"shell": {"GetName": 12}, "settings": {"GetName": 90}}}"#,
        )
        .unwrap();
        let m = evaluate("idle", &d, KeyMode::PathOnly).unwrap();
        assert!((m.metrics["B1-MSG.idle"] - 1.5).abs() < 1e-9);
        assert!(m.invalid.is_empty());
    }

    #[test]
    fn scripted_shortfall_and_signal_mismatch_invalidate() {
        let d = tmp("invalid");
        std::fs::write(
            d.join("counters.json"),
            r#"{"idle_window_s": 60, "idle_apps": ["a"], "signals_received": 100, "signals_monitored": 150,
                "scripted": {"focus": 10}, "actual": {"focus": 9}}"#,
        )
        .unwrap();
        let m = evaluate("idle", &d, KeyMode::PathOnly).unwrap();
        assert_eq!(m.invalid.len(), 2, "{:?}", m.invalid);
    }

    #[test]
    fn rss_growth_needs_the_full_window() {
        let ok: Value = serde_json::from_str(
            r#"{"rss_series_kib": [[0, 100], [600, 50000], [2400, 51024], [3600, 57168]]}"#,
        )
        .unwrap();
        assert!((rss_growth_mib(&ok).unwrap() - 7.0).abs() < 1e-6);
        let short: Value =
            serde_json::from_str(r#"{"rss_series_kib": [[600, 1], [900, 2]]}"#).unwrap();
        assert!(rss_growth_mib(&short).is_none());
    }

    #[test]
    fn walk_run_end_to_end_on_hand_made_logs() {
        let d = tmp("walk");
        std::fs::write(
            d.join("truth.jsonl"),
            "{\"t_ns\":1000000,\"focus\":true,\"path\":\"/a\"}\n{\"t_ns\":50000000,\"focus\":true,\"path\":\"/a\"}\n",
        )
        .unwrap();
        let p = |t: u64, id: u64, src: &str| {
            format!(
                "{{\"t_ns\":{t},\"utt_id\":{id},\"epoch\":1,\"app_bus\":\":1.1\",\"path\":\"/a\",\"source\":\"{src}\",\"role\":\"button\",\"states\":[],\"name\":\"n\",\"value\":null}}"
            )
        };
        std::fs::write(
            d.join("present.jsonl"),
            format!(
                "{}\n{}\n",
                p(4_000_000, 1, "miss_reply"),
                p(50_800_000, 2, "hit")
            ),
        )
        .unwrap();
        std::fs::write(
            d.join("counters.json"),
            r#"{"duration_s": 10, "calls": {"a": {"GetName": 10}}, "signals_received": 50, "signals_monitored": 50}"#,
        )
        .unwrap();
        let m = evaluate("walk", &d, KeyMode::PathOnly).unwrap();
        assert!(m.invalid.is_empty(), "{:?}", m.invalid);
        assert_eq!(m.metrics["B1-STALE@walk"], 0.0);
        assert_eq!(m.metrics["B1-LOST@walk"], 0.0);
        assert!((m.metrics["B1-L1h.p99"] - 0.8).abs() < 1e-6);
        assert!((m.metrics["B1-L1c.p99"] - 3.0).abs() < 1e-6);
        assert!((m.metrics["B1-HIT.all"] - 0.5).abs() < 1e-9);
        assert!((m.metrics["B1-HIT.revisit"] - 1.0).abs() < 1e-9);
        assert!(
            (m.metrics["B1-MSG.walk"] - 7.0).abs() < 1e-9,
            "2*10+50 over 10 s"
        );
    }
}
