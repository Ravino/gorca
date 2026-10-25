// SPDX-License-Identifier: GPL-3.0-or-later
//! M18a-4: the cost of one plugin call in isolation (no handoff, no wake): the real first-party
//! Telegram plugin deriving one chat row of the PX-6 fixture, N times on one thread. Prints
//! p50/p95/p99 of the whole executor call and of the guest part, and appends them to
//! `GORCA_BENCH_OUT`. `GORCA_CALLS=<n>` sets the number of calls (default 20 000).
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use gorca_core::plugin_port::{FrozenRow, FrozenRows, Row, ScopeSpec};
use gorca_daemon::plugins::{PluginOptions, PluginRuntime, bench_view, parse_pins};
use gorca_model::sem::SemRole;
use gorca_plugin_host::call::{DeriveJob, PluginReply};
use gorca_plugin_host::instance::DirtyNode;
use gorca_plugin_host::limits::{B2_INTERACTIVE, FuelGrant, KAPPA_PROVISIONAL};

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

fn rows(badge: &str) -> Arc<FrozenRows> {
    let mut m = gorca_core::plugin_port::RowMap::default();
    for n in CHAT {
        let t = if n.0 == 11 { badge } else { n.4 };
        m.insert(
            n.0,
            FrozenRow {
                row: Row {
                    role: if t.is_empty() {
                        SemRole::Generic.code()
                    } else {
                        SemRole::StaticText.code()
                    },
                    name: (!t.is_empty()).then(|| Arc::from(t)),
                    tag: (!n.2.is_empty()).then(|| Arc::from(n.2)),
                    class: (!n.3.is_empty()).then(|| Arc::from(n.3)),
                    ..Row::default()
                },
                generation: 1,
                parent: n.1,
                children: CHAT
                    .iter()
                    .filter(|c| c.1 == Some(n.0))
                    .map(|c| c.0)
                    .collect(),
            },
        );
    }
    Arc::new(FrozenRows {
        scope: ScopeSpec {
            app: Arc::from("firefox-esr"),
            url: Some(Arc::from("https://web.telegram.org/a/")),
            engine: 1,
            verified: true,
            doc: true,
        },
        rows: m,
    })
}

fn pct(sorted: &[u64], p: f64) -> f64 {
    let i = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[i.saturating_sub(1).min(sorted.len() - 1)] as f64 / 1e3
}

fn env_us(name: &str) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// What one worker thread measures: (whole call ns, guest ns) per call.
fn worker(
    mut exec: Box<dyn gorca_plugin_host::call::Exec>,
    n: usize,
    gap: Duration,
    sleep: bool,
    barrier: Arc<Barrier>,
) -> (Vec<u64>, Vec<u64>) {
    exec.warm();
    let views: Vec<_> = (0..64)
        .map(|i| bench_view(rows(&format!("{}", 2 + i))))
        .collect();
    let grant = FuelGrant::for_time(Duration::from_micros(200), KAPPA_PROVISIONAL)
        .with_wall(B2_INTERACTIVE);
    let mut whole = Vec::with_capacity(n);
    let mut guest = Vec::with_capacity(n);
    for i in 0..n {
        // GORCA_CALL_PRESPIN_US: the pre-wake: spin this long after the (sleeping) gap, then call.
        let pre = env_us("GORCA_CALL_PRESPIN_US");
        if pre > 0 {
            let end = Instant::now() + Duration::from_micros(pre);
            while Instant::now() < end {
                std::hint::spin_loop();
            }
        }
        // All threads start the call together (the barrier is the "focus change").
        barrier.wait();
        let job = DeriveJob {
            plugin: 0,
            job: i as u64 + 1,
            slot_epoch: 1,
            interactive: true,
            epoch: 1,
            nodes: vec![DirtyNode {
                index: 1,
                watch: 1,
                cause: 0,
            }],
            view: Arc::clone(&views[i % views.len()]),
            grant,
            queued_at: Instant::now(),
        };
        let t = Instant::now();
        let r = exec.run(&job);
        whole.push(t.elapsed().as_nanos() as u64);
        match r {
            PluginReply::Done { effects, exec, .. } => {
                assert!(!effects.is_empty());
                guest.push(exec.as_nanos() as u64);
            }
            other => panic!("call did not return normally: {other:?}"),
        }
        if !gap.is_zero() {
            if sleep {
                std::thread::sleep(gap);
            } else {
                let end = Instant::now() + gap;
                while Instant::now() < end {
                    std::hint::spin_loop();
                }
            }
        }
    }
    (whole, guest)
}

#[test]
fn one_call_in_isolation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(
        Command::new(root.join("tools/build-plugins.sh"))
            .status()
            .unwrap()
            .success()
    );
    let pins = parse_pins(&std::fs::read_to_string(root.join("plugins/pins.txt")).unwrap());
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            first_party_dir: Some(root.join("plugins/dist")),
            ..PluginOptions::default()
        },
        &pins,
    );
    let n = env_us("GORCA_CALLS").max(1) as usize;
    let n = if std::env::var("GORCA_CALLS").is_ok() {
        n
    } else {
        20_000
    };
    let threads = (env_us("GORCA_CALL_THREADS") as usize).max(1);
    let gap = Duration::from_micros(env_us("GORCA_CALL_GAP_US"));
    let sleep = std::env::var("GORCA_CALL_SLEEP").is_ok();
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            // GORCA_CALL_SEPARATE: one engine (and one compiled component) per thread, to tell
            // contention on shared wasmtime state from contention in the host's own code.
            let exec = if std::env::var("GORCA_CALL_SEPARATE").is_ok() {
                PluginRuntime::load_with_pins(
                    &PluginOptions {
                        first_party_dir: Some(root.join("plugins/dist")),
                        ..PluginOptions::default()
                    },
                    &pins,
                )
                .bench_exec(0)
                .expect("one plugin")
            } else {
                rt.bench_exec(0).expect("one plugin")
            };
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || worker(exec, n, gap, sleep, b))
        })
        .collect();
    let mut whole = Vec::new();
    let mut guest = Vec::new();
    for h in handles {
        let (w, g) = h.join().unwrap();
        whole.extend(w);
        guest.extend(g);
    }
    whole.sort_unstable();
    guest.sort_unstable();
    let load1 = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let line = format!(
        "CALL threads={threads} gap_us={} {} n={} whole_us p50={:.1} p95={:.1} p99={:.1} | guest_us p50={:.1} p95={:.1} p99={:.1} | host_overhead_p50={:.1} load1={}",
        gap.as_micros(),
        if sleep { "sleep" } else { "spin" },
        whole.len(),
        pct(&whole, 0.5),
        pct(&whole, 0.95),
        pct(&whole, 0.99),
        pct(&guest, 0.5),
        pct(&guest, 0.95),
        pct(&guest, 0.99),
        pct(&whole, 0.5) - pct(&guest, 0.5),
        load1.split_whitespace().next().unwrap_or("?"),
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
}
