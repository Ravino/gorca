//! Scenario bodies. Each builds a [`Rig`], drives a stimulus, correlates the mock speech
//! server's records with the stimulus timestamps and returns raw samples (milliseconds).
//!
//! | id | what | samples (ms) |
//! |---|---|---|
//! | F0 | harness floor: client -> mock server round trip, no reader | `f0_mock_ms` |
//! | L1 | focus emitted -> SPEAK received, idle | `l1_ms` (emit->SPEAK), `l1g_ms` (bus arrival->SPEAK, i.e. gorca side), `l0_ms` (emit->bus arrival, floor) |
//! | L1s | same against the speech-dispatcher text mock | same |
//! | L2 | newer focus -> STOP of stale speech | `l2_ms` (emit->STOP), `l2s_ms` (emit->new SPEAK) |
//! | L3-10k, L3-50k | L1 while another app floods the a11y bus | as L1 |
//! | L4, L4c | L1 while an app hangs on every D-Bus call (L4) / answers normally (L4c, control) | as L1 |
//! | K1 | emulated Mutter KeyEvent -> SPEAK | `k1_ms` |
//! | K2, K3, K4 | M1 command layer gates, see [`super::m1`]; skipped until M1 lands | `k2_ms`, `k3_ms`, `lost`... |

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};
use gorca_bench_b2::clock::Rng;
use gorca_bench_b2::feeder::{AppPlan, SocketSink, run_feeder};
use gorca_bench_b2::wire::BusConn;
use gorca_bench_b2::workload::{Mix, local_of};

use super::apps::ReaderSpec;
use super::mock::{Flavor, Kind, Rec};
use super::mono::now_ns;
use super::runner::{HostEnv, Rig, RigOpts};

/// First node used by measured series (warm-up uses 1..70).
const FIRST_NODE: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scn {
    F0,
    L1,
    L1s,
    L2,
    L3(u32),
    L4,
    L4c,
    K1,
    K2,
    K3,
    K4,
}

impl Scn {
    pub fn id(&self) -> String {
        match self {
            Self::F0 => "F0".into(),
            Self::L1 => "L1".into(),
            Self::L1s => "L1s".into(),
            Self::L2 => "L2".into(),
            Self::L3(r) => format!("L3-{}k", r / 1000),
            Self::L4 => "L4".into(),
            Self::L4c => "L4c".into(),
            Self::K1 => "K1".into(),
            Self::K2 => "K2".into(),
            Self::K3 => "K3".into(),
            Self::K4 => "K4".into(),
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "F0" => Self::F0,
            "L1" => Self::L1,
            "L1s" => Self::L1s,
            "L2" => Self::L2,
            "L3-10k" => Self::L3(10_000),
            "L3-50k" => Self::L3(50_000),
            "L4" => Self::L4,
            "L4c" => Self::L4c,
            "K1" => Self::K1,
            "K2" => Self::K2,
            "K3" => Self::K3,
            "K4" => Self::K4,
            _ => return None,
        })
    }

    pub fn all() -> Vec<Self> {
        [
            "F0", "L1", "L1s", "L2", "L3-10k", "L3-50k", "L4", "L4c", "K1", "K2", "K3", "K4",
        ]
        .iter()
        .filter_map(|s| Self::parse(s))
        .collect()
    }
}

/// Sizes of a run.
#[derive(Copy, Clone, Debug)]
pub struct Size {
    /// Probes per latency series.
    pub n: usize,
    /// Trials of L2.
    pub trials: usize,
}

impl Size {
    pub fn full() -> Self {
        Self {
            n: 1200,
            trials: 400,
        }
    }

    pub fn quick() -> Self {
        Self { n: 150, trials: 40 }
    }
}

#[derive(Debug, Default)]
pub struct RunOut {
    /// (metric id, samples in ms)
    pub samples: Vec<(String, Vec<f64>)>,
    /// (metric id, value, unit): one number per run
    pub scalars: Vec<(String, f64, String)>,
    pub notes: Vec<String>,
}

impl RunOut {
    pub(super) fn s(&mut self, id: &str, v: Vec<f64>) {
        self.samples.push((id.to_owned(), v));
    }

