// SPDX-License-Identifier: GPL-3.0-or-later
//! `e2e-m8`: the M8 (NVDA browse-mode parity) gate runner. Run it inside the private sessions:
//!
//! ```text
//! tests/web/m8/run_m8.sh <label> [--quick] [--repeats N] [--only M8-QN,M8-TB] [--no-fail]
//! target/release/e2e-m8 list            the scenario and step table (no rig needed)
//! ```
//!
//! Results: `bench/results/e2e/<label>/<UTC>.{json,txt}` (the standard gate rows) and
//! `<UTC>-steps.md` (one row per step with its status in every repeat, the median latency and what
//! was heard). The gate rules:
//!
//! | row | rule |
//! |---|---|
//! | `<scn>.fail` | 0 in every repeat: a step that is neither passing nor marked expected-fail |
//! | `<scn>.key_ms.p99` | at most 5 ms (ADR-0003 14: key to speech) |
//! | `<scn>.mode_ms.p99` | at most 15 ms (mode switch to earcon and speech) |
//! | `<scn>.live_ms.p99` | at most 65 ms (LV1, AM-9) plus the rig allowance [`LIVE_RIG_ALLOW_MS`] |
//! | `M8-PF1.w1_100k_ms.p99` | at most 5 ms at 100k nodes; `lost_w1_100k_ms` 0 |
//! | `M8-PF2.tb1k_ms.p99` | at most 5 ms on a 12k-cell table; `lost_tb1k_ms` 0 (expected fail until M8 tables land) |
//!
//! `xfail` and `xpass` are informational: an XPASS is the signal to remove the step's mark.
//! Everything is skipped unless `GORCA_M8=1` and the private compositor are present.

#![allow(clippy::cast_precision_loss, clippy::match_same_arms)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use gorca_bench::e2e::apps::ReaderSpec;
use gorca_bench::e2e::host;
use gorca_bench::e2e::mock::Flavor;
use gorca_bench::e2e::runner::HostEnv;
use gorca_bench::e2e::scenarios::{RunOut, Size};
use gorca_bench::e2e::web::m8::{self, Scenario, Status, StepRes};
use gorca_bench_b2::report::{Row, Verdict, json, table};
use gorca_bench_b2::stats::{Summary, summarize};

/// ADR-0003 14: quick nav, line and table moves, gorca side.
const KEY_P99_MS: f64 = 5.0;
/// Mode switch to earcon and speech.
const MODE_P99_MS: f64 = 15.0;
/// LV1 (ADR-0003 21 AM-9): last mutation to speech 65 ms. The rig measures from the AT-SPI
/// `DoAction` call, which adds the page's click handler and the browser's event dispatch (observed
/// 5 to 30 ms); the allowance is revisited when the page can timestamp its own mutations.
const LIVE_RIG_ALLOW_MS: f64 = 35.0;
const LIVE_P99_MS: f64 = 65.0 + LIVE_RIG_ALLOW_MS;

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
    samples: BTreeMap<(String, String), Vec<Summary>>,
    scalars: BTreeMap<(String, String), (Vec<f64>, String)>,
    ok_runs: BTreeMap<String, usize>,
    failed_runs: BTreeMap<String, usize>,
    notes: Vec<String>,
    /// (scenario, step id) -> one result per repeat
    steps: BTreeMap<(String, String), Vec<StepRes>>,
    order: Vec<(String, String)>,
}

