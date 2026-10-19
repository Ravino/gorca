// SPDX-License-Identifier: GPL-3.0-or-later
//! The app actor's `FUSE` writer (M22a-C1b, ADR-0022 section 2.11, ADR-0024 section 2.2): the
//! registers and the incremental fusion engine of one application, fed from the normalised source
//! records the connector put on its snapshots.
//!
//! # Off the key path
//!
//! Applying a reply to the mirror only *queues* the snapshots that carry a record (an `Arc`
//! clone each). The registers are written, the dirty nodes fused and the fused entries attached
//! to the mirror's snapshots by [`FuseWriter::step`], which the actor runs **after** it routed
//! the pump's events, under a time budget (the focus chain first: the engine's priority classes).
//! What does not fit stays queued or dirty for the next step. Nothing here waits on the
//! application, and nothing reads the fused entry for speech before M22a-C3 (dual-carry).
//!
//! The entry attached to a snapshot is read through [`gorca_fuse::Ivm::entry_view`], never the
//! unmasked one (ADR-0024 review: a silenced source must not leak through the cache).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gorca_fuse::{
    Budget, Input, Ivm, NoPatches, NodeIdx, PiData, PrimarySlot, Profile, QuirkSet, Rules, Val,
    Write, parse_inexpressible, parse_pi,
};
use gorca_mirror::MirrorWriter;
use gorca_model::sem::{Group, SemEntry, SrcRecord};
use gorca_model::{NodeKey, NodeSnapshot, SemTables};

/// Snapshots waiting to be written into the registers. A flood beyond this drops the **oldest**
/// (the newer content supersedes it anyway); the dropped nodes simply carry no fused entry.
const QUEUE_CAP: usize = 8192;
/// Fused entries waiting to be attached to the mirror.
const AWAITING_CAP: usize = 8192;
/// Entries examined per step when the engine still has dirty nodes.
const AWAITING_SCAN: usize = 256;
/// Nodes the engine is asked to hold: the mirror's cap plus the slack of tombstoned and evicted
/// keys it has not heard of. Beyond it the engine is rebuilt (the mirror evicts silently).
const MAX_NODES: usize = gorca_mirror::limits::MAX_MIRROR_NODES + 4096;
/// Longest chain of ancestors handed to the engine as the focus chain.
const FOCUS_CHAIN: usize = 64;
/// Disagreements one writer logs (the counters keep counting past it).
const MAX_AGREEMENT_WARNINGS: u16 = 64;
/// Fusions a step makes even when its time is spent (the focus chain comes first).
const FLOOR_FUSIONS: usize = 4;
/// Default time budget of one [`FuseWriter::step`].
pub(super) const STEP_BUDGET: Duration = Duration::from_micros(200);

/// Counters of one writer (relaxed atomics: diagnostics and tests, never control flow).
#[derive(Debug, Default)]
pub struct FuseStats {
    /// Snapshots queued for fusion.
    pub queued: AtomicU64,
    /// Queued snapshots dropped because the queue was full.
    pub dropped: AtomicU64,
    /// Snapshots written into the registers.
    pub applied: AtomicU64,
    /// Fused entries attached to the mirror.
    pub attached: AtomicU64,
    /// Entries the mirror refused (the snapshot was replaced, evicted, or the budget was full).
    pub attach_missed: AtomicU64,
    /// Times the engine was rebuilt because it held more nodes than the mirror can.
    pub resets: AtomicU64,
    /// `step` calls that left work behind.
    pub carried: AtomicU64,
    /// Legacy-agreement checks made (by node and group) and their disagreements.
    pub agree_checked: AtomicU64,
    pub agree_mismatch: AtomicU64,
}

/// A plain copy of [`FuseStats`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FuseStatsView {
    pub queued: u64,
    pub dropped: u64,
    pub applied: u64,
    pub attached: u64,
    pub attach_missed: u64,
    pub resets: u64,
    pub carried: u64,
    pub agree_checked: u64,
    pub agree_mismatch: u64,
}

