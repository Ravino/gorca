#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later

use std::time::Duration;

use gorca_model::{
    AttrMap, ChildList, ConnectorId, Ifaces, LocalId, Relations, Role, StateSet, Value,
};

use super::*;

fn app() -> AppId {
    AppId {
        connector: ConnectorId(0),
        serial: 1,
    }
}

fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn node(n: u64, name: &str) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: key(n),
        role: Role::Button,
        states: StateSet::empty(),
        name: Some(Arc::from(name)),
        description: None,
        value: None,
        parent: None,
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: AttrMap::default(),
        relations: Relations::default(),
        bounds: None,
        have: PropMask::PRESENT,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}

fn batch(nodes: Vec<Arc<NodeSnapshot>>) -> SnapshotBatch {
    SnapshotBatch::new(nodes)
}

fn reply_at(w: &MirrorWriter) -> ApplyOrigin {
    ApplyOrigin::Reply { issued_at: w.seq() }
}

fn name_of(r: &MirrorHandle, n: u64) -> Option<String> {
    r.get(key(n))
        .hit()
        .and_then(|s| s.name.as_deref().map(str::to_owned))
}

#[test]
fn miss_hit_gone() {
    let (mut w, r) = mirror();
    assert_eq!(r.get(key(1)), Lookup::Miss);
    w.apply(batch(vec![node(1, "ok")]), ApplyOrigin::Push);
    assert_eq!(name_of(&r, 1).as_deref(), Some("ok"));
    w.tombstone(key(1), Instant::now(), RemovalCause::Event);
    assert_eq!(r.get(key(1)), Lookup::Gone);
    assert_eq!(r.view(key(1)), Lookup::Gone);
    w.check_invariants().unwrap();
}

#[test]
fn lr1_a_reply_issued_before_the_tombstone_never_revives() {
    let (mut w, r) = mirror();
    let issued_at = w.seq();
    w.tombstone(key(1), Instant::now(), RemovalCause::Event);
    let out = w.apply(
        batch(vec![node(1, "late reply")]),
        ApplyOrigin::Reply { issued_at },
    );
    assert_eq!(out.dropped_tombstoned, 1);
    assert_eq!(r.get(key(1)), Lookup::Gone);
    assert_eq!(r.stats().revivals, 0);
    w.check_invariants().unwrap();
}

#[test]
fn lr3_a_reply_issued_after_the_tombstone_is_proof_of_life() {
    let (mut w, r) = mirror();
    w.apply(batch(vec![node(1, "a")]), ApplyOrigin::Push);
    w.tombstone(key(1), Instant::now(), RemovalCause::Event);
    let out = w.apply(batch(vec![node(1, "b")]), reply_at(&w));
    assert_eq!(out.revived, 1);
    assert_eq!(name_of(&r, 1).as_deref(), Some("b"));
    let st = r.stats();
    assert_eq!((st.live, st.tombstones, st.revivals), (1, 0, 1));
    w.check_invariants().unwrap();
}

#[test]
fn a_push_and_a_dump_before_the_tombstone_never_revive() {
    let (mut w, r) = mirror();
    let before = w.seq();
    w.tombstone(key(1), Instant::now(), RemovalCause::CacheSignal);
    w.apply(batch(vec![node(1, "p")]), ApplyOrigin::Push);
    assert_eq!(r.get(key(1)), Lookup::Gone);
    w.apply(
        batch(vec![node(1, "d")]),
        ApplyOrigin::CacheDump { issued_at: before },
    );
    assert_eq!(r.get(key(1)), Lookup::Gone);
}

#[test]
fn lr2_a_node_invalidated_during_its_fetch_stays_stale() {
    let (mut w, r) = mirror();
    w.apply(batch(vec![node(1, "old")]), ApplyOrigin::Push);
    let issued_at = w.seq();
    assert!(w.invalidate(key(1), PropMask::NAME, Instant::now()));
    let out = w.apply(
        batch(vec![node(1, "pre-mutation reply")]),
        ApplyOrigin::Reply { issued_at },
    );
    assert_eq!(out.kept_stale, 1);
    let v = r.view(key(1)).hit().unwrap();
    assert!(
        v.stale.contains(PropMask::NAME),
        "never Fresh with the old value"
    );
    // A fetch issued after the event confirms it.
    w.apply(batch(vec![node(1, "new")]), reply_at(&w));
    let v = r.view(key(1)).hit().unwrap();
    assert_eq!(v.stale, PropMask::empty());
    assert!(v.satisfies(PropMask::PRESENT));
    w.check_invariants().unwrap();
}

