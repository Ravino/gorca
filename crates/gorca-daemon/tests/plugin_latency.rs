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
const SPACING: Duration = Duration::from_millis(24);

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

fn pct(sorted: &[u64], p: f64) -> u64 {
    let i = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[i.saturating_sub(1).min(sorted.len() - 1)]
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
    let pins = parse_pins(&std::fs::read_to_string(root().join("plugins/pins.txt")).unwrap());
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            first_party_dir: Some(dist),
            user_dir: Some(user.path().to_path_buf()),
            allow_sideloaded: true,
            // GORCA_BENCH_SPIN_US=10 is the old parking writer; unset is the whole window.
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

    let use_prewake = std::env::var("GORCA_BENCH_NO_PREWAKE").is_err();
    let mut on_ns: Vec<u64> = Vec::with_capacity(TRIALS);
    let mut off_ns: Vec<u64> = Vec::with_capacity(TRIALS);
    let mut in_time = [0usize; PLUGINS];
    let mut all_in_time = 0usize;
    // Facts in by the end of the 15 ms that follow (first utterance or refinement).
    let mut refined = [0usize; PLUGINS];
    for trial in 0..TRIALS {
        std::thread::sleep(SPACING);
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
        "per turn: wait {:.1} us, admit {:.1} us",
        st.handoff_waited_ns as f64 / 1e3 / TRIALS as f64,
        st.handoff_drain_ns as f64 / 1e3 / TRIALS as f64
    );
    let line = format!(
        "PX-6 plugins={PLUGINS} trials={TRIALS} on_us p50={:.1} p99={:.1} max={:.1} off_us p50={:.1} p99={:.1} \
         delta_p99_us={:.1} facts_at_first_utterance overall={:.3} min_plugin={:.3} per_plugin={:?} \
         min_eventually_within_15ms={:.3} load1={load1:.1} cores={cores} release={release} debug_log={} prewake={} spin_us={} refinement_dummies={} quiet_violated={}",
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
