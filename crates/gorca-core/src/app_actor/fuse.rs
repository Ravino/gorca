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
//! application, and speech does not read this entry yet (dual-carry): the web overlay of the Reader
//! reads the document actor's entries (`NodeData::sem`), the presenter switches in M22b.
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
#[cfg(test)]
use gorca_model::sem::SemEntry;
use gorca_model::sem::{Group, SemText, SrcRecord};
use gorca_model::{NodeKey, NodeSnapshot, SemTables};

use crate::plugin_port::{
    PluginBridge, PluginService, PluginSources, Row, ScopeSpec, Tree, now_ns,
};

/// Snapshots waiting to be written into the registers. A flood beyond this drops the **oldest**
/// (the newer content supersedes it anyway); the dropped nodes simply carry no fused entry.
const QUEUE_CAP: usize = 8192;
/// Bytes of snapshots waiting to be written (mirror accounting, `node_bytes`): a hostile app's
/// long names must not grow what the writer holds beyond a bound of its own.
const QUEUE_BYTES: usize = 32 << 20;
/// Bytes of source records waiting for their entry (they outlive a replaced mirror snapshot).
const AWAITING_BYTES: usize = 32 << 20;
/// Bytes the engine's registers may hold (strings plus a fixed share per node) before it is
/// rebuilt, next to the node-count bound.
const ENGINE_BYTES: usize = 96 << 20;
/// Fixed register bytes charged per engine node.
const NODE_FIXED: usize = 256;
/// Fused entries waiting to be attached to the mirror.
const AWAITING_CAP: usize = 8192;
/// Entries examined per step when the engine still has dirty nodes.
const AWAITING_SCAN: usize = 256;
/// Nodes the engine is asked to hold: the mirror's cap plus the slack of tombstoned and evicted
/// keys it has not heard of. Beyond it the engine is rebuilt (the mirror evicts silently).
const MAX_NODES: usize = gorca_mirror::limits::MAX_MIRROR_NODES + 4096;
/// Property-event invalidations kept waiting; beyond it an arbitrary waiting one (not the oldest) is dropped to
/// make room (the mirror still marks those snapshots stale, only the entry's `stale` mask is
/// lost for them).
const INVAL_CAP: usize = 4096;
/// Nodes of a whole-application invalidation marked per step.
const INVAL_SWEEP_CHUNK: usize = 512;
/// Children waiting for a parent the engine does not hold yet. Beyond it the list is dropped (the
/// children simply stay roots, which only costs them a content-named parent).
const ORPHAN_CAP: usize = 4096;
/// Longest chain of ancestors handed to the engine as the focus chain.
const FOCUS_CHAIN: usize = 64;
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
    /// Entries of snapshots that already carried one, replaced by a re-fused one (a content-named
    /// parent, a node a property event made stale).
    pub refreshed: AtomicU64,
    /// Waiting invalidations dropped because too many were waiting.
    pub inval_dropped: AtomicU64,
    /// Entries the mirror refused (the snapshot was replaced, evicted, or the budget was full).
    pub attach_missed: AtomicU64,
    /// Times the engine was rebuilt because it held more nodes than the mirror can.
    pub resets: AtomicU64,
    /// `step` calls that left work behind.
    pub carried: AtomicU64,
}

/// A plain copy of [`FuseStats`].
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FuseStatsView {
    pub queued: u64,
    pub dropped: u64,
    pub applied: u64,
    pub attached: u64,
    pub refreshed: u64,
    pub inval_dropped: u64,
    pub attach_missed: u64,
    pub resets: u64,
    pub carried: u64,
}

impl FuseStats {
    pub fn view(&self) -> FuseStatsView {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        FuseStatsView {
            queued: g(&self.queued),
            dropped: g(&self.dropped),
            applied: g(&self.applied),
            attached: g(&self.attached),
            refreshed: g(&self.refreshed),
            inval_dropped: g(&self.inval_dropped),
            attach_missed: g(&self.attach_missed),
            resets: g(&self.resets),
            carried: g(&self.carried),
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
            Group::RoleDescription
            | Group::Placeholder
            | Group::KeyShortcuts
            | Group::ValueText
            | Group::ErrorText
            | Group::NameFrom => gorca_fuse::rare_write(s, *g).unwrap_or(Write::Absent(*g)),
        })
}