impl FuseStats {
    pub fn view(&self) -> FuseStatsView {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        FuseStatsView {
            queued: g(&self.queued),
            dropped: g(&self.dropped),
            applied: g(&self.applied),
            attached: g(&self.attached),
            attach_missed: g(&self.attach_missed),
            resets: g(&self.resets),
            carried: g(&self.carried),
            agree_checked: g(&self.agree_checked),
            agree_mismatch: g(&self.agree_mismatch),
        }
    }
}

fn bump(a: &AtomicU64, n: u64) {
    a.fetch_add(n, Ordering::Relaxed);
}

/// The registers' view of a record: one write per group the record has an opinion on (the
/// `known` mask: an unfetched group writes nothing, so it never overwrites an older observation).
/// An empty name or description, a zero level and a missing position or grid are `Absent`.
pub fn writes_of(rec: &SrcRecord) -> impl Iterator<Item = Write> + '_ {
    let s = &rec.sem;
    Group::ALL
        .iter()
        .filter(|g| rec.known.has(**g))
        .map(move |g| match g {
            Group::Role => Write::Val(Val::Role(s.role)),
            Group::Name => match &rec.name {
                Some(n) if !n.is_empty() => Write::Val(Val::Name(Arc::clone(n))),
                Some(_) | None => Write::Absent(Group::Name),
            },
            Group::Description => match s.description() {
                Some(d) if !d.is_empty() => Write::Val(Val::Description(Arc::from(d))),
                Some(_) | None => Write::Absent(Group::Description),
            },
            Group::Check => Write::Val(Val::Check(s.check)),
            Group::Press => Write::Val(Val::Press(s.press)),
            Group::Expand => Write::Val(Val::Expand(s.expand)),
            Group::Select => Write::Val(Val::Select(s.select)),
            Group::Current => Write::Val(Val::Current(s.current)),
            Group::Invalid => Write::Val(Val::Invalid(s.invalid)),
            Group::Popup => Write::Val(Val::Popup(s.popup)),
            Group::Avail => Write::Val(Val::Avail(s.avail)),
            Group::Flags => Write::Val(Val::Flags(s.flags)),
            Group::Level if s.level == 0 => Write::Absent(Group::Level),
            Group::Level => Write::Val(Val::Level(s.level)),
            Group::SetPos => s
                .pos
                .map_or(Write::Absent(Group::SetPos), |p| Write::Val(Val::SetPos(p))),
            Group::Grid => s
                .grid
                .map_or(Write::Absent(Group::Grid), |p| Write::Val(Val::Grid(p))),
            Group::Live => Write::Val(Val::Live(s.live)),
            Group::Orientation => Write::Val(Val::Orientation(s.orientation)),
            Group::Sort => Write::Val(Val::Sort(s.sort)),
        })
}

struct Queued {
    snap: Arc<NodeSnapshot>,
    seq: u64,
}

struct Awaiting {
    key: NodeKey,
    idx: NodeIdx,
    rec: Arc<SrcRecord>,
}

/// The engine and its bookkeeping for one application.
pub(super) struct FuseWriter {
    ivm: Ivm<QuirkSet, NoPatches>,
    tables: Option<SemTables>,
    index: HashMap<NodeKey, NodeIdx>,
    /// Dense, indexed by `NodeIdx`: the key and the parent of every live engine node.
    nodes: Vec<Option<(NodeKey, Option<NodeIdx>)>>,
    free: Vec<u32>,
    queue: VecDeque<Queued>,
    awaiting: VecDeque<Awaiting>,
    focus: Option<NodeKey>,
    focus_dirty: bool,
    started: Instant,
    stats: Arc<FuseStats>,
    /// Tests: a legacy disagreement panics (`debug_assert`) instead of only being counted.
    strict: bool,
    /// Disagreements logged so far: a page full of them must not flood the log.
    warned: std::cell::Cell<u16>,
}

impl std::fmt::Debug for FuseWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuseWriter")
            .field("nodes", &self.index.len())
            .field("queued", &self.queue.len())
            .field("awaiting", &self.awaiting.len())
            .finish_non_exhaustive()
    }
}