    pub(super) fn x(&mut self, id: &str, v: f64, unit: &str) {
        self.scalars.push((id.to_owned(), v, unit.to_owned()));
    }
}

pub(super) fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

pub(super) fn pause(rng: &mut Rng, lo_ms: u64, hi_ms: u64) {
    std::thread::sleep(Duration::from_micros(
        lo_ms * 1000 + rng.below((hi_ms - lo_ms) * 1000 + 1),
    ));
}

/// Which node/key a speech text refers to. Understands the stub's tags (`f:<hash>`, `k:<n>`)
/// and the daemon's natural text (`... node-<n> ...`).
struct Decoder {
    hashes: HashMap<u64, usize>,
}

impl Decoder {
    fn new(max_node: usize) -> Self {
        Self {
            hashes: (1..=max_node).map(|i| (local_of(i as u64), i)).collect(),
        }
    }

    fn node(&self, text: &str) -> Option<usize> {
        if let Some(h) = text.strip_prefix("f:") {
            return u64::from_str_radix(h.trim(), 16)
                .ok()
                .and_then(|h| self.hashes.get(&h).copied());
        }
        let i = text.find("node-")? + 5;
        let digits: String = text[i..].chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }
}

pub(super) fn speaks(recs: &[Rec]) -> impl Iterator<Item = (&Rec, &str)> {
    recs.iter().filter_map(|r| match &r.kind {
        Kind::Speak { text, .. } => Some((r, text.as_str())),
        _ => None,
    })
}

pub(super) fn base_opts(reader: &ReaderSpec, flavor: Flavor) -> RigOpts {
    RigOpts {
        flavor,
        reader: reader.clone(),
        calls: false,
        keys: false,
        apps: vec![("app".to_owned(), 3000)],
        hung_apps: Vec::new(),
    }
}

/// One latency series on app `role`: focus `n` distinct nodes with a jittered gap.
struct Probe {
    node: usize,
    t_emit: u64,
}

fn focus_series(
    rig: &mut Rig,
    role: &str,
    n: usize,
    gap: (u64, u64),
    seed: u64,
) -> Result<Vec<Probe>> {
    let mut rng = Rng::new(seed);
    let mut out = Vec::with_capacity(n);
    let app = rig
        .apps
        .get_mut(role)
        .ok_or_else(|| anyhow::anyhow!("no app {role}"))?;
    for i in 0..n {
        let node = FIRST_NODE + i;
        let t = app.focus(node)?;
        if t == 0 {
            bail!("fakeapp.py reported no t_emit_ns: is tests/harness/fakeapp.py current?");
        }
        out.push(Probe { node, t_emit: t });
        pause(&mut rng, gap.0, gap.1);
    }
    Ok(out)
}

/// How an unspoken probe is accounted for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Accounting {
    /// Idle reader: every unspoken probe is `lost`.
    Strict,
    /// Reader under stress (flood, hung peer): a probe is legitimately dropped when a newer focus
    /// event overtakes it before it is processed (ADR-0001 R-1: the ingress slot `Focus` keeps
    /// only the newest node), so the user hears the newest focus and not the stale one. Only an
    /// unspoken probe with no spoken successor is `lost` (the newest focus never reached
    /// speech); the others are `superseded`, and the longest run of them is bounded.
    Coalescing,
}

/// Longest tolerated run of consecutive superseded probes under stress: at the 8-16 ms probe
/// gap, 10 probes are about 120 ms of staleness.
pub const SUPERSEDED_RUN_MAX: f64 = 10.0;

/// `(lost, superseded, longest superseded run)` for probes in emit order; `spoken[i]` says the
/// reader spoke probe `i`.
fn account(spoken: &[bool], mode: Accounting) -> (u64, u64, u64) {
    let last_spoken = spoken.iter().rposition(|s| *s);
    let (mut lost, mut sup, mut run, mut max_run) = (0u64, 0u64, 0u64, 0u64);
    for (i, s) in spoken.iter().enumerate() {
        if *s {
            run = 0;
            continue;
        }
        let overtaken = mode == Accounting::Coalescing && last_spoken.is_some_and(|l| l > i);
        if overtaken {
            sup += 1;
            run += 1;
            max_run = max_run.max(run);
        } else {
            lost += 1;
        }
    }
    (lost, sup, max_run)
}

