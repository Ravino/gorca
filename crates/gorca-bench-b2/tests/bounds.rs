//! Hard resource bounds (SPEC 3.6 / 4.5): hostile input must not grow memory, and every cap
//! counter must equal the real size of its structure after every operation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::disallowed_methods
)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Fixture, random_script};
use gorca_bench_b2::clock::Rng;
use gorca_bench_b2::mockbus::MockOpts;
use gorca_bench_b2::model::Ev;
use gorca_bench_b2::procfs::rss_bytes;
use gorca_bench_b2::rig::ConsumerCfg;
use gorca_bench_b2::wire::{Any, Enc, Fields, SIGNAL, encode_event_body, encode_message};
use gorca_bench_b2::workload::{FENCE_ID, IFACE_OBJECT, canon, local_of, node_path};
use gorca_ingress::{IngressConfig, SlotBudget, ingress, ingress_with_budget};
use gorca_model::{AppId, ConnectorId};

const APP: AppId = AppId {
    connector: ConnectorId(0),
    serial: 1,
};

/// RSS is per process, so the memory tests of this file take turns.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn mib_grown(base: u64) -> u64 {
    rss_bytes().saturating_sub(base) / (1024 * 1024)
}

/// B-1: counters equal structure sizes after every push and drain, through merges, supersession,
/// flooding, rule H and dirty overflow.
#[test]
fn counters_equal_structure_sizes_after_every_operation() {
    for (seed, max_keys, hard) in [
        (1u64, 8usize, 40usize),
        (2, 4096, 65_536),
        (3, 3, 12),
        (4, 1, 6),
    ] {
        let mut rng = Rng::new(seed);
        let cfg = IngressConfig {
            max_keys,
            hard_cap: hard,
            announcement_cap: 3,
            max_text_deltas: 4,
            ..IngressConfig::default()
        };
        let (tx, rx) = ingress(cfg);
        for ev in random_script(&mut rng, 6000, 60) {
            tx.push(ev.to_node_event(APP));
            rx.check_invariants()
                .unwrap_or_else(|e| panic!("seed {seed} after push of {ev:?}: {e}"));
            if rng.below(40) == 0 {
                let _ = rx.drain();
                rx.check_invariants()
                    .unwrap_or_else(|e| panic!("seed {seed} after drain: {e}"));
            }
            assert!(rx.pending() <= hard + 1);
        }
    }
}

/// B-2: a stream of Removed/Props for one node, never drained, must not leave tombstones.
#[test]
fn supersession_does_not_leak_tombstones() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let base = rss_bytes();
    let (tx, rx) = ingress(IngressConfig::default());
    for i in 0..3_000_000u32 {
        tx.push(Ev::Props(7, 1).to_node_event(APP));
        tx.push(Ev::Removed(7).to_node_event(APP));
        if i % 100_000 == 0 {
            rx.check_invariants().unwrap();
        }
    }
    assert!(rx.pending() <= 2);
    assert!(
        mib_grown(base) < 40,
        "RSS grew {} MiB for 6M pushes on one node",
        mib_grown(base)
    );
}

/// B-3: many apps share a global budget; focus still gets through; the budget returns to zero.
#[test]
fn a_global_budget_bounds_many_apps() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let budget = SlotBudget::new(10_000);
    let base = rss_bytes();
    let mut apps = Vec::new();
    for a in 0..300u32 {
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        let app = AppId {
            connector: ConnectorId(0),
            serial: a,
        };
        for n in 0..3_000u64 {
            tx.push(Ev::Removed(n).to_node_event(app));
            tx.push(Ev::Children(n + 1_000_000).to_node_event(app));
        }
        tx.push(Ev::Focus(42).to_node_event(app));
        apps.push((tx, rx));
    }
    assert!(
        budget.used() <= 10_000 + 300,
        "budget used {}",
        budget.used()
    );
    let pending: usize = apps.iter().map(|(_, rx)| rx.pending()).sum();
    assert!(pending <= 10_000 + 300, "pending across apps {pending}");
    for (_, rx) in &apps {
        rx.check_invariants().unwrap();
    }
    for (_, rx) in &apps {
        let d = rx.drain();
        assert!(
            d.events.iter().any(|e| canon(e) == Ev::Focus(42)),
            "focus lost under the global cap"
        );
        assert!(
            d.dirty.is_some_and(|x| x.is_all() || !x.is_empty()),
            "overflow without a resync mark"
        );
    }
    assert_eq!(budget.used(), 0, "budget accounting drifted");
    assert!(mib_grown(base) < 64);
}

