//! X1: extension hook overhead (ADR-0001 M-7, phase-2 plan M5 acceptance; the ex-B-4 benchmark).
//!
//! Times the *whole* host path of a hook as the core sees it: `AppExts::on_event` and
//! `AppExts::present`, including event/node conversion, the slot lock, the call, lifting the
//! answer and clamping it. Both builds of the same source (WASM component and native). Also the
//! fault paths (reset after a crash, interruption of a loop) and a multi-threaded storm.
//!
//! ```sh
//! cargo build -p ext-test --target wasm32-wasip2 --release --target-dir target/ext-wasm
//! cargo run --release -p gorca-ext-host --example x1 -- [iterations] [component.wasm] > out.tsv
//! ```
//!
//! Output (TSV, nanoseconds): `case n p50 p99 p999 max mean`. Gate: X1 p99 <= 100 us for the
//! hook rows; trap recovery < 50 ms.
#![allow(
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::disallowed_methods
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gorca_core::{ExtSink, ExtSource, MirrorDirectory, PresentContext};
use gorca_ext_host::{ExtHost, Manifest, NativeFactory};
use gorca_model::{
    AppId, AppInfo, AttrMap, ChildList, ConnectorId, ConnectorKind, Freshness, Ifaces, LocalId,
    NodeEvent, NodeKey, NodeSnapshot, Politeness, PropMask, Relations, Role, StateSet,
};

const MANIFEST: &str = include_str!("../../../extensions/ext-test/ext.toml");

#[derive(Debug)]
struct NullSink;

impl ExtSink for NullSink {
    fn speak(&self, _text: String, _interrupt: bool) {}
}

fn app(serial: u32) -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial,
    }
}

fn info() -> AppInfo {
    AppInfo {
        toolkit: Arc::from("gtk4"),
        app_name: Arc::from("x1"),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
        verified_shell: false,
        dump_complete: false,
        text_unit: gorca_model::TextUnit::Scalar,
        root: None,
    }
}

fn snap(local: u64, name: &str) -> NodeSnapshot {
    NodeSnapshot {
        key: NodeKey {
            app: app(1),
            local: LocalId(local),
        },
        role: Role::Button,
        states: StateSet::empty(),
        name: Some(Arc::from(name)),
        description: None,
        value: None,
        parent: None,
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: AttrMap::default(),
        relations: Relations::default(),
        bounds: None,
        have: PropMask::PRESENT,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    }
}

fn ann(text: &str) -> NodeEvent {
    NodeEvent::Announcement {
        node: None,
        text: Arc::from(text),
        politeness: Politeness::Polite,
    }
}

fn host(wasm: Option<&[u8]>) -> ExtHost {
    let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
    match wasm {
        Some(bytes) => b
            .add_wasm(Manifest::parse(MANIFEST).unwrap(), bytes)
            .unwrap(),
        None => {
            let m =
                Manifest::parse(&MANIFEST.replace("backend = \"wasm\"", "backend = \"native\""))
                    .unwrap();
            let f: NativeFactory = Arc::new(|| Box::new(ext_test::TestExtension::default()));
            b.add_native(m, f).unwrap();
        }
    }
    let h = b.build();
    h.set_sink(Arc::new(NullSink));
    h
}

struct Stats {
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
    mean: f64,
}

fn stats(mut v: Vec<u64>) -> Stats {
    v.sort_unstable();
    let at = |q: f64| v[((v.len() as f64 * q) as usize).min(v.len() - 1)];
    Stats {
        p50: at(0.5),
        p99: at(0.99),
        p999: at(0.999),
        max: *v.last().unwrap(),
        mean: v.iter().sum::<u64>() as f64 / v.len() as f64,
    }
}

fn row(case: &str, n: usize, s: &Stats) {
    println!(
        "{case}\t{n}\t{}\t{}\t{}\t{}\t{:.0}",
        s.p50, s.p99, s.p999, s.max, s.mean
    );
}

fn time(n: usize, mut f: impl FnMut()) -> Vec<u64> {
    for _ in 0..n / 10 {
        f();
    }
    (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX)
        })
        .collect()
}

