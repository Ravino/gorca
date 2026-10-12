//! Ingress conformance (SPEC section 4): the candidate's `IngressSender`/`IngressReceiver`
//! against the executable specification in `gorca_bench_b2::model`, no bus involved.

#![allow(
    clippy::items_after_statements,
    clippy::redundant_closure_for_method_calls,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::disallowed_methods,
    clippy::manual_is_multiple_of,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use common::random_script;
use gorca_bench_b2::clock::Rng;
use gorca_bench_b2::model::{Batch, Ev, Fold, SpecCfg, SpecModel, compare};
use gorca_bench_b2::procfs::rss_bytes;
use gorca_bench_b2::workload::canon;
use gorca_ingress::{Drained, IngressConfig, ingress};
use gorca_model::{AppId, ConnectorId};

const APP: AppId = AppId {
    connector: ConnectorId(0),
    serial: 1,
};

fn to_batch(d: &Drained) -> Batch {
    Batch {
        events: d.events.iter().map(canon).collect(),
        flooded: d.stats.flooded,
        dirty_all: d.dirty.as_ref().is_some_and(|x| x.is_all()),
        dirty: d
            .dirty
            .as_ref()
            .map(|x| x.iter().map(|k| k.local.0).collect::<BTreeSet<_>>())
            .unwrap_or_default(),
        pushed: d.stats.pushed,
        focus_sequence: d.stats.focus_sequence,
        announcements_dropped: d.stats.announcements_dropped,
    }
}

fn cfg_pair(max_keys: usize, ann: usize, text: usize, hard: usize) -> (IngressConfig, SpecCfg) {
    (
        IngressConfig {
            max_keys,
            announcement_cap: ann,
            max_text_deltas: text,
            hard_cap: hard,
        },
        SpecCfg {
            max_keys,
            announcement_cap: ann,
            max_text_deltas: text,
            hard_cap: hard,
            ..SpecCfg::default()
        },
    )
}

/// C-I1: every batch equals the model's batch, for random scripts and drain points.
#[test]
fn sequential_behaviour_equals_the_model() {
    let configs = [
        cfg_pair(4096, 16, 64, 65_536),
        cfg_pair(8, 3, 4, 65_536),
        cfg_pair(32, 2, 3, 64),
        cfg_pair(1, 1, 1, 1024),
    ];
    for seed in 0..120u64 {
        let mut rng = Rng::new(seed);
        let (icfg, scfg) = configs[(seed % configs.len() as u64) as usize];
        let nodes = [3, 6, 20, 80][rng.below(4) as usize];
        let n = 50 + rng.below(1500) as usize;
        let script = random_script(&mut rng, n, nodes);
        let every = [1, 3, 20, 200, 100_000][rng.below(5) as usize];
        let (tx, rx) = ingress(icfg);
        let mut model = SpecModel::new(scfg);
        for (i, ev) in script.iter().enumerate() {
            tx.push(ev.to_node_event(APP));
            model.push(ev);
            if (i + 1) % every == 0 {
                let got = to_batch(&rx.drain());
                let want = model.drain();
                assert_eq!(
                    got, want,
                    "seed {seed}, batch ending at event {i}, cfg {icfg:?}"
                );
            }
        }
        let got = to_batch(&rx.drain());
        let want = model.drain();
        assert_eq!(got, want, "seed {seed}, final batch, cfg {icfg:?}");
        assert_eq!(rx.pending(), 0);
    }
}

/// C-I2: the node is attached to the right app and `NodeKey`s survive the round trip.
#[test]
fn events_keep_their_app_and_node() {
    let (tx, rx) = ingress(IngressConfig::default());
    for n in 0..50u64 {
        tx.push(Ev::Caret(n, 7).to_node_event(APP));
    }
    let d = rx.drain();
    assert_eq!(d.events.len(), 50);
    for e in &d.events {
        assert_eq!(e.node().map(|k| k.app), Some(APP));
    }
}

