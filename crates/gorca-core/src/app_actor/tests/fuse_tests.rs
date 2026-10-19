#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The app actor's fusion writer (M22a-C1b): records become registers, fused entries are attached
//! to the mirror's snapshots, legacy agreement is counted, and none of it is on the event path.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_ingress::IngressConfig;
use gorca_mirror::MirrorRead;
use gorca_model::sem::{Avail, Check, Group, GroupMask, SemRole, Semantics, SrcRecord};
use gorca_model::{
    ChildList, Freshness, Ifaces, NodeEvent, NodeSem, NodeSnapshot, PropMask, Role, SemTables,
    SnapshotBatch, StateSet,
};

use super::{Rig, key, rig};
use crate::app_actor::fuse::{FuseWriter, writes_of};

/// A record as the normaliser makes it for a plain, enabled button named `name`.
fn button_rec(name: &str) -> SrcRecord {
    let mut known = GroupMask::EMPTY;
    for g in [
        Group::Role,
        Group::Name,
        Group::Description,
        Group::Check,
        Group::Press,
        Group::Expand,
        Group::Select,
        Group::Avail,
        Group::Invalid,
    ] {
        known.set(g, true);
    }
    SrcRecord {
        sem: Semantics {
            role: SemRole::Button,
            avail: Avail::Enabled,
            ..Semantics::default()
        },
        name: (!name.is_empty()).then(|| Arc::from(name)),
        known,
        raw_role: 43,
        ..SrcRecord::default()
    }
}

/// A snapshot whose legacy fields agree with `button_rec(name)`.
fn snap_of(n: u64, parent: Option<u64>, name: &str) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: key(n),
        role: Role::Button,
        states: StateSet::ENABLED | StateSet::SHOWING,
        name: (!name.is_empty()).then(|| Arc::from(name)),
        description: None,
        value: None,
        parent: parent.map(key),
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: gorca_model::AttrMap::default(),
        relations: gorca_model::Relations::default(),
        bounds: None,
        have: PropMask::ROLE | PropMask::STATES | PropMask::NAME | PropMask::DESCRIPTION,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
        sem: NodeSem {
            rec: Some(Arc::new(button_rec(name))),
            entry: None,
        },
    })
}

fn push(r: &mut Rig, nodes: Vec<Arc<NodeSnapshot>>) {
    r.port
        .push_snapshot(SnapshotBatch {
            nodes,
            ..Default::default()
        })
        .unwrap();
}

/// Pump until the writer has nothing left (bounded: a loop that never ends fails the test).
fn drain(r: &mut Rig) {
    for _ in 0..10_000 {
        r.actor.pump(Instant::now());
        if r.actor.fuse_work_left() == 0 {
            return;
        }
    }
    panic!("the fusion writer did not drain");
}

#[test]
fn a_record_is_fused_and_its_entry_replaces_the_record_on_the_mirror_snapshot() {
    let mut r = rig(IngressConfig::default());
    r.actor.set_strict_agreement(true);
    push(&mut r, vec![snap_of(1, None, "OK")]);
    drain(&mut r);
    let s = r.read.get(key(1)).hit().unwrap();
    assert!(s.sem.rec.is_none(), "the record is dropped once fused");
    let e = s.sem.entry.as_ref().expect("fused entry attached");
    assert_eq!(e.sem.role, SemRole::Button);
    assert_eq!(e.name.as_deref(), Some("OK"));
    assert_eq!(e.sem.avail, Avail::Enabled);
    assert_eq!(e.sem.check, Check::NotCheckable);
    let st = r.actor.fuse_stats();
    assert_eq!((st.queued, st.applied, st.attached), (1, 1, 1));
    assert_eq!(st.agree_mismatch, 0);
    assert!(st.agree_checked >= 4, "{st:?}");
}

#[test]
fn a_snapshot_without_a_record_is_left_alone() {
    let mut r = rig(IngressConfig::default());
    let mut s = (*snap_of(1, None, "x")).clone();
    s.sem = NodeSem::NONE;
    push(&mut r, vec![Arc::new(s)]);
    drain(&mut r);
    let got = r.read.get(key(1)).hit().unwrap();
    assert!(got.sem.rec.is_none() && got.sem.entry.is_none());
    assert_eq!(r.actor.fuse_stats().queued, 0);
}

#[test]
fn a_legacy_disagreement_is_counted_and_logged_not_fatal_unless_strict() {
    let mut r = rig(IngressConfig::default());
    let mut s = (*snap_of(1, None, "OK")).clone();
    // The legacy name says something else than the record does.
    s.name = Some(Arc::from("Cancel"));
    push(&mut r, vec![Arc::new(s)]);
    drain(&mut r);
    let st = r.actor.fuse_stats();
    assert_eq!(st.agree_mismatch, 1, "{st:?}");
    // The entry still fuses what the source said.
    let e = r.read.get(key(1)).hit().unwrap().sem.entry.clone().unwrap();
    assert_eq!(e.name.as_deref(), Some("OK"));
}

