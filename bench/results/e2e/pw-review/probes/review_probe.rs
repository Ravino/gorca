//! Reviewer probes for the B-1 prework store (2026-10-13). Each test asserts the SPEC behaviour;
//! a failure demonstrates a defect.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gorca_mirror::{ApplyOrigin, MirrorBudget, MirrorRead, RemovalCause, mirror, mirror_for, node_bytes};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, Lookup, NodeKey,
    NodeSnapshot, PropMask, Relations, Role, SnapshotBatch, StateSet,
};

fn app(s: u32) -> AppId {
    AppId { connector: ConnectorId(0), serial: s }
}
fn key(a: u32, n: u64) -> NodeKey {
    NodeKey { app: app(a), local: LocalId(n) }
}
fn snap(a: u32, n: u64, name: &str, have: PropMask, parent: Option<NodeKey>) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: key(a, n),
        role: Role::Button,
        states: StateSet::empty(),
        name: Some(Arc::from(name)),
        description: if have.contains(PropMask::DESCRIPTION) { Some(Arc::from("desc")) } else { None },
        value: None,
        parent,
        children: if have.contains(PropMask::CHILDREN) { ChildList::Count(3) } else { ChildList::Unknown },
        interfaces: Ifaces::empty(),
        attrs: AttrMap::default(),
        relations: Relations::default(),
        bounds: None,
        have,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}
const FOCUS_FIELDS: PropMask = PropMask::PRESENT.difference(PropMask::DESCRIPTION);

/// LR-2 on the revival path: an invalidation that arrives while the proof-of-life fetch is in
/// flight must keep the revived node Stale.
#[test]
fn probe_lr2_invalidation_of_tombstoned_key_is_lost_on_revival() {
    let (mut w, r) = mirror();
    w.apply(SnapshotBatch::new(vec![snap(1, 1, "Old", PropMask::PRESENT, None)]), ApplyOrigin::Push);
    w.tombstone(key(1, 1), Instant::now(), RemovalCause::Event);
    let issued = w.seq(); // proof-of-life fetch issued after the removal (5.5)
    w.invalidate(key(1, 1), PropMask::NAME, Instant::now()); // rename event while in flight
    let out = w.apply(
        SnapshotBatch::new(vec![snap(1, 1, "Old (pre-rename)", PropMask::PRESENT, None)]),
        ApplyOrigin::Reply { issued_at: issued },
    );
    assert_eq!(out.revived, 1);
    let v = r.view(key(1, 1)).hit().unwrap();
    assert!(
        v.stale.contains(PropMask::NAME),
        "revived node is Fresh with pre-invalidation NAME: stale={:?}",
        v.stale
    );
}

/// A Reader fetch (FOCUS_FIELDS, no PARENT/CHILDREN/DESCRIPTION) over a node that knew its parent
/// must not forget the parent: P9-3 ancestor invalidation and focus-ancestry protection need it.
#[test]
fn probe_partial_reply_forgets_parent_children_description() {
    let (mut w, r) = mirror();
    let full = PropMask::PRESENT | PropMask::PARENT | PropMask::CHILDREN;
    w.apply(
        SnapshotBatch::new(vec![snap(1, 2, "Label", full, Some(key(1, 1)))]),
        ApplyOrigin::Reply { issued_at: w.seq() },
    );
    w.apply(
        SnapshotBatch::new(vec![snap(1, 2, "Label", FOCUS_FIELDS, None)]),
        ApplyOrigin::Reply { issued_at: w.seq() },
    );
    let v = r.view(key(1, 2)).hit().unwrap();
    eprintln!("after partial reply: have={:?} parent={:?} desc={:?}", v.snap.have, v.snap.parent, v.snap.description);
    assert_eq!(v.snap.parent, Some(key(1, 1)), "parent lost by a partial reply");
    assert!(v.snap.have.contains(PropMask::PARENT | PropMask::CHILDREN | PropMask::DESCRIPTION));
}

/// MAX_MIRROR_NODES must hold: revivals skip make_room.
#[test]
fn probe_revivals_bypass_the_node_cap() {
    let (mut w, r) = mirror();
    w.set_caps(10, u64::MAX);
    let live: Vec<_> = (0..10).map(|i| snap(1, i, "n", PropMask::PRESENT, None)).collect();
    w.apply(SnapshotBatch::new(live), ApplyOrigin::Push);
    for i in 100..110 {
        w.tombstone(key(1, i), Instant::now(), RemovalCause::Event);
    }
    let issued = w.seq();
    let back: Vec<_> = (100..110).map(|i| snap(1, i, "back", PropMask::PRESENT, None)).collect();
    let out = w.apply(SnapshotBatch::new(back), ApplyOrigin::CacheDump { issued_at: issued });
    eprintln!("revived {} live {}", out.revived, r.stats().live);
    assert!(r.stats().live <= 10, "live {} > cap 10", r.stats().live);
}