fn announcement(sender: &str, text: &str) -> Vec<u8> {
    let mut p = String::new();
    node_path(5, &mut p);
    let mut b = Enc::default();
    encode_event_body(&mut b, "", 2, 0, Any::Str(text), None);
    let mut out = Vec::new();
    encode_message(
        &mut out,
        SIGNAL,
        1,
        &Fields {
            path: Some(&p),
            interface: Some(IFACE_OBJECT),
            member: Some("Announcement"),
            sender: Some(sender),
            signature: Some("siiva{sv}"),
            ..Fields::default()
        },
        &b.buf,
    );
    out
}

/// B-4: oversized payloads and bus-name churn through the real drain.
#[test]
fn oversized_payloads_and_sender_churn_stay_bounded() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = Fixture::start(
        MockOpts {
            quota: 1 << 30,
            ..MockOpts::default()
        },
        &ConsumerCfg {
            fold: false,
            ..ConsumerCfg::default()
        },
    );
    let base = rss_bytes();
    let big = "x".repeat(8 * 1024 * 1024);
    for _ in 0..24 {
        f.link.send(&announcement(&f.sender, &big));
    }
    // 30 000 distinct, unregistered senders: the route cache must not grow without bound.
    let mut out = Vec::new();
    for i in 0..30_000 {
        out.extend(announcement(&format!(":1.{}", 10_000 + i), "hi"));
        if out.len() > 1 << 20 {
            f.link.send(&out);
            out.clear();
        }
    }
    f.link.send(&out);
    let mut fence = Vec::new();
    gorca_bench_b2::workload::GenEvent {
        node: FENCE_ID,
        kind: gorca_bench_b2::workload::Kind::FocusSignal,
        seq: 0,
    }
    .encode(
        &mut fence,
        &mut gorca_bench_b2::workload::Scratch::default(),
        1,
        Some(&f.sender),
    );
    f.link.send(&fence);
    assert!(
        f.router.wait_fence("app", Duration::from_secs(30)),
        "fence lost"
    );
    let grown = mib_grown(base);
    assert!(
        grown < 400,
        "RSS grew {grown} MiB for 24 x 8 MiB announcements and 30k senders"
    );
    let (cons, stats) = f.finish();
    assert_eq!(cons.fold.announcements, 0); // fold off: only the drain survived
    assert_eq!(stats.unrouted, 30_000);
    let _ = (Instant::now(), local_of(1), Arc::new(0));
}

/// B-5 (candidates only): a single message beyond `MAX_MESSAGE_BYTES` (16 MiB) is skipped
/// while streaming, never buffered. The zbus reference buffers whole messages, so it is
/// expected to fail this one; `run-all.sh` runs it with `--include-ignored` and reports it.
#[test]
#[ignore = "reference arm (zbus) buffers whole messages; candidates must pass"]
fn a_giant_message_is_skipped_not_buffered() {
    let _g = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let f = Fixture::start(
        MockOpts {
            quota: 1 << 30,
            ..MockOpts::default()
        },
        &ConsumerCfg::default(),
    );
    let base = rss_bytes();
    let giant = "y".repeat(100 * 1024 * 1024);
    f.link.send(&announcement(&f.sender, &giant));
    let mut fence = Vec::new();
    gorca_bench_b2::workload::GenEvent {
        node: FENCE_ID,
        kind: gorca_bench_b2::workload::Kind::FocusSignal,
        seq: 0,
    }
    .encode(
        &mut fence,
        &mut gorca_bench_b2::workload::Scratch::default(),
        1,
        Some(&f.sender),
    );
    f.link.send(&fence);
    assert!(f.router.wait_fence("app", Duration::from_secs(60)));
    assert!(
        mib_grown(base) < 150,
        "RSS grew {} MiB for one 100 MiB message",
        mib_grown(base)
    );
    let (_, stats) = f.finish();
    assert!(
        stats.malformed >= 1,
        "a skipped giant message must be counted as malformed"
    );
}
