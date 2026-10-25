// SPDX-License-Identifier: GPL-3.0-or-later
//! Criterion microbenchmarks (MB-*) and the entry points of the M-1 / M-3 / M-11 harnesses.
//!
//! Responsibility (ADR-0001 sections 14 and 11): "criterion microbenches (MB-*) +
//! M-1/M-3/M-11 harness entry points". Owner: perf-engineer.
//!
//! > "MB-1 mirror store read and write under contention (section 5.2); MB-2 `EpochCell` to
//! > speech-lane wakeup latency; MB-3 drain header-peek cost per message."
//!
//! Phase 0 benchmarks (`benches/ingress_mirror.rs`) cover the two reference implementations
//! whose internals are open bake-off or microbenchmark decisions: the ingress `push`/`drain`
//! (B-2) and the mirror store read/write (MB-1). They exist so those numbers are recorded from
//! the first commit. Nothing here runs in `cargo test` beyond a smoke test of the fixtures.
//!
//! Allowed dependencies: any neutral-layer crate; `criterion` as a dev-dependency.

pub mod e2e;

use gorca_model::{AppId, ConnectorId, Freshness, LocalId, NodeKey, NodeSnapshot, Role, StateSet};
use std::sync::Arc;
use std::time::Instant;

pub fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

pub fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

/// A representative snapshot for mirror benchmarks.
pub fn snapshot(n: u64) -> Arc<NodeSnapshot> {
    Arc::new(
        NodeSnapshot::new(key(n), Freshness::Fresh { at: Instant::now() })
            .with_name(Some(Arc::from("benchmark node")))
            .with_have(gorca_model::PropMask::PRESENT)
            .with_source(Role::Button, StateSet::FOCUSABLE),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_are_consistent() {
        assert_eq!(snapshot(7).key(), key(7));
    }
}