/// The global budget must be released when an app's writer goes away for any reason.
#[test]
fn probe_budget_released_when_writer_dropped() {
    let b = MirrorBudget::new(u64::MAX);
    {
        let (mut w, _r) = mirror_for(app(1), Some(Arc::clone(&b)));
        let n: Vec<_> = (0..100).map(|i| snap(1, i, "n", PropMask::PRESENT, None)).collect();
        w.apply(SnapshotBatch::new(n), ApplyOrigin::Push);
        assert!(b.total() > 0);
    }
    assert_eq!(b.total(), 0, "global budget leaks {} bytes after the writer is dropped", b.total());
}

/// The all-apps cap: how far over it does the total go while the largest holder is idle?
#[test]
fn probe_global_cap_overshoot_with_idle_victim() {
    let one = node_bytes(&snap(1, 0, "n", PropMask::PRESENT, None));
    let b = MirrorBudget::new(one * 100);
    let (mut wa, ra) = mirror_for(app(1), Some(Arc::clone(&b)));
    let (mut wb, rb) = mirror_for(app(2), Some(Arc::clone(&b)));
    let n: Vec<_> = (0..100).map(|i| snap(1, i, "n", PropMask::PRESENT, None)).collect();
    wa.apply(SnapshotBatch::new(n), ApplyOrigin::Push);
    let mut peak = 0u64;
    for i in 0..300 {
        wb.apply(SnapshotBatch::new(vec![snap(2, i, "n", PropMask::PRESENT, None)]), ApplyOrigin::Push);
        peak = peak.max(b.total());
    }
    eprintln!(
        "cap {} peak {} ({:.2}x); A live {} (idle, never swept); B live {}",
        b.cap(), peak, peak as f64 / b.cap() as f64, ra.stats().live, rb.stats().live
    );
    wa.sweep(Instant::now());
    eprintln!("after A sweeps: total {} A live {} B live {}", b.total(), ra.stats().live, rb.stats().live);
    assert!(peak <= b.cap() + one, "global cap exceeded: peak {peak} cap {}", b.cap());
}

/// Lost-key check: readers of stable keys must never see Miss while the writer swaps them and
/// churns many other keys (index growth and shrink).
#[test]
fn probe_readers_never_miss_stable_keys_under_index_churn() {
    let (mut w, r) = mirror();
    w.set_caps(1_000_000, u64::MAX);
    let stable: Vec<_> = (0..512).map(|i| snap(1, i, "s", PropMask::PRESENT, None)).collect();
    w.apply(SnapshotBatch::new(stable), ApplyOrigin::Push);
    let stop = Arc::new(AtomicBool::new(false));
    let misses = Arc::new(AtomicU64::new(0));
    let reads = Arc::new(AtomicU64::new(0));
    let mut hs = Vec::new();
    for t in 0..4 {
        let (r, stop, misses, reads) = (r.clone(), Arc::clone(&stop), Arc::clone(&misses), Arc::clone(&reads));
        hs.push(std::thread::spawn(move || {
            let mut i = t as u64;
            while !stop.load(Ordering::Relaxed) {
                match r.view(key(1, i % 512)) {
                    Lookup::Hit(_) => {}
                    Lookup::Miss | Lookup::Gone => { misses.fetch_add(1, Ordering::Relaxed); }
                }
                reads.fetch_add(1, Ordering::Relaxed);
                i = i.wrapping_add(7);
            }
        }));
    }
    let t0 = Instant::now();
    let mut wave = 0u64;
    while t0.elapsed() < Duration::from_secs(4) {
        let base = 10_000 + wave * 40_000;
        let batch: Vec<_> = (base..base + 40_000).map(|i| snap(1, i, "c", PropMask::PRESENT, None)).collect();
        w.apply(SnapshotBatch::new(batch), ApplyOrigin::Push);
        for i in 0..512 {
            w.invalidate(key(1, i), PropMask::NAME, Instant::now());
            w.apply(SnapshotBatch::new(vec![snap(1, i, "s2", PropMask::PRESENT, None)]), ApplyOrigin::Reply { issued_at: w.seq() });
        }
        for i in base..base + 40_000 {
            w.evict(key(1, i));
        }
        wave += 1;
    }
    stop.store(true, Ordering::Relaxed);
    for h in hs { h.join().unwrap(); }
    eprintln!("waves {wave} reads {} misses {}", reads.load(Ordering::Relaxed), misses.load(Ordering::Relaxed));
    assert_eq!(misses.load(Ordering::Relaxed), 0);
    w.check_invariants().unwrap();
}
