//! Regression tests for the findings of the review of the B-2 fixes (2026-10-12):
//! F-1 (Focus in the ring survives preemption), F-2 (failed preemption is O(1)),
//! F-3 (the reserve is exact with any number of producers), F-4 (a backed-up reaper hands the
//! garbage to the victim), F-5 (a producer facing a full ring makes progress).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use gorca_ingress::{
    IngressConfig, IngressReceiver, IngressSender, RESERVED_SLOTS, SlotBudget, ingress_with_budget,
};
use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, WindowKind};

fn node(app: u32, n: u64) -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(0),
            serial: app,
        },
        local: LocalId(n),
    }
}

fn window(app: u32, n: u64) -> NodeEvent {
    NodeEvent::WindowEvent {
        node: node(app, n),
        kind: WindowKind::Create,
    }
}

type Conn = (IngressSender, IngressReceiver);

/// F-1. The newest Focus of a preempted victim is lossless (ADR-0004 section 8), also when it
/// still sits in the victim's ring. The reviewer's probe: before the fix the victim drained
/// `flooded=true dirty_all=true events=0`.
#[test]
fn audit_preemption_keeps_the_newest_focus_from_the_ring() {
    let budget = SlotBudget::new(240);
    let (vtx, vrx) = ingress_with_budget(IngressConfig::default(), &budget);
    for n in 0..250 {
        vtx.push(window(1, n));
    }
    vtx.push(NodeEvent::FocusChanged { node: node(1, 7) });
    // Another app grows until the pool is exhausted and then takes the victim's slots back.
    let (ftx, frx) = ingress_with_budget(IngressConfig::default(), &budget);
    for n in 0..200 {
        ftx.push(window(2, n));
    }
    let vd = vrx.drain();
    assert!(vd.stats.flooded, "the victim was not preempted");
    assert!(
        vd.dirty
            .as_ref()
            .is_some_and(gorca_ingress::DirtySet::is_all)
    );
    let focus: Vec<_> = vd
        .events
        .iter()
        .filter_map(|e| match e {
            NodeEvent::FocusChanged { node } => Some(*node),
            _ => None,
        })
        .collect();
    assert_eq!(focus, vec![node(1, 7)], "newest focus of the victim lost");
    drop(frx.drain());
    drop((vtx, vrx, ftx, frx));
    assert_eq!(budget.used(), 0);
}

fn many_apps() -> u32 {
    if cfg!(debug_assertions) {
        2_300
    } else {
        10_000
    }
}

fn push_bound() -> Duration {
    if cfg!(debug_assertions) {
        Duration::from_millis(5)
    } else {
        Duration::from_micros(250)
    }
}

/// F-2, victims exist. `N` apps each hold one unit above the reserve and exhaust the pool; the
/// Focus push of a fresh app finds the pool exhausted, takes one victim's slots (a window of
/// the apps, not a scan of all of them) and returns. Bound: 250 us, at 10,000 apps.
#[test]
fn audit_focus_push_with_many_apps_and_exhausted_pool_is_bounded() {
    let n = many_apps();
    let budget = SlotBudget::new(n as usize);
    let apps: Vec<Conn> = (0..n)
        .map(|a| {
            let c = ingress_with_budget(IngressConfig::default(), &budget);
            for k in 0..=RESERVED_SLOTS as u64 {
                c.0.push(window(a, k));
            }
            c.1.pending();
            c
        })
        .collect();
    assert!(budget.pool_used() >= budget.limit() - 64, "pool exhausted");
    let mut worst = Duration::ZERO;
    for i in 0..20u32 {
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        for k in 0..RESERVED_SLOTS as u64 {
            tx.push(window(n + i, k));
        }
        rx.pending();
        let t = Instant::now();
        tx.push(NodeEvent::FocusChanged {
            node: node(n + i, 99),
        });
        rx.pending();
        worst = worst.max(t.elapsed());
        let d = rx.drain();
        assert!(
            d.events
                .iter()
                .any(|e| matches!(e, NodeEvent::FocusChanged { .. })),
            "the newest focus must be queued"
        );
    }
    assert!(
        worst <= push_bound(),
        "Focus push with {n} apps took {worst:?}"
    );
    drop(apps);
}

/// F-2, nobody to take from. `N` apps hold exactly the reserve; one app has the whole pool; its
/// own Focus finds the pool exhausted and no victim. The failed preemption must not scan.
#[test]
fn audit_focus_push_without_a_victim_is_bounded() {
    let n = many_apps();
    let budget = SlotBudget::new(500);
    let apps: Vec<Conn> = (0..n)
        .map(|a| {
            let c = ingress_with_budget(IngressConfig::default(), &budget);
            for k in 0..RESERVED_SLOTS as u64 {
                c.0.push(window(a, k));
            }
            c.1.pending();
            c
        })
        .collect();
    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    for k in 0..2_000u64 {
        tx.push(window(n + 1, k));
    }
    rx.pending();
    assert!(budget.pool_used() >= budget.limit());
    let mut worst = Duration::ZERO;
    for i in 0..20u64 {
        let t = Instant::now();
        tx.push(NodeEvent::FocusChanged {
            node: node(n + 1, 10_000 + i),
        });
        rx.pending();
        worst = worst.max(t.elapsed());
    }
    assert!(
        worst <= push_bound(),
        "Focus push with {n} apps and no victim took {worst:?}"
    );
    drop(apps);
}

