//! AU-3 / AU-6 / AU-1 regression tests for the judge's findings on candidate A (B-2 judgement,
//! section 4.4): many-connection starvation, the cost of a preemption, uncharged ring records.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_ingress::{
    GLOBAL_SLOT_BUDGET, HARD_CAP, IngressConfig, IngressReceiver, IngressSender, RESERVED_SLOTS,
    SlotBudget, ingress_with_budget,
};
use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, Politeness, WindowKind};

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

fn announcement(app: u32) -> NodeEvent {
    NodeEvent::Announcement {
        node: Some(node(app, 0)),
        text: Arc::from("new mail"),
        politeness: Politeness::Polite,
    }
}

type Conn = (IngressSender, IngressReceiver);

/// The judge's attack: `k` connections of one hostile process, each shaped to hold about `per`
/// distinct slots (then repeats of one key, so ring folds stay below any size threshold).
fn hostile(budget: &Arc<SlotBudget>, k: u32, per: u64, repeats: u32) -> Vec<Conn> {
    (0..k)
        .map(|app| {
            let (tx, rx) = ingress_with_budget(IngressConfig::default(), budget);
            for n in 0..per {
                tx.push(window(app, n));
            }
            for _ in 0..repeats {
                tx.push(NodeEvent::NodeRemoved {
                    node: node(app, 999_999),
                });
            }
            (tx, rx)
        })
        .collect()
}

/// Judge probe `judge_shaped_holders_below_preempt_threshold`: about 2,300 connections each
/// holding about 121 slots exhaust the pool; the victim (20 events, announcement included) must
/// keep every one of them. Before the fix the victim kept 0 of 20.
#[test]
fn audit_many_connections_cannot_starve_a_victim() {
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    let attackers = hostile(&budget, 2_300, 120, 1_200);
    assert!(
        budget.used() + 1_000 >= GLOBAL_SLOT_BUDGET,
        "attack must exhaust the pool"
    );

    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    let victim = 1_000_000u32;
    tx.push(NodeEvent::NodeRemoved {
        node: node(victim, 7),
    });
    for n in 0..18 {
        tx.push(window(victim, 8 + n));
    }
    tx.push(announcement(victim));
    let d = rx.drain();
    assert_eq!(d.events.len(), 20, "victim lost events: {:?}", d.stats);
    assert_eq!(d.stats.announcements_dropped, 0);
    assert!(d.dirty.is_none(), "victim was flooded by others");
    drop(attackers);
    drop((tx, rx));
    assert_eq!(budget.used(), 0);
}

/// Preemption works at any victim size: holders of only a little more than the reserve are
/// shed (flooded, dirty-all, never silent) so that a newcomer above its own reserve is served.
#[test]
fn audit_small_holders_are_preempted_for_a_larger_claim() {
    let limit = 1_000;
    let budget = SlotBudget::new(limit);
    // Ten holders of 100 slots each exactly fill the pool (each is above the reserve of 32).
    let holders: Vec<Conn> = hostile(&budget, 10, 100, 0);
    assert_eq!(budget.used(), limit);

    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    let wanted = RESERVED_SLOTS as u64 + 8;
    for n in 0..wanted {
        tx.push(window(77, n));
    }
    let d = rx.drain();
    assert_eq!(
        d.events.len() as u64,
        wanted,
        "newcomer above its reserve was not served: {:?}",
        d.stats
    );
    assert!(d.dirty.is_none());
    // Somebody paid, and was told so: a shed holder is flooded with a full dirty set.
    let mut shed = 0;
    for (_, hrx) in &holders {
        let hd = hrx.drain();
        if hd.stats.flooded {
            assert!(
                hd.dirty
                    .as_ref()
                    .is_some_and(gorca_ingress::DirtySet::is_all)
            );
            shed += 1;
        } else {
            assert_eq!(hd.events.len(), 100);
        }
    }
    assert!(shed >= 1, "nobody was preempted");
    drop(holders);
    drop((tx, rx));
    assert_eq!(budget.used(), 0);
}

