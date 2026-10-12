//! Security audit (2026-10-12, commit 74dbd07): a `Text` slot holds up to `max_text_deltas`
//! deltas but is charged as one slot, to `live`, to `hard_cap` and to the global `SlotBudget`.
//! Fails today; `#[ignore]` so `make check` stays green. Run with
//! `cargo test -p gorca-ingress --test audit_text_weight -- --include-ignored`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gorca_ingress::{GLOBAL_SLOT_BUDGET, HARD_CAP, IngressConfig, SlotBudget, ingress_with_budget};
use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, TextDelta};

fn node(n: u64) -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(0),
            serial: 1,
        },
        local: LocalId(n),
    }
}

/// Finding 4. 4,096 nodes x 64 non-collapsing inserts stay under `max_keys`, never flood, and
/// one `drain()` returns 262,144 events: four times the SPEC T5 bound (`hard_cap + 4096`) while
/// the budget believes 4,096 slots are in use. Across apps the budget admits 64 such apps.
#[test]
#[ignore = "audit: fails on the reference (text deltas are not accounted)"]
fn audit_text_deltas_are_charged_and_bounded() {
    let cfg = IngressConfig::default();
    let budget = SlotBudget::new(GLOBAL_SLOT_BUDGET);
    let (tx, rx) = ingress_with_budget(cfg, &budget);
    let per_node = u32::try_from(cfg.max_text_deltas).unwrap();
    for n in 0..cfg.max_keys as u64 {
        for i in 0..per_node {
            tx.push(NodeEvent::TextChanged {
                node: node(n),
                delta: TextDelta::Insert {
                    offset: i * 2,
                    len: 1,
                    text: None,
                },
            });
        }
    }
    let charged = budget.used();
    let d = rx.drain();
    assert!(
        !d.stats.flooded,
        "precondition: the storm stays under max_keys"
    );
    assert!(
        d.events.len() <= HARD_CAP + 4096,
        "one drain returned {} events (T5 bound {}), budget charged {charged}",
        d.events.len(),
        HARD_CAP + 4096
    );
    assert!(
        charged >= d.events.len(),
        "budget charged {charged} for {} queued events",
        d.events.len()
    );
}
