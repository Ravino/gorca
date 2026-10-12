//! Security audit (2026-10-12, commit 74dbd07): worst-case cost of one critical section.
//!
//! The reference drain is one thread that pushes for every app; a push that holds an app's lock
//! for O(live) delays every other app's events, focus included. Amortised O(1) is not enough
//! for p99 < 15 ms end to end when the attacker controls when the O(n) step happens. The
//! 250 us bound is a release-build requirement: the tests are ignored when `debug_assertions` is
//! on (debug builds are 10x+ slower and flaked 1-2 runs in 5 on every B-2 candidate), and run
//! by `cargo test --release -p gorca-ingress --test audit_latency -- --test-threads=1`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use gorca_ingress::{HARD_CAP, IngressConfig, ingress};
use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, WindowKind};

/// Bound on one `push` or one `drain` critical section. 250 us is 1/60 of the latency budget.
const MAX_CRITICAL_SECTION: Duration = Duration::from_micros(250);

fn node(n: u64) -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(0),
            serial: 1,
        },
        local: LocalId(n),
    }
}

fn window(n: u64) -> NodeEvent {
    NodeEvent::WindowEvent {
        node: node(n),
        kind: WindowKind::Create,
    }
}

/// Finding 6a. Fill `hard_cap - 1` lossless slots, then repeat `Removed(x)` on one node: every
/// ~65.8k pushes one push compacts 65k slots and rebuilds two hash maps under the lock.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing bound applies to release builds; debug is 10x+ slower and flaky"
)]
fn audit_single_push_is_bounded_under_tombstone_compaction() {
    let (tx, _rx) = ingress(IngressConfig::default());
    for n in 0..(HARD_CAP as u64 - 1) {
        tx.push(window(n));
    }
    let mut worst = Duration::ZERO;
    for _ in 0..200_000 {
        let t = Instant::now();
        tx.push(NodeEvent::NodeRemoved {
            node: node(u64::MAX),
        });
        worst = worst.max(t.elapsed());
    }
    assert!(
        worst <= MAX_CRITICAL_SECTION,
        "worst single push took {worst:?}"
    );
}

/// Finding 6b. `drain()` builds the whole event vector of up to `hard_cap` slots while holding
/// the lock the drain thread needs to push the next event of this app.
#[test]
#[cfg_attr(
    debug_assertions,
    ignore = "timing bound applies to release builds; debug is 10x+ slower and flaky"
)]
fn audit_drain_critical_section_is_bounded() {
    let (tx, rx) = ingress(IngressConfig::default());
    for n in 0..HARD_CAP as u64 {
        tx.push(window(n));
    }
    let t = Instant::now();
    let d = rx.drain();
    let took = t.elapsed();
    assert_eq!(d.events.len(), HARD_CAP);
    assert!(
        took <= MAX_CRITICAL_SECTION,
        "drain held the producer lock for {took:?}"
    );
}
