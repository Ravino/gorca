//! Reviewer wiring (2026-10-13): MA-1..MA-6 mirror audits against the real gorca-mirror store.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Instant;

use gorca_mirror::{ApplyOrigin, MirrorHandle, MirrorRead, MirrorWriter, RemovalCause, mirror_for};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, Lookup, NodeKey,
    NodeSnapshot, PropMask, Relations, Role, SnapshotBatch, StateSet,
};
use gorca_testkit::b1::audit::{self, AStats, AView, ANode, ApplyOutcome, Key, MirrorAdapter, Origin};

struct Real {
    w: MirrorWriter,
    r: MirrorHandle,
}

fn nk(k: Key) -> NodeKey {
    NodeKey { app: AppId { connector: ConnectorId(0), serial: k.0 }, local: LocalId(k.1) }
}

fn snap(n: ANode) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: nk(n.key),
        role: Role::Button,
        states: StateSet::empty(),
        name: Some(Arc::from(n.name.as_str())),
        description: None,
        value: None,
        parent: n.parent.map(nk),
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: AttrMap::default(),
        relations: Relations::default(),
        bounds: None,
        have: PropMask::from_bits_truncate(n.have),
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}

impl MirrorAdapter for Real {
    fn new(app: u32) -> Self {
        let (w, r) = mirror_for(AppId { connector: ConnectorId(0), serial: app }, None);
        Self { w, r }
    }
    fn seq(&self) -> u64 { self.w.seq() }
    fn apply(&mut self, nodes: Vec<ANode>, origin: Origin) -> ApplyOutcome {
        let o = match origin {
            Origin::Reply { issued_at } => ApplyOrigin::Reply { issued_at },
            Origin::Push => ApplyOrigin::Push,
            Origin::CacheDump { issued_at } => ApplyOrigin::CacheDump { issued_at },
        };
        let out = self.w.apply(SnapshotBatch::new(nodes.into_iter().map(snap).collect()), o);
        ApplyOutcome {
            applied: out.applied,
            dropped_tombstoned: out.dropped_tombstoned,
            kept_stale: out.kept_stale,
            revived: out.revived,
            dropped_foreign: out.dropped_foreign,
        }
    }
    fn invalidate(&mut self, k: Key, mask: u32) -> bool {
        self.w.invalidate(nk(k), PropMask::from_bits_truncate(mask), Instant::now())
    }
    fn invalidate_all(&mut self) { self.w.invalidate_all(Instant::now()) }
    fn tombstone(&mut self, k: Key) { self.w.tombstone(nk(k), Instant::now(), RemovalCause::Event) }
    fn evict(&mut self, k: Key) -> bool { self.w.evict(nk(k)) }
    fn sweep(&mut self) { let _ = self.w.sweep(Instant::now()); }
    fn retire_all(&mut self) { self.w.retire_all() }
    fn view(&self, k: Key) -> AView {
        match self.r.view(nk(k)) {
            Lookup::Hit(v) => AView::Hit {
                name: v.snap.name.as_deref().unwrap_or("").to_owned(),
                stale: v.stale.bits(),
                have: v.snap.have.bits(),
            },
            Lookup::Miss => AView::Miss,
            Lookup::Gone => AView::Gone,
        }
    }
    fn stats(&self) -> AStats {
        let s = self.r.stats();
        AStats { live: s.live, tombstones: s.tombstones, stale: s.stale, bytes: s.bytes,
                 partial: s.partial, evictions: s.evictions, revivals: s.revivals }
    }
    fn check_invariants(&self) -> Result<(), String> {
        self.w.check_invariants().map_err(|e| e.to_string())
    }
}

#[test]
fn real_mirror_counters_exact() {
    for seed in 1..=20 {
        audit::run_mirror_counters_exact::<Real>(seed, 3_000).unwrap();
    }
}
#[test]
fn real_app_churn_releases_everything() { audit::run_app_churn::<Real>(100, false).unwrap(); }
#[test]
#[ignore = "RSS: run alone"]
fn real_app_churn_rss() { audit::run_app_churn::<Real>(100, true).unwrap(); }
#[test]
fn real_foreign_refs_rejected() { audit::run_foreign_refs::<Real>().unwrap(); }
#[test]
fn real_late_reply_never_revives() { audit::run_late_reply_never_revives::<Real>().unwrap(); }
#[test]
fn real_proof_of_life_revives() { audit::run_proof_of_life_revives::<Real>().unwrap(); }
#[test]
fn real_redirty_during_fetch() { audit::run_redirty_during_fetch::<Real>().unwrap(); }
#[test]
#[ignore = "timing: release, --test-threads=1"]
fn real_apply_bounded_4096() {
    let worst = audit::run_apply_bounded::<Real>(50, 250);
    eprintln!("real apply(4096): {worst:?}");
    worst.unwrap();
}
#[test]
#[ignore = "timing: release, --test-threads=1"]
fn real_apply_bounded_slice_128() {
    // The driver applies replies in 128-node slices; time one slice at a 65k-node mirror.
    let mut m = Real::new(1);
    let mut worst = 0u128;
    for r in 0..512u64 {
        let nodes: Vec<ANode> = (0..128).map(|i| audit::node(1, r * 128 + i, "node")).collect();
        let t = Instant::now();
        m.apply(nodes, Origin::Push);
        worst = worst.max(t.elapsed().as_micros());
    }
    eprintln!("real apply(128) worst {worst} us at {} live", m.stats().live);
    assert!(worst <= 250);
}
