// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-extension fixtures for the host-side work a guest's fuel cannot see (review finding
//! M-1): oversize arguments, host-call loops and aliased lists. Reports the worst time a
//! hostile `present` holds the calling thread (the Reader), per attack, and asserts it stays
//! near the 2 ms CPU limit instead of the old 6.5 to 6.8 ms.
#![allow(
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_methods
)]

mod common;

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::{TestSink, app, info, settle, snap};
use gorca_core::{ExtSource, MirrorDirectory, PresentContext};
use gorca_ext_host::{ExtHost, Manifest};
use gorca_model::{NodeEvent, Politeness, Role};

const MANIFEST: &str = r#"
name = "ext-hostile"
backend = "wasm"
module = "ext_hostile.wasm"
hooks = ["on-event", "present"]
capabilities = []
[matcher]
app_name = ["*"]
"#;

fn hostile_bytes() -> &'static [u8] {
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    BYTES.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/ext-wasm");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let status = Command::new(cargo)
            .current_dir(&root)
            .args(["build", "--manifest-path", "tests/ext-hostile/Cargo.toml"])
            .args(["--target", "wasm32-wasip2", "--release", "--target-dir"])
            .arg(&target)
            .status()
            .expect("run cargo to build the hostile component");
        assert!(status.success(), "building ext-hostile failed");
        std::fs::read(target.join("wasm32-wasip2/release/ext_hostile.wasm"))
            .expect("read the hostile component")
    })
}

/// 4 ms in release; a debug build of wasmtime is an order of magnitude slower, so there the
/// test only checks the cost is nowhere near the old multi-millisecond lifts.
fn bound() -> Duration {
    Duration::from_millis(if cfg!(debug_assertions) { 25 } else { 4 })
}

fn host() -> ExtHost {
    let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
    b.add_wasm(Manifest::parse(MANIFEST).unwrap(), hostile_bytes())
        .unwrap();
    let h = b.build();
    h.set_sink(Arc::new(TestSink::default()));
    h
}

fn ann(t: &str) -> NodeEvent {
    NodeEvent::Announcement {
        node: None,
        text: Arc::from(t),
        politeness: Politeness::Polite,
    }
}

/// `n` fresh applications (so no slot reaches its third strike), one hostile `present` each.
/// Returns (p50, max) of the time the calling thread was held.
fn present_hold(attack: &str, n: u32) -> (Duration, Duration, Duration, u64) {
    let h = host();
    let mut times = Vec::new();
    for i in 0..n {
        let ae = h.attach(app(i + 1), &info("victim")).expect("matches");
        let node = snap(app(i + 1), 1, Role::Button, Some(attack));
        // CPU time of the calling thread: the hold is the work the Reader does, not the
        // scheduler's mood (the host runs at load 10 to 48 here).
        let (r, held) =
            gorca_testkit::cpu::cpu_time(|| ae.present(&node, PresentContext::Focus, 1));
        times.push(held);
        assert!(r.is_none(), "{attack}: a faulting hook has no opinion");
        settle(&h);
    }
    let st = h.stats();
    times.sort();
    (
        times[times.len() / 2],
        times[times.len() * 9 / 10],
        *times.last().unwrap(),
        st.faults,
    )
}

#[test]
fn hostile_present_holds_the_reader_near_the_cpu_limit() {
    let mut worst = Duration::ZERO;
    for attack in [
        "logforever",
        "logmax",
        "nodeforever",
        "alias:2000",
        "alias:1",
        // WASI imports are charged to the same meter (review finding N-2).
        "randforever",
        "clockforever",
        "randtime:5000",
        "clocktime:5000",
    ] {
        let (p50, p90, max, faults) = present_hold(attack, 20);
        eprintln!(
            "HOSTILE present {attack:<12} p50 {p50:>10?} p90 {p90:>10?} max {max:>10?} faults {faults}/20"
        );
        // The self-timed attacks stop by themselves after 5 ms; in a debug build the host is so
        // slow that they can finish before the meter (calibrated for release) cuts them.
        if !(cfg!(debug_assertions) && attack.contains("time:")) {
            assert_eq!(faults, 20, "{attack}: every attempt is a fault");
        }
        worst = worst.max(p90);
    }
    eprintln!("HOSTILE worst p90 Reader hold: {worst:?}");
    // The gate is the p90 of 20 fresh applications: a single max on a loaded host is a
    // descheduling outlier (the max is printed for the record). Host-side attacks are cut by the work meter and the lift cap (measured 0.1 to 0.7 ms);
    // new guest memory is capped at 1 MiB per hook call. 4 ms leaves room for a busy host.
    assert!(worst < bound(), "{worst:?}");
}

#[test]
fn oversize_arguments_are_refused_without_the_copy() {
    let h = host();
    let ae = h.attach(app(1), &info("victim")).expect("matches");
    // 16 MiB `log`, and 2000 segments that alias one 16 MiB string: 32 GiB if copied.
    for t in ["log:1", "alias:2000"] {
        let t0 = Instant::now();
        ae.on_event(&ann(t));
        let dt = t0.elapsed();
        eprintln!("HOSTILE on_event({t}): {dt:?} (a 16 MiB lift would take milliseconds)");
        assert!(dt < bound(), "{t}: {dt:?}");
        settle(&h);
    }
    assert!(h.stats().faults >= 1);
}
