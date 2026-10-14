//! `e2e`: end-to-end latency harness runner (Phase 1). Run it inside the isolated session:
//!
//! ```text
//! tests/harness/run-isolated.sh target/release/e2e run [--label L] [--quick] [--repeats N]
//!       [--reader stub|daemon:/path/to/gorca-daemon[,arg...]] [--backend voicego|ssip]
//!       [--only L1,L2,L3-10k,L3-50k,L4,L4c,K1,L1s,F0] [--out DIR] [--seed S] [--no-fail]
//! ```
//!
//! or simply `bench/e2e/run.sh <label> [--quick]`. Results: `bench/results/e2e/<label>/<UTC>.{json,txt}`.
//! Repeats are interleaved (round-robin over scenarios, rotated every round) and every gate
//! is applied to the **median over repeats** of the per-run percentile, never to a pooled or
//! single number (the B-2 lesson on a loaded host).

#![allow(clippy::cast_precision_loss, clippy::match_same_arms)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use gorca_bench::e2e::apps::ReaderSpec;
use gorca_bench::e2e::host;
use gorca_bench::e2e::mock::Flavor;
use gorca_bench::e2e::runner::HostEnv;
use gorca_bench::e2e::scenarios::{RunOut, Scn, Size, run};
use gorca_bench_b2::report::{Row, Verdict, json, table};
use gorca_bench_b2::stats::{Summary, summarize};

/// Gate thresholds (see bench/e2e/README.md for the rationale). Milliseconds unless noted.
const L1_P99_MS: f64 = 15.0;
const L2_P99_MS: f64 = 5.0;
/// M1 gates (docs/plan/phase-2.md 2.1): ceilings; targets are 2 ms (K2) and 5 ms (K3).
const K2_P99_MS: f64 = 5.0;
const K3_P99_MS: f64 = 15.0;
/// K2 under the L3-50k storm: K2's ceiling plus the 1 ms allowed by ADR-0005 10.1.
const K2S_P99_MS: f64 = 6.0;
/// M4a gate R1 (ADR-0006 18): a review step on a valid model, p99 3 ms (target 1 ms).
const R1_P99_MS: f64 = 3.0;
/// M3 gates (ADR-0007 16), ceilings: caret echo with one fetch, with a window hit, typing echo,
/// selection delta, say-all start, silence after a key press, cursor placement after it.
const C1_P99_MS: f64 = 15.0;
const C1H_P99_MS: f64 = 3.0;
const E1_P99_MS: f64 = 3.0;
const S2_P99_MS: f64 = 15.0;
const SA1_P99_MS: f64 = 15.0;
const SA3_STOP_P99_MS: f64 = 2.0;
/// R1 under the L3-50k storm: R1's ceiling plus the 1 ms allowed by ADR-0006 18.
const R1S_P99_MS: f64 = 4.0;
/// M7a gates (browse-mode design 15): ceilings in ms; W2b is wall time.
const W1_P99_MS: f64 = 5.0;
const FM1_P99_MS: f64 = 15.0;
/// W2b: 150 ms as the design says, measured on gorca-side time (wall minus what Firefox
/// needed to answer the locate walk and the fetch). The wall time stays in `w2b_ms` as
/// information. Blocked on the rig until the per-phase timing shows where the time goes.
const W2B_MS: f64 = 150.0;

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn parse_reader(s: &str) -> Result<ReaderSpec> {
    if s == "stub" {
        return Ok(ReaderSpec::Stub);
    }
    let Some(rest) = s.strip_prefix("daemon:") else {
        bail!("--reader must be `stub` or `daemon:<path>[,arg...]`");
    };
    let mut it = rest.split(',');
    let path = it.next().unwrap_or_default();
    Ok(ReaderSpec::Daemon {
        path: PathBuf::from(path),
        args: it.map(str::to_owned).collect(),
    })
}

#[derive(Default)]
struct Acc {
    /// (scenario, metric) -> per-run summaries
    samples: BTreeMap<(String, String), Vec<Summary>>,
    /// (scenario, metric) -> (per-run values, unit)
    scalars: BTreeMap<(String, String), (Vec<f64>, String)>,
    ok_runs: BTreeMap<String, usize>,
    failed_runs: BTreeMap<String, usize>,
    notes: Vec<String>,
}

