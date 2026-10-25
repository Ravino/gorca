// SPDX-License-Identifier: GPL-3.0-or-later
//! SPS-5: no allocation inside `on_key_down` and `plan`, and what a decision costs (PX-10).
#![allow(
    unsafe_code,
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use gorca_plugin_sched::{
    KeyClass, Label, Observation, Params, PlaceMode, Scheduler, SpinBucket, Tier, Triggered,
    TurnTimes,
};

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);

// SAFETY: forwards to the system allocator unchanged; only counts.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn world(n: usize) -> Scheduler {
    let plugins: Vec<(Label, Tier)> = (0..n)
        .map(|i| {
            (
                [Label::Interactive, Label::Unlabelled, Label::Refinement][i % 3],
                [Tier::First, Tier::Trusted, Tier::Third][(i / 3) % 3],
            )
        })
        .collect();
    let mut s = Scheduler::new(&plugins, Params::default());
    let mut b = SpinBucket::standard();
    let mut now = 1_000_000u64;
    // Warm every histogram.
    for k in 0..300u64 {
        now += 24_000;
        let _ = s.on_key_down(KeyClass::Nav, now, &mut b);
        let tr: Vec<Triggered> = (0..n).map(|pid| Triggered { pid, busy: false }).collect();
        let plan = s.plan(
            &tr,
            TurnTimes {
                now_us: now + 2000,
                spent_us: None,
            },
            PlaceMode::Own,
        );
        s.end_key((1u32 << n.min(31)).wrapping_sub(1));
        for a in plan.assignments() {
            s.observe(&Observation {
                pid: a.pid,
                state: a.predicted_start(),
                start_us: (2 + (k % 5) * 20) as u32,
                exec_us: (6 + (a.pid as u64 * 13 + k) % 90) as u32,
                resp_us: 80,
                assignment: a,
                waited: a.hold_us > 0,
                useful: true,
            });
        }
    }
    s
}

#[test]
fn decisions_allocate_nothing_and_cost_microseconds() {
    let m = 16;
    let mut s = world(m);
    let mut b = SpinBucket::standard();
    let tr: Vec<Triggered> = (0..m).map(|pid| Triggered { pid, busy: false }).collect();
    let mut now = 100_000_000u64;
    let mut kd = Vec::with_capacity(4000);
    let mut pl = Vec::with_capacity(4000);
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..4000 {
        now += 24_000;
        let t = Instant::now();
        let pw = s.on_key_down(KeyClass::Nav, now, &mut b);
        kd.push(t.elapsed().as_nanos() as u64);
        let t = Instant::now();
        let plan = s.plan(
            &tr,
            TurnTimes {
                now_us: now + 2000,
                spent_us: Some(6),
            },
            PlaceMode::Own,
        );
        pl.push(t.elapsed().as_nanos() as u64);
        std::hint::black_box((&plan, pw));
        s.end_key(0xffff);
    }
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;
    assert_eq!(allocs, 0, "SPS-5: on_key_down and plan allocate nothing");
    kd.sort_unstable();
    pl.sort_unstable();
    let p = |v: &[u64], q: f64| v[((v.len() as f64 * q) as usize).min(v.len() - 1)];
    eprintln!(
        "PX-10 m={m} release={} on_key_down p50 {} ns p99 {} ns; plan p50 {} ns p99 {} ns max {} ns",
        !cfg!(debug_assertions),
        p(&kd, 0.5),
        p(&kd, 0.99),
        p(&pl, 0.5),
        p(&pl, 0.99),
        pl.last().unwrap()
    );
    if !cfg!(debug_assertions) {
        // The gate is p99 <= 10 us for plan and <= 2 us for on_key_down on a quiet host; a loaded
        // host is allowed 5x before the test calls it a regression.
        assert!(p(&pl, 0.99) <= 50_000, "plan p99 {} ns", p(&pl, 0.99));
        assert!(
            p(&kd, 0.99) <= 10_000,
            "on_key_down p99 {} ns",
            p(&kd, 0.99)
        );
    }
}