/// L1 / L0 / L1g samples for a finished series.
fn score_series(
    rig: &Rig,
    role: &str,
    probes: &[Probe],
    mark: usize,
    out: &mut RunOut,
    suffix: &str,
    mode: Accounting,
) {
    let dec = Decoder::new(FIRST_NODE + probes.len() + 8);
    let recs = rig.mock.since(mark);
    let mut first: HashMap<usize, u64> = HashMap::new();
    for (r, text) in speaks(&recs) {
        if let Some(n) = dec.node(text) {
            first.entry(n).or_insert(r.t_ns);
        }
    }
    let bus = rig
        .apps
        .get(role)
        .map(|a| a.bus_name.clone())
        .unwrap_or_default();
    let (mut l1, mut l1g, mut l0) = (Vec::new(), Vec::new(), Vec::new());
    let mut spoken = Vec::with_capacity(probes.len());
    for p in probes {
        let tap = rig.tap.arrival(&bus, p.node);
        if let Some(t) = tap {
            l0.push(ms(t.saturating_sub(p.t_emit)));
        }
        if let Some(&t) = first.get(&p.node) {
            l1.push(ms(t.saturating_sub(p.t_emit)));
            if let Some(a) = tap {
                l1g.push(ms(t.saturating_sub(a)));
            }
        }
        spoken.push(first.contains_key(&p.node));
    }
    let (lost, superseded, max_run) = account(&spoken, mode);
    out.s(&format!("l1_ms{suffix}"), l1);
    out.s(&format!("l1g_ms{suffix}"), l1g);
    out.s(&format!("l0_ms{suffix}"), l0);
    out.x(&format!("lost{suffix}"), lost as f64, "events");
    if mode == Accounting::Coalescing {
        out.x(&format!("superseded{suffix}"), superseded as f64, "events");
        out.x(
            &format!("superseded_max_run{suffix}"),
            max_run as f64,
            "events",
        );
    }
}

pub fn run(
    host: &HostEnv,
    scn: &Scn,
    reader: &ReaderSpec,
    flavor: Flavor,
    size: Size,
    seed: u64,
) -> Result<RunOut> {
    let mut out = RunOut::default();
    match scn {
        Scn::F0 => floor(host, &mut out, flavor, size)?,
        Scn::L1 | Scn::L1s => {
            let fl = if *scn == Scn::L1s {
                Flavor::Ssip
            } else {
                flavor
            };
            let mut rig = Rig::start(host, &base_opts(reader, fl))?;
            rig.mock
                .configure(Duration::from_millis(1), Duration::from_millis(3));
            l1(&mut rig, &mut out, size, seed, "app")?;
        }
        Scn::L2 => {
            let mut rig = Rig::start(host, &base_opts(reader, flavor))?;
            l2(&mut rig, &mut out, size, seed)?;
        }
        Scn::L3(rate) => {
            let mut rig = Rig::start(host, &base_opts(reader, flavor))?;
            rig.mock
                .configure(Duration::from_millis(1), Duration::from_millis(3));
            l3(&mut rig, &mut out, size, seed, f64::from(*rate))?;
        }
        Scn::L4 | Scn::L4c => {
            let mut o = base_opts(reader, flavor);
            o.calls = true;
            if *scn == Scn::L4 {
                o.hung_apps = vec![("sick".to_owned(), 200)];
            } else {
                o.apps.push(("sick".to_owned(), 200));
            }
            let mut rig = Rig::start(host, &o)?;
            rig.mock
                .configure(Duration::from_millis(1), Duration::from_millis(3));
            l4(&mut rig, &mut out, size, seed)?;
        }
        Scn::K1 => {
            let mut o = base_opts(reader, flavor);
            o.keys = true;
            let mut rig = Rig::start(host, &o)?;
            rig.mock
                .configure(Duration::from_millis(1), Duration::from_millis(3));
            k1(&mut rig, &mut out, size, seed)?;
        }
        Scn::K2 => super::m1::k2(host, reader, flavor, size, seed, &mut out)?,
        Scn::K3 => super::m1::k3(host, reader, flavor, size, seed, &mut out)?,
        Scn::K4 => super::m1::k4(host, reader, flavor, size, seed, &mut out)?,
    }
    Ok(out)
}