struct Queued {
    bytes: usize,
    snap: Arc<NodeSnapshot>,
    seq: u64,
}

struct Awaiting {
    bytes: usize,
    key: NodeKey,
    idx: NodeIdx,
    rec: Arc<SrcRecord>,
}

/// The app actor's plugin scope (M18a): the service, the identity the scope opens for, and the
/// open bridge with its engine sources.
struct AppPlugins {
    service: Arc<dyn PluginService>,
    scope: ScopeSpec,
    open: Option<(PluginBridge, PluginSources)>,
}

/// The engine's structure as the plugin snapshot reads it (S0 structure only, PF-2).
struct IvmTree<'a>(&'a Ivm<QuirkSet, NoPatches>);

impl Tree for IvmTree<'_> {
    fn parent(&self, n: u32) -> Option<u32> {
        self.0.parent(NodeIdx(n)).map(|p| p.0)
    }
    fn children(&self, n: u32) -> Vec<u32> {
        self.0.children(NodeIdx(n)).iter().map(|c| c.0).collect()
    }
}

/// The S0 row of a snapshot: the observed record and neutral attributes, never a fused entry.
fn row_of(snap: &NodeSnapshot, rec: &SrcRecord) -> Row {
    Row {
        role: rec.sem.role.code(),
        states: snap.states.bits(),
        name: rec.name.clone(),
        text: None,
        tag: (!rec.tag.is_empty()).then(|| Arc::from(rec.tag.as_str())),
        class: rec.class.clone(),
        aria_role: rec.aria_role.clone(),
        dom_id: rec.dom_id.clone(),
        secret: crate::doc_actor::fuse::rec_is_secret(rec),
    }
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
    queue_bytes: usize,
    awaiting_bytes: usize,
    engine_bytes: usize,
    /// Register bytes charged per engine node, by `NodeIdx`.
    reg_bytes: Vec<u32>,
    /// Children whose parent was not in the engine yet, by the parent's key: the parent adopts
    /// them when its own snapshot arrives (`Ivm::reparent`), so a content name sees its children
    /// whatever the order the replies came in. Bounded by [`ORPHAN_CAP`].
    orphans: HashMap<NodeKey, Vec<NodeKey>>,
    orphan_count: usize,
    /// Property-event invalidations waiting to be written (newest sequence per node), and the
    /// sweep of a whole-application invalidation: `(seq, nodes still to mark)`.
    pending_inval: HashMap<NodeKey, u64>,
    inval_sweep: Option<(u64, Vec<NodeIdx>)>,
    focus: Option<NodeKey>,
    focus_dirty: bool,
    plugins: Option<AppPlugins>,
    started: Instant,
    stats: Arc<FuseStats>,
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
    let mut ivm = Ivm::new(profile, quirks, NoPatches);
    // Production only: tests and replays keep the drain purely work-based (the published stream
    // is the same either way).
    if !cfg!(test) {
        ivm.set_sync_valve(Some(gorca_fuse::SYNC_WALL_VALVE));
    }
    ivm
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
            queue_bytes: 0,
            awaiting_bytes: 0,
            engine_bytes: 0,
            reg_bytes: Vec::new(),
            orphans: HashMap::new(),
            orphan_count: 0,
            pending_inval: HashMap::new(),
            inval_sweep: None,
            focus: None,
            focus_dirty: false,
            plugins: None,
            started: Instant::now(),
            stats: Arc::new(FuseStats::default()),
        }
    }

    /// Enable plugins for this application: `scope` is the identity they match on.
    pub(super) fn set_plugins(&mut self, service: Arc<dyn PluginService>, scope: ScopeSpec) {
        self.plugins = Some(AppPlugins {
            service,
            scope,
            open: None,
        });
        self.reopen_plugins();
    }

    /// (Re)open the plugin scope over a fresh engine; nodes are re-fed as their snapshots arrive.
    fn reopen_plugins(&mut self) {
        let Some(pl) = self.plugins.as_mut() else {
            return;
        };
        pl.open = None;
        if pl.service.wants_url() && pl.scope.url.is_none() {
            // Manifests with URL globs match documents (the document actor's scope), not apps.
        }
        let Some(runner) = pl.service.open(&pl.scope, Arc::new(|| {})) else {
            return;
        };
        let metas = runner.plugins().to_vec();
        let bridge = PluginBridge::new(runner, pl.service.governor(), pl.scope.clone());
        let sources = PluginSources::register(&mut self.ivm, &metas);
        pl.open = Some((bridge, sources));
    }

    fn bridge(&mut self) -> Option<&mut PluginBridge> {
        self.plugins
            .as_mut()
            .and_then(|p| p.open.as_mut())
            .map(|(b, _)| b)
    }

    fn apply_plugin_ops(&mut self) {
        let Some((bridge, sources)) = self.plugins.as_mut().and_then(|p| p.open.as_mut()) else {
            return;
        };
        let ops = bridge.take_ops();
        if !ops.is_empty() {
            sources.apply(&mut self.ivm, ops);
        }
    }

    pub(super) fn stats(&self) -> &Arc<FuseStats> {
        &self.stats
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
        self.reopen_plugins();
        self.index.clear();
        self.nodes.clear();
        self.reg_bytes.clear();
        self.orphans.clear();
        self.orphan_count = 0;
        self.pending_inval.clear();
        self.inval_sweep = None;
        self.engine_bytes = 0;
        self.awaiting_bytes = 0;
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
            let bytes = usize::try_from(gorca_mirror::node_bytes(s)).unwrap_or(usize::MAX);
            while !self.queue.is_empty()
                && (self.queue.len() >= QUEUE_CAP || self.queue_bytes + bytes > QUEUE_BYTES)
            {
                self.pop_queue();
                bump(&self.stats.dropped, 1);
            }
            self.queue_bytes += bytes;
            self.queue.push_back(Queued {
                bytes,
                snap: Arc::clone(s),
                seq,
            });
            bump(&self.stats.queued, 1);
        }
    }

    fn pop_queue(&mut self) -> Option<Queued> {
        let q = self.queue.pop_front()?;
        self.queue_bytes = self.queue_bytes.saturating_sub(q.bytes);
        Some(q)
    }

    fn pop_awaiting(&mut self) -> Option<Awaiting> {
        let a = self.awaiting.pop_front()?;
        self.awaiting_bytes = self.awaiting_bytes.saturating_sub(a.bytes);
        Some(a)
    }

    fn push_awaiting(&mut self, a: Awaiting) {
        self.awaiting_bytes += a.bytes;
        self.awaiting.push_back(a);
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
            if let Some(b) = self.bridge() {
                b.remove(p.node.0);
            }
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
            if let Some(b) = self.reg_bytes.get_mut(i) {
                self.engine_bytes = self.engine_bytes.saturating_sub(*b as usize);
                *b = 0;
            }
        }
    }

    /// The focused node of this app changed (or was cleared).
    pub(super) fn set_focus(&mut self, focus: Option<NodeKey>) {
        if self.focus != focus {
            self.focus = focus;
            self.focus_dirty = true;
        }
    }

    /// A property event marked fields of `key` untrustworthy at mirror sequence `seq` (W-6): the
    /// registers older than it read stale, and the entry the mirror carries is refreshed.
    pub(super) fn invalidate(&mut self, key: NodeKey, seq: u64) {
        if !self.index.contains_key(&key) {
            return;
        }
        if self.pending_inval.len() >= INVAL_CAP && !self.pending_inval.contains_key(&key) {
            // Drop an arbitrary waiting entry: they are all equally old to within a pump.
            if let Some(k) = self.pending_inval.keys().next().copied() {
                self.pending_inval.remove(&k);
            }
            bump(&self.stats.inval_dropped, 1);
        }
        let e = self.pending_inval.entry(key).or_insert(seq);
        *e = (*e).max(seq);
    }

    /// Every node of the application is untrustworthy at `seq` (flood resync, dirty-all).
    pub(super) fn invalidate_all(&mut self, seq: u64) {
        let nodes: Vec<NodeIdx> = self
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| n.as_ref().and_then(|_| u32::try_from(i).ok().map(NodeIdx)))
            .collect();
        self.pending_inval.clear();
        self.inval_sweep = Some((seq, nodes));
    }

    /// Write the waiting invalidations into the engine (a bounded part of a sweep per step).
    fn apply_invalidations(&mut self, inputs: &mut Vec<Input>) {
        for (key, seq) in self.pending_inval.drain() {
            if let Some(&node) = self.index.get(&key) {
                inputs.push(Input::Inval {
                    node,
                    src: PrimarySlot::P0,
                    seq,
                });
            }
        }
        if let Some((seq, nodes)) = self.inval_sweep.as_mut() {
            let take = nodes.len().min(INVAL_SWEEP_CHUNK);
            let start = nodes.len() - take;
            inputs.extend(nodes.drain(start..).map(|node| Input::Inval {
                node,
                src: PrimarySlot::P0,
                seq: *seq,
            }));
            if nodes.is_empty() {
                self.inval_sweep = None;
            }
        }
    }

    /// True while queued or dirty work is left: the actor asks to be woken again.
    pub(super) fn has_work(&self) -> bool {
        !self.queue.is_empty()
            || !self.awaiting.is_empty()
            || self.ivm.pending() > 0
            || self
                .plugins
                .as_ref()
                .and_then(|p| p.open.as_ref())
                .is_some_and(|(b, _)| b.has_work())
            || !self.pending_inval.is_empty()
            || self.inval_sweep.is_some()
    }

    pub(super) fn work_left(&self) -> usize {
        self.queue.len()
            + self.awaiting.len()
            + self.ivm.pending()
            + self.pending_inval.len()
            + self.inval_sweep.as_ref().map_or(0, |(_, n)| n.len())
    }

    /// Bytes the writer holds: queued snapshots, waiting records, engine registers.
    #[cfg(test)]
    pub(super) fn held_bytes(&self) -> (usize, usize, usize) {
        (self.queue_bytes, self.awaiting_bytes, self.engine_bytes)
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
        self.adopt_orphans(key, idx);
        Some(idx)
    }

    /// `child` is `parent`'s child (the engine learned it after the child existed): link it and
    /// keep the focus-chain bookkeeping in step. A refused move (a cycle in a hostile tree) leaves
    /// the node where it was.
    fn link(&mut self, child: NodeIdx, parent: NodeIdx) {
        if self.ivm.parent(child) == Some(parent) {
            return;
        }
        if self.ivm.reparent(child, Some(parent)).is_ok()
            && let Some(Some((_, p))) = self.nodes.get_mut(child.0 as usize)
        {
            *p = Some(parent);
            if let Some(b) = self.bridge() {
                b.touch(parent.0);
            }
        }
    }

    fn adopt_orphans(&mut self, key: NodeKey, idx: NodeIdx) {
        let Some(kids) = self.orphans.remove(&key) else {
            return;
        };
        self.orphan_count = self.orphan_count.saturating_sub(kids.len());
        for k in kids {
            if let Some(&ci) = self.index.get(&k)
                && self.ivm.parent(ci).is_none()
            {
                self.link(ci, idx);
            }
        }
    }

    fn remember_orphan(&mut self, parent: NodeKey, child: NodeKey) {
        if self.orphan_count >= ORPHAN_CAP {
            self.orphans.clear();
            self.orphan_count = 0;
        }
        let list = self.orphans.entry(parent).or_default();
        if !list.contains(&child) {
            list.push(child);
            self.orphan_count += 1;
        }
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
        match (q.snap.parent, parent) {
            // The parent is known now but was not when the node was first added.
            (Some(_), Some(p)) => self.link(idx, p),
            (Some(pk), None) => self.remember_orphan(pk, key),
            (None, _) => {}
        }
        if let Some(b) = self.bridge() {
            b.upsert(idx.0, row_of(&q.snap, &rec));
        }
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
        // Every text register counts (the rare ones too), so a hostile page cannot hide bytes
        // in a field the bound does not see.
        let strings = rec.name.as_deref().map_or(0, str::len)
            + rec.sem.text.as_deref().map_or(0, SemText::heap_bytes);
        let regs = NODE_FIXED + strings;
        if self.reg_bytes.len() <= idx.0 as usize {
            self.reg_bytes.resize(idx.0 as usize + 1, 0);
        }
        let old = std::mem::replace(
            &mut self.reg_bytes[idx.0 as usize],
            u32::try_from(regs).unwrap_or(u32::MAX),
        ) as usize;
        self.engine_bytes = (self.engine_bytes + regs).saturating_sub(old);
        let bytes = std::mem::size_of::<SrcRecord>()
            + rec.name.as_deref().map_or(0, str::len)
            + rec.sem.text.as_deref().map_or(0, SemText::heap_bytes);
        while !self.awaiting.is_empty()
            && (self.awaiting.len() >= AWAITING_CAP || self.awaiting_bytes + bytes > AWAITING_BYTES)
        {
            self.pop_awaiting();
        }
        self.push_awaiting(Awaiting {
            bytes,
            key,
            idx,
            rec,
        });
        bump(&self.stats.applied, 1);
    }

    fn refresh_focus_chain(&mut self) -> Vec<NodeIdx> {
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
        chain
    }

    /// One bounded unit of work: write queued snapshots into the registers, fuse (focus chain
    /// first), attach the results to the mirror. Returns whether work is left.
    pub(super) fn step(&mut self, mirror: &mut MirrorWriter, budget: Duration) -> bool {
        if !self.has_work() && !self.focus_dirty {
            return false;
        }
        // The start of this writer turn: the plugins' bounded wait is measured from here.
        let t0 = now_ns();
        if let Some(b) = self.bridge() {
            b.begin_turn();
        }
        if self.index.len() >= MAX_NODES || self.engine_bytes >= ENGINE_BYTES {
            bump(&self.stats.resets, 1);
            self.reset();
        }
        let deadline = Instant::now() + budget;
        let mut inputs: Vec<Input> = Vec::new();
        let mut n = 0u32;
        while let Some(q) = self.pop_queue() {
            self.apply_one(&q, &mut inputs);
            n += 1;
            // The clock is read every few nodes: a node is a handful of register writes.
            if n.is_multiple_of(16) && Instant::now() >= deadline {
                break;
            }
        }
        self.apply_invalidations(&mut inputs);
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        inputs.push(Input::Tick(ms));
        let rep = self.ivm.apply(inputs);
        for e in rep.errors {
            tracing::debug!(error = %e, "fuse input refused");
        }
        let mut chain: Vec<NodeIdx> = Vec::new();
        if self.focus_dirty {
            chain = self.refresh_focus_chain();
        }
        // Plugins (M18a): the focus chain's watched nodes derive first (one bounded wait per
        // keypress path), then the dirty ones in the background; what arrived is admitted.
        if let Some((bridge, _)) = self.plugins.as_mut().and_then(|p| p.open.as_mut()) {
            let tree = IvmTree(&self.ivm);
            if !chain.is_empty() {
                let ids: Vec<u32> = chain.iter().map(|c| c.0).collect();
                bridge.interactive(&ids, &tree, t0);
            }
            bridge.background(&tree);
        }
        self.apply_plugin_ops();
        // Progress is guaranteed: a spent budget still fuses a few nodes, focus chain first.
        let budget = if Instant::now() >= deadline {
            Budget::calls(FLOOR_FUSIONS)
        } else {
            Budget::until(deadline)
        };
        let mut published: Vec<NodeIdx> = Vec::new();
        let run = self.ivm.run_with(budget, &mut |p| {
            if p.entry.is_some() {
                published.push(p.node);
            }
        });
        self.attach(mirror, run.pending == 0, deadline);
        self.refresh(mirror, &published);
        let more = self.has_work();
        if more {
            bump(&self.stats.carried, 1);
        }
        more
    }

    /// Re-fused entries of nodes whose snapshot already carries one: a parent whose content name
    /// followed its children, a node a property event made stale. (A snapshot still waiting for
    /// its first entry is [`Self::attach`]'s.)
    fn refresh(&mut self, mirror: &mut MirrorWriter, published: &[NodeIdx]) {
        for &idx in published {
            let Some(Some((key, _))) = self.nodes.get(idx.0 as usize) else {
                continue;
            };
            let key = *key;
            let Some(entry) = self.ivm.entry_view(idx) else {
                continue;
            };
            if mirror.refresh_sem(key, entry) {
                bump(&self.stats.refreshed, 1);
            }
        }
    }

    /// Attach the entries of the nodes that are no longer dirty. With nothing pending every
    /// waiting entry is ready; otherwise only a bounded prefix is examined.
    fn attach(&mut self, mirror: &mut MirrorWriter, all: bool, deadline: Instant) {
        let mut scan = if all {
            self.awaiting.len()
        } else {
            self.awaiting.len().min(AWAITING_SCAN)
        };
        let mut done = 0u32;
        while scan > 0 {
            // Bounded like the rest of the step: after a large dump the waiting entries drain
            // over several steps, never in one multi-millisecond burst (the first 16 always go).
            if done >= 16 && done.is_multiple_of(16) && Instant::now() >= deadline {
                break;
            }
            done += 1;
            scan -= 1;
            let Some(a) = self.pop_awaiting() else {
                break;
            };
            if self.index.get(&a.key) != Some(&a.idx) {
                continue;
            }
            // PV-1: a node still waiting for its fusion gets its provisional entry now; the real
            // fusion publishes later and `refresh` replaces it.
            let Some(entry) = self.ivm.entry_or_provisional(a.idx) else {
                if self.ivm.is_dirty(a.idx) {
                    self.push_awaiting(a);
                }
                continue;
            };
            if mirror.attach_sem(a.key, &a.rec, entry) {
                bump(&self.stats.attached, 1);
            } else {
                bump(&self.stats.attach_missed, 1);
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_port::testing::svc;
    use gorca_mirror::ApplyOrigin;
    use gorca_model::sem::{Group, SemRole};
    use gorca_model::{AppId, ChildList, ConnectorId, Freshness, Ifaces, LocalId, Role, StateSet};

    fn key(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(n),
        }
    }

    fn snap(
        n: u64,
        parent: Option<u64>,
        role: SemRole,
        name: &str,
        class: Option<&str>,
    ) -> Arc<NodeSnapshot> {
        let mut rec = SrcRecord::default();
        rec.sem.role = role;
        rec.name = Some(Arc::from(name));
        rec.known.set(Group::Role, true);
        rec.known.set(Group::Name, true);
        rec.class = class.map(Arc::from);
        Arc::new(NodeSnapshot {
            key: key(n),
            role: Role::Unknown,
            states: StateSet::empty(),
            name: Some(Arc::from(name)),
            description: None,
            value: None,
            parent: parent.map(key),
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            have: gorca_model::PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
            sem: gorca_model::NodeSem {
                rec: Some(Arc::new(rec)),
                entry: None,
            },
        })
    }

    fn run(w: &mut FuseWriter, m: &mut gorca_mirror::MirrorWriter) {
        while w.step(m, Duration::from_millis(50)) {}
    }

    fn name(w: &FuseWriter, n: u64) -> Option<String> {
        w.entry_of(key(n))
            .and_then(|e| e.name.as_deref().map(str::to_owned))
    }

    #[test]
    fn a_plugin_patch_reaches_the_app_actors_fused_entry() {
        let (service, jobs) = svc(false);
        let (mut mirror, _read) = gorca_mirror::mirror();
        let mut w = FuseWriter::new(None);
        w.set_plugins(
            service,
            ScopeSpec {
                app: Arc::from("somebrowser"),
                ..ScopeSpec::default()
            },
        );
        let nodes = vec![
            snap(1, None, SemRole::Generic, "root", None),
            snap(2, Some(1), SemRole::Button, "AL Alice 15:00", Some("watch")),
            snap(3, Some(2), SemRole::StaticText, "Alice", None),
        ];
        mirror.apply(
            gorca_model::SnapshotBatch::new(nodes.clone()),
            ApplyOrigin::Reply { issued_at: 1 },
        );
        w.enqueue(&nodes, 1);
        run(&mut w, &mut mirror);
        assert_eq!(name(&w, 2).as_deref(), Some("Alice!"));
        assert_eq!(*jobs.lock().unwrap(), 1);
        // The focus lands on the watched node: it already has a live fact, so nothing is asked.
        w.set_focus(Some(key(2)));
        run(&mut w, &mut mirror);
        assert_eq!(*jobs.lock().unwrap(), 1);
        // The child the call read changes: one more call.
        let n3 = snap(3, Some(2), SemRole::StaticText, "Bob", None);
        mirror.apply(
            gorca_model::SnapshotBatch::new(vec![Arc::clone(&n3)]),
            ApplyOrigin::Reply { issued_at: 2 },
        );
        w.enqueue(&[n3], 2);
        run(&mut w, &mut mirror);
        assert_eq!(name(&w, 2).as_deref(), Some("Bob!"));
        assert_eq!(*jobs.lock().unwrap(), 2);
    }
}