#[test]
fn lr2_holds_for_a_key_that_was_not_in_the_store() {
    let (mut w, r) = mirror();
    let issued_at = w.seq();
    assert!(!w.invalidate(key(9), PropMask::NAME, Instant::now()));
    w.apply(
        batch(vec![node(9, "pre-mutation")]),
        ApplyOrigin::Reply { issued_at },
    );
    assert!(r.view(key(9)).hit().unwrap().stale.contains(PropMask::NAME));
    w.check_invariants().unwrap();
}

#[test]
fn memory_of_unknown_invalidations_is_bounded_and_conservative() {
    let (mut w, r) = mirror();
    let issued_at = w.seq();
    for n in 0..(MAX_RECENT_INVALIDATIONS as u64 + 10) {
        w.invalidate(key(100_000 + n), PropMask::NAME, Instant::now());
    }
    // key 5 was never invalidated, but the table overflowed after `issued_at`: stale ALL.
    w.apply(batch(vec![node(5, "x")]), ApplyOrigin::Reply { issued_at });
    assert_eq!(r.view(key(5)).hit().unwrap().stale, PropMask::ALL);
    assert!(w.recent_inval.len() <= MAX_RECENT_INVALIDATIONS);
}

#[test]
fn the_stale_epoch_is_o1_and_clears_with_a_newer_request() {
    let (mut w, r) = mirror();
    w.apply(
        batch((0..100).map(|n| node(n, "x")).collect()),
        ApplyOrigin::Push,
    );
    w.invalidate_all(Instant::now());
    for n in 0..100 {
        assert_eq!(r.view(key(n)).hit().unwrap().stale, PropMask::ALL);
    }
    w.apply(batch(vec![node(7, "fresh")]), reply_at(&w));
    assert_eq!(r.view(key(7)).hit().unwrap().stale, PropMask::empty());
    assert_eq!(r.view(key(8)).hit().unwrap().stale, PropMask::ALL);
    // A reply issued before the epoch stays stale.
    let old = w.seq() - 1;
    w.apply(
        batch(vec![node(8, "y")]),
        ApplyOrigin::Reply { issued_at: old },
    );
    assert_eq!(r.view(key(8)).hit().unwrap().stale, PropMask::ALL);
}

#[test]
fn partial_nodes_do_not_satisfy_a_full_need() {
    let (mut w, r) = mirror();
    let mut n = (*node(1, "item")).clone();
    n.have = PropMask::CACHE_ITEM;
    w.apply(batch(vec![Arc::new(n)]), ApplyOrigin::Push);
    let v = r.view(key(1)).hit().unwrap();
    assert!(v.satisfies(PropMask::NAME | PropMask::ROLE | PropMask::STATES));
    assert!(!v.satisfies(PropMask::PRESENT), "no value in a dump row");
    assert_eq!(r.stats().partial, 1);
    w.apply(batch(vec![node(1, "item")]), reply_at(&w));
    assert_eq!(r.stats().partial, 0);
    w.check_invariants().unwrap();
}

#[test]
fn a_partial_reply_keeps_fresh_fields_only_the_old_snapshot_had() {
    let (mut w, r) = mirror();
    let mut full = (*node(1, "slider")).clone();
    full.value = Some(Value::Range {
        current: 3.0,
        min: 0.0,
        max: 10.0,
    });
    w.apply(batch(vec![Arc::new(full)]), ApplyOrigin::Push);
    let mut dump_row = (*node(1, "slider")).clone();
    dump_row.have = PropMask::CACHE_ITEM;
    w.apply(batch(vec![Arc::new(dump_row)]), reply_at(&w));
    let v = r.view(key(1)).hit().unwrap();
    assert!(v.snap.value.is_some());
    assert!(v.satisfies(PropMask::PRESENT));
    // Once an event says the value is stale it is not carried over.
    w.invalidate(key(1), PropMask::VALUE, Instant::now());
    let mut dump_row = (*node(1, "slider")).clone();
    dump_row.have = PropMask::CACHE_ITEM;
    w.apply(batch(vec![Arc::new(dump_row)]), reply_at(&w));
    let v = r.view(key(1)).hit().unwrap();
    assert!(!v.snap.have.contains(PropMask::VALUE));
    w.check_invariants().unwrap();
}

