// SPDX-License-Identifier: GPL-3.0-or-later
//! PX-6 (ADR-0026 section 2.9): the writer's added delay per focus change with the real
//! first-party plugin plus nine more plugins of the same shape matching the same node, against
//! the same turn with plugins off, and the share of focus changes whose facts are in by the end
//! of the bounded wait.
//!
//! "Keypress to speech" for a focus change is the writer turn (the document actor's `fact`)
//! plus everything after it, which plugins do not touch; so the delta of the writer turn is the
//! delta of keypress to speech. The turn measured here is exactly what the writer adds:
//! snapshot, send to every actor, bounded wait, admission of what arrived.
//!
//! The gate (delta p99 <= 0.3 ms; facts at first utterance >= 99 % overall and >= 97 % per
//! plugin) is asserted only on a quiet host in a release build, as the rules of the benchmarks
//! say; otherwise the run is labelled `quiet_violated` and the numbers are printed and, with
//! `GORCA_BENCH_OUT`, appended to that file.
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_core::plugin_port::{
    FactOp, Governor, Handoff, Job, PluginBridge, PluginMeta, PluginService, Reply, Row,
    ScopeRunner, ScopeSpec, Tree, now_ns,
};
use gorca_daemon::plugins::{PluginOptions, PluginRuntime, parse_pins};
use gorca_model::sem::SemRole;

const TRIALS: usize = 400;
const PLUGINS: usize = 10;
/// Spacing of the focus changes: a fast typist; above the writer's own once-per-20-ms limit so
/// every turn takes the wait.
const SPACING_DEFAULT: Duration = Duration::from_millis(24);