impl Acc {
    fn add(&mut self, sid: &str, out: RunOut, steps: Vec<StepRes>) {
        for (m, v) in out.samples {
            self.samples
                .entry((sid.to_owned(), m))
                .or_default()
                .push(summarize(&v));
        }
        for (m, v, u) in out.scalars {
            let e = self
                .scalars
                .entry((sid.to_owned(), m))
                .or_insert_with(|| (Vec::new(), u));
            e.0.push(v);
        }
        for n in out.notes {
            if !self.notes.contains(&n) {
                self.notes.push(n);
            }
        }
        for r in steps {
            let key = (sid.to_owned(), r.id.to_owned());
            if !self.steps.contains_key(&key) {
                self.order.push(key.clone());
            }
            self.steps.entry(key).or_default().push(r);
        }
        *self.ok_runs.entry(sid.to_owned()).or_default() += 1;
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

fn rows(acc: &Acc, label: &str, scns: &[Scenario]) -> Vec<Row> {
    let mut out = Vec::new();
    for sc in scns {
        let sid = sc.id().to_owned();
        for ((s, metric), runs) in &acc.samples {
            if *s != sid {
                continue;
            }
            for p in ["n", "p50", "p95", "p99", "max"] {
                let vals: Vec<f64> = runs.iter().map(|r| pct_of(r, p)).collect();
                let id = format!("{sid}.{metric}.{p}");
                let unit = if p == "n" { "samples" } else { "ms" };
                let mut row = Row::info(&id, &sid, label, unit, &vals);
                let thr = match (metric.as_str(), p) {
                    ("key_ms" | "w1_100k_ms", "p99") => Some(KEY_P99_MS),
                    ("tb1k_ms", "p99") if sc.xfail().is_none() => Some(KEY_P99_MS),
                    ("mode_ms", "p99") => Some(MODE_P99_MS),
                    ("live_ms", "p99") => Some(LIVE_P99_MS),
                    _ => None,
                };
                if let Some(t) = thr {
                    row = row.at_most(t);
                }
                if metric == "live_ms" && p == "p99" {
                    row = row.note("65 ms (LV1) + 35 ms rig allowance (DoAction + page handler)");
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
            match metric.as_str() {
                "fail" => {
                    row = row
                        .at_most_in_every_run(0.0)
                        .note("steps that fail and are not marked expected-fail");
                }
                "xpass" => {
                    row = row.note("expected-fail steps that pass: remove their xf() mark");
                }
                "xfail" => row = row.note("expected-fail steps that fail (gap table)"),
                "gap" => row = row.note("core ok, NVDA extras missing"),
                "lost_w1_100k_ms" => row = row.at_most_in_every_run(0.0),
                "lost_tb1k_ms" => {
                    row = match sc.xfail() {
                        None => row.at_most_in_every_run(0.0),
                        Some(why) => row.note(format!("expected fail: {why}")),
                    };
                }
                _ => {}
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

fn median(v: &mut [f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

/// The per-step report: one table row per step, the status of each repeat joined by `/`.
fn steps_md(label: &str, meta: &[(String, String)], acc: &Acc, scns: &[Scenario]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# M8 step results: {label}\n");
    for (k, v) in meta {
        if matches!(
            k.as_str(),
            "label" | "loadavg_end" | "repeats" | "reader" | "backend" | "date" | "host" | "kernel"
        ) || k.starts_with("load")
            || k.contains("governor")
            || k.contains("cpu")
        {
            let _ = writeln!(s, "- {k}: {v}");
        }
    }
    for sc in scns {
        let mine: Vec<&(String, String)> = acc.order.iter().filter(|k| k.0 == sc.id()).collect();
        if mine.is_empty() {
            continue;
        }
        let _ = writeln!(s, "\n## {} ({})\n", sc.id(), sc.area());
        let _ = writeln!(
            s,
            "| step | P | status | ms | what NVDA does | wanted | heard / reason |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|");
        for k in mine {
            let rs = &acc.steps[k];
            let status = rs.iter().map(|r| r.status.label()).collect::<Vec<_>>();
            let status = if status.iter().all(|x| *x == status[0]) {
                status[0].to_owned()
            } else {
                format!("FLAKY {}", status.join("/"))
            };
            let mut lats: Vec<f64> = rs.iter().filter_map(|r| r.lat_ms).collect();
            let lat = median(&mut lats).map_or(String::from("-"), |m| format!("{m:.1}"));
            let pick = rs
                .iter()
                .find(|r| r.status != Status::Pass)
                .unwrap_or(&rs[0]);
            let detail = if pick.status == Status::Pass {
                String::new()
            } else if pick.heard.is_empty() {
                format!("(silent) {}", pick.why)
            } else {
                format!("{} {}", pick.heard, pick.why)
            };
            let esc = |t: &str| t.replace('|', "\\|").replace('\n', " ");
            let _ = writeln!(
                s,
                "| {} | P{} | {} | {} | {} | {} | {} |",
                pick.id,
                pick.prio,
                status,
                lat,
                esc(pick.what),
                esc(&pick.wanted),
                esc(&detail)
            );
        }
    }
    s
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => {
            print!("{}", m8::list());
            return Ok(());
        }
        Some("run") => {}
        _ => bail!(
            "usage: e2e-m8 list | run [--label L] [--quick] [--repeats N] [--reader stub|daemon:PATH] [--backend voicego|ssip] [--only IDS] [--reader-env K=V] [--out DIR] [--no-fail]"
        ),
    }
    if cfg!(debug_assertions) && !args.iter().any(|a| a == "--allow-debug") {
        bail!("debug build: numbers would be meaningless; use --release (or pass --allow-debug)");
    }
    let quick = args.iter().any(|a| a == "--quick");
    let label = arg_value(&args, "--label").unwrap_or_else(|| "m8".to_owned());
    let repeats: usize = arg_value(&args, "--repeats")
        .and_then(|s| s.parse().ok())
        .unwrap_or(if quick { 1 } else { 3 });
    let reader = parse_reader(&arg_value(&args, "--reader").unwrap_or_else(|| "stub".to_owned()))?;
    let flavor = arg_value(&args, "--backend")
        .map(|s| Flavor::parse(&s).context("--backend must be voicego or ssip"))
        .transpose()?
        .unwrap_or(Flavor::Voicego);
    let size = if quick { Size::quick() } else { Size::full() };
    let all = m8::scenarios();
    let scns: Vec<Scenario> = match arg_value(&args, "--only") {
        Some(s) => s
            .split(',')
            .map(|x| {
                all.iter()
                    .find(|sc| sc.id().eq_ignore_ascii_case(x))
                    .copied()
                    .with_context(|| format!("unknown scenario {x}"))
            })
            .collect::<Result<_>>()?,
        None => all,
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
        "== e2e-m8 {label}: reader={} backend={} repeats={repeats} load={load_before}",
        reader.label(),
        flavor.name()
    );

    let mut acc = Acc::default();
    for round in 0..repeats {
        // Rotate the order every repeat so no scenario always runs first or last.
        for k in 0..scns.len() {
            let sc = &scns[(k + round) % scns.len()];
            eprintln!(
                "-- round {}/{repeats} {} (load {})",
                round + 1,
                sc.id(),
                host::loadavg()
            );
            match m8::run(&host_env, &reader, flavor, size, sc) {
                Ok((mut out, steps)) => {
                    m8::latency_series(sc, &steps, &mut out);
                    acc.add(sc.id(), out, steps);
                }
                Err(e) => {
                    eprintln!("   {} FAILED: {e:#}", sc.id());
                    *acc.failed_runs.entry(sc.id().to_owned()).or_default() += 1;
                    acc.notes.push(format!("{} run failed: {e:#}", sc.id()));
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
    let reader_meta = host::reader_meta(&reader, &host_env.reader_env);
    let mut meta = host::meta(&[
        ("label", label.clone()),
        ("reader", reader.label()),
        ("backend", flavor.name().to_owned()),
        ("repeats", repeats.to_string()),
        ("samples_per_series", size.n.to_string()),
        ("quick", quick.to_string()),
        ("loadavg_before", load_before),
        ("loadavg_end", host::loadavg()),
        (
            "gates",
            format!(
                "fail==0 in every repeat; key p99<={KEY_P99_MS}ms, mode p99<={MODE_P99_MS}ms, live p99<={LIVE_P99_MS}ms; median over repeats"
            ),
        ),
    ]);
    meta.extend(reader_meta.into_iter().map(|(k, v)| (k.to_owned(), v)));
    for (i, n) in acc.notes.iter().enumerate() {
        meta.push((format!("note{i}"), n.clone()));
    }
    let text = format!(
        "# e2e-m8 {label}\n# {}\n\n{}",
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
    std::fs::write(
        out_dir.join(format!("{stamp}-steps.md")),
        steps_md(&label, &meta, &acc, &scns),
    )?;
    eprintln!(
        "== wrote {}/{stamp}.{{json,txt}} and -steps.md",
        out_dir.display()
    );
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
