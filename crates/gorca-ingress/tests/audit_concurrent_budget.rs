// SPDX-License-Identifier: GPL-3.0-or-later
//! AU-7: concurrent pushes never overshoot the global budget, Focus charges and ring records
//! included. Several producers per app, text deltas and focus are charged too.
//!
//! The pool is compare-and-swap bounded; the only way past `limit` is the per-app reserve and the
//! newest Focus slot: `used <= limit + apps * (R + 1)`, exactly, at every instant.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gorca_ingress::{IngressConfig, RESERVED_SLOTS, SlotBudget, ingress_with_budget};
use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, TextDelta, WindowKind};

fn node(app: u32, n: u64) -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(0),
            serial: app,
        },
        local: LocalId(n),
    }
}

#[test]
fn audit_concurrent_pushes_never_overshoot_the_budget() {
    const APPS: u32 = 8;
    const PRODUCERS_PER_APP: u32 = 2;
    let budget = SlotBudget::new(1_000);
    // SPEC B-3: the reserve is exact (a per-app compare-and-swap counter) and the pool is
    // compare-and-swap bounded, so the bound holds at every instant, whatever the producers.
    let ceiling = budget.limit() + APPS as usize * (RESERVED_SLOTS + 1);
    let stop = Arc::new(AtomicBool::new(false));
    let worst = Arc::new(AtomicUsize::new(0));
    let sampler = {
        let (budget, stop, worst) = (Arc::clone(&budget), Arc::clone(&stop), Arc::clone(&worst));
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                worst.fetch_max(budget.used(), Ordering::Relaxed);
            }
        })
    };
    let receivers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let drainer_stop = Arc::clone(&stop);
    let drainer = {
        let (receivers, stop) = (Arc::clone(&receivers), Arc::clone(&drainer_stop));
        std::thread::spawn(move || {
            // A consumer that drains every app all the time, as the app actors do.
            while !stop.load(Ordering::Acquire) {
                let all = receivers.lock().unwrap();
                for rx in all.iter() {
                    let rx: &gorca_ingress::IngressReceiver = rx;
                    drop(rx.drain());
                }
            }
        })
    };
    let mut producers = Vec::new();
    for app in 0..APPS {
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        receivers.lock().unwrap().push(rx);
        for p in 0..PRODUCERS_PER_APP {
            let tx = tx.clone();
            producers.push(std::thread::spawn(move || {
                for i in 0..20_000u64 {
                    let n = u64::from(p) * 1_000_000 + i;
                    tx.push(NodeEvent::WindowEvent {
                        node: node(app, n),
                        kind: WindowKind::Create,
                    });
                    tx.push(NodeEvent::TextChanged {
                        node: node(app, i % 7),
                        delta: TextDelta::Insert {
                            offset: u32::try_from(i).unwrap_or(0) * 2,
                            len: 1,
                            text: None,
                        },
                    });
                    tx.push(NodeEvent::FocusChanged {
                        node: node(app, i % 5),
                    });
                }
            }));
        }
    }
    for p in producers {
        p.join().unwrap();
    }
    stop.store(true, Ordering::Release);
    sampler.join().unwrap();
    drainer.join().unwrap();
    let receivers = receivers.lock().unwrap();
    assert!(
        worst.load(Ordering::Relaxed) <= ceiling,
        "budget used {} at its peak, ceiling {ceiling}",
        worst.load(Ordering::Relaxed)
    );
    // AU-1, exact: at quiescence the budget holds precisely the slots and the charged text
    // deltas of the ingresses.
    let charged: usize = receivers
        .iter()
        .map(gorca_ingress::IngressReceiver::charged_units)
        .sum();
    assert_eq!(budget.used(), charged, "AU-1: used != sum of charged units");
    for rx in receivers.iter() {
        rx.check_invariants().unwrap();
        drop(rx.drain());
    }
    assert_eq!(budget.used(), 0);
}

/// The reviewer's `judge_r4_au1` probe: AU-1 equality at quiescence with text deltas, two
/// producers per app and a concurrent drainer, measured with the units the drains deliver
/// (slots and text deltas), for a small pool (deltas collapse) and a large one (deltas queue).
#[test]
fn audit_au1_is_exact_at_quiescence() {
    for round in 0..8u32 {
        let budget = SlotBudget::new(if round % 2 == 0 { 1_000 } else { 100_000 });
        let rxs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let drainer = {
            let (r, s) = (Arc::clone(&rxs), Arc::clone(&stop));
            std::thread::spawn(move || {
                while !s.load(Ordering::Acquire) {
                    for rx in r.lock().unwrap().iter() {
                        let rx: &gorca_ingress::IngressReceiver = rx;
                        drop(rx.drain());
                    }
                }
            })
        };
        let mut ps = Vec::new();
        for app in 0..8u32 {
            let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
            rxs.lock().unwrap().push(rx);
            for p in 0..2u64 {
                let tx = tx.clone();
                ps.push(std::thread::spawn(move || {
                    for i in 0..5_000u64 {
                        tx.push(NodeEvent::WindowEvent {
                            node: node(app, p * 1_000_000 + i),
                            kind: WindowKind::Create,
                        });
                        tx.push(NodeEvent::TextChanged {
                            node: node(app, i % 7),
                            delta: TextDelta::Insert {
                                offset: u32::try_from(i).unwrap_or(0) * 2,
                                len: 1,
                                text: None,
                            },
                        });
                        tx.push(NodeEvent::FocusChanged {
                            node: node(app, i % 5),
                        });
                    }
                }));
            }
        }
        for p in ps {
            p.join().unwrap();
        }
        stop.store(true, Ordering::Release);
        drainer.join().unwrap();
        let rxs = rxs.lock().unwrap();
        for rx in rxs.iter() {
            rx.check_invariants().unwrap();
        }
        let used = budget.used();
        let charged: usize = rxs
            .iter()
            .map(gorca_ingress::IngressReceiver::charged_units)
            .sum();
        assert_eq!(used, charged, "round {round}: used != charged units");
        let delivered: usize = rxs.iter().map(|rx| rx.drain().events.len()).sum();
        assert_eq!(delivered, charged, "round {round}: delivered != charged");
        assert_eq!(budget.used(), 0);
    }
}