/// F-3. One app with `P` producers and an empty pool: whatever happens, `used()` never passes
/// `R + 1` (the reserve and the Focus unit). The reviewer measured 48 to 80 before the fix.
#[test]
fn audit_reserve_is_exact_with_concurrent_producers() {
    for &p in &[1u32, 2, 4, 8, 16] {
        for _round in 0..10 {
            let budget = SlotBudget::new(0);
            let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
            let stop = Arc::new(AtomicBool::new(false));
            let worst = Arc::new(AtomicUsize::new(0));
            let sampler = {
                let (b, s, w) = (Arc::clone(&budget), Arc::clone(&stop), Arc::clone(&worst));
                std::thread::spawn(move || {
                    while !s.load(Ordering::Acquire) {
                        w.fetch_max(b.used(), Ordering::Relaxed);
                    }
                })
            };
            let bar = Arc::new(Barrier::new(p as usize));
            let hs: Vec<_> = (0..p)
                .map(|k| {
                    let (tx, bar) = (tx.clone(), Arc::clone(&bar));
                    std::thread::spawn(move || {
                        bar.wait();
                        for i in 0..2_000u64 {
                            tx.push(window(1, u64::from(k) * 1_000_000 + i));
                        }
                        tx.push(NodeEvent::FocusChanged { node: node(1, 1) });
                    })
                })
                .collect();
            for h in hs {
                h.join().unwrap();
            }
            stop.store(true, Ordering::Release);
            sampler.join().unwrap();
            let peak = worst.load(Ordering::Relaxed);
            assert!(
                peak <= RESERVED_SLOTS + 1,
                "{p} producers: peak {peak}, bound {}",
                RESERVED_SLOTS + 1
            );
            let pending = rx.pending();
            assert_eq!(budget.used(), pending, "AU-1 after settle");
            drop((tx, rx));
            assert_eq!(budget.used(), 0);
        }
    }
}

/// AU-3 with two producers: an app within its reserve is untouched while a hostile app has
/// exhausted the pool.
#[test]
fn audit_reserve_holds_with_two_producers_under_an_exhausted_pool() {
    for _round in 0..50 {
        let budget = SlotBudget::new(2_000);
        let (htx, hrx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..5_000u64 {
            htx.push(window(9, n));
        }
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        let bar = Arc::new(Barrier::new(2));
        let hs: Vec<_> = (0..2u64)
            .map(|k| {
                let (tx, bar) = (tx.clone(), Arc::clone(&bar));
                std::thread::spawn(move || {
                    bar.wait();
                    for i in 0..(RESERVED_SLOTS as u64 / 2) {
                        tx.push(window(1, k * 1_000 + i));
                    }
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), RESERVED_SLOTS);
        assert!(d.dirty.is_none());
        drop(hrx.drain());
        drop((htx, hrx, tx, rx));
        assert_eq!(budget.used(), 0);
    }
}

/// F-5. Many producers on a small number of ring slots, with a consumer that never drains:
/// every producer finishes (the wait for a free ring slot is bounded and ends in a fold).
#[test]
fn audit_full_ring_producers_make_progress() {
    let (tx, rx) = gorca_ingress::ingress(IngressConfig::default());
    let hs: Vec<_> = (0..8u64)
        .map(|k| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for i in 0..20_000u64 {
                    tx.push(window(1, k * 1_000_000 + i));
                }
            })
        })
        .collect();
    let t = Instant::now();
    for h in hs {
        h.join().unwrap();
    }
    assert!(t.elapsed() < Duration::from_secs(30));
    rx.check_invariants().unwrap();
    assert!(rx.pending() > 0);
}

/// AU-6: the push that finds the ring full folds a chunk of it under the table lock. Even with
/// distinct keys (the most expensive records) that must stay under 250 us.
#[test]
fn audit_help_fold_of_a_full_ring_is_bounded() {
    let bound = if cfg!(debug_assertions) {
        Duration::from_millis(10)
    } else {
        Duration::from_micros(250)
    };
    let mut worst = Duration::ZERO;
    for round in 0..20u64 {
        let budget = SlotBudget::new(1 << 20);
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..gorca_ingress::RING_CAP as u64 {
            tx.push(window(1, round * 10_000 + n));
        }
        let t = Instant::now();
        tx.push(window(1, 9_000_000 + round));
        worst = worst.max(t.elapsed());
        assert_eq!(rx.pending(), gorca_ingress::RING_CAP + 1);
    }
    assert!(worst <= bound, "help fold took {worst:?}");
}