#[test]
fn an_older_reply_does_not_replace_newer_content() {
    let (mut w, r) = mirror();
    let first = w.seq();
    w.invalidate(key(2), PropMask::NAME, Instant::now());
    let second = w.seq();
    w.apply(
        batch(vec![node(1, "new")]),
        ApplyOrigin::Reply { issued_at: second },
    );
    let out = w.apply(
        batch(vec![node(1, "old")]),
        ApplyOrigin::Reply { issued_at: first },
    );
    assert_eq!(out.superseded, 1);
    assert_eq!(name_of(&r, 1).as_deref(), Some("new"));
}

#[test]
fn foreign_keys_are_dropped_and_counted() {
    let (mut w, r) = mirror_for(app(), None);
    let mut foreign = (*node(1, "evil")).clone();
    foreign.key.app = AppId {
        connector: ConnectorId(0),
        serial: 2,
    };
    let out = w.apply(
        batch(vec![Arc::new(foreign.clone()), node(2, "mine")]),
        ApplyOrigin::Push,
    );
    assert_eq!((out.dropped_foreign, out.applied), (1, 1));
    assert_eq!(r.get(foreign.key), Lookup::Miss);
    w.tombstone(foreign.key, Instant::now(), RemovalCause::Event);
    assert_eq!(r.stats().tombstones, 0);
}

#[test]
fn the_node_cap_evicts_oldest_but_never_the_focus_or_its_ancestry() {
    let (mut w, r) = mirror();
    w.set_caps(8, u64::MAX);
    // 1 <- 2 <- 3 chain; 3 is focused.
    for (n, p) in [(1u64, None), (2, Some(1u64)), (3, Some(2))] {
        let mut s = (*node(n, "chain")).clone();
        s.parent = p.map(key);
        w.apply(batch(vec![Arc::new(s)]), ApplyOrigin::Push);
    }
    w.set_focus(Some(key(3)));
    for n in 10..40 {
        w.apply(batch(vec![node(n, "filler")]), ApplyOrigin::Push);
    }
    let st = r.stats();
    assert_eq!(st.live, 8);
    assert!(st.evictions >= 25);
    for n in 1..=3 {
        assert!(r.get(key(n)).hit().is_some(), "protected node {n}");
    }
    assert_eq!(
        r.get(key(10)),
        Lookup::Miss,
        "evicted keys read Miss, not Gone"
    );
    w.check_invariants().unwrap();
}

#[test]
fn policy_evict_hints_go_first() {
    let (mut w, r) = mirror();
    w.set_caps(4, u64::MAX);
    w.apply(
        batch((0..4).map(|n| node(n, "x")).collect()),
        ApplyOrigin::Push,
    );
    w.hint_evict(key(2));
    w.apply(batch(vec![node(9, "y")]), ApplyOrigin::Push);
    assert_eq!(r.get(key(2)), Lookup::Miss);
    assert!(r.get(key(0)).hit().is_some());
}

#[test]
fn the_byte_cap_evicts() {
    let (mut w, r) = mirror();
    let one = node_bytes(&node(0, "x"));
    w.set_caps(usize::MAX, one * 5);
    w.apply(
        batch((0..50).map(|n| node(n, "x")).collect()),
        ApplyOrigin::Push,
    );
    assert!(r.stats().bytes <= one * 5);
    assert_eq!(r.stats().live, 5);
    w.check_invariants().unwrap();
}