/// The engine for one profile. A table that does not load is a build defect (they are embedded
/// and tested); the default profile and no rules are the safe answer.
fn build_ivm(tables: Option<SemTables>) -> Ivm<QuirkSet, NoPatches> {
    let mut profile = Profile::new(&[PrimarySlot::P0], Rules::aria());
    let mut quirks = QuirkSet::default();
    if let Some(t) = tables {
        match load(t, &mut profile) {
            Ok(q) => quirks = q,
            Err(e) => {
                tracing::error!(profile = t.name, error = %e, "source-profile tables do not load");
                profile = Profile::new(&[PrimarySlot::P0], Rules::aria());
            }
        }
    }
    Ivm::new(profile, quirks, NoPatches)
}

fn load(t: SemTables, profile: &mut Profile) -> Result<QuirkSet, gorca_fuse::QuirkError> {
    let pi: PiData = parse_pi(t.pi)?;
    pi.apply(profile);
    profile.inexpressible = parse_inexpressible(t.inexpressible)?;
    let q = QuirkSet::parse(t.quirks)?;
    q.validate_against(profile.inexpressible)?;
    Ok(q)
}

impl FuseWriter {
    pub(super) fn new(tables: Option<SemTables>) -> FuseWriter {
        FuseWriter {
            ivm: build_ivm(tables),
            tables,
            index: HashMap::new(),
            nodes: Vec::new(),
            free: Vec::new(),
            queue: VecDeque::new(),
            awaiting: VecDeque::new(),
            focus: None,
            focus_dirty: false,
            started: Instant::now(),
            stats: Arc::new(FuseStats::default()),
            strict: false,
            warned: std::cell::Cell::new(0),
        }
    }

    pub(super) fn stats(&self) -> &Arc<FuseStats> {
        &self.stats
    }

