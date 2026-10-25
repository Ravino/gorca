// SPDX-License-Identifier: GPL-3.0-or-later
//! The wall-clock backstop of `present` and `policy` only faults a hook that consumed CPU or is
//! genuinely stuck (docs/reports/m5-impl.md, "CPU-time backstop"). A healthy hook that is slow in
//! wall time (descheduled, or waiting in a slow host call) must never fault: three faults in 60 s
//! disable the extension for that application.
#![allow(
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_methods
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::{Backend, app, info, manifest, settle, snap, wasm_bytes};
use gorca_core::{
    AppExt, ExtSink, ExtSource, MirrorDirectory, PolicyMoment, PolicyView, PresentContext,
};
use gorca_ext_host::ExtHost;
use gorca_model::Role;

/// A sink whose `speak` takes `delay` of wall time without using CPU: a host call that is slow
/// in wall time, exactly what a descheduled thread looks like to the epoch clock.
#[derive(Debug)]
struct SlowSink {
    delay: Duration,
    calls: AtomicU64,
}

impl ExtSink for SlowSink {
    fn speak(&self, _text: String, _interrupt: bool) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        std::thread::sleep(self.delay);
    }
}

fn host_with_sink(delay: Duration) -> (ExtHost, Arc<SlowSink>) {
    let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
    b.add_wasm(manifest(Backend::Wasm), wasm_bytes())
        .expect("load the component");
    let h = b.build();
    let sink = Arc::new(SlowSink {
        delay,
        calls: AtomicU64::new(0),
    });
    h.set_sink(sink.clone());
    (h, sink)
}

fn attach(h: &ExtHost, serial: u32) -> Arc<dyn AppExt> {
    h.attach(app(serial), &info("x")).expect("matches")
}

fn facts(name: &str) -> PolicyView<'_> {
    PolicyView {
        moment: PolicyMoment::Focus,
        role: gorca_model::sem::SemRole::Button,
        flags: 0,
        name: Some(name),
        description: None,
        range: None,
    }
}

#[test]
fn a_hook_that_is_slow_in_wall_time_but_uses_no_cpu_does_not_fault() {
    // 40 ms is far past the old 6 ms wall backstop and well inside the 100 ms ceiling.
    let (h, sink) = host_with_sink(Duration::from_millis(40));
    let ae = attach(&h, 1);
    let node = snap(app(1), 1, Role::Button, Some("gorca-ext-speak"));
    for i in 0..5 {
        let got = ae.present(
            &gorca_core::ExtNode::from_snapshot(&node),
            PresentContext::Focus,
            1,
        );
        assert!(got.is_some(), "present {i} answered");
        assert_eq!(
            ae.policy(&facts("gorca-ext-speak")),
            None,
            "defer: no opinion"
        );
    }
    assert!(sink.calls.load(Ordering::Relaxed) >= 5, "the host call ran");
    let st = h.stats();
    assert_eq!(st.faults, 0, "{st:?}");
    assert_eq!(st.disabled, 0, "{st:?}");
}

#[test]
fn a_hook_stuck_in_a_host_call_is_cut_at_the_wall_ceiling() {
    // 250 ms in the host call: past the 100 ms ceiling for a 2 ms hook.
    let (h, _sink) = host_with_sink(Duration::from_millis(250));
    let ae = attach(&h, 1);
    let node = snap(app(1), 1, Role::Button, Some("gorca-ext-speak"));
    let t = Instant::now();
    let got = ae.present(
        &gorca_core::ExtNode::from_snapshot(&node),
        PresentContext::Focus,
        1,
    );
    let wall = t.elapsed();
    assert!(got.is_none(), "a cut hook has no opinion");
    assert!(wall >= Duration::from_millis(100), "{wall:?}");
    assert!(wall < Duration::from_secs(30), "bounded: {wall:?}");
    assert_eq!(h.stats().faults, 1);
    settle(&h);
}

#[test]
fn a_looping_hook_is_still_cut_quickly() {
    let (h, _sink) = host_with_sink(Duration::ZERO);
    let ae = attach(&h, 1);
    let node = snap(app(1), 1, Role::Button, Some("gorca-ext-spin"));
    let (got, cpu) = gorca_testkit::cpu::cpu_time(|| {
        ae.present(
            &gorca_core::ExtNode::from_snapshot(&node),
            PresentContext::Focus,
            1,
        )
    });
    assert!(got.is_none());
    // Fuel ends it at about 2 ms of CPU; the CPU backstop (6 ms) is the outer bound.
    assert!(cpu < Duration::from_millis(40), "{cpu:?}");
    assert_eq!(h.stats().faults, 1);
    settle(&h);
    let before = h.stats().faults;
    assert_eq!(ae.policy(&facts("gorca-ext-spin")), None);
    assert_eq!(h.stats().faults, before + 1);
}

/// Busy threads, about one per core, for as long as the guard lives.
struct Hog {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Hog {
    fn start(n: usize) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let threads = (0..n)
            .map(|_| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut x = 1u64;
                    while !stop.load(Ordering::Relaxed) {
                        for _ in 0..10_000 {
                            x = std::hint::black_box(
                                x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1),
                            );
                        }
                    }
                })
            })
            .collect();
        Self { stop, threads }
    }
}

impl Drop for Hog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[test]
fn a_healthy_hook_never_faults_under_heavy_cpu_load() {
    let cores = std::thread::available_parallelism().map_or(4, usize::from);
    let _hog = Hog::start(cores);
    // The healthy hook speaks (a host call that sleeps 1 ms and is descheduled under load), so a
    // wall-clock backstop of 6 ms would be crossed regularly.
    let (h, _sink) = host_with_sink(Duration::from_millis(1));
    let ae = attach(&h, 1);
    let node = snap(app(1), 1, Role::Button, Some("gorca-ext-speak"));
    let plain = snap(app(1), 2, Role::Button, Some("gorca-ext-present"));
    let t0 = Instant::now();
    let end = t0 + Duration::from_secs(8);
    let mut calls = 0u64;
    while Instant::now() < end {
        assert!(
            ae.present(
                &gorca_core::ExtNode::from_snapshot(&node),
                PresentContext::Focus,
                1
            )
            .is_some()
        );
        assert!(
            ae.present(
                &gorca_core::ExtNode::from_snapshot(&plain),
                PresentContext::Focus,
                1
            )
            .is_some()
        );
        assert_eq!(ae.policy(&facts("gorca-ext-speak")), None);
        calls += 3;
    }
    let st = h.stats();
    eprintln!(
        "STRESS calls {calls} in {:?} on {cores} hog threads: {st:?}",
        t0.elapsed()
    );
    assert_eq!(st.faults, 0, "{st:?}");
    assert_eq!(st.disabled, 0, "{st:?}");
}
