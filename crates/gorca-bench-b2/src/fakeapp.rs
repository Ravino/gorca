// SPDX-License-Identifier: GPL-3.0-or-later
//! Scenarios that run the Python fake apps of `tests/harness` (storm, mixed, hung) against the
//! drain under test, inside `tests/harness/run-isolated.sh`. A truth tap on the same bus
//! provides the ground truth and the arrival times.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gorca_dbus::drain::{DrainConfig, default_match_rules, spawn_drain};

use crate::clock::now_ns;
use crate::limits as L;
use crate::model::compare;
use crate::procfs::{CpuSnapshot, reset_rss_peak, rss_bytes, rss_peak_bytes};
use crate::report::Row;
use crate::rig::{ConsumerCfg, HarnessRouter};
use crate::stats::summarize;
use crate::tap::Tap;

fn json_str(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let rest = &line[line.find(&pat)? + pat.len()..];
    let rest = rest
        .trim_start()
        .strip_prefix(':')?
        .trim_start()
        .strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

fn json_num(line: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\"");
    let rest = &line[line.find(&pat)? + pat.len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E' | '+')))
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

struct FakeApp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    pub bus_name: String,
}

impl FakeApp {
    fn spawn(py: &Path, args: &[&str]) -> Result<Self> {
        let mut child = Command::new("/usr/bin/python3")
            .arg(py)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("starting {}", py.display()))?;
        let stdin = child.stdin.take();
        let out = child.stdout.take().context("no stdout")?;
        let (tx, lines) = channel();
        std::thread::Builder::new()
            .name("hx-fa-read".to_owned())
            .spawn(move || {
                for l in BufReader::new(out).lines().map_while(Result::ok) {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
            })?;
        let mut app = Self {
            child,
            stdin,
            lines,
            bus_name: String::new(),
        };
        let ready = app.wait_line(
            |l| l.contains("\"ready\"") || l.contains("\"error\""),
            Duration::from_secs(60),
        )?;
        if !ready.contains("\"ready\"") {
            bail!("fake app failed to start: {ready}");
        }
        app.bus_name = json_str(&ready, "bus_name").context("no bus_name in ready line")?;
        Ok(app)
    }

    fn wait_line(&mut self, pred: impl Fn(&str) -> bool, timeout: Duration) -> Result<String> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left.max(Duration::from_millis(1))) {
                Ok(l) if pred(&l) => return Ok(l),
                Ok(_) => {}
                Err(_) => bail!("timed out waiting for a fake app line"),
            }
        }
    }

    fn cmd(&mut self, json: &str) -> Result<()> {
        let stdin = self.stdin.as_mut().context("stdin closed")?;
        writeln!(stdin, "{json}")?;
        stdin.flush()?;
        Ok(())
    }

    fn signal(&self, sig: &str) {
        let _ = Command::new("kill")
            .arg(format!("-{sig}"))
            .arg(self.child.id().to_string())
            .status();
    }

    fn stop(mut self) {
        drop(self.stdin.take());
        std::thread::sleep(Duration::from_millis(100));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaScenario {
    Storm,
    Mixed,
    Hung,
}

impl FaScenario {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "storm" => Self::Storm,
            "mixed" => Self::Mixed,
            "hung" => Self::Hung,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Storm => "storm",
            Self::Mixed => "mixed",
            Self::Hung => "hung",
        }
    }
}

