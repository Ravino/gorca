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
    let mut receivers = Vec::new();
    let mut producers = Vec::new();
    for app in 0..APPS {
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        receivers.push(rx);
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
    assert!(
        worst.load(Ordering::Relaxed) <= ceiling,
        "budget used {} at its peak, ceiling {ceiling}",
        worst.load(Ordering::Relaxed)
    );
    let pending: usize = receivers
        .iter()
        .map(gorca_ingress::IngressReceiver::pending)
        .sum();
    assert_eq!(
        budget.used(),
        pending,
        "sum of pending units must equal used"
    );
    for rx in &receivers {
        rx.check_invariants().unwrap();
        drop(rx.drain());
    }
    assert_eq!(budget.used(), 0);
}