#[test]
fn tombstones_expire_after_the_ttl_and_are_bounded() {
    let (mut w, r) = mirror();
    let t0 = Instant::now();
    w.tombstone(key(1), t0, RemovalCause::Defunct);
    assert_eq!(w.sweep(t0 + Duration::from_secs(5)).tombstones_expired, 0);
    assert_eq!(r.get(key(1)), Lookup::Gone);
    let st = w.sweep(t0 + TOMBSTONE_TTL + Duration::from_millis(1));
    assert_eq!(st.tombstones_expired, 1);
    assert_eq!(r.get(key(1)), Lookup::Miss);
    // The cap: 2 * live + slack.
    for n in 0..(TOMBSTONE_SLACK as u64 + 500) {
        w.tombstone(key(n), t0, RemovalCause::Event);
    }
    assert!(r.stats().tombstones <= TOMBSTONE_SLACK as u64);
    w.check_invariants().unwrap();
}

#[test]
fn sweep_work_is_bounded_per_call() {
    let (mut w, r) = mirror();
    let t0 = Instant::now();
    for n in 0..1000 {
        w.tombstone(key(n), t0, RemovalCause::Event);
    }
    let st = w.sweep(t0 + Duration::from_secs(60));
    assert_eq!(st.tombstones_expired as usize, SWEEP_BUDGET);
    assert_eq!(r.stats().tombstones, 1000 - SWEEP_BUDGET as u64);
}

#[test]
fn retiring_releases_everything() {
    let (mut w, r) = mirror();
    w.apply(
        batch((0..50).map(|n| node(n, "x")).collect()),
        ApplyOrigin::Push,
    );
    w.tombstone(key(3), Instant::now(), RemovalCause::Event);
    w.retire_all();
    assert_eq!(r.get(key(1)), Lookup::Gone);
    assert_eq!(r.get(key(777)), Lookup::Gone);
    assert_eq!(r.stats(), MirrorStats::default());
    w.check_invariants().unwrap();
}

fn app_n(n: u32) -> AppId {
    AppId {
        connector: ConnectorId(0),
        serial: n,
    }
}

fn node_of(a: AppId, n: u64) -> Arc<NodeSnapshot> {
    let mut s = (*node(n, "x")).clone();
    s.key = NodeKey {
        app: a,
        local: LocalId(n),
    };
    Arc::new(s)
}

/// The all-apps cap holds at every instant: the over-cap writer's background write degrades to a
/// miss, the largest unprotected holder is asked to shed, and the write succeeds afterwards.
#[test]
fn the_global_cap_is_exact_and_the_largest_unprotected_holder_sheds() {
    let one = node_bytes(&node(0, "x"));
    let budget = MirrorBudget::new(one * 40);
    let (a1, a2) = (app_n(1), app_n(2));
    let (mut w1, r1) = mirror_for(a1, Some(Arc::clone(&budget)));
    let (mut w2, r2) = mirror_for(a2, Some(Arc::clone(&budget)));
    w1.apply(
        batch((0..30).map(|n| node_of(a1, n)).collect()),
        ApplyOrigin::Push,
    );
    let out = w2.apply(
        batch((0..20).map(|n| node_of(a2, n)).collect()),
        ApplyOrigin::Push,
    );
    assert!(budget.total() <= budget.cap(), "never over the cap");
    assert_eq!(out.dropped_cap, 10);
    assert_eq!(r2.stats().live, 10);
    w1.sweep(Instant::now());
    assert!(r1.stats().live < 30, "the victim shed");
    let out = w2.apply(
        batch((10..20).map(|n| node_of(a2, n)).collect()),
        ApplyOrigin::Push,
    );
    assert_eq!(
        out.dropped_cap, 0,
        "the shed freed what the dropped batch needed"
    );
    assert_eq!(r2.stats().live, 20);
    assert!(budget.total() <= budget.cap());
    w1.check_invariants().unwrap();
    w2.check_invariants().unwrap();
}