/// `GORCA_BENCH_SPACING_MS` overrides the spacing: 24 is a stress rate (41 keys/s) that the
/// pre-wake CPU bucket of SPS cannot serve for every plugin; 100-200 ms is a navigation rate.
fn spacing() -> Duration {
    std::env::var("GORCA_BENCH_SPACING_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map_or(SPACING_DEFAULT, Duration::from_millis)
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Flat(HashMap<u32, Vec<u32>>);
impl Tree for Flat {
    fn parent(&self, n: u32) -> Option<u32> {
        self.0.iter().find(|(_, c)| c.contains(&n)).map(|(p, _)| *p)
    }
    fn children(&self, n: u32) -> Vec<u32> {
        self.0.get(&n).cloned().unwrap_or_default()
    }
}

type N = (u32, Option<u32>, &'static str, &'static str, &'static str);
const CHAT: &[N] = &[
    (0, None, "div", "", ""),
    (1, Some(0), "div", "ListItem-button", ""),
    (2, Some(1), "div", "Avatar", ""),
    (3, Some(2), "", "", "AL"),
    (4, Some(1), "h3", "fullName", ""),
    (5, Some(4), "", "", "Alice"),
    (6, Some(1), "div", "LastMessageMeta", ""),
    (7, Some(6), "", "", "15:00"),
    (8, Some(1), "div", "last-message", ""),
    (9, Some(8), "", "", "see you soon"),
    (10, Some(1), "div", "ChatBadge unread", ""),
    (11, Some(10), "", "", "1"),
];

fn row(n: &N, text: Option<&str>) -> Row {
    let (_, _, tag, class, t) = *n;
    let t = text.unwrap_or(t);
    Row {
        role: if t.is_empty() {
            SemRole::Generic.code()
        } else {
            SemRole::StaticText.code()
        },
        name: (!t.is_empty()).then(|| Arc::from(t)),
        tag: (!tag.is_empty()).then(|| Arc::from(tag)),
        class: (!class.is_empty()).then(|| Arc::from(class)),
        ..Row::default()
    }
}

/// The writer with plugins off: the same bridge calls over a runner that has no plugins.
struct NullRunner;

impl ScopeRunner for NullRunner {
    fn plugins(&self) -> &[PluginMeta] {
        &[]
    }
    fn watches(&self, _row: &Row) -> Vec<(usize, u16)> {
        Vec::new()
    }
    fn interactive(&mut self, _t0: u64, _wait: bool, _jobs: Vec<Job>) -> Handoff {
        Handoff::default()
    }
    fn background(&mut self, _jobs: Vec<Job>) {}
    fn poll(&mut self, _out: &mut Vec<Reply>) {}
    fn outstanding(&self) -> bool {
        false
    }
}

/// Mark the plugins whose name fact for the badge `want` is among `ops`.
fn mark(bridge: &PluginBridge, ops: &[FactOp], want: &str, seen: &mut [bool]) {
    for op in ops {
        let (plugin_key, name) = match op {
            FactOp::Patch {
                plugin,
                write: gorca_fuse::Write::Val(gorca_fuse::Val::Name(n)),
                ..
            } => (plugin.to_string(), n.to_string()),
            FactOp::Inferred {
                plugin,
                write: gorca_fuse::Write::Val(gorca_fuse::Val::Name(n)),
                ..
            } => (bridge.metas()[*plugin].name.to_string(), n.to_string()),
            _ => continue,
        };
        if name.contains(want)
            && let Some(i) = bridge.metas().iter().position(|m| *m.name == *plugin_key)
        {
            seen[i] = true;
        }
    }
}

/// Process CPU time (user + system, all threads) in microseconds, from `/proc/self/stat`.
fn proc_cpu_us() -> u64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let tail = stat.rsplit(')').next().unwrap_or("");
    let f: Vec<&str> = tail.split_whitespace().collect();
    let tick = 10_000u64; // USER_HZ = 100
    let get = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (get(11) + get(12)) * tick
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    let i = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[i.saturating_sub(1).min(sorted.len() - 1)]
}

/// Turn one trial's stage records into intervals (ns), keyed by name.
fn collect_stages(
    recs: &[gorca_rt::stage::Record],
    out: &mut std::collections::BTreeMap<&'static str, Vec<u64>>,
) {
    use gorca_rt::stage::id::*;
    let at = |stage: u16| recs.iter().find(|r| r.stage == stage).map(|r| r.ns);
    let Some(t0) = at(W_ENTRY) else { return };
    let mut push = |name: &'static str, v: Option<u64>| {
        if let Some(v) = v {
            out.entry(name).or_default().push(v);
        }
    };
    let d = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    };
    push("w1 snapshot+jobs_for", d(Some(t0), at(W_SNAP)));
    push("w2 claim", d(at(W_SNAP), at(W_CLAIM)));
    push("w3 runner entry", d(at(W_CLAIM), at(SC_ENTER)));
    push("w4 plan", d(at(SC_ENTER), at(SC_PLAN)));
    push("w5 build DeriveJobs", d(at(SC_PLAN), at(SC_BUILT)));
    push("w6 to handoff", d(at(SC_BUILT), at(H_ENTER)));
    push("w7 send waited+wake", d(at(H_ENTER), at(H_SENT_W)));
    push("w8 send rest", d(at(H_SENT_W), at(H_SENT_ALL)));
    push("TOTAL pre-send (t0..sent)", d(Some(t0), at(H_SENT_W)));
    push("t0 -> wait end", d(Some(t0), at(H_WAIT_END)));
    push("w9 poll (convert)", d(at(H_WAIT_END), at(B_POLL)));
    push("w10 admit", d(at(B_POLL), at(B_ADMIT)));
    push("TOTAL wait end -> done", d(at(H_WAIT_END), at(B_ADMIT)));
    push("TOTAL turn", d(Some(t0), at(B_ADMIT)));
    let sent = at(H_SENT_W);
    for r in recs.iter().filter(|r| r.stage == K_PICK) {
        let tag = r.tag;
        let of = |s: u16| {
            recs.iter()
                .filter(|x| x.stage == s && x.tag == tag && x.ns >= r.ns)
                .map(|x| x.ns)
                .min()
        };
        let hot = d(sent, Some(r.ns)).is_some_and(|v| v < 20_000);
        push("k1 pick-up after send", d(sent, Some(r.ns)));
        push(
            if hot {
                "k1 pick-up (hot)"
            } else {
                "k1 pick-up (cold)"
            },
            d(sent, Some(r.ns)),
        );
        push("k2 running slot", d(Some(r.ns), of(K_CALL0)));
        push("k3 derive prep", d(of(K_CALL0), of(K_G0)));
        push("k4 guest call", d(of(K_G0), of(K_G1)));
        push(
            if hot {
                "k4h guest call (hot worker)"
            } else {
                "k4c guest call (cold worker)"
            },
            d(of(K_G0), of(K_G1)),
        );
        push("k5 derive tail+resolve", d(of(K_G1), of(K_RUN1)));
        push("k6 reply bookkeeping", d(of(K_RUN1), of(K_POST0)));
        push("k7 post reply", d(of(K_POST0), of(K_POST1)));
        push("k8 gate", d(of(K_POST1), of(K_GATE)));
        push("K pick..reply posted", d(Some(r.ns), of(K_POST1)));
        let val = |s: u16| {
            recs.iter()
                .find(|x| x.stage == s && x.tag == tag)
                .map(|x| x.ns)
        };
        push("k4a guest call: in host imports", val(K_HOSTNS));
        push(
            "k4b guest call: import count",
            val(K_NHOST).map(|n| n * 1000),
        );
        push("K send..reply posted", d(sent, of(K_POST1)));
        push("K t0..reply posted", d(Some(t0), of(K_POST1)));
    }
}

