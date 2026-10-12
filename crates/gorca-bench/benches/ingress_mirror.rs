//! B-2 (ingress push/drain) and MB-1 (mirror read/write) baselines for the reference
//! implementations.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use gorca_bench::{key, snapshot};
use gorca_ingress::{IngressConfig, ingress};
use gorca_mirror::{MirrorRead, mirror};
use gorca_model::{NodeEvent, PropMask, SnapshotBatch};

fn ingress_push(c: &mut Criterion) {
    let mut g = c.benchmark_group("ingress");
    g.bench_function("push_props_same_node", |b| {
        let (tx, rx) = ingress(IngressConfig::default());
        b.iter(|| {
            tx.push(black_box(NodeEvent::PropertiesChanged {
                node: key(1),
                mask: PropMask::NAME,
            }));
        });
        drop(rx);
    });
    g.bench_function("push_distinct_nodes_then_drain_1k", |b| {
        b.iter_batched(
            ingress_pair,
            |(tx, rx)| {
                for n in 0..1000 {
                    tx.push(NodeEvent::ChildrenChanged { node: key(n) });
                }
                black_box(rx.drain().events.len())
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
}

fn ingress_pair() -> (gorca_ingress::IngressSender, gorca_ingress::IngressReceiver) {
    ingress(IngressConfig::default())
}

fn mirror_ops(c: &mut Criterion) {
    let mut g = c.benchmark_group("mirror");
    let (mut w, r) = mirror();
    w.apply(SnapshotBatch {
        nodes: (0..10_000).map(snapshot).collect(),
    });
    g.bench_function("get_hit", |b| {
        b.iter(|| {
            black_box(r.get(key(black_box(4242))));
        });
    });
    g.bench_function("get_miss", |b| {
        b.iter(|| {
            black_box(r.get(key(black_box(99_999))));
        });
    });
    g.bench_function("apply_one_node", |b| {
        b.iter(|| {
            w.apply(SnapshotBatch {
                nodes: vec![snapshot(black_box(4242))],
            });
        });
    });
    g.finish();
}

criterion_group!(benches, ingress_push, mirror_ops);
criterion_main!(benches);