    pub(super) fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }

    /// The application announced itself with its profile tables: rebuild the engine for them.
    /// What was queued stays queued (it is written into the new engine); nodes known to the old
    /// one are re-added when their next snapshot arrives.
    pub(super) fn set_tables(&mut self, tables: Option<SemTables>) {
        if tables == self.tables && !self.index.is_empty() {
            return;
        }
        self.tables = tables;
        self.reset();
    }

    fn reset(&mut self) {
        self.ivm = build_ivm(self.tables);
        self.index.clear();
        self.nodes.clear();
        self.free.clear();
        self.awaiting.clear();
        self.focus_dirty = self.focus.is_some();
    }

    /// Queue the snapshots of a reply or push that carry a source record. `seq_of` gives the
    /// write sequence of each: the request's `issued_at` for a reply (so that a late reply never
    /// overwrites a newer observation, like the mirror's own rule), the mirror's current sequence
    /// for a push.
    pub(super) fn enqueue<'a>(
        &mut self,
        snaps: impl IntoIterator<Item = &'a Arc<NodeSnapshot>>,
        seq: u64,
    ) {
        for s in snaps {
            if s.sem.rec.is_none() {
                continue;
            }
            if self.queue.len() >= QUEUE_CAP {
                self.queue.pop_front();
                bump(&self.stats.dropped, 1);
            }
            self.queue.push_back(Queued {
                snap: Arc::clone(s),
                seq,
            });
            bump(&self.stats.queued, 1);
        }
    }

    /// The node is gone (a tombstone, a `Gone` reply): its subtree leaves the engine.
    pub(super) fn forget(&mut self, key: NodeKey) {
        let Some(&idx) = self.index.get(&key) else {
            return;
        };
        let Ok(pubs) = self.ivm.remove_subtree(idx) else {
            return;
        };
        for p in pubs {
            self.release(p.node);
        }
    }

    fn release(&mut self, idx: NodeIdx) {
        let i = idx.0 as usize;
        if let Some(slot) = self.nodes.get_mut(i)
            && let Some((key, _)) = slot.take()
        {
            self.index.remove(&key);
            self.free.push(idx.0);
        }
    }

    /// The focused node of this app changed (or was cleared).
    pub(super) fn set_focus(&mut self, focus: Option<NodeKey>) {
        if self.focus != focus {
            self.focus = focus;
            self.focus_dirty = true;
        }
    }

    /// True while queued or dirty work is left: the actor asks to be woken again.
    pub(super) fn has_work(&self) -> bool {
        !self.queue.is_empty() || !self.awaiting.is_empty() || self.ivm.pending() > 0
    }

    pub(super) fn work_left(&self) -> usize {
        self.queue.len() + self.awaiting.len() + self.ivm.pending()
    }

    pub(super) fn node_count(&self) -> usize {
        self.index.len()
    }

    /// The fused entry of `key` as a reader must see it (`Ivm::entry_view`).
    #[cfg(test)]
    pub(super) fn entry_of(&self, key: NodeKey) -> Option<Arc<SemEntry>> {
        self.ivm.entry_view(*self.index.get(&key)?)
    }

    fn alloc(&mut self, key: NodeKey, parent: Option<NodeIdx>) -> Option<NodeIdx> {
        if let Some(&i) = self.index.get(&key) {
            return Some(i);
        }
        let n = match self.free.pop() {
            Some(n) => n,
            None => u32::try_from(self.nodes.len()).ok()?,
        };
        let idx = NodeIdx(n);
        self.ivm.add_node(idx, parent).ok()?;
        let slot = n as usize;
        if self.nodes.len() <= slot {
            self.nodes.resize(slot + 1, None);
        }
        self.nodes[slot] = Some((key, parent));
        self.index.insert(key, idx);
        Some(idx)
    }

    /// Write one queued snapshot into the registers.
    fn apply_one(&mut self, q: &Queued, inputs: &mut Vec<Input>) {
        let Some(rec) = q.snap.sem.rec.clone() else {
            return;
        };
        let key = q.snap.key;
        let parent = q.snap.parent.and_then(|p| self.index.get(&p).copied());
        let Some(idx) = self.alloc(key, parent) else {
            return;
        };
        inputs.push(Input::Life {
            node: idx,
            src: PrimarySlot::P0,
            seq: q.seq,
            alive: true,
        });
        for w in writes_of(&rec) {
            inputs.push(Input::Obs {
                node: idx,
                src: PrimarySlot::P0,
                seq: q.seq,
                write: w,
            });
        }
        if self.awaiting.len() >= AWAITING_CAP {
            self.awaiting.pop_front();
        }
        self.awaiting.push_back(Awaiting { key, idx, rec });
        bump(&self.stats.applied, 1);
    }

    fn refresh_focus_chain(&mut self) {
        self.focus_dirty = false;
        let mut chain = Vec::new();
        let mut cur = self.focus.and_then(|k| self.index.get(&k).copied());
        while let Some(i) = cur {
            if chain.len() >= FOCUS_CHAIN || chain.contains(&i) {
                break;
            }
            chain.push(i);
            cur = self
                .nodes
                .get(i.0 as usize)
                .and_then(Option::as_ref)
                .and_then(|(_, p)| *p);
        }
        self.ivm.set_focus_chain(&chain);
    }

    /// One bounded unit of work: write queued snapshots into the registers, fuse (focus chain
    /// first), attach the results to the mirror. Returns whether work is left.
    pub(super) fn step(&mut self, mirror: &mut MirrorWriter, budget: Duration) -> bool {
        if !self.has_work() && !self.focus_dirty {
            return false;
        }
        if self.index.len() >= MAX_NODES {
            bump(&self.stats.resets, 1);
            self.reset();
        }
        let deadline = Instant::now() + budget;
        let mut inputs: Vec<Input> = Vec::new();
        let mut n = 0u32;
        while let Some(q) = self.queue.pop_front() {
            self.apply_one(&q, &mut inputs);
            n += 1;
            // The clock is read every few nodes: a node is a handful of register writes.
            if n.is_multiple_of(16) && Instant::now() >= deadline {
                break;
            }
        }
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        inputs.push(Input::Tick(ms));
        let rep = self.ivm.apply(inputs);
        for e in rep.errors {
            tracing::debug!(error = %e, "fuse input refused");
        }
        if self.focus_dirty {
            self.refresh_focus_chain();
        }
        // Progress is guaranteed: a spent budget still fuses a few nodes, focus chain first.
        let budget = if Instant::now() >= deadline {
            Budget::calls(FLOOR_FUSIONS)
        } else {
            Budget::until(deadline)
        };
        let run = self.ivm.run_with(budget, &mut |_| {});
        self.attach(mirror, run.pending == 0);
        let more = self.has_work();
        if more {
            bump(&self.stats.carried, 1);
        }
        more
    }

    /// Attach the entries of the nodes that are no longer dirty. With nothing pending every
    /// waiting entry is ready; otherwise only a bounded prefix is examined.
    fn attach(&mut self, mirror: &mut MirrorWriter, all: bool) {
        let mut scan = if all {
            self.awaiting.len()
        } else {
            self.awaiting.len().min(AWAITING_SCAN)
        };
        while scan > 0 {
            scan -= 1;
            let Some(a) = self.awaiting.pop_front() else {
                break;
            };
            if self.ivm.is_dirty(a.idx) {
                self.awaiting.push_back(a);
                continue;
            }
            if self.index.get(&a.key) != Some(&a.idx) {
                continue;
            }
            let Some(entry) = self.ivm.entry_view(a.idx) else {
                continue;
            };
            if cfg!(any(debug_assertions, feature = "legacy-agree")) {
                self.check_agreement(mirror, &a, &entry);
            }
            if mirror.attach_sem(a.key, &a.rec, entry) {
                bump(&self.stats.attached, 1);
            } else {
                bump(&self.stats.attach_missed, 1);
            }
        }
    }

    /// The dual-carry check (ADR-0022 section 2.11): the legacy fields of the snapshot the entry
    /// was fused from must agree with it, group by group, outside the documented deviations.
    fn check_agreement(&self, mirror: &MirrorWriter, a: &Awaiting, entry: &SemEntry) {
        use gorca_mirror::MirrorRead;
        let Some(snap) = mirror
            .reader()
            .get(a.key)
            .hit()
            .filter(|s| s.sem.rec.as_ref().is_some_and(|r| Arc::ptr_eq(r, &a.rec)))
        else {
            return;
        };
        let report = super::agree::check(&snap, &a.rec, entry);
        bump(&self.stats.agree_checked, report.checked);
        super::agree::note_checked(report.checked);
        for m in &report.mismatches {
            bump(&self.stats.agree_mismatch, 1);
            super::agree::note_mismatch();
            let n = self.warned.get();
            if n < MAX_AGREEMENT_WARNINGS {
                self.warned.set(n + 1);
                tracing::warn!(
                    node = ?snap.key,
                    group = m.group.name(),
                    legacy = m.legacy,
                    fused = m.fused,
                    "legacy agreement: the fused entry disagrees with the legacy snapshot"
                );
            }
        }
        debug_assert!(
            !self.strict || report.mismatches.is_empty(),
            "legacy agreement broken: {:?}",
            report.mismatches
        );
    }
}

/// Fuse a set of snapshots (with records) the way an app actor would and return the mirror's
/// copies, in the order given, each with its fused entry attached. For conformance tests and
/// tools: the corpus tests of the connector run the real writer on recorded pages with it.
/// Nodes the mirror refuses are returned as given.
pub fn fuse_snapshots(
    tables: Option<SemTables>,
    nodes: &[Arc<NodeSnapshot>],
) -> Vec<Arc<NodeSnapshot>> {
    use gorca_mirror::{ApplyOrigin, MirrorRead};
    let (mut mirror, read) = gorca_mirror::mirror();
    let mut w = FuseWriter::new(tables);
    for chunk in nodes.chunks(512) {
        let o = mirror.apply(
            gorca_model::SnapshotBatch::new(chunk.to_vec()),
            ApplyOrigin::Reply { issued_at: 1 },
        );
        let _ = o;
        w.enqueue(chunk, 1);
        while w.step(&mut mirror, Duration::from_millis(50)) {}
    }
    while w.step(&mut mirror, Duration::from_millis(50)) {}
    nodes
        .iter()
        .map(|n| read.get(n.key).hit().unwrap_or_else(|| Arc::clone(n)))
        .collect()
}