fn run_build(label: &str, wasm: Option<&[u8]>, n: usize) {
    let h = host(wasm);
    let ae = h.attach(app(1), &info()).expect("attach");
    let pass = ann("hello");
    let rewrite = ann("rewrite:spoken");
    let node_none = snap(1, "OK");
    let node_some = snap(2, "gorca-ext-present");
    let ctx = PresentContext::Focus;

    row(
        &format!("{label}-on_event_pass"),
        n,
        &stats(time(n, || {
            std::hint::black_box(ae.on_event(std::hint::black_box(&pass)));
        })),
    );
    row(
        &format!("{label}-on_event_rewrite"),
        n,
        &stats(time(n, || {
            std::hint::black_box(ae.on_event(std::hint::black_box(&rewrite)));
        })),
    );
    row(
        &format!("{label}-present_none"),
        n,
        &stats(time(n, || {
            std::hint::black_box(ae.present(std::hint::black_box(&node_none), ctx, 1));
        })),
    );
    row(
        &format!("{label}-present_some"),
        n,
        &stats(time(n, || {
            std::hint::black_box(ae.present(std::hint::black_box(&node_some), ctx, 1));
        })),
    );
    let focus = NodeEvent::FocusChanged {
        node: node_none.key,
    };
    row(
        &format!("{label}-on_event_focus"),
        n,
        &stats(time(n, || {
            std::hint::black_box(ae.on_event(std::hint::black_box(&focus)));
        })),
    );

    // Fault path: fresh app each time (the three-strikes rule would disable one slot), crash,
    // then the first call of the replacement instance: the reset a user would live through.
    let crash = ann("crash");
    let reps = 300.min(n);
    let mut v = Vec::with_capacity(reps);
    for i in 0..reps {
        let a = u32::try_from(1000 + i).unwrap();
        let ae2 = h.attach(app(a), &info()).expect("attach");
        ae2.on_event(&pass);
        let t = Instant::now();
        ae2.on_event(&crash);
        ae2.on_event(&pass);
        v.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
        h.detach(app(a));
    }
    row(&format!("{label}-crash_reset_next_call"), reps, &stats(v));

    if wasm.is_some() {
        let spin = ann("spin");
        let mut v = Vec::with_capacity(100);
        for i in 0..100 {
            let a = u32::try_from(5000 + i).unwrap();
            let ae2 = h.attach(app(a), &info()).expect("attach");
            ae2.on_event(&pass);
            let t = Instant::now();
            ae2.on_event(&spin);
            ae2.on_event(&pass);
            v.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
            h.detach(app(a));
        }
        row(
            &format!("{label}-spin_interrupt_reset_next_call"),
            100,
            &stats(v),
        );
    }

    // Attach cost (an app appears): instantiate one store.
    let mut v = Vec::new();
    for i in 0..100 {
        let a = u32::try_from(9000 + i).unwrap();
        let t = Instant::now();
        let _ = h.attach(app(a), &info());
        v.push(u64::try_from(t.elapsed().as_nanos()).unwrap_or(u64::MAX));
        h.detach(app(a));
    }
    row(&format!("{label}-attach"), 100, &stats(v));

    // Q-E3: resident memory per live instance (one extension, 100 applications).
    if wasm.is_some() {
        let rss = || -> u64 {
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|t| {
                    t.lines()
                        .find(|l| l.starts_with("VmRSS:"))
                        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
                })
                .unwrap_or(0)
        };
        let before = rss();
        let keep: Vec<_> = (0..100u32)
            .filter_map(|i| h.attach(app(30_000 + i), &info()))
            .collect();
        for a in &keep {
            a.on_event(&pass);
        }
        let after = rss();
        eprintln!(
            "{label} rss per instance: {} KiB over {} instances",
            (after.saturating_sub(before)) / keep.len().max(1) as u64,
            keep.len()
        );
        for i in 0..100u32 {
            h.detach(app(30_000 + i));
        }
    }

    // Storm: T threads, each with its own app (one store each), hammering on_event for a while;
    // the per-call distribution under contention for the host's shared structures and cores.
    for threads in [1usize, 4, 8] {
        let before = h.stats();
        let stop = Arc::new(AtomicBool::new(false));
        let mut hs = Vec::new();
        for t in 0..threads {
            let a = u32::try_from(20_000 + t).unwrap();
            let ae = h.attach(app(a), &info()).expect("attach");
            let stop = Arc::clone(&stop);
            let ev = ann("hello");
            hs.push(std::thread::spawn(move || {
                let mut v = Vec::with_capacity(1 << 20);
                while !stop.load(Ordering::Relaxed) && v.len() < (1 << 20) {
                    let t0 = Instant::now();
                    std::hint::black_box(ae.on_event(&ev));
                    v.push(u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX));
                }
                v
            }));
        }
        std::thread::sleep(Duration::from_secs(2));
        stop.store(true, Ordering::Relaxed);
        let mut all = Vec::new();
        for j in hs {
            all.extend(j.join().unwrap());
        }
        let n_all = all.len();
        row(
            &format!("{label}-storm_on_event_{threads}t"),
            n_all,
            &stats(all),
        );
        // A healthy extension must never fault under load: the CPU limit is fuel, not wall time.
        let s = h.stats();
        eprintln!(
            "{label} storm {threads}t: faults {} disabled {}",
            s.faults - before.faults,
            s.disabled - before.disabled
        );
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let n: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(200_000);
    let path = args.next().unwrap_or_else(|| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../target/ext-wasm/wasm32-wasip2/release/ext_test.wasm"
        )
        .to_owned()
    });
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    println!("case\tn\tp50_ns\tp99_ns\tp999_ns\tmax_ns\tmean_ns");
    run_build("wasm", Some(&bytes), n);
    run_build("native", None, n);
}