/// C-I3: `notified()` fires after a push into an empty ingress, and no wake-up is lost while a
/// consumer drains concurrently with a producer.
#[test]
fn no_lost_wakeup_with_a_concurrent_consumer() {
    let (tx, rx) = ingress(IngressConfig::default());
    let done = Arc::new(AtomicBool::new(false));
    let producer = {
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let mut rng = Rng::new(5);
            for i in 0..200_000u32 {
                tx.push(Ev::Caret(u64::from(i % 50), i).to_node_event(APP));
                if rng.below(2000) == 0 {
                    std::thread::yield_now();
                }
                if rng.below(20_000) == 0 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            tx.push(Ev::Focus(999_999).to_node_event(APP));
            done.store(true, Ordering::Release);
        })
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    let mut saw_focus = false;
    let mut timeouts = 0;
    rt.block_on(async {
        loop {
            let woke = tokio::time::timeout(Duration::from_millis(500), rx.notified())
                .await
                .is_ok();
            let d = rx.drain();
            if !woke && !d.events.is_empty() {
                timeouts += 1;
            }
            if d.events
                .iter()
                .any(|e| matches!(canon(e), Ev::Focus(999_999)))
            {
                saw_focus = true;
            }
            if done.load(Ordering::Acquire) && !woke && d.events.is_empty() {
                break;
            }
            if saw_focus && done.load(Ordering::Acquire) {
                let rest = rx.drain();
                assert!(rest.events.is_empty());
                break;
            }
        }
    });
    producer.join().unwrap();
    assert!(saw_focus, "the final focus event was never delivered");
    assert_eq!(
        timeouts, 0,
        "work was pending but notified() did not fire (lost wake-up)"
    );
}

/// C-I4: several producers (clones of the sender) on disjoint nodes plus one focus writer;
/// a concurrent consumer; the folded state equals the ground truth, per-node order holds.
#[test]
fn concurrent_producers_converge_to_the_truth() {
    for max_keys in [4096usize, 64] {
        let cfg = IngressConfig {
            max_keys,
            ..IngressConfig::default()
        };
        let (tx, rx) = ingress(cfg);
        const THREADS: u64 = 4;
        const PER: u32 = 60_000;
        let scripts: Vec<Vec<Ev>> = (0..THREADS)
            .map(|t| {
                let mut rng = Rng::new(100 + t);
                (0..PER)
                    .map(|i| {
                        let node = t * 1000 + rng.below(40);
                        match rng.below(8) {
                            0 => Ev::Props(node, 1 << rng.below(9)),
                            1 => Ev::Caret(node, i),
                            2 => Ev::TextIns(node, i * 3, 1),
                            3 => Ev::TextDel(node, i * 3 + 1, 1),
                            4 => Ev::Children(node),
                            5 => Ev::Selection(node),
                            6 => Ev::Active(node, Some(node + 1)),
                            _ => Ev::Doc(node, (i % 3) as u8),
                        }
                    })
                    .chain(std::iter::once(Ev::Focus(t * 1000 + 999)))
                    .collect()
            })
            .collect();
        let producers: Vec<_> = scripts
            .iter()
            .cloned()
            .enumerate()
            .map(|(t, s)| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for (i, e) in s.iter().enumerate() {
                        tx.push(e.to_node_event(APP));
                        if t == 0 && i % 5000 == 0 {
                            std::thread::yield_now();
                        }
                    }
                })
            })
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let consumer = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut fold = Fold::default();
                let mut last_caret = std::collections::HashMap::new();
                let mut order_violations = 0;
                loop {
                    let finished = stop.load(Ordering::Acquire);
                    let d = rx.drain();
                    let evs: Vec<Ev> = d.events.iter().map(canon).collect();
                    for e in &evs {
                        if let Ev::Caret(n, o) = e
                            && last_caret.insert(*n, *o).is_some_and(|p| *o < p)
                        {
                            order_violations += 1;
                        }
                    }
                    let (all, nodes): (bool, Vec<u64>) = match &d.dirty {
                        Some(x) => (x.is_all(), x.iter().map(|k| k.local.0).collect()),
                        None => (false, vec![]),
                    };
                    fold.apply_batch(
                        &evs,
                        all,
                        nodes,
                        d.stats.pushed,
                        d.stats.announcements_dropped,
                        d.stats.flooded,
                    );
                    if finished && d.events.is_empty() {
                        return (fold, order_violations);
                    }
                    // Let batches grow so coalescing really happens under concurrency.
                    std::thread::sleep(Duration::from_micros(300));
                }
            })
        };
        for p in producers {
            p.join().unwrap();
        }
        stop.store(true, Ordering::Release);
        let (got, order_violations) = consumer.join().unwrap();
        // Truth: the last focus is whichever producer wrote last; accept any of the four.
        let mut truth = Fold::default();
        for s in &scripts {
            for e in s {
                truth.pushed += 1;
                truth.apply(e);
            }
        }
        let focuses: Vec<u64> = (0..THREADS).map(|t| t * 1000 + 999).collect();
        assert!(
            got.focus.is_some_and(|f| focuses.contains(&f)),
            "focus {:?}",
            got.focus
        );
        truth.focus = got.focus;
        let v = compare(&truth, &got);
        assert!(v.ok(), "max_keys {max_keys}: {:?}", v.violations);
        assert_eq!(
            order_violations, 0,
            "per-node order violated (max_keys {max_keys})"
        );
        if max_keys == 4096 {
            assert!(v.compared > 100, "vacuous comparison");
        }
    }
}