/// A shed request is a target, not an increment: repeated asks do not make the victim drop
/// everything (probe: all 100 nodes were dropped for a handful of bytes).
#[test]
fn repeated_shed_requests_do_not_accumulate() {
    let one = node_bytes(&node(0, "x"));
    let budget = MirrorBudget::new(one * 100);
    let (a1, a2) = (app_n(1), app_n(2));
    let (mut w1, r1) = mirror_for(a1, Some(Arc::clone(&budget)));
    let (mut w2, _r2) = mirror_for(a2, Some(Arc::clone(&budget)));
    w1.apply(
        batch((0..100).map(|n| node_of(a1, n)).collect()),
        ApplyOrigin::Push,
    );
    let mut peak = 0;
    for i in 0..300 {
        w2.apply(batch(vec![node_of(a2, i)]), ApplyOrigin::Push);
        peak = peak.max(budget.total());
    }
    assert!(
        peak <= budget.cap(),
        "peak {peak} over cap {}",
        budget.cap()
    );
    w1.sweep(Instant::now());
    assert!(
        r1.stats().live >= 90,
        "asked for a few nodes, shed {}",
        100 - r1.stats().live
    );
}

/// The focused app and the Shell are never asked to shed.
#[test]
fn a_protected_app_is_never_asked_to_shed() {
    let one = node_bytes(&node(0, "x"));
    let budget = MirrorBudget::new(one * 40);
    let (a1, a2) = (app_n(1), app_n(2));
    let (mut w1, r1) = mirror_for(a1, Some(Arc::clone(&budget)));
    let (mut w2, r2) = mirror_for(a2, Some(Arc::clone(&budget)));
    w1.set_protected(true);
    w1.apply(
        batch((0..40).map(|n| node_of(a1, n)).collect()),
        ApplyOrigin::Push,
    );
    let out = w2.apply(batch(vec![node_of(a2, 1)]), ApplyOrigin::Push);
    assert_eq!(out.dropped_cap, 1);
    w1.sweep(Instant::now());
    assert_eq!(r1.stats().live, 40);
    assert_eq!(r2.stats().live, 0);
    // Its own reply to a request the user waits on makes room by evicting its own oldest.
    let before = r1.stats().live;
    let issued_at = w1.next_seq();
    let out = w1.apply(
        batch(vec![node_of(a1, 99)]),
        ApplyOrigin::Reply { issued_at },
    );
    assert_eq!(out.applied, 1);
    assert_eq!(r1.stats().live, before, "one in, one out");
    assert!(budget.total() <= budget.cap());
}

/// Reads are the LRU signal: a node that is read survives the eviction of older ones.
#[test]
fn eviction_prefers_nodes_nobody_reads() {
    let (mut w, r) = mirror();
    w.set_caps(4, u64::MAX);
    w.apply(
        batch((0..4).map(|n| node(n, "x")).collect()),
        ApplyOrigin::Push,
    );
    w.next_seq();
    for _ in 0..3 {
        // Node 0 is the oldest, but it is read.
        let _ = r.view(key(0));
        r.bump_seq();
    }
    w.apply(batch(vec![node(10, "y")]), ApplyOrigin::Push);
    assert!(r.get(key(0)).hit().is_some(), "the read node stays");
    assert_eq!(r.get(key(1)), Lookup::Miss, "the oldest unread node goes");
}

#[test]
fn dropping_a_writer_returns_its_share_of_the_budget() {
    let budget = MirrorBudget::new(u64::MAX);
    {
        let (mut w, _r) = mirror_for(app(), Some(Arc::clone(&budget)));
        w.apply(
            batch((0..100).map(|n| node(n, "n")).collect()),
            ApplyOrigin::Push,
        );
        assert!(budget.total() > 0);
    }
    assert_eq!(budget.total(), 0);
}

/// LR-2 on the proof-of-life path (reviewer probe): an event that arrives while the fetch of a
/// removed key is in flight keeps the revived node stale.
#[test]
fn lr2_an_invalidation_of_a_tombstoned_key_survives_revival() {
    let (mut w, r) = mirror();
    w.apply(batch(vec![node(1, "Old")]), ApplyOrigin::Push);
    w.tombstone(key(1), Instant::now(), RemovalCause::Event);
    let issued_at = w.next_seq();
    w.invalidate(key(1), PropMask::NAME, Instant::now());
    let out = w.apply(
        batch(vec![node(1, "Old (pre-rename)")]),
        ApplyOrigin::Reply { issued_at },
    );
    assert_eq!(out.revived, 1);
    assert!(r.view(key(1)).hit().unwrap().stale.contains(PropMask::NAME));
    w.check_invariants().unwrap();
}