#[test]
#[should_panic(expected = "legacy agreement broken")]
fn strict_mode_makes_a_disagreement_a_failure() {
    let mut r = rig(IngressConfig::default());
    r.actor.set_strict_agreement(true);
    let mut s = (*snap_of(1, None, "OK")).clone();
    s.name = Some(Arc::from("Cancel"));
    push(&mut r, vec![Arc::new(s)]);
    drain(&mut r);
}

#[test]
fn a_newer_observation_wins_and_a_late_older_one_does_not_overwrite_it() {
    let mut w = FuseWriter::new(None);
    let (mut mirror, read) = gorca_mirror::mirror();
    let new = snap_of(1, None, "new");
    let old = snap_of(1, None, "old");
    mirror.apply(
        SnapshotBatch::new(vec![Arc::clone(&new)]),
        gorca_mirror::ApplyOrigin::Reply { issued_at: 9 },
    );
    w.enqueue([&new], 9);
    // A late reply, issued before: its record is older than what the registers hold.
    w.enqueue([&old], 3);
    while w.step(&mut mirror, Duration::from_millis(5)) {}
    let e = read.get(key(1)).hit().unwrap().sem.entry.clone().unwrap();
    assert_eq!(e.name.as_deref(), Some("new"));
    assert_eq!(w.entry_of(key(1)).unwrap().name.as_deref(), Some("new"));
}

#[test]
fn an_entry_fused_from_a_replaced_snapshot_is_not_attached_to_the_new_one() {
    let mut w = FuseWriter::new(None);
    let (mut mirror, read) = gorca_mirror::mirror();
    let first = snap_of(1, None, "first");
    mirror.apply(
        SnapshotBatch::new(vec![Arc::clone(&first)]),
        gorca_mirror::ApplyOrigin::Reply { issued_at: 1 },
    );
    w.enqueue([&first], 1);
    // The mirror takes a newer snapshot (another record) before the writer ran.
    let second = snap_of(1, None, "second");
    mirror.apply(
        SnapshotBatch::new(vec![Arc::clone(&second)]),
        gorca_mirror::ApplyOrigin::Reply { issued_at: 2 },
    );
    w.enqueue([&second], 2);
    while w.step(&mut mirror, Duration::from_millis(5)) {}
    let s = read.get(key(1)).hit().unwrap();
    assert_eq!(s.name.as_deref(), Some("second"));
    assert_eq!(
        s.sem.entry.as_ref().and_then(|e| e.name.as_deref()),
        Some("second"),
        "the entry of the first record must not stick to the second snapshot"
    );
    assert!(w.stats().view().attach_missed >= 1);
}

#[test]
fn a_removed_node_leaves_the_engine() {
    let mut r = rig(IngressConfig::default());
    push(
        &mut r,
        vec![snap_of(1, None, "root"), snap_of(2, Some(1), "child")],
    );
    drain(&mut r);
    assert_eq!(r.actor.fuse_nodes(), 2);
    r.port.push(NodeEvent::NodeRemoved { node: key(1) });
    r.actor.pump(Instant::now());
    assert_eq!(r.actor.fuse_nodes(), 0, "the subtree leaves with its root");
}

#[test]
fn a_big_batch_is_fused_in_bounded_steps_focus_chain_first() {
    let mut w = FuseWriter::new(None);
    let (mut mirror, read) = gorca_mirror::mirror();
    let nodes: Vec<_> = (1..=400u64)
        .map(|n| snap_of(n, (n > 1).then_some(1), "x"))
        .collect();
    mirror.apply(
        SnapshotBatch::new(nodes.clone()),
        gorca_mirror::ApplyOrigin::Reply { issued_at: 1 },
    );
    w.enqueue(nodes.iter(), 1);
    w.set_focus(Some(key(3)));
    // A zero budget still does a bounded slice of work and leaves the rest.
    assert!(w.step(&mut mirror, Duration::ZERO));
    let attached = w.stats().view().attached;
    assert!(
        attached < 400,
        "one step must not do everything: {attached}"
    );
    let focused = read.get(key(3)).hit().unwrap();
    assert!(
        focused.sem.entry.is_some(),
        "the focused node is fused in the first step"
    );
    while w.step(&mut mirror, Duration::from_millis(2)) {}
    assert_eq!(w.stats().view().attached, 400);
}

#[test]
fn a_flood_of_snapshots_drops_the_oldest_and_counts_it() {
    let mut w = FuseWriter::new(None);
    let nodes: Vec<_> = (1..=9000u64).map(|n| snap_of(n, None, "x")).collect();
    w.enqueue(nodes.iter(), 1);
    let v = w.stats().view();
    assert_eq!(v.queued, 9000);
    assert_eq!(v.dropped, 9000 - 8192);
}