/// Reader CPU over `f`, minus the idle rate, per event.
fn cpu_around<T>(
    rig: &mut Rig,
    out: &mut RunOut,
    events: impl FnOnce(&T) -> f64,
    f: impl FnOnce(&mut Rig) -> Result<T>,
) -> Result<T> {
    // idle rate first (1 s of nothing)
    let (c0, t0) = (rig.reader.cpu_ns(), now_ns());
    std::thread::sleep(Duration::from_millis(1000));
    let (c1, t1) = (rig.reader.cpu_ns(), now_ns());
    let idle_rate = c1.saturating_sub(c0) as f64 / (t1 - t0) as f64; // cpu-ns per wall-ns
    let (c2, t2) = (rig.reader.cpu_ns(), now_ns());
    let r = f(rig)?;
    let (c3, t3) = (rig.reader.cpu_ns(), now_ns());
    let wall = (t3 - t2) as f64;
    let busy = c3.saturating_sub(c2) as f64 - idle_rate * wall;
    let ev = events(&r).max(1.0);
    out.x(
        "cpu_us_per_event",
        (busy.max(0.0) / ev) / 1000.0,
        "us/event",
    );
    out.x(
        "cpu_pct",
        c3.saturating_sub(c2) as f64 / wall * 100.0,
        "% of one core",
    );
    out.x("idle_cpu_pct", idle_rate * 100.0, "% of one core");
    Ok(r)
}

fn l1(rig: &mut Rig, out: &mut RunOut, size: Size, seed: u64, role: &str) -> Result<()> {
    let mark = rig.mock.mark();
    rig.tap.clear();
    let probes = cpu_around(
        rig,
        out,
        // three AT-SPI signals per focus change: old unfocus, new focus state, Focus
        |p: &Vec<Probe>| p.len() as f64 * 3.0,
        |rig| focus_series(rig, role, size.n, (8, 16), seed),
    )?;
    std::thread::sleep(Duration::from_millis(400));
    score_series(rig, role, &probes, mark, out, "", Accounting::Strict);
    Ok(())
}

fn l2(rig: &mut Rig, out: &mut RunOut, size: Size, seed: u64) -> Result<()> {
    // long utterances: stale speech is certainly still playing when the newer focus arrives
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_secs(3));
    let mut rng = Rng::new(seed);
    let (mut l2, mut l2s) = (Vec::new(), Vec::new());
    let (mut no_cancel, mut no_speak) = (0u64, 0u64);
    let dec = Decoder::new(FIRST_NODE + 2 * size.trials + 8);
    let mark0 = rig.mock.mark();
    for k in 0..size.trials {
        let (a, b) = (FIRST_NODE + 2 * k, FIRST_NODE + 2 * k + 1);
        let mark = rig.mock.mark();
        let app = rig
            .apps
            .get_mut("app")
            .ok_or_else(|| anyhow::anyhow!("no app"))?;
        app.focus(a)?;
        let sa = rig.mock.wait_for(
            mark,
            Duration::from_secs(1),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if dec.node(text) == Some(a)),
        );
        if sa.is_none() {
            no_speak += 1;
            continue;
        }
        pause(&mut rng, 15, 45); // the user keeps moving while A is being spoken
        let mark_b = rig.mock.mark();
        let app = rig
            .apps
            .get_mut("app")
            .ok_or_else(|| anyhow::anyhow!("no app"))?;
        let t_b = app.focus(b)?;
        let cancel = rig.mock.wait_for(mark_b, Duration::from_secs(1), |r| {
            matches!(
                &r.kind,
                Kind::Cancel {
                    had_speech: true,
                    ..
                }
            )
        });
        let speak_b = rig.mock.wait_for(
            mark_b,
            Duration::from_secs(1),
            |r| matches!(&r.kind, Kind::Speak { text, .. } if dec.node(text) == Some(b)),
        );
        match cancel {
            Some(c) => l2.push(ms(c.t_ns.saturating_sub(t_b))),
            None => no_cancel += 1,
        }
        match speak_b {
            Some(s) => l2s.push(ms(s.t_ns.saturating_sub(t_b))),
            None => no_speak += 1,
        }
        pause(&mut rng, 30, 60);
    }
    let _ = mark0;
    out.s("l2_ms", l2);
    out.s("l2s_ms", l2s);
    out.x("lost", (no_cancel + no_speak) as f64, "events");
    out.x("stale_not_cancelled", no_cancel as f64, "trials");
    Ok(())
}