/// An app that stays within the reserve is never affected by anybody, however full the pool.
#[test]
fn audit_reserve_is_untouchable() {
    let budget = SlotBudget::new(256);
    let holders: Vec<Conn> = hostile(&budget, 4, 64, 0);
    assert!(budget.used() >= 256);
    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    for n in 0..RESERVED_SLOTS as u64 {
        tx.push(window(9, n));
    }
    let d = rx.drain();
    assert_eq!(d.events.len(), RESERVED_SLOTS);
    assert!(d.dirty.is_none());
    drop(holders);
}

/// AU-1: records waiting in the ring are charged at push, so `used()` is exact without any fold.
#[test]
fn audit_ring_records_are_charged() {
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    // Fewer than the ring and the eager-fold threshold: everything sits in the ring.
    for n in 0..500 {
        tx.push(window(1, n));
    }
    assert_eq!(budget.used(), 500, "ring records are not charged");
    // Repeats of one key coalesce when folded, but they were charged while queued.
    for _ in 0..100 {
        tx.push(NodeEvent::NodeRemoved { node: node(1, 5) });
    }
    assert_eq!(budget.used(), 600);
    assert_eq!(rx.pending(), 500, "window 5 is superseded by Removed(5)");
    assert_eq!(budget.used(), rx.pending());
    drop(rx.drain());
    assert_eq!(budget.used(), 0);
    for n in 0..300 {
        tx.push(window(1, n));
    }
    drop(rx);
    assert_eq!(
        budget.used(),
        0,
        "dropping the receiver must return the ring"
    );
    drop(tx);
    assert_eq!(budget.used(), 0);
}

/// Rings alone cannot be used to exhaust the pool beyond the limit: the 1,024 records of every
/// app are part of the bound.
#[test]
fn audit_ring_charges_respect_the_limit() {
    let limit = 2_000;
    let budget = SlotBudget::new(limit);
    let apps: Vec<Conn> = (0..8)
        .map(|a| {
            let c = ingress_with_budget(IngressConfig::default(), &budget);
            for n in 0..900 {
                c.0.push(window(a, n));
            }
            c
        })
        .collect();
    let ceiling = limit + apps.len() * (RESERVED_SLOTS + 1);
    assert!(budget.used() <= ceiling, "used {}", budget.used());
    drop(apps);
    assert_eq!(budget.used(), 0);
}

/// AU-6: the push that preempts a 65,536-slot holder must not pay for freeing it. The old
/// implementation took 0.7 to 2 ms (judge, `preempt-cost.txt`); the bound is 250 us.
#[test]
fn audit_preempting_push_is_bounded() {
    let limit = HARD_CAP;
    let bound = if cfg!(debug_assertions) {
        Duration::from_millis(5)
    } else {
        Duration::from_micros(250)
    };
    let mut worst = Duration::ZERO;
    for round in 0..5u32 {
        let budget = SlotBudget::new(limit);
        let (htx, hrx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..HARD_CAP as u64 {
            htx.push(window(1, n));
        }
        assert_eq!(hrx.pending(), HARD_CAP);
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..RESERVED_SLOTS as u64 {
            tx.push(window(2, n));
        }
        // The next push finds the pool exhausted and takes the holder's slots back.
        let t = Instant::now();
        tx.push(window(2, 1_000 + u64::from(round)));
        let took = t.elapsed();
        worst = worst.max(took);
        assert_eq!(rx.drain().events.len(), RESERVED_SLOTS + 1);
        let hd = hrx.drain();
        assert!(hd.stats.flooded, "the holder was not preempted");
        assert!(
            hd.dirty
                .as_ref()
                .is_some_and(gorca_ingress::DirtySet::is_all)
        );
        drop((htx, hrx, tx, rx));
        assert_eq!(budget.used(), 0);
    }
    assert!(
        worst <= bound,
        "preempting push took {worst:?}, bound {bound:?}"
    );
}