/// C-I5: bounded memory (PC-7). Distinct keys, then distinct lossless keys, with nothing
/// draining. Pending slots stay under the configured bounds and a flood leaves the focus.
#[test]
fn pending_is_bounded_under_distinct_floods() {
    let base = rss_bytes();
    let cfg = IngressConfig::default();
    let (tx, rx) = ingress(cfg);
    tx.push(Ev::Focus(1).to_node_event(APP));
    for n in 0..1_000_000u64 {
        tx.push(Ev::Children(n + 10).to_node_event(APP));
    }
    assert!(
        rx.pending() <= cfg.max_keys + 1,
        "pending {} after a Children flood",
        rx.pending()
    );
    for n in 0..1_000_000u64 {
        tx.push(Ev::Removed(n + 10).to_node_event(APP));
    }
    assert!(
        rx.pending() <= cfg.hard_cap + 1,
        "pending {} after a Removed flood",
        rx.pending()
    );
    for n in 0..100_000u64 {
        tx.push(Ev::Announce(Some(n), "x".into(), false).to_node_event(APP));
    }
    assert!(rx.pending() <= cfg.hard_cap + 1);
    tx.push(Ev::Focus(77).to_node_event(APP));
    let d = rx.drain();
    assert!(d.stats.flooded);
    let evs: Vec<Ev> = d.events.iter().map(canon).collect();
    assert!(evs.contains(&Ev::Focus(77)), "focus lost in a flood");
    assert!(
        d.dirty
            .as_ref()
            .is_some_and(|x| x.contains(gorca_model::NodeKey {
                app: APP,
                local: gorca_model::LocalId(12345)
            }))
    );
    let grown = rss_bytes().saturating_sub(base) / (1024 * 1024);
    assert!(
        grown < 200,
        "RSS grew by {grown} MiB for 2.1M pushes without draining"
    );
}

/// C-I6: lossless classes survive a flood: Window, Removed and the newest focus, plus the
/// caret of the focused node, are delivered; everything else of the flood becomes dirty.
#[test]
fn flood_keeps_the_lossless_classes() {
    let (icfg, scfg) = cfg_pair(16, 4, 8, 65_536);
    let (tx, rx) = ingress(icfg);
    let mut model = SpecModel::new(scfg);
    let mut script = vec![Ev::Focus(500)];
    for n in 0..200u64 {
        script.push(Ev::Props(n, 1));
        script.push(Ev::TextReplaced(n));
    }
    script.push(Ev::Window(900, 0));
    script.push(Ev::Caret(500, 42));
    script.push(Ev::Caret(501, 43));
    script.push(Ev::Removed(3));
    script.push(Ev::Window(900, 1));
    script.push(Ev::Focus(501));
    for e in &script {
        tx.push(e.to_node_event(APP));
        model.push(e);
    }
    let got = to_batch(&rx.drain());
    assert_eq!(got, model.drain());
    assert!(got.events.contains(&Ev::Window(900, 1)));
    assert!(got.events.contains(&Ev::Removed(3)));
    assert!(got.events.contains(&Ev::Focus(501)));
    assert!(got.dirty.contains(&7) && got.dirty.contains(&501));
}