pub(super) struct Storm {
    pub(super) stop: Arc<AtomicBool>,
    pub(super) thread: std::thread::JoinHandle<gorca_bench_b2::feeder::FeedResult>,
}

pub(super) fn start_storm(a11y: &str, rate: f64, secs: f64) -> Result<Storm> {
    let c = BusConn::connect(a11y, Duration::from_secs(5))?;
    let stop = Arc::new(AtomicBool::new(false));
    let st = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("hx-storm".to_owned())
        .spawn(move || {
            let mut plan = AppPlan::new("storm", 7, Mix::Terminal, rate);
            plan.fence = false;
            let mut sink = SocketSink { stream: c.stream };
            run_feeder(&plan, None, &mut sink, Duration::from_secs_f64(secs), &st)
        })?;
    Ok(Storm { stop, thread })
}

fn l3(rig: &mut Rig, out: &mut RunOut, size: Size, seed: u64, rate: f64) -> Result<()> {
    let series_s = size.n as f64 * 0.012 + 1.0;
    let mark = rig.mock.mark();
    rig.tap.clear();
    let storm = start_storm(&rig.env.a11y_addr, rate, series_s + 3.0)?;
    std::thread::sleep(Duration::from_millis(1000)); // let the flood reach steady state
    let (c0, t0) = (rig.reader.cpu_ns(), now_ns());
    let probes = focus_series(rig, "app", size.n, (8, 16), seed)?;
    let (c1, t1) = (rig.reader.cpu_ns(), now_ns());
    std::thread::sleep(Duration::from_millis(300));
    storm.stop.store(true, Ordering::Release);
    let fr = storm
        .thread
        .join()
        .map_err(|_| anyhow::anyhow!("storm thread panicked"))?;
    score_series(rig, "app", &probes, mark, out, "", Accounting::Coalescing);
    out.x("storm_target_eps", rate, "ev/s");
    out.x("storm_achieved_eps", fr.achieved_rate(), "ev/s");
    out.x("storm_max_lag_ms", ms(fr.max_lag_ns), "ms");
    let wall = (t1 - t0) as f64;
    let events = fr.achieved_rate() * wall / 1e9 + probes.len() as f64 * 3.0;
    out.x(
        "cpu_us_per_event",
        (c1.saturating_sub(c0) as f64 / events.max(1.0)) / 1000.0,
        "us/event",
    );
    out.x(
        "cpu_pct",
        c1.saturating_sub(c0) as f64 / wall * 100.0,
        "% of one core",
    );
    Ok(())
}

