// SPDX-License-Identifier: GPL-3.0-or-later
//! MA-6 evidence: the cost of `apply` per node, and of `sweep` and `invalidate_all`, at the caps.
//! Printed, and asserted against a generous bound so a regression of an order of magnitude fails.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::sync::Arc;
use std::time::Instant;

use gorca_mirror::{ApplyOrigin, MirrorRead, mirror};
use gorca_model::{
    AppId, ChildList, ConnectorId, Freshness, LocalId, NodeKey, NodeSnapshot, PropMask, Role,
    SnapshotBatch, StateSet,
};

fn node(n: u64) -> Arc<NodeSnapshot> {
    Arc::new(
        NodeSnapshot::new(
            NodeKey {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                local: LocalId(n),
            },
            Freshness::Fresh { at: Instant::now() },
        )
        .with_name(Some(Arc::from("a typical accessible name")))
        .with_children(ChildList::Count(3))
        .with_have(PropMask::CACHE_ITEM)
        .with_source(Role::Button, StateSet::empty()),
    )
}

#[test]
fn apply_cost_per_node_at_the_caps() {
    let (mut w, r) = mirror();
    let t = Instant::now();
    for chunk in 0..16u64 {
        let nodes: Vec<_> = (0..4096).map(|i| node(chunk * 4096 + i)).collect();
        w.apply(SnapshotBatch::new(nodes), ApplyOrigin::Push);
    }
    let per_node_ns = t.elapsed().as_nanos() as f64 / 65_536.0;
    // Re-applying (the common case after the first fill) and invalidating.
    let t = Instant::now();
    let nodes: Vec<_> = (0..4096).map(node).collect();
    w.apply(SnapshotBatch::new(nodes), ApplyOrigin::Push);
    let reapply = t.elapsed();
    let t = Instant::now();
    w.invalidate_all(Instant::now());
    let invalidate_all = t.elapsed();
    let t = Instant::now();
    w.sweep(Instant::now());
    let sweep = t.elapsed();
    println!(
        "apply fresh {per_node_ns:.0} ns/node ({:.0} us per 4096), re-apply of 4096: {reapply:?}, \
         invalidate_all: {invalidate_all:?}, sweep: {sweep:?}, live {}",
        per_node_ns * 4096.0 / 1000.0,
        r.stats().live
    );
    assert!(invalidate_all.as_micros() < 250, "{invalidate_all:?}");
    assert!(sweep.as_micros() < 2_000, "{sweep:?}");
    assert!(per_node_ns < 20_000.0, "{per_node_ns}");
}

/// Per-slice cost while a mirror grows to the node cap: where do the spikes come from?
#[test]
fn slice_cost_distribution_while_growing() {
    let (mut w, _r) = mirror();
    let mut times = Vec::new();
    for slice in 0..512u64 {
        let nodes: Vec<_> = (0..128).map(|i| node(slice * 128 + i)).collect();
        let t = Instant::now();
        w.apply(SnapshotBatch::new(nodes), ApplyOrigin::Push);
        times.push((t.elapsed().as_micros(), slice));
    }
    let mut sorted = times.clone();
    sorted.sort_unstable();
    let top: Vec<_> = sorted.iter().rev().take(6).collect();
    println!(
        "slices: p50 {} us, p99 {} us, top {top:?}",
        sorted[256].0, sorted[506].0
    );
}