/// Run one fake-app scenario on the bus named by `address`.
pub fn run(scn: FaScenario, address: &str, secs: u64, fakeapp_py: &Path) -> Result<Vec<Row>> {
    let tag = scn.name();
    let router = HarnessRouter::new();
    router.auto_register(ConsumerCfg {
        fold: true,
        record_focus: true,
        ..ConsumerCfg::default()
    });
    reset_rss_peak();
    let rss0 = rss_bytes();
    let drain = spawn_drain(DrainConfig::new(address), Arc::clone(&router) as _)
        .map_err(|e| anyhow::anyhow!("spawn_drain: {e}"))?;
    let tap = Tap::start(address, &default_match_rules())?;

    let storm_rate = if scn == FaScenario::Storm {
        40_000
    } else {
        20_000
    };
    let dur = secs.to_string();
    let rate = storm_rate.to_string();
    let mut storm = FakeApp::spawn(
        fakeapp_py,
        &[
            "--scenario",
            "storm",
            "--name",
            "gorca-storm",
            "--storm-rate",
            &rate,
            "--storm-duration",
            &dur,
            "--storm-mix",
            "children=1,text=1,state=1,focus=1",
        ],
    )?;
    let mut form = None;
    let mut extra = Vec::new();
    if scn != FaScenario::Storm {
        form = Some(FakeApp::spawn(
            fakeapp_py,
            &["--scenario", "form", "--name", "gorca-form"],
        )?);
        extra.push(FakeApp::spawn(
            fakeapp_py,
            &[
                "--scenario",
                "terminal",
                "--name",
                "gorca-term",
                "--term-gen",
                "20",
                "--term-fps",
                "60",
            ],
        )?);
    }
    if scn == FaScenario::Hung {
        extra.push(FakeApp::spawn(
            fakeapp_py,
            &[
                "--scenario",
                "hung",
                "--name",
                "gorca-hung",
                "--hang",
                "never",
            ],
        )?);
    }
    let cpu0 = CpuSnapshot::take();
    let t0 = now_ns();

    // Drive: focus changes on the form every 50 ms; in `hung`, freeze the storm app for 2 s.
    let mut focus_cmds = 0u32;
    let end = Instant::now() + Duration::from_secs(secs);
    let mut next = Instant::now();
    let mut stopped_at: Option<Instant> = None;
    let mut resumed = false;
    while Instant::now() < end {
        if let Some(f) = form.as_mut()
            && Instant::now() >= next
        {
            let node = if focus_cmds.is_multiple_of(2) {
                "name"
            } else {
                "email"
            };
            f.cmd(&format!(
                "{{\"cmd\":\"focus\",\"node\":\"{node}\",\"id\":{focus_cmds}}}"
            ))?;
            focus_cmds += 1;
            next += Duration::from_millis(50);
        }
        if scn == FaScenario::Hung {
            let el = secs as f64 - end.saturating_duration_since(Instant::now()).as_secs_f64();
            if stopped_at.is_none() && el > secs as f64 * 0.3 {
                storm.signal("STOP");
                stopped_at = Some(Instant::now());
            }
            if let Some(s) = stopped_at
                && !resumed
                && s.elapsed() > Duration::from_secs(2)
            {
                storm.signal("CONT");
                resumed = true;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    if scn == FaScenario::Hung && !resumed {
        storm.signal("CONT");
    }
    let storm_line = storm
        .wait_line(|l| l.contains("storm_done"), Duration::from_secs(secs + 20))
        .unwrap_or_default();
    let emitted_rate = json_num(&storm_line, "rate").unwrap_or(0.0);
    let cpu1 = CpuSnapshot::take();
    let elapsed = (now_ns() - t0) as f64 / 1e9;
    // Silence every app first (the terminal generates until told to stop), then wait.
    for a in std::iter::once(storm).chain(form.take()).chain(extra) {
        a.stop();
    }

    // Quiescence: the tap and the drain stop seeing messages.
    let mut last = (0u64, 0u64);
    let mut still = 0;
    let q_end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < q_end && still < 8 {
        std::thread::sleep(Duration::from_millis(100));
        let now = (
            tap.seen.load(std::sync::atomic::Ordering::Relaxed),
            drain.stats().messages,
        );
        still = if now == last { still + 1 } else { 0 };
        last = now;
    }
    let rss_peak = rss_peak_bytes();
    let drain_end = drain.end();
    let stats = drain.stop();
    let tapped = tap.stop();
    let roles = router.roles();
    let mut results = Vec::new();
    for r in &roles {
        results.push((r.clone(), router.finish(r).unwrap_or_default()));
    }

    // Verdicts per sender.
    let mut failures = 0usize;
    let mut first = String::new();
    let mut compared = 0u64;
    for (role, cons) in &results {
        match tapped.folds.get(role) {
            Some(truth) => {
                let v = compare(truth, &cons.fold);
                if truth.pushed != cons.fold.pushed {
                    eprintln!(
                        "sender {role}: tap {} events, drain delivered {}",
                        truth.pushed, cons.fold.pushed
                    );
                }
                compared += v.compared;
                failures += v.violations.len();
                if first.is_empty() {
                    first = v.violations.first().cloned().unwrap_or_default();
                }
            }
            None => failures += 1,
        }
    }
    failures += usize::from(matches!(
        drain_end,
        gorca_dbus::drain::DrainEnd::Disconnected(_)
    ));
    failures += usize::try_from(stats.malformed + stats.unrouted).unwrap_or(usize::MAX);

    // Focus latency of the form app relative to the tap's arrival time of the same message.
    let mut lat = Vec::new();
    if let Some(form_sender) = results
        .iter()
        .map(|r| r.0.as_str())
        .find(|r| tapped.focus.iter().any(|f| f.1 == *r) && !r.is_empty() && is_form(r, &tapped))
    {
        let obs = results
            .iter()
            .find(|r| r.0 == form_sender)
            .map(|r| r.1.focus_obs.clone())
            .unwrap_or_default();
        // Distinct focus changes as the tap saw them (StateChanged + Focus:Focus of one change
        // arrive within 2 ms and count once).
        let mut changes: Vec<(u64, u64)> = Vec::new();
        for f in tapped.focus.iter().filter(|f| f.1 == form_sender) {
            if changes
                .last()
                .is_some_and(|&(pt, pn)| pn == f.2 && f.0.saturating_sub(pt) < 2_000_000)
            {
                continue;
            }
            changes.push((f.0, f.2));
        }
        // A change superseded by the next one inside a batch has no output of its own; it is
        // skipped, never matched to a later observation of the same node.
        let mut j = 0usize;
        for (k, &(t, n)) in changes.iter().enumerate() {
            let upper = changes.get(k + 1).map_or(u64::MAX, |c| c.0);
            while j < obs.len() && obs[j].0 + 1_000_000 < t {
                j += 1;
            }
            if let Some(o) = obs.get(j)
                && o.1 == n
                && o.0 < upper.saturating_add(1_000_000)
            {
                lat.push(o.0 as f64 - t as f64);
                j += 1;
            }
        }
    }
    let s = summarize(&lat);
    let id = |m: &str| format!("FA.{tag}.{m}");
    let mut rows = vec![
        Row::info(
            &id("failures"),
            "fakeapp",
            "isolated",
            "count",
            &[failures as f64],
        )
        .at_most(0.0)
        .note(format!(
            "compared {compared} nodes over {} senders; {first}",
            roles.len()
        )),
        Row::info(
            &id("storm_emitted_eps"),
            "fakeapp",
            "isolated",
            "events/s",
            &[emitted_rate],
        ),
        Row::info(
            &id("tap_messages"),
            "fakeapp",
            "isolated",
            "messages",
            &[tapped.messages as f64],
        ),
        Row::info(
            &id("drain_messages"),
            "fakeapp",
            "isolated",
            "messages",
            &[stats.messages as f64],
        ),
        Row::info(
            &id("drain_pushed"),
            "fakeapp",
            "isolated",
            "events",
            &[stats.pushed as f64],
        ),
        Row::info(
            &id("drain_core_pct"),
            "fakeapp",
            "isolated",
            "% of one core",
            &[cpu1.ns_since_by_prefix(&cpu0, "gorca-drain") as f64 / 1e9 / elapsed * 100.0],
        ),
        Row::info(
            &id("all_cpu_core_pct"),
            "fakeapp",
            "isolated",
            "% of one core",
            &[cpu1.candidate_ns_since(&cpu0) as f64 / 1e9 / elapsed * 100.0],
        ),
        Row::info(
            &id("rss_delta_mib"),
            "fakeapp",
            "isolated",
            "MiB",
            &[rss_peak.saturating_sub(rss0) as f64 / 1048576.0],
        )
        .at_most(L::T5_RSS_DELTA_MAX_MIB),
    ];
    if !lat.is_empty() {
        rows.push(
            Row::info(
                &id("focus_p50_us"),
                "fakeapp",
                "isolated",
                "us vs tap",
                &[s.p50 / 1e3],
            )
            .note(format!("n={}", s.n)),
        );
        rows.push(
            Row::info(
                &id("focus_p99_us"),
                "fakeapp",
                "isolated",
                "us vs tap",
                &[s.p99 / 1e3],
            )
            .at_most(L::T3_P99_MAX_US),
        );
        rows.push(Row::info(
            &id("focus_p999_us"),
            "fakeapp",
            "isolated",
            "us vs tap",
            &[s.p999 / 1e3],
        ));
        rows.push(Row::info(
            &id("focus_max_us"),
            "fakeapp",
            "isolated",
            "us vs tap",
            &[s.max / 1e3],
        ));
    }
    Ok(rows)
}

/// The form app is the sender whose focus events alternate between exactly two nodes.
fn is_form(sender: &str, t: &crate::tap::TapResult) -> bool {
    let nodes: std::collections::HashSet<u64> = t
        .focus
        .iter()
        .filter(|f| f.1 == sender)
        .map(|f| f.2)
        .collect();
    let n = t.focus.iter().filter(|f| f.1 == sender).count();
    nodes.len() <= 4 && (4..20_000).contains(&n)
}

pub fn default_fakeapp() -> PathBuf {
    std::env::var_os("B2_FAKEAPP")
        .map_or_else(|| PathBuf::from("tests/harness/fakeapp.py"), PathBuf::from)
}