#[test]
fn the_engine_is_rebuilt_not_grown_past_what_the_mirror_can_hold() {
    let mut w = FuseWriter::new(None);
    let (mut mirror, _read) = gorca_mirror::mirror();
    let cap = gorca_mirror::limits::MAX_MIRROR_NODES + 4096;
    let mut n = 0u64;
    while w.node_count() < cap {
        let batch: Vec<_> = (0..8000u64)
            .map(|i| snap_of(n + i + 1, None, "x"))
            .collect();
        n += 8000;
        w.enqueue(batch.iter(), 1);
        while w.step(&mut mirror, Duration::from_millis(50)) {}
        if n > 3 * cap as u64 {
            break;
        }
    }
    // One more step notices the cap and starts over.
    let more = snap_of(n + 1, None, "x");
    w.enqueue([&more], 1);
    w.step(&mut mirror, Duration::from_millis(50));
    assert!(w.stats().view().resets >= 1);
    assert!(w.node_count() < cap);
}

#[test]
fn profile_tables_load_and_a_quirk_row_changes_the_fused_name_only() {
    let tables = SemTables {
        name: "test",
        quirks: r#"
[[rule]]
id = 1
name = "t.marker"
roles = ["button"]
action = "strip-name-prefix"
prefixes = ["• "]
"#,
        pi: "[pi]\ndefault = [\"p0\", \"p1\"]\n",
        inexpressible: "",
    };
    let mut w = FuseWriter::new(Some(tables));
    let (mut mirror, read) = gorca_mirror::mirror();
    let s = snap_of(1, None, "• Alpha");
    mirror.apply(
        SnapshotBatch::new(vec![Arc::clone(&s)]),
        gorca_mirror::ApplyOrigin::Reply { issued_at: 1 },
    );
    w.enqueue([&s], 1);
    while w.step(&mut mirror, Duration::from_millis(5)) {}
    let got = read.get(key(1)).hit().unwrap();
    assert_eq!(
        got.name.as_deref(),
        Some("• Alpha"),
        "legacy name untouched"
    );
    assert_eq!(
        got.sem.entry.as_ref().and_then(|e| e.name.as_deref()),
        Some("Alpha")
    );
}

#[test]
fn unknown_groups_never_overwrite_an_earlier_observation() {
    // First a full record, then a cache-item-like one that knows only role, name and state.
    let full = snap_of(1, None, "OK");
    let mut partial_rec = button_rec("OK");
    partial_rec.known = GroupMask::EMPTY;
    partial_rec.known.set(Group::Role, true);
    partial_rec.known.set(Group::Name, true);
    let rec_group_count = writes_of(&partial_rec).count();
    assert_eq!(rec_group_count, 2, "only known groups are written");
    let mut w = FuseWriter::new(None);
    let (mut mirror, _read) = gorca_mirror::mirror();
    mirror.apply(
        SnapshotBatch::new(vec![Arc::clone(&full)]),
        gorca_mirror::ApplyOrigin::Reply { issued_at: 1 },
    );
    w.enqueue([&full], 1);
    let mut p = (*snap_of(1, None, "OK")).clone();
    p.sem = NodeSem {
        rec: Some(Arc::new(partial_rec)),
        entry: None,
    };
    let p = Arc::new(p);
    w.enqueue([&p], 2);
    while w.step(&mut mirror, Duration::from_millis(5)) {}
    let e = w.entry_of(key(1)).unwrap();
    assert_eq!(
        e.sem.avail,
        Avail::Enabled,
        "the earlier avail register stays"
    );
}

#[test]
fn the_pump_stays_cheap_when_there_is_nothing_to_fuse() {
    let mut r = rig(IngressConfig::default());
    push(&mut r, vec![snap_of(1, None, "OK")]);
    drain(&mut r);
    assert!(
        r.actor.next_wake().is_none(),
        "no timer is armed for an idle writer"
    );
    let t = Instant::now();
    for _ in 0..1000 {
        r.actor.pump(Instant::now());
    }
    // 1000 idle pumps: far below a millisecond each (generous bound for a loaded host).
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
}

#[test]
fn what_the_writer_holds_is_capped_in_bytes_not_only_in_nodes() {
    let mut w = FuseWriter::new(None);
    let (mut mirror, _read) = gorca_mirror::mirror();
    // 400 snapshots with 1 MiB names: 400 MiB if held, far below the node-count caps.
    for n in 1..=400u64 {
        let name = "x".repeat(1 << 20);
        let s = snap_of(n, None, &name);
        w.enqueue([&s], 1);
        let (q, a, e) = w.held_bytes();
        assert!(
            q <= 32 << 20 && a <= 32 << 20 && e <= 96 << 20,
            "{q} {a} {e}"
        );
    }
    assert!(w.stats().view().dropped > 300);
    while w.step(&mut mirror, Duration::from_millis(50)) {
        let (q, a, e) = w.held_bytes();
        assert!(
            q <= 32 << 20 && a <= 32 << 20 && e <= 96 << 20,
            "{q} {a} {e}"
        );
    }
}
