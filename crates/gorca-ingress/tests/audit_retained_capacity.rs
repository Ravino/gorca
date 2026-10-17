// SPDX-License-Identifier: GPL-3.0-or-later
//! Security audit (2026-10-12, commit 74dbd07): `take()`/`enter_flooded()`/`compact_if_sparse()`
//! call `HashMap::clear()` on `index` and `by_node`, which keeps the capacity. One storm to
//! `hard_cap` leaves ~11 MiB per app allocated for the life of the app, outside the global
//! `SlotBudget`. One test per binary because RSS is process-wide. Fails today; `#[ignore]` so
//! `make check` stays green. Run with
//! `cargo test --release -p gorca-ingress --test audit_retained_capacity -- --include-ignored`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gorca_ingress::{GLOBAL_SLOT_BUDGET, HARD_CAP, IngressConfig, SlotBudget, ingress_with_budget};
use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, WindowKind};

fn rss_mib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap();
    let kib: u64 = s
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|x| x.parse().ok())
        .unwrap();
    kib / 1024
}

/// Finding 5. Sixteen apps storm one after the other; each is drained before the next starts,
/// so the global budget never holds more than 65,536 slots and ends at 0. Memory still grows
/// by about 11 MiB per app and stays allocated while the apps live.
#[test]
fn audit_drained_storm_releases_memory() {
    const APPS: u32 = 16;
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    let base = rss_mib();
    let mut apps = Vec::new();
    for a in 0..APPS {
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..HARD_CAP as u64 {
            tx.push(NodeEvent::WindowEvent {
                node: NodeKey {
                    app: AppId {
                        connector: ConnectorId(0),
                        serial: a,
                    },
                    local: LocalId(n),
                },
                kind: WindowKind::Create,
            });
        }
        drop(rx.drain());
        assert_eq!(rx.pending(), 0);
        apps.push((tx, rx));
    }
    assert_eq!(budget.used(), 0);
    let grown = rss_mib().saturating_sub(base);
    // Generous: 1 MiB per idle, empty app.
    assert!(
        grown <= u64::from(APPS),
        "{APPS} drained, empty apps retain {grown} MiB (budget used: 0)"
    );
}