/// Invalidations of keys the mirror does not hold cost O(1) memory however many arrive
/// (reviewer probe: +229 MiB after 10 M events for one key).
#[test]
fn remembering_invalidations_of_unknown_keys_is_bounded() {
    let (mut w, _r) = mirror();
    let now = Instant::now();
    for _ in 0..200_000 {
        w.invalidate(key(7), PropMask::NAME | PropMask::VALUE, now);
    }
    assert_eq!(w.remembered_invalidations(), 1, "one key, one record");
    for n in 0..(MAX_RECENT_INVALIDATIONS as u64 * 3) {
        w.invalidate(key(1_000 + n), PropMask::NAME, now);
    }
    assert!(w.remembered_invalidations() <= MAX_RECENT_INVALIDATIONS);
}

/// A reply that carries fewer fields must not make the node forget its parent, children or
/// description (reviewer probe).
#[test]
fn a_partial_reply_keeps_parent_children_and_description() {
    let (mut w, r) = mirror();
    let mut full = (*node(2, "Label")).clone();
    full.parent = Some(key(1));
    full.description = Some(Arc::from("desc"));
    full.children = ChildList::Count(3);
    full.have = PropMask::PRESENT | PropMask::PARENT | PropMask::CHILDREN;
    w.apply(batch(vec![Arc::new(full)]), reply_at(&w));
    let mut part = (*node(2, "Label")).clone();
    part.have = PropMask::PRESENT.difference(PropMask::DESCRIPTION);
    w.apply(batch(vec![Arc::new(part)]), reply_at(&w));
    let v = r.view(key(2)).hit().unwrap();
    assert_eq!(v.snap.parent, Some(key(1)));
    assert_eq!(v.snap.children, ChildList::Count(3));
    assert!(v.snap.have.contains(PropMask::PARENT | PropMask::CHILDREN));
    // But a field an event made stale is not carried over.
    w.invalidate(key(2), PropMask::CHILDREN, Instant::now());
    let mut part = (*node(2, "Label")).clone();
    part.have = PropMask::PRESENT.difference(PropMask::DESCRIPTION);
    w.apply(batch(vec![Arc::new(part)]), reply_at(&w));
    let v = r.view(key(2)).hit().unwrap();
    assert!(!v.snap.have.contains(PropMask::CHILDREN));
    assert_eq!(v.snap.parent, Some(key(1)));
}

/// Revived keys count against the node cap (reviewer probe: live 20 with a cap of 10).
#[test]
fn revivals_respect_the_node_cap() {
    let (mut w, r) = mirror();
    w.set_caps(10, u64::MAX);
    w.apply(
        batch((0..10).map(|n| node(n, "n")).collect()),
        ApplyOrigin::Push,
    );
    for i in 100..110 {
        w.tombstone(key(i), Instant::now(), RemovalCause::Event);
    }
    let issued_at = w.next_seq();
    let back: Vec<_> = (100..110).map(|i| node(i, "back")).collect();
    let out = w.apply(batch(back), ApplyOrigin::CacheDump { issued_at });
    assert_eq!(out.revived, 10);
    assert!(r.stats().live <= 10, "live {}", r.stats().live);
    w.check_invariants().unwrap();
}

/// 5.1: a reply that says `defunct` is a removal.
#[test]
fn a_defunct_reply_becomes_a_tombstone() {
    let (mut w, r) = mirror();
    w.apply(batch(vec![node(1, "alive")]), ApplyOrigin::Push);
    let mut dead = (*node(1, "dying")).clone();
    dead.states = StateSet::DEFUNCT;
    let out = w.apply(batch(vec![Arc::new(dead)]), reply_at(&w));
    assert_eq!(out.defunct, 1);
    assert_eq!(r.get(key(1)), Lookup::Gone);
    let mut dead2 = (*node(2, "never seen")).clone();
    dead2.states = StateSet::DEFUNCT;
    w.apply(batch(vec![Arc::new(dead2)]), reply_at(&w));
    assert_eq!(r.get(key(2)), Lookup::Gone);
    w.check_invariants().unwrap();
}

