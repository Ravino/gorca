#![allow(clippy::unwrap_used, clippy::expect_used)]

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

#[test]
fn global_budget_preempts_the_largest_unprotected_app() {
    let budget = MirrorBudget::new(node_bytes(&node(0, "x")) * 40);
    let a1 = AppId {
        connector: ConnectorId(0),
        serial: 1,
    };
    let a2 = AppId {
        connector: ConnectorId(0),
        serial: 2,
    };
    let (mut w1, r1) = mirror_for(a1, Some(Arc::clone(&budget)));
    let (mut w2, r2) = mirror_for(a2, Some(Arc::clone(&budget)));
    let nk = |a: AppId, n: u64| {
        let mut s = (*node(n, "x")).clone();
        s.key = NodeKey {
            app: a,
            local: LocalId(n),
        };
        Arc::new(s)
    };
    w1.apply(
        batch((0..30).map(|n| nk(a1, n)).collect()),
        ApplyOrigin::Push,
    );
    // App 2 pushes the total over the cap; app 1 (the larger holder) is asked to shed.
    w2.apply(
        batch((0..20).map(|n| nk(a2, n)).collect()),
        ApplyOrigin::Push,
    );
    assert!(budget.total() > budget.cap());
    w1.sweep(Instant::now());
    assert!(budget.total() <= budget.cap());
    assert!(r1.stats().live < 30);
    assert_eq!(r2.stats().live, 20, "the small holder keeps its nodes");
    w1.check_invariants().unwrap();
    w2.check_invariants().unwrap();
}

#[test]
fn a_protected_app_keeps_its_floor_when_it_is_the_only_holder() {
    let budget = MirrorBudget::new(1);
    let (mut w, r) = mirror_for(app(), Some(budget));
    w.set_protected(true);
    w.apply(
        batch((0..100).map(|n| node(n, "x")).collect()),
        ApplyOrigin::Push,
    );
    assert_eq!(
        r.stats().live,
        100,
        "below MIN_PROTECTED_NODES nothing is shed"
    );
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