fn l4(rig: &mut Rig, out: &mut RunOut, size: Size, seed: u64) -> Result<()> {
    let mark = rig.mock.mark();
    rig.tap.clear();
    let mut sick = rig
        .apps
        .remove("sick")
        .ok_or_else(|| anyhow::anyhow!("no sick app"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let st = Arc::clone(&stop);
    // The troubled app keeps emitting events (each batch makes the reader call into it).
    let bg = std::thread::Builder::new()
        .name("hx-sick".to_owned())
        .spawn(move || {
            let mut i = 1usize;
            while !st.load(Ordering::Acquire) {
                i = if i >= 150 { 1 } else { i + 1 };
                // a plain property event: makes the reader call into the app, never speaks
                let _ = sick.proc.send(
                    "emit",
                    &format!("\"node\":{i},\"class\":\"Object\",\"member\":\"StateChanged\",\"detail\":\"showing\",\"d1\":1"),
                );
                std::thread::sleep(Duration::from_millis(40));
            }
            sick
        })?;
    std::thread::sleep(Duration::from_millis(300));
    let probes = focus_series(rig, "app", size.n, (8, 16), seed)?;
    std::thread::sleep(Duration::from_millis(400));
    stop.store(true, Ordering::Release);
    if let Ok(sick) = bg.join() {
        rig.apps.insert("sick".to_owned(), sick);
    }
    // proof that the reader really called into the troubled app (and, for L4, was left hanging)
    if let Some(sick) = rig.apps.get_mut("sick")
        && let Ok(r) = sick.proc.request("stats", "", Duration::from_secs(5))
    {
        out.x(
            "peer_calls_received",
            super::apps::json_num(&r, "calls").unwrap_or(0.0),
            "calls",
        );
        out.x(
            "peer_calls_hung",
            super::apps::json_num(&r, "hung").unwrap_or(0.0),
            "calls",
        );
    }
    score_series(rig, "app", &probes, mark, out, "", Accounting::Coalescing);
    Ok(())
}

/// Key `i` sent `97 + i % 26` as its character at `sent[i].1`. If every spoken text is a single
/// character, return for each key the latency of the first unused echo of its character that
/// arrived at or after the emit time; `None` if there is none (lost). `None` overall when the
/// texts are not single characters.
fn match_by_char(sent: &[(u16, u64)], all: &[(&Rec, &str)], n: usize) -> Option<Vec<Option<f64>>> {
    let single = |t: &str| t.chars().count() == 1;
    if all.is_empty() || !all.iter().all(|(_, t)| single(t)) || n == 0 {
        return None;
    }
    let mut used = vec![false; all.len()];
    let mut out = Vec::with_capacity(sent.len());
    for (i, (_, t)) in sent.iter().enumerate() {
        let want = char::from(b'a' + u8::try_from(i % 26).unwrap_or(0));
        let hit = all
            .iter()
            .enumerate()
            .find(|(j, (r, text))| !used[*j] && r.t_ns >= *t && text.starts_with(want));
        match hit {
            Some((j, (r, _))) => {
                used[j] = true;
                out.push(Some(ms(r.t_ns - *t)));
            }
            None => out.push(None),
        }
    }
    Some(out)
}

fn k1(rig: &mut Rig, out: &mut RunOut, size: Size, seed: u64) -> Result<()> {
    if !rig.keys_ok {
        out.notes.push(
            "K1 skipped: the reader did not register with the emulated keyboard monitor".into(),
        );
        return Ok(());
    }
    let mut rng = Rng::new(seed);
    let mark = rig.mock.mark();
    let mut sent: Vec<(u16, u64)> = Vec::with_capacity(size.n);
    let m = rig
        .mutter
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("no mutter"))?;
    for i in 0..size.n {
        let kc = (10 + i) as u16;
        let (t, n) = m.key(kc, 97 + (i % 26) as u32)?;
        if n == 0 {
            bail!("the emulated monitor had no watcher");
        }
        sent.push((kc, t));
        pause(&mut rng, 8, 16);
    }
    std::thread::sleep(Duration::from_millis(400));
    let recs = rig.mock.since(mark);
    let all: Vec<(&Rec, &str)> = speaks(&recs).collect();
    let tagged = all.iter().filter(|(_, t)| t.starts_with("k:")).count();
    let (mut k1, mut lost) = (Vec::new(), 0u64);
    if tagged * 2 >= all.len() && tagged > 0 {
        let by_kc: HashMap<u16, u64> = all
            .iter()
            .filter_map(|(r, t)| {
                t.strip_prefix("k:")
                    .and_then(|x| x.parse::<u16>().ok())
                    .map(|k| (k, r.t_ns))
            })
            .collect();
        for (kc, t) in &sent {
            match by_kc.get(kc) {
                Some(&s) => k1.push(ms(s.saturating_sub(*t))),
                None => lost += 1,
            }
        }
    } else if let Some(matched) = match_by_char(&sent, &all, size.n) {
        // The daemon echoes the typed character; the keys cycle through 26 letters, so a key and
        // its echo are matched by character and by time (echo after the emit), never by order.
        out.notes
            .push("K1: correlated by echoed character and time".into());
        for m in matched {
            match m {
                Some(ms_) => k1.push(ms_),
                None => lost += 1,
            }
        }
    } else {
        out.notes
            .push("K1: correlated by order (speech text carries no key tag)".into());
        for (i, (_, t)) in sent.iter().enumerate() {
            match all.get(i) {
                Some((r, _)) => k1.push(ms(r.t_ns.saturating_sub(*t))),
                None => lost += 1,
            }
        }
    }
    out.s("k1_ms", k1);
    out.x("lost", lost as f64, "events");
    Ok(())
}

/// F0: how well can this harness measure at all? A client writes SPEAK frames straight to the
/// mock server (no reader, no bus); the server stamps arrival.
fn floor(host: &HostEnv, out: &mut RunOut, flavor: Flavor, size: Size) -> Result<()> {
    let path = host.sock_dir.join("floor.sock");
    let mock = super::mock::MockServer::start(&path, flavor)?;
    mock.configure(Duration::from_millis(1), Duration::from_millis(1));
    let mut s = UnixStream::connect(mock.path())?;
    let binary = flavor == Flavor::Voicego;
    if binary {
        s.write_all(&[0xAA, b'V', b'G', 1])?;
    }
    let send = |s: &mut UnixStream, line: &str| -> Result<()> {
        if binary {
            let mut v = (line.len() as u32).to_be_bytes().to_vec();
            v.extend_from_slice(line.as_bytes());
            s.write_all(&v)?;
        } else {
            s.write_all(format!("{line}\r\n").as_bytes())?;
        }
        Ok(())
    };
    // drain replies on a thread so the mock never blocks on us
    let mut rd = s.try_clone()?;
    let drain = std::thread::spawn(move || {
        let mut b = vec![0u8; 8192];
        while matches!(rd.read(&mut b), Ok(n) if n > 0) {}
    });
    let mut rng = Rng::new(3);
    let mut v = Vec::new();
    for i in 0..size.n.max(200) {
        let t0 = now_ns();
        let mark = mock.mark();
        send(&mut s, "SPEAK 4")?;
        // the body follows the 230 in the real protocol; the floor does not wait for it
        if binary {
            send(&mut s, &format!("f{:03}", i % 1000))?;
        } else {
            s.write_all(b"abcd\r\n.\r\n")?;
        }
        if let Some(r) = mock.wait_for(mark, Duration::from_secs(1), |r| {
            matches!(r.kind, Kind::Speak { .. })
        }) {
            v.push(ms(r.t_ns.saturating_sub(t0)));
        }
        pause(&mut rng, 2, 6);
    }
    let _ = s.shutdown(std::net::Shutdown::Both);
    let _ = drain.join();
    out.s("f0_mock_ms", v);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounting_separates_superseded_from_lost() {
        let t = true;
        let f = false;
        // idle: every unspoken probe is lost
        assert_eq!(account(&[t, f, t, f], Accounting::Strict), (2, 0, 0));
        // stress: only the unspoken tail (no spoken successor) is lost
        assert_eq!(account(&[t, f, t, f], Accounting::Coalescing), (1, 1, 1));
        assert_eq!(account(&[f, f, t, f, t], Accounting::Coalescing), (0, 3, 2));
        // a reader that spoke nothing lost everything
        assert_eq!(account(&[f, f, f], Accounting::Coalescing), (3, 0, 0));
        assert_eq!(account(&[], Accounting::Coalescing), (0, 0, 0));
    }

    #[test]
    fn decoder_understands_stub_tags_and_natural_text() {
        let d = Decoder::new(50);
        assert_eq!(d.node(&format!("f:{:016x}", local_of(7))), Some(7));
        assert_eq!(d.node("node-12, list item"), Some(12));
        assert_eq!(d.node("f:zz"), None);
        assert_eq!(d.node("hello"), None);
    }

    #[test]
    fn scenario_ids_round_trip() {
        for s in Scn::all() {
            assert_eq!(Scn::parse(&s.id()), Some(s));
        }
    }
}