impl Acc {
    fn add(&mut self, scn: &str, out: RunOut) {
        for (m, v) in out.samples {
            self.samples
                .entry((scn.to_owned(), m))
                .or_default()
                .push(summarize(&v));
        }
        for (m, v, u) in out.scalars {
            let e = self
                .scalars
                .entry((scn.to_owned(), m))
                .or_insert_with(|| (Vec::new(), u));
            e.0.push(v);
        }
        for n in out.notes {
            if !self.notes.contains(&n) {
                self.notes.push(n);
            }
        }
        *self.ok_runs.entry(scn.to_owned()).or_default() += 1;
    }
}

fn pct_of(s: &Summary, p: &str) -> f64 {
    match p {
        "p50" => s.p50,
        "p95" => s.p95,
        "p99" => s.p99,
        "p999" => s.p999,
        "max" => s.max,
        _ => s.n as f64,
    }
}

fn rows(acc: &Acc, label: &str, scns: &[Scn]) -> Vec<Row> {
    let mut out = Vec::new();
    for scn in scns {
        let sid = scn.id();
        for ((s, metric), runs) in &acc.samples {
            if *s != sid {
                continue;
            }
            for p in ["n", "p50", "p95", "p99", "p999", "max"] {
                let vals: Vec<f64> = runs.iter().map(|r| pct_of(r, p)).collect();
                let id = format!("{sid}.{metric}.{p}");
                let unit = if p == "n" { "samples" } else { "ms" };
                let mut row = Row::info(&id, &sid, label, unit, &vals);
                let thr = match (sid.as_str(), metric.as_str(), p) {
                    ("L1" | "L1s" | "L3-10k" | "L3-50k" | "L4", "l1_ms", "p99") => Some(L1_P99_MS),
                    ("K1", "k1_ms", "p99") => Some(L1_P99_MS),
                    ("L2", "l2_ms", "p99") => Some(L2_P99_MS),
                    ("K2", "k2_ms", "p99") => Some(K2_P99_MS),
                    ("K2s", "k2s_ms", "p99") => Some(K2S_P99_MS),
                    ("K3", "k3_ms", "p99") => Some(K3_P99_MS),
                    ("R1", "r1_ms", "p99") => Some(R1_P99_MS),
                    ("R1s", "r1s_ms", "p99") => Some(R1S_P99_MS),
                    ("W1", "w1_ms", "p99")
                    | ("BR1", "br1_ms" | "br1c_ms" | "br1w_ms", "p99")
                    | ("FM2", "fm2_ms", "p99") => Some(W1_P99_MS),
                    ("FM1", "fm1_ms" | "fm_grab_ms", "p99") | ("W3", "w3_ms", "p99") => {
                        Some(FM1_P99_MS)
                    }
                    ("W2b", "w2b_gorca_ms", "p99") => Some(W2B_MS),
                    ("C1", "c1_ms", "p99") => Some(C1_P99_MS),
                    ("C1h", "c1h_ms", "p99") => Some(C1H_P99_MS),
                    ("E1", "e1_ms", "p99") => Some(E1_P99_MS),
                    ("C1hs", "c1hs_ms", "p99") => Some(C1H_P99_MS + 1.0),
                    ("E1s", "e1s_ms", "p99") => Some(E1_P99_MS + 1.0),
                    ("S2", "s2_ms", "p99") => Some(S2_P99_MS),
                    ("SA1", "sa1_ms", "p99") => Some(SA1_P99_MS),
                    ("SA3", "sa3_stop_ms", "p99") => Some(SA3_STOP_P99_MS),
                    _ => None,
                };
                if let Some(t) = thr {
                    row = row.at_most(t);
                }
                if sid == "L1s" {
                    row = row.note("speech-dispatcher text mock");
                }
                if p == "p99" && metric.starts_with("l1g") {
                    row = row.note("gorca side: bus arrival to SPEAK");
                }
                out.push(row);
            }
        }
        for ((s, metric), (vals, unit)) in &acc.scalars {
            if *s != sid {
                continue;
            }
            let id = format!("{sid}.{metric}");
            let mut row = Row::info(&id, &sid, label, unit, vals);
            if matches!(
                metric.as_str(),
                "lost"
                    | "stale_not_cancelled"
                    | "not_consumed"
                    | "app_lost"
                    | "app_doubled"
                    | "chord_leaked"
                    | "forged_accepted"
                    | "rs_failures"
                    | "restore_failed"
                    | "grab_state_wrong"
                    | "held_keys"
                    | "xkb_stuck"
                    | "key_unbalanced"
                    | "wait_timeouts"
                    | "flap_failed"
                    | "web_failures"
                    | "stale"
                    | "lost_w1_ms"
                    | "lost_br1_ms"
                    | "lost_br1c_ms"
                    | "lost_br1w_ms"
                    | "lost_after_switch"
                    | "e2_missing"
                    | "e2_double"
                    | "e2_keyname"
                    | "e2_wrong"
                    | "pw_plaintext_in_logs"
                    | "pw_echo_missing"
                    | "sa3_caret_wrong"
                    | "sa3a_wrong"
                    | "sa3a_caret_moved_back"
                    | "sa3_not_silenced"
                    | "c1h_ipc_calls"
            ) {
                // Worst repeat, not the median: one lost focus in three runs is a failure.
                row = row.at_most_in_every_run(0.0);
            }
            if sid == "K4" && metric == "chord_resent_per_10k" {
                // A chord that raced a pending grab change is pressed again by the harness; more
                // than a handful per 10k switches would hide a regression that drops commands.
                row = row
                    .at_most_in_every_run(5.0)
                    .note("Ambiguous chords repeated per 10k switches");
            }
            if metric == "sa2_ahead_ratio" {
                row = row
                    .at_least(0.99)
                    .note("next chunk at the server before the previous one ended (ADR-0007 SA2)");
            }
            if metric == "pw_log_lines" {
                row = row
                    .at_least(1.0)
                    .note("the presentation log was written, so an empty grep means something");
            }
            if metric == "superseded_max_run" {
                row = row
                    .at_most_in_every_run(gorca_bench::e2e::scenarios::SUPERSEDED_RUN_MAX)
                    .note("consecutive focus probes overtaken by a newer one (ADR-0001 R-1)");
            }
            if sid == "L4" && metric == "peer_calls_hung" {
                row = row
                    .at_least(1.0)
                    .note("the hang must actually have been exercised");
            }
            if metric == "storm_achieved_eps"
                && let Some((t, _)) = acc.scalars.get(&(sid.clone(), "storm_target_eps".into()))
            {
                let target = t.first().copied().unwrap_or(0.0);
                row = row
                    .at_least(0.9 * target)
                    .note("must reach 90% of the target rate");
            }
            out.push(row);
        }
        let failed = acc.failed_runs.get(&sid).copied().unwrap_or(0);
        let ok = acc.ok_runs.get(&sid).copied().unwrap_or(0);
        if failed > 0 || ok == 0 {
            let mut r = Row::info(
                &format!("{sid}.failed_runs"),
                &sid,
                label,
                "runs",
                &[failed as f64],
            )
            .at_most(0.0);
            r.note = format!("{ok} ok, {failed} failed");
            out.push(r);
        }
    }
    out
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("run") {
        bail!(
            "usage: e2e run [--label L] [--quick] [--repeats N] [--reader stub|daemon:PATH] [--backend voicego|ssip] [--only IDS] [--reader-env K=V] [--out DIR] [--no-fail]"
        );
    }
    if cfg!(debug_assertions) && !args.iter().any(|a| a == "--allow-debug") {
        bail!("debug build: numbers would be meaningless; use --release (or pass --allow-debug)");
    }
    let quick = args.iter().any(|a| a == "--quick");
    let label = arg_value(&args, "--label").unwrap_or_else(|| "adhoc".to_owned());
    let repeats: usize = arg_value(&args, "--repeats")
        .and_then(|s| s.parse().ok())
        .unwrap_or(if quick { 1 } else { 3 });
    let reader = parse_reader(&arg_value(&args, "--reader").unwrap_or_else(|| "stub".to_owned()))?;
    let flavor = arg_value(&args, "--backend")
        .map(|s| Flavor::parse(&s).context("--backend must be voicego or ssip"))
        .transpose()?
        .unwrap_or(Flavor::Voicego);
    let seed: u64 = arg_value(&args, "--seed")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let size = if quick { Size::quick() } else { Size::full() };
    let scns: Vec<Scn> = match arg_value(&args, "--only") {
        Some(s) => s
            .split(',')
            .map(|x| Scn::parse(x).with_context(|| format!("unknown scenario {x}")))
            .collect::<Result<_>>()?,
        None => Scn::all(),
    };
    let mut host_env = HostEnv::from_env()?;
    for (i, a) in args.iter().enumerate() {
        if a == "--reader-env"
            && let Some((k, v)) = args.get(i + 1).and_then(|kv| kv.split_once('='))
        {
            host_env.reader_env.push((k.to_owned(), v.to_owned()));
        }
    }
    let out_dir = PathBuf::from(
        arg_value(&args, "--out").unwrap_or_else(|| format!("bench/results/e2e/{label}")),
    );
    let load_before = host::loadavg();
    eprintln!(
        "== e2e {label}: reader={} backend={} repeats={repeats} load={load_before}",
        reader.label(),
        flavor.name()
    );

    let mut acc = Acc::default();
    for round in 0..repeats {
        // rotate the order every round so no scenario always runs first or last
        for k in 0..scns.len() {
            let scn = &scns[(k + round) % scns.len()];
            let sid = scn.id();
            eprintln!(
                "-- round {}/{repeats} {sid} (load {})",
                round + 1,
                host::loadavg()
            );
            match run(
                &host_env,
                scn,
                &reader,
                flavor,
                size,
                seed + round as u64 * 1000 + k as u64,
            ) {
                Ok(o) => acc.add(&sid, o),
                Err(e) => {
                    eprintln!("   {sid} FAILED: {e:#}");
                    *acc.failed_runs.entry(sid.clone()).or_default() += 1;
                    acc.notes.push(format!("{sid} run failed: {e:#}"));
                }
            }
        }
    }

    let label_bus = format!(
        "{}/{}",
        reader.label().split(':').next().unwrap_or("?"),
        flavor.name()
    );
    let all_rows = rows(&acc, &label_bus, &scns);
    let mut meta = host::meta(&[
        ("label", label.clone()),
        ("reader", reader.label()),
        ("backend", flavor.name().to_owned()),
        ("repeats", repeats.to_string()),
        ("samples_per_series", size.n.to_string()),
        ("l2_trials", size.trials.to_string()),
        ("quick", quick.to_string()),
        ("loadavg_end", host::loadavg()),
        (
            "reader_env",
            host_env
                .reader_env
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        (
            "gates",
            format!(
                "L1/L3/L4/K1 p99<={L1_P99_MS}ms, L2 p99<={L2_P99_MS}ms, lost==0; median over repeats"
            ),
        ),
    ]);
    for (i, n) in acc.notes.iter().enumerate() {
        meta.push((format!("note{i}"), n.clone()));
    }
    let text = format!(
        "# e2e {label}\n# {}\n\n{}",
        meta.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n# "),
        table(&all_rows)
    );
    println!("{text}");
    std::fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let stamp = host::stamp();
    std::fs::write(
        out_dir.join(format!("{stamp}.json")),
        json(&meta, &all_rows),
    )?;
    std::fs::write(out_dir.join(format!("{stamp}.txt")), &text)?;
    eprintln!("== wrote {}/{stamp}.{{json,txt}}", out_dir.display());
    let failed = all_rows
        .iter()
        .filter(|r| matches!(r.verdict, Verdict::Fail))
        .count();
    if failed > 0 {
        eprintln!("== {failed} gate(s) FAILED");
        if !args.iter().any(|a| a == "--no-fail") {
            std::process::exit(1);
        }
    }
    Ok(())
}