#[test]
fn writer_delay_with_ten_plugins_against_none() {
    // GORCA_BENCH_LOG=1 shows the plugin calls (grant and guest time, never text) and faults.
    if std::env::var("GORCA_BENCH_LOG").is_ok() {
        let _ = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(std::io::stderr)
            .try_init();
    }
    let st = Command::new(root().join("tools/build-plugins.sh"))
        .status()
        .expect("build plugins");
    assert!(st.success());
    let dist = root().join("plugins/dist");
    // Nine dummies: the same component under other ids (third parties, matched to the same node).
    let user = tempfile::tempdir().unwrap();
    for i in 1..PLUGINS {
        let d = user.path().join(format!("example.dummy{i}"));
        std::fs::create_dir_all(&d).unwrap();
        let m = std::fs::read_to_string(dist.join("gorca.telegram-web/manifest.toml"))
            .unwrap()
            .replace("gorca.telegram-web", &format!("example.dummy{i}"));
        // GORCA_BENCH_REFINEMENT_DUMMIES: the nine copies never gate a turn.
        let m = if std::env::var("GORCA_BENCH_REFINEMENT_DUMMIES").is_ok() {
            m.replacen(
                "world = \"site\"",
                "world = \"site\"\npriority = \"refinement\"",
                1,
            )
        } else {
            m
        };
        std::fs::write(d.join("manifest.toml"), m).unwrap();
        std::fs::copy(
            dist.join("gorca.telegram-web/plugin.wasm"),
            d.join("plugin.wasm"),
        )
        .unwrap();
        let cat = d.join("i18n");
        std::fs::create_dir_all(&cat).unwrap();
        for f in ["en.msg", "ru.msg"] {
            std::fs::copy(dist.join("gorca.telegram-web/i18n").join(f), cat.join(f)).unwrap();
        }
    }
    let mut pins = parse_pins(&std::fs::read_to_string(root().join("plugins/pins.txt")).unwrap());
    // GORCA_BENCH_DUMMY_TIER=first pins the nine copies (the same component), so they are
    // first-party plugins: the C-4 fixture of ADR-0026 section 2.4.7.12 where no tier cap
    // applies. The default (third parties, sideloaded) is the harsher reading: a third party has
    // one hot place, a 100 us hold and is never waited for when its deadline was clipped.
    if std::env::var("GORCA_BENCH_DUMMY_TIER").as_deref() == Ok("first") {
        let base = pins[0].sha256;
        for i in 1..PLUGINS {
            let id: &'static str = Box::leak(format!("example.dummy{i}").into_boxed_str());
            pins.push(gorca_plugin_host::package::Pin { id, sha256: base });
        }
    }
    let dummies_first = std::env::var("GORCA_BENCH_DUMMY_TIER").as_deref() == Ok("first");
    if dummies_first {
        // Installed packages only can be first party: the original goes next to its copies.
        let t = user.path().join("gorca.telegram-web");
        std::fs::create_dir_all(t.join("i18n")).unwrap();
        for f in ["manifest.toml", "plugin.wasm", "i18n/en.msg", "i18n/ru.msg"] {
            std::fs::copy(dist.join("gorca.telegram-web").join(f), t.join(f)).unwrap();
        }
    }
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            first_party_dir: Some(if dummies_first {
                user.path().to_path_buf()
            } else {
                dist
            }),
            user_dir: (!dummies_first).then(|| user.path().to_path_buf()),
            allow_sideloaded: true,
            sps_k_max: std::env::var("GORCA_SPS_KMAX")
                .ok()
                .and_then(|v| v.parse().ok()),
            scheduler: std::env::var("GORCA_PLUGIN_SCHEDULER")
                .ok()
                .and_then(|v| gorca_daemon::plugins::SchedulerPolicy::parse(&v))
                .unwrap_or_default(),
            // GORCA_BENCH_SPIN_US=10 is the old parking writer; unset is the whole window.
            warm_on_arm: std::env::var("GORCA_BENCH_WARM").ok().map(|_| true),
            writer_spin: std::env::var("GORCA_BENCH_SPIN_US")
                .ok()
                .and_then(|v| v.parse().ok())
                .map(Duration::from_micros),
            ..PluginOptions::default()
        },
        &pins,
    );
    assert_eq!(rt.ids().len(), PLUGINS);
    let scope = ScopeSpec {
        app: Arc::from("firefox-esr"),
        url: Some(Arc::from("https://web.telegram.org/a/")),
        engine: 1,
        verified: true,
        doc: true,
    };
    let runner = rt.open(&scope, Arc::new(|| {})).expect("scope");
    let mut bridge = PluginBridge::new(runner, rt.governor(), scope);
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for n in CHAT {
        if let Some(p) = n.1 {
            kids.entry(p).or_default().push(n.0);
        }
    }
    let tree = Flat(kids);
    let mut off = PluginBridge::new(
        Box::new(NullRunner),
        Arc::new(Governor::default()),
        ScopeSpec::default(),
    );
    for n in CHAT {
        off.upsert(n.0, row(n, None));
    }
    for n in CHAT {
        bridge.upsert(n.0, row(n, None));
    }
    // Warm: derive everything once in the background (first-call page faults, instance state).
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end && (bridge.live_len() < PLUGINS || bridge.has_work()) {
        bridge.background(&tree);
        let _ = bridge.take_ops();
        std::thread::sleep(Duration::from_millis(2));
    }
    let load_now: f64 = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0);
    if bridge.live_len() < PLUGINS
        && load_now >= std::thread::available_parallelism().map_or(1, usize::from) as f64
    {
        // Oversubscribed: the wall watchdog fails calls open, which is the intended behaviour.
        eprintln!(
            "host oversubscribed (load1 {load_now:.1}): only {} of {PLUGINS} plugins derived; no measurement",
            bridge.live_len()
        );
        return;
    }
    assert_eq!(
        bridge.live_len(),
        PLUGINS,
        "every plugin derived the row once"
    );

    let stages_on = std::env::var("GORCA_BENCH_STAGES").is_ok();
    let mut stage_iv: std::collections::BTreeMap<&'static str, Vec<u64>> = Default::default();
    let use_prewake = std::env::var("GORCA_BENCH_NO_PREWAKE").is_err();
    let mut on_ns: Vec<u64> = Vec::with_capacity(TRIALS);
    let mut off_ns: Vec<u64> = Vec::with_capacity(TRIALS);
    let mut in_time = [0usize; PLUGINS];
    let mut all_in_time = 0usize;
    // Facts in by the end of the 15 ms that follow (first utterance or refinement).
    let mut refined = [0usize; PLUGINS];
    let cpu0 = proc_cpu_us();
    let wall0 = Instant::now();
    for trial in 0..TRIALS {
        std::thread::sleep(spacing());
        // A new view of the row each time (the badge counts up): every plugin has to re-derive.
        let text = format!("{}", 2 + trial);
        // Plugins off: the same turn without the call (the writer's own work is identical).
        off.begin_turn();
        off.upsert(11, row(&CHAT[11], Some(&text)));
        let t = now_ns();
        off.interactive(&[1, 0], &tree, t);
        let _ = off.take_ops();
        off_ns.push(now_ns() - t);
        // Plugins on. The key reaches gorca a few milliseconds before the focus change it causes:
        // the pre-wake (what the keyboard pump does) has the workers running by then.
        if use_prewake {
            rt.prewake().key_down();
            std::thread::sleep(Duration::from_millis(3));
        }
        bridge.begin_turn();
        bridge.upsert(11, row(&CHAT[11], Some(&text)));
        if stages_on {
            let _ = gorca_rt::stage::drain();
            gorca_rt::stage::enable(true);
        }
        let t0 = now_ns();
        bridge.interactive(&[1, 0], &tree, t0);
        let ops = bridge.take_ops();
        let took = now_ns() - t0;
        on_ns.push(took);
        if trial < 3 {
            eprintln!(
                "trial {trial}: took {took} ns, ops {}, stats {:?}",
                ops.len(),
                bridge.stats()
            );
            for op in &ops {
                match op {
                    FactOp::Patch { plugin, write, .. } => eprintln!("  patch {plugin} {write:?}"),
                    FactOp::Inferred { plugin, write, .. } => {
                        eprintln!("  inferred {plugin} {write:?}")
                    }
                    FactOp::Retract { plugin, .. } => eprintln!("  retract {plugin}"),
                    _ => {}
                }
            }
        }
        let want = format!(", {text} unread");
        let mut seen = vec![false; PLUGINS];
        mark(&bridge, &ops, &want, &mut seen);
        for (i, s) in seen.iter().enumerate() {
            in_time[i] += usize::from(*s);
        }
        all_in_time += usize::from(seen.iter().all(|s| *s));
        // Late facts are refinements; drain them before the next trial.
        let mut eventually = seen.clone();
        let end = Instant::now() + Duration::from_millis(15);
        while Instant::now() < end && bridge.has_work() {
            bridge.background(&tree);
            let late = bridge.take_ops();
            mark(&bridge, &late, &want, &mut eventually);
            std::thread::sleep(Duration::from_micros(200));
        }
        for (i, e) in eventually.iter().enumerate() {
            refined[i] += usize::from(*e);
        }
        if stages_on {
            gorca_rt::stage::enable(false);
            collect_stages(&gorca_rt::stage::drain(), &mut stage_iv);
        }
        if trial < 6 {
            eprintln!(
                "trial {trial}: in time {} eventually {} want {want:?} pending {} live {}",
                seen.iter().filter(|s| **s).count(),
                eventually.iter().filter(|s| **s).count(),
                bridge.pending_len(),
                bridge.live_len()
            );
        }
    }
    let cpu_pct = (proc_cpu_us() - cpu0) as f64 / 1e4 / wall0.elapsed().as_secs_f64().max(1e-3);
    on_ns.sort_unstable();
    off_ns.sort_unstable();
    let delta_p99 = pct(&on_ns, 0.99).saturating_sub(pct(&off_ns, 0.99));
    let load1: f64 = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok()))
        .unwrap_or(0.0);
    let cores = std::thread::available_parallelism().map_or(1, usize::from) as f64;
    let quiet = load1 < cores / 4.0;
    let release = !cfg!(debug_assertions);
    let per_plugin: Vec<f64> = in_time.iter().map(|c| *c as f64 / TRIALS as f64).collect();
    let overall = all_in_time as f64 / TRIALS as f64;
    let min_plugin = per_plugin.iter().copied().fold(1.0, f64::min);
    let min_refined = refined
        .iter()
        .map(|c| *c as f64 / TRIALS as f64)
        .fold(1.0, f64::min);
    let st = bridge.stats();
    eprintln!(
        "per turn: wait {:.1} us, admit {:.1} us (of which poll {:.1} us)",
        st.handoff_waited_ns as f64 / 1e3 / TRIALS as f64,
        st.handoff_drain_ns as f64 / 1e3 / TRIALS as f64,
        st.handoff_poll_ns as f64 / 1e3 / TRIALS as f64
    );
    if stages_on {
        let mut f: Box<dyn std::io::Write> = match std::env::var("GORCA_BENCH_OUT") {
            Ok(path) => Box::new(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .unwrap(),
            ),
            Err(_) => Box::new(std::io::sink()),
        };
        for (name, v) in &mut stage_iv {
            v.sort_unstable();
            let mean = v.iter().sum::<u64>() as f64 / v.len().max(1) as f64;
            let line = format!(
                "STAGE {name:<22} n={:<5} p50={:>7.1} p95={:>7.1} p99={:>7.1} mean={:>7.1} us",
                v.len(),
                pct(v, 0.5) as f64 / 1e3,
                pct(v, 0.95) as f64 / 1e3,
                pct(v, 0.99) as f64 / 1e3,
                mean / 1e3
            );
            eprintln!("{line}");
            writeln!(f, "{line}").unwrap();
        }
    }
    let gov = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .unwrap_or_default()
        .trim()
        .to_string();
    let sched = std::env::var("GORCA_PLUGIN_SCHEDULER").unwrap_or_else(|_| "default".into());
    let spacing_ms = spacing().as_millis();
    let line = format!(
        "PX-6 plugins={PLUGINS} trials={TRIALS} on_us p50={:.1} p99={:.1} max={:.1} off_us p50={:.1} p99={:.1} \
         delta_p99_us={:.1} facts_at_first_utterance overall={:.3} min_plugin={:.3} per_plugin={:?} \
         min_eventually_within_15ms={:.3} proc_cpu_pct_of_core={cpu_pct:.1} governor={gov} scheduler={sched} spacing_ms={spacing_ms} load1={load1:.1} cores={cores} release={release} debug_log={} prewake={} spin_us={} refinement_dummies={} dummy_tier={} quiet_violated={}",
        pct(&on_ns, 0.5) as f64 / 1e3,
        pct(&on_ns, 0.99) as f64 / 1e3,
        *on_ns.last().unwrap() as f64 / 1e3,
        pct(&off_ns, 0.5) as f64 / 1e3,
        pct(&off_ns, 0.99) as f64 / 1e3,
        delta_p99 as f64 / 1e3,
        overall,
        min_plugin,
        per_plugin,
        min_refined,
        std::env::var("GORCA_BENCH_LOG").is_ok(),
        use_prewake,
        std::env::var("GORCA_BENCH_SPIN_US").unwrap_or_else(|_| "window".into()),
        std::env::var("GORCA_BENCH_REFINEMENT_DUMMIES").is_ok(),
        std::env::var("GORCA_BENCH_DUMMY_TIER").unwrap_or_else(|_| "third".into()),
        !quiet,
    );
    eprintln!("{line}");
    if let Ok(path) = std::env::var("GORCA_BENCH_OUT") {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }
    if quiet && release {
        assert!(
            delta_p99 <= 300_000,
            "delta p99 {delta_p99} ns over the 0.3 ms gate"
        );
        assert!(
            overall >= 0.99,
            "facts at first utterance overall {overall:.3}"
        );
        assert!(min_plugin >= 0.97, "weakest plugin {min_plugin:.3}");
    } else if load1 < cores && release {
        // A busy but not oversubscribed host: the wait is bounded and nothing is starved.
        assert!(
            pct(&on_ns, 0.99) < 5_000_000,
            "the bounded wait took {} ns at p99",
            pct(&on_ns, 0.99)
        );
        // Whatever missed the wait arrives as a refinement.
        assert!(
            min_refined >= 0.99,
            "a plugin's facts arrived within 15 ms only {min_refined:.3} of the turns"
        );
    } else {
        // Oversubscribed (other benchmarks running): the numbers are printed and labelled
        // `quiet_violated`; they say nothing about the runtime.
        eprintln!("host oversubscribed (load1 {load1:.1} >= {cores} cores): no assertion");
    }
}
