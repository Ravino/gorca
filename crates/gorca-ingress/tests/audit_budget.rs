//! Security audit of the `SlotBudget` accounting (2026-10-12, commit 74dbd07).
//!
//! Threat model: any process on the a11y bus can send unlimited signals, open several
//! connections (each one is a separate app with its own ingress) and connect/disconnect at will.
//! Every test here demonstrates a defect of the reference coalescer and fails today; they are
//! `#[ignore]` so `make check` stays green. Run with
//! `cargo test -p gorca-ingress --test audit_budget -- --include-ignored`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use gorca_ingress::{GLOBAL_SLOT_BUDGET, HARD_CAP, IngressConfig, SlotBudget, ingress_with_budget};
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

/// `Window` is a kept (lossless) class, so it fills an app up to `hard_cap` even after the app
/// is `Flooded` at `max_keys`.
fn fill_to_hard_cap(tx: &gorca_ingress::IngressSender, app: u32) {
    for n in 0..HARD_CAP as u64 {
        tx.push(window(app, n));
    }
}

/// Finding 1. Teardown of an app (both ends dropped) with slots pending must return them to the
/// global budget. `Inner` has no `Drop`, so they are leaked forever.
#[test]
#[ignore = "audit: fails on the reference (budget leak on drop)"]
fn audit_budget_released_when_ingress_dropped() {
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    {
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..1000 {
            tx.push(window(1, n));
        }
        assert_eq!(rx.pending(), 1000);
        // App disconnects: the router drops its route and the actor drops the receiver.
    }
    assert_eq!(
        budget.used(),
        0,
        "slots of a torn-down app are still charged to the global budget"
    );
}

/// Finding 1, escalated. Five short-lived hostile apps (connect, flood, disconnect before the
/// actor drains) exhaust the 262,144-slot global budget permanently. From then on every new app
/// loses its lossless events: a `Window` event and an announcement of a fresh, well-behaved app
/// are dropped (dirty-all / `announcements_dropped`) although nothing is pending anywhere.
#[test]
#[ignore = "audit: fails on the reference (sender churn exhausts the global budget forever)"]
fn audit_sender_churn_exhausts_global_budget_permanently() {
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    for app in 0..5u32 {
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        fill_to_hard_cap(&tx, app);
        drop((tx, rx));
    }
    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    tx.push(window(99, 1));
    tx.push(announcement(99));
    let d = rx.drain();
    assert_eq!(
        budget.used(),
        0,
        "budget used {} with no live ingress holding a slot",
        budget.used()
    );
    assert_eq!(
        d.events.len(),
        2,
        "a fresh app lost its window event and announcement: events={:?} dropped={} dirty_all={}",
        d.events,
        d.stats.announcements_dropped,
        d.dirty
            .as_ref()
            .is_some_and(gorca_ingress::DirtySet::is_all)
    );
}

/// Finding 2. After teardown the drain's route cache (`reference.rs` `routes`, cleared only at
/// 4,096 distinct senders) still holds the `IngressSender`. Signals still in flight from the dead
/// sender keep filling an ingress nobody will ever drain, charged to the global budget.
#[test]
#[ignore = "audit: fails on the reference (push into a dead ingress is accepted and charged)"]
fn audit_dead_receiver_does_not_hold_budget() {
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    drop(rx);
    for n in 0..100_000 {
        tx.push(window(1, n));
    }
    assert_eq!(
        budget.used(),
        0,
        "an ingress without a receiver holds {} budget slots",
        budget.used()
    );
}

/// Finding 3. The global budget is first come, first served. Four connections of one hostile
/// process whose actors do not drain (hung app, slow resync) take all 262,144 slots; a victim
/// app with nothing pending then loses its announcement (unrecoverable: a resync cannot
/// rediscover speech) and its `Removed`/`Window` events (dirty-all).
#[test]
#[ignore = "audit: fails on the reference (one hostile process starves every other app)"]
fn audit_hostile_apps_cannot_starve_a_victim() {
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    let hostile: Vec<_> = (0..4u32)
        .map(|app| {
            let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
            fill_to_hard_cap(&tx, app);
            (tx, rx)
        })
        .collect();
    assert!(budget.used() >= GLOBAL_SLOT_BUDGET);

    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
    tx.push(NodeEvent::NodeRemoved { node: node(99, 7) });
    tx.push(window(99, 8));
    tx.push(announcement(99));
    let d = rx.drain();
    assert_eq!(
        d.stats.announcements_dropped, 0,
        "victim announcement dropped because other apps used the global budget"
    );
    assert_eq!(
        d.events.len(),
        3,
        "victim lost lossless events to other apps' budget use: {:?}",
        d.events
    );
    drop(hostile);
}