/// Every request and every trigger gets its own sequence number, so "issued after" is strict
/// even when the app is quiet.
#[test]
fn sequence_numbers_are_unique_per_request() {
    let (mut w, r) = mirror();
    let a = r.bump_seq();
    let b = w.next_seq();
    let c = r.bump_seq();
    assert!(a < b && b < c);
    assert_eq!(w.seq(), c);
}

#[test]
fn counters_stay_exact_under_random_operations() {
    let (mut w, r) = mirror();
    w.set_caps(64, u64::MAX);
    let mut x: u64 = 0x1234_5678_9abc_def0;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let t0 = Instant::now();
    for i in 0..5000u64 {
        let k = next() % 150;
        match next() % 8 {
            0 | 1 => {
                w.apply(batch(vec![node(k, "n")]), reply_at(&w));
            }
            2 => {
                let mut s = (*node(k, "n")).clone();
                s.have = PropMask::CACHE_ITEM;
                let issued_at = w.seq().saturating_sub(next() % 3);
                w.apply(
                    batch(vec![Arc::new(s)]),
                    ApplyOrigin::CacheDump { issued_at },
                );
            }
            3 => {
                w.invalidate(key(k), PropMask::NAME | PropMask::STATES, t0);
            }
            4 => w.tombstone(key(k), t0, RemovalCause::Event),
            5 => {
                w.evict(key(k));
            }
            6 => w.invalidate_all(t0),
            _ => {
                w.sweep(t0 + Duration::from_secs(next() % 30));
            }
        }
        if i % 97 == 0 {
            w.check_invariants().unwrap();
        }
    }
    w.check_invariants().unwrap();
    assert!(r.stats().live <= 64);
}

#[test]
fn readers_see_whole_nodes_while_the_writer_runs() {
    let (mut w, r) = mirror();
    w.apply(batch(vec![node(1, "v0")]), ApplyOrigin::Push);
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let r = r.clone();
            std::thread::spawn(move || {
                for _ in 0..20_000 {
                    let n = r.view(key(1)).hit().unwrap();
                    let name = n.snap.name.as_deref().unwrap();
                    assert!(name.starts_with('v'));
                }
            })
        })
        .collect();
    for i in 1..2000 {
        w.apply(batch(vec![node(1, &format!("v{i}"))]), ApplyOrigin::Push);
        if i % 7 == 0 {
            w.invalidate(key(1), PropMask::NAME, Instant::now());
        }
    }
    for h in readers {
        h.join().unwrap();
    }
}

#[test]
fn revision_bumps_per_batch() {
    let (mut w, r) = mirror();
    assert_eq!(r.revision(), 0);
    w.apply(
        batch((0..100).map(|n| node(n, "x")).collect()),
        ApplyOrigin::Push,
    );
    assert_eq!(r.revision(), 1);
    for n in 0..100 {
        assert!(r.get(key(n)).hit().is_some());
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // aging is wall-clock based: the test must really wait
fn a_fresh_node_ages_on_read_after_the_ttl_and_a_stale_one_does_not() {
    let (mut w, r) = mirror();
    w.apply(batch(vec![node(1, "a"), node(2, "b")]), ApplyOrigin::Push);
    assert!(!r.view(key(1)).hit().unwrap().aged);
    w.set_ttl(Duration::from_millis(20));
    std::thread::sleep(Duration::from_millis(40));
    let v = r.view(key(1)).hit().unwrap();
    assert!(v.aged, "no timer: evaluated on read");
    assert!(
        v.stale.is_empty(),
        "aged is not stale: it may still be presented"
    );
    // Zero disables aging; a refresh resets it.
    w.set_ttl(Duration::ZERO);
    assert!(!r.view(key(1)).hit().unwrap().aged);
    w.set_ttl(Duration::from_millis(20));
    w.apply(batch(vec![node(1, "a2")]), reply_at(&w));
    assert!(!r.view(key(1)).hit().unwrap().aged);
    // Stale nodes are not "aged": they are already marked.
    w.invalidate(key(2), PropMask::NAME, Instant::now());
    std::thread::sleep(Duration::from_millis(40));
    assert!(!r.view(key(2)).hit().unwrap().aged);
}
