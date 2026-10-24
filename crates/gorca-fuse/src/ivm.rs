// SPDX-License-Identifier: GPL-3.0-or-later
//! Incremental maintenance of `FUSE` (ADR-0022 section 2.2 "Incremental maintenance", math section
//! 6): a dirty set, early cutoff, reverse indices, a per-run work budget with the focus chain
//! first, and epochs with a synchronous re-fuse hook for masking and revocation (FS-7).
//!
//! The engine owns the writer's registers, the tree, the side table and the epochs. Inputs join
//! into registers and mark nodes dirty; [`Ivm::run`] re-fuses dirty nodes, publishes only entries
//! that changed, and dirties a neighbour only when something it reads changed:
//!
//! * **content name** (child to parent): the child's name group (value, provenance, hedge, taint)
//!   or liveness changed, and the child lies within the first `max_children` of its parent;
//! * **selector reads** (S0 to neighbours): only the groups the patch source declares
//!   ([`PatchSource::reads`]) dirty the parent or the children;
//! * **relations** (target to sources): reverse index with a fan-out cap of 64; dependents beyond
//!   the cap are marked stale at once and re-fused in background chunks;
//! * **bindings**: a cache of every anchor's resolution; a lifecycle or DOM-id change re-resolves
//!   the cached anchors and dirties the nodes whose binding moved; TTL expiry, `vis_rev` bumps
//!   and source epochs dirty exactly the nodes with affected registers.
//!
//! Carry-over under the budget changes latency only, never the result: after the dirty set drains,
//! the published entries equal [`Ivm::full_check`] (F7). `vis_rev` is writer-supplied state
//! (bounds, visibility, children), never derived from fusion results, so the result does not depend
//! on how inputs were batched.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_model::fact::{BindGen, SourceId};
use gorca_model::sem::{BindQuality, Group, GroupMask, SemEntry};

use crate::epoch::{Epochs, silences, sticky};
use crate::explain::NoTrace;
use crate::profile::Profile;
use crate::register::{NodeRegs, PrimarySlot, UserOp};
use crate::sched::{DirtyKey, DirtySet, FxMap};
use crate::side::{
    AnchorId, AnchorIndex, EpochSnapshot, FactIn, FuseCtx, NodeIdx, Ranked, Resolved, SideError,
    SideTable, SourceInfo, SourceState,
};
use crate::strata::{
    ChildView, Neighbours, PatchSource, Quirks, S0Node, S0View, Scratch, changed_vs, delta_of,
    fuse_node_with, s0_into, s0_with,
};
use crate::value::{Seq, Slot, Write};

/// Fan-out cap of the relation reverse index (ADR-0022 section 2.4).
pub const RELATION_FANOUT_CAP: usize = 64;
/// Background dependents re-fused per `run` round once the dirty set is empty.
const BACKGROUND_CHUNK: usize = 64;

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum IvmError {
    #[error("node already exists")]
    NodeExists,
    #[error("unknown node")]
    UnknownNode,
    #[error("unknown parent")]
    UnknownParent,
    #[error("a node cannot be its own ancestor")]
    Cycle,
    #[error(transparent)]
    Side(#[from] SideError),
}

/// One input of the writer.
#[derive(Clone, Debug)]
pub enum Input {
    Obs {
        node: NodeIdx,
        src: PrimarySlot,
        seq: Seq,
        write: Write,
    },
    Life {
        node: NodeIdx,
        src: PrimarySlot,
        seq: Seq,
        alive: bool,
    },
    Inval {
        node: NodeIdx,
        src: PrimarySlot,
        seq: Seq,
    },
    User {
        node: NodeIdx,
        group: Group,
        seq: Seq,
        op: UserOp,
    },
    /// The primary-reported HTML `id` of a node (drives `DomId` binding).
    Attr {
        node: NodeIdx,
        seq: Seq,
        id: Option<Arc<str>>,
    },
    /// The node's `vis_rev` (bounds, visibility, children, primary name changes).
    Vis { node: NodeIdx, rev: u32 },
    /// The node's document revision (for `Basis::DocRev`).
    DocRev { node: NodeIdx, rev: u64 },
    /// The logical clock in milliseconds.
    Tick(u64),
    /// A non-primary fact, already admitted.
    Fact(Box<FactIn>),
}

/// A published change: the new entry, or `None` when the node stopped existing.
#[derive(Clone, Debug)]
pub struct Published {
    pub node: NodeIdx,
    pub entry: Option<Arc<SemEntry>>,
}

/// The work allowed in one `run`. Carry-over changes latency, never the result.
#[derive(Copy, Clone, Debug)]
pub struct Budget {
    pub max_calls: usize,
    pub deadline: Option<Instant>,
}

impl Budget {
    pub const UNLIMITED: Budget = Budget {
        max_calls: usize::MAX,
        deadline: None,
    };

    /// At most `n` node fusions.
    pub const fn calls(n: usize) -> Budget {
        Budget {
            max_calls: n,
            deadline: None,
        }
    }

    /// Until `deadline` (checked after every node fusion).
    pub const fn until(deadline: Instant) -> Budget {
        Budget {
            max_calls: usize::MAX,
            deadline: Some(deadline),
        }
    }

    fn exhausted(&self, calls: usize) -> bool {
        calls >= self.max_calls || self.deadline.is_some_and(|d| Instant::now() >= d)
    }
}

/// What a `run` did.
#[derive(Clone, Debug, Default)]
pub struct RunReport {
    /// Node fusions performed.
    pub fuse_calls: usize,
    /// Fusions whose result equalled the published entry (early cutoff).
    pub cutoff: usize,
    pub published: Vec<Published>,
    /// Dirty nodes plus background dependents left for the next run.
    pub pending: usize,
    /// With the `fuse_full_check` feature: nodes where the incremental result differs from the
    /// from-scratch one after the run drained (always 0 for a correct engine).
    pub check_mismatches: Option<usize>,
}

/// Heap bytes of a stored delta.
fn delta_bytes(d: &[(Group, Slot)]) -> usize {
    std::mem::size_of::<Vec<(Group, Slot)>>() + std::mem::size_of_val(d)
}

/// The view of a live node's S0 (delta form), if it is alive.
fn s0_view_of(nodes: &[Option<Node>], n: NodeIdx) -> Option<S0View<'_>> {
    let nd = node_of(nodes, n)?;
    let e = nd.published.as_deref()?;
    Some(S0View::delta(
        e,
        nd.delta.as_deref().map_or(&[], Vec::as_slice),
    ))
}

/// Counters for tuning the S0 storage choice (resident compact S0, recompute, neighbour reads).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct IvmStats {
    /// Live nodes.
    pub nodes: usize,
    /// Nodes that hold a stored S0 (only the groups selectors read are kept).
    pub s0_resident_nodes: usize,
    /// Heap bytes of the stored S0 lists, plus the inline `Option<S0Node>` of every live node.
    pub s0_resident_bytes: usize,
    /// Size of one engine node record.
    pub node_record_bytes: usize,
    /// S0 computations (one per fusion: every S0 is recomputed from the registers).
    pub s0_computes: u64,
    /// Neighbour S0 reads served from the resident store (parent and children).
    pub s0_neighbour_hits: u64,
    /// Fusions performed.
    pub fusions: u64,
}

/// The result of applying a batch of inputs.
#[derive(Debug, Default)]
pub struct ApplyReport {
    pub applied: usize,
    pub errors: Vec<IvmError>,
}

/// Fusion classes, front to back: 0 the focused node and the descendants whose names it reads
/// (deepest first, so they fuse before the name that reads them); 1 its ancestors, nearest first;
/// 2 the visible window in reading order from the focus; 3 everything else (the backlog, deepest
/// first, aging).
const REST_CLASS: u8 = 3;

/// Fusions (the work clock: the scheduler never reads wall time) a node must wait in the dirty set
/// before aging may pick it ahead of depth order.
const AGE_FUSIONS: u64 = 1024;

/// A node also becomes age-eligible after it stayed pending through this many budget-cut runs
/// (a run that returned with work left): with `Budget::until` runs of about 2 ms and the actors
/// re-arming `run` within 1 ms (never more than 4 ms) while `pending > 0`, this bounds the
/// wall-clock wait to about 32 ms however slow the fusions are, and the rule stays a function of
/// the sequence of runs, not of the clock.
const AGE_CUT_RUNS: u64 = 8;

/// The wall-clock valve of the synchronous drain in production: after at least one fusion the
/// drain returns once it has run this long, and the rest of its work is finished by the start of
/// the next [`Ivm::run`] in the same order (see [`Ivm::set_sync_valve`]).
pub const SYNC_WALL_VALVE: Duration = Duration::from_micros(400);

/// While the dirty set is busy, the background queue gets `FEED_SHARE` units every `FEED_EVERY`
/// fusions (a quarter of the work), counted over all runs.
const FEED_EVERY: u64 = 16;
const FEED_SHARE: usize = 4;

/// Aging takes the fusion whose index is `AGE_SLOT` modulo `AGE_PERIOD`: a quarter of all
/// fusions, and never the first of a period, so a priority node is not starved by one slot.
const AGE_PERIOD: u64 = 4;
const AGE_SLOT: u64 = 3;

/// Debug builds only: a node fused more often than this inside one drain is a scheduling loop
/// (a dependency cycle between propagation and fusion); fail fast instead of hanging.
#[cfg(debug_assertions)]
const MAX_REFUSIONS_PER_DRAIN: u32 = 256;

/// A per-source re-fuse sweep; the anchor list is built when the sweep starts, inside `run`.
#[derive(Debug)]
struct SourceSweep {
    src: SourceId,
    anchors: Option<Vec<AnchorId>>,
    cur: usize,
}

#[derive(Debug)]
struct Node {
    parent: Option<NodeIdx>,
    children: Vec<NodeIdx>,
    /// Index in the parent's children list.
    pos: u32,
    depth: u32,
    regs: NodeRegs,
    /// Life seq of the highest-priority source that has an opinion, when alive.
    alive: Option<Seq>,
    vis: u32,
    /// Exceptions to the S0 rebuilt from `published` over the selector-read groups (usually
    /// empty, then no allocation).
    #[allow(clippy::box_collection)] // 8 bytes in the node record instead of 24
    delta: Option<Box<Vec<(Group, Slot)>>>,
    published: Option<Arc<SemEntry>>,
    /// Which non-primary source produced which group of the published entry (including through
    /// tainted content): what the silence mask consults at read time.
    /// Rarely used fields (a DOM id, a document revision, the non-primary sources behind the
    /// entry) live behind one pointer: most nodes pay 8 bytes for them.
    cold: Option<Box<Cold>>,
}

#[derive(Debug, Default)]
struct Cold {
    attr: Option<(Seq, Option<Arc<str>>)>,
    doc_rev: u64,
    contrib: Box<[(Group, SourceId)]>,
}

impl Node {
    fn attr(&self) -> Option<&(Seq, Option<Arc<str>>)> {
        self.cold.as_deref().and_then(|c| c.attr.as_ref())
    }

    fn doc_rev(&self) -> u64 {
        self.cold.as_deref().map_or(0, |c| c.doc_rev)
    }

    fn contrib(&self) -> &[(Group, SourceId)] {
        self.cold.as_deref().map_or(&[], |c| &c.contrib)
    }

    fn cold_mut(&mut self) -> &mut Cold {
        self.cold.get_or_insert_with(Box::default)
    }
}

/// Relation reverse index: `target -> sources` that read the target's text.
#[derive(Debug, Default)]
struct Relations {
    targets: HashMap<NodeIdx, Vec<NodeIdx>>,
    rev: HashMap<NodeIdx, BTreeSet<NodeIdx>>,
}

impl Relations {
    fn set(&mut self, source: NodeIdx, targets: Vec<NodeIdx>) {
        if let Some(old) = self.targets.remove(&source) {
            for t in old {
                if let Some(s) = self.rev.get_mut(&t) {
                    s.remove(&source);
                    if s.is_empty() {
                        self.rev.remove(&t);
                    }
                }
            }
        }
        for t in &targets {
            self.rev.entry(*t).or_default().insert(source);
        }
        if !targets.is_empty() {
            self.targets.insert(source, targets);
        }
    }

    fn forget(&mut self, node: NodeIdx) {
        self.set(node, Vec::new());
        self.rev.remove(&node);
    }

    fn dependents(&self, target: NodeIdx) -> Vec<NodeIdx> {
        self.rev
            .get(&target)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default()
    }
}

/// The incremental `FUSE` engine of one writer.
#[derive(Debug)]
pub struct Ivm<Q: Quirks, P: PatchSource> {
    profile: Profile,
    quirks: Q,
    patches: P,
    nodes: Vec<Option<Node>>,
    side: SideTable,
    epochs: Epochs,
    snap: EpochSnapshot,
    clock: u64,
    dom: HashMap<Arc<str>, BTreeSet<NodeIdx>>,
    exact: HashMap<(u16, u128), NodeIdx>,
    resolved: HashMap<AnchorId, Resolved>,
    anchor_nodes: FxMap<NodeIdx, BTreeSet<AnchorId>>,
    /// Nodes whose liveness or DOM id changed in this batch, DOM ids whose holders changed, and
    /// anchors to re-resolve directly: per-key re-resolution, never "all anchors".
    touched_nodes: Vec<NodeIdx>,
    touched_dom: Vec<Arc<str>>,
    touched_anchors: Vec<AnchorId>,
    dom_anchors: HashMap<Arc<str>, BTreeSet<AnchorId>>,
    exact_by_node: HashMap<NodeIdx, Vec<(u16, u128)>>,
    /// Scheduler, all on the work clock (fusion counts, never wall time): the fusion count at the
    /// first-dirty stamp of each dirty node (aging), the fusion
    /// count of the next background feed, a global sweep cursor and lazily expanded per-source
    /// sweeps (epochs without an O(n) sync part).
    fused_total: u64,
    aged_total: u64,
    next_feed: u64,
    /// Runs that returned with work left (the second aging clock, see `AGE_CUT_RUNS`).
    cut_runs: u64,
    /// Wall-clock valve of the synchronous drain (production only; `None` is fully work-based).
    sync_valve: Option<Duration>,
    /// Fusions of the synchronous drain's cap still owed after the valve cut it: the next `run`
    /// finishes them first, in heap order, before any aging or background feed.
    sync_resume: usize,
    #[cfg(debug_assertions)]
    refusions: HashMap<NodeIdx, u32>,
    s0_computes: u64,
    s0_neighbour_hits: u64,
    s0_resident_nodes: usize,
    s0_resident_bytes: usize,
    rebinds: u64,
    sweep: Option<usize>,
    source_sweeps: VecDeque<SourceSweep>,
    dirty: DirtySet,
    focus: Vec<NodeIdx>,
    visible: Vec<NodeIdx>,
    /// The priority set: focus chain, visible window and the descendants that feed them (bounded
    /// by `Limits::max_priority`), computed top-down once per change.
    /// Class and position of each member (see `REST_CLASS`); every other node is in the last class.
    prio: HashMap<NodeIdx, (u8, u32)>,
    /// Work of the last priority-set computation (nodes visited), for the cost tests.
    prio_work: usize,
    /// Silencing states by stable source name: a revoked, quarantined or paused source keeps its
    /// state when it reconnects or is re-registered.
    retained: HashMap<Arc<str>, (u64, SourceState)>,
    /// Silenced sources (masked, paused, quarantined, revoked, detached, released): consulted at
    /// read time so a published value of such a source is never read, whatever the drain cap.
    silenced: HashSet<SourceId>,
    relations: Relations,
    background: VecDeque<NodeIdx>,
    in_background: HashSet<NodeIdx>,
    scratch: Scratch,
    /// Reused S0 buffer: one allocation for the engine instead of one per fusion.
    s0_buf: S0Node,
    /// Nodes whose registers changed in this batch: trimmed to exact capacity at its end.
    shrink_list: Vec<NodeIdx>,
}

fn slot(n: NodeIdx) -> usize {
    n.0 as usize
}

fn intersects(a: GroupMask, b: GroupMask) -> bool {
    a.0 & b.0 != 0
}

/// The binding generation of a seq. `BindGen` is 32-bit: a seq that does not fit makes the
/// anchor **unbound** (fail closed) instead of colliding with other generations.
fn bind_gen(seq: Seq) -> Option<BindGen> {
    u32::try_from(seq).ok().map(BindGen)
}

fn node_of(nodes: &[Option<Node>], n: NodeIdx) -> Option<&Node> {
    nodes.get(slot(n)).and_then(Option::as_ref)
}

fn by_node(nodes: &[Option<Node>], n: NodeIdx, quality: BindQuality) -> Resolved {
    match node_of(nodes, n).and_then(|nd| nd.alive).and_then(bind_gen) {
        Some(generation) => Resolved {
            node: Some(n),
            quality,
            generation,
        },
        None => Resolved::UNBOUND,
    }
}

/// The anchor index over the live state: nodes bind `Exact` at their life seq, a DOM id binds
/// `Unique` only when its count is 1 **and** exactly one live node holds the id (else
/// `Heuristic`), with the holder's `attr` seq as generation.
struct Live<'a> {
    nodes: &'a [Option<Node>],
    dom: &'a HashMap<Arc<str>, BTreeSet<NodeIdx>>,
    exact: &'a HashMap<(u16, u128), NodeIdx>,
}

impl AnchorIndex for Live<'_> {
    fn resolve(&self, anchor: &AnchorId) -> Resolved {
        resolve_in(self.nodes, self.dom, self.exact, anchor)
    }
}

fn resolve_in(
    nodes: &[Option<Node>],
    dom: &HashMap<Arc<str>, BTreeSet<NodeIdx>>,
    exact: &HashMap<(u16, u128), NodeIdx>,
    anchor: &AnchorId,
) -> Resolved {
    match anchor {
        AnchorId::Node(n) => by_node(nodes, *n, BindQuality::Exact),
        AnchorId::Exact { space, id } => exact.get(&(*space, *id)).map_or(Resolved::UNBOUND, |n| {
            by_node(nodes, *n, BindQuality::Exact)
        }),
        AnchorId::Dom { id, count, .. } => {
            let holders: Vec<NodeIdx> = dom
                .get(id)
                .map(|s| {
                    s.iter()
                        .copied()
                        .filter(|n| node_of(nodes, *n).is_some_and(|nd| nd.alive.is_some()))
                        .collect()
                })
                .unwrap_or_default();
            let Some(first) = holders.first().copied() else {
                return Resolved::UNBOUND;
            };
            let seq = node_of(nodes, first)
                .and_then(|nd| nd.attr())
                .map_or(0, |a| a.0);
            let Some(generation) = bind_gen(seq) else {
                return Resolved::UNBOUND;
            };
            Resolved {
                node: Some(first),
                quality: if *count == 1 && holders.len() == 1 {
                    BindQuality::Unique
                } else {
                    BindQuality::Heuristic
                },
                generation,
            }
        }
        // Region, structural and content anchors need geometry or hashing indices (M24).
        AnchorId::Content(_) | AnchorId::Region { .. } | AnchorId::Structural { .. } => {
            Resolved::UNBOUND
        }
    }
}

/// The views of the first `cap` children, from a getter of their current values. With `depth > 0`
/// a child that is alive and has no name of its own is replaced by its own children (and theirs,
/// down to `depth` levels): the name from content of the parent then reads the text of nameless
/// containers too (accname 2F). The output is still at most `cap` views; the flag says whether
/// anything was left out.
fn views<'a, 'n>(
    children: &[NodeIdx],
    cap: usize,
    depth: u8,
    kids_of: impl Fn(NodeIdx) -> &'n [NodeIdx],
    get: impl Fn(NodeIdx) -> (Option<S0View<'a>>, Option<&'a SemEntry>),
) -> (Vec<ChildView<'a>>, bool) {
    let mut out: Vec<ChildView<'a>> = Vec::new();
    let mut more = false;
    // Depth-first, document order; the stack holds the siblings still to visit at each level.
    let mut stack: Vec<std::slice::Iter<'_, NodeIdx>> = vec![children.iter()];
    'walk: while let Some(top) = stack.last_mut() {
        let Some(&c) = top.next() else {
            stack.pop();
            continue;
        };
        let (s0, fused) = get(c);
        let nameless = fused.is_some_and(|e| e.name.as_deref().is_none_or(str::is_empty));
        let kids = kids_of(c);
        if nameless && !kids.is_empty() && stack.len() <= usize::from(depth) {
            stack.push(kids.iter());
            continue;
        }
        if out.len() >= cap {
            more = true;
            break 'walk;
        }
        out.push(ChildView { id: c.0, s0, fused });
    }
    (out, more)
}

/// What the parent reads of a child's entry: the name group and liveness.
type NameProj = Option<(Option<Arc<str>>, Option<gorca_model::sem::ProvCode>, u8, u8)>;

fn name_proj(e: Option<&SemEntry>) -> NameProj {
    e.map(|e| {
        (
            e.name.clone(),
            e.prov.get(Group::Name),
            e.hedge.get(Group::Name),
            e.taint.get(Group::Name),
        )
    })
}

type TextProj = Option<(Option<Arc<str>>, Option<Arc<str>>)>;

fn text_proj(e: Option<&SemEntry>) -> TextProj {
    e.map(|e| (e.name.clone(), e.sem.description().map(Arc::from)))
}

/// The alive seq of a node: the lifecycle of the highest-priority source that has an opinion.
fn alive_seq(profile: &Profile, regs: &NodeRegs) -> Option<Seq> {
    profile
        .pi(Group::Role)
        .iter()
        .find_map(|p| regs.src(*p).life())
        .and_then(|(seq, a)| a.then_some(seq))
}

impl<Q: Quirks, P: PatchSource> Ivm<Q, P> {
    pub fn new(profile: Profile, quirks: Q, patches: P) -> Ivm<Q, P> {
        Ivm {
            profile,
            quirks,
            patches,
            nodes: Vec::new(),
            side: SideTable::default(),
            epochs: Epochs::default(),
            snap: EpochSnapshot::default(),
            clock: 0,
            dom: HashMap::new(),
            exact: HashMap::new(),
            resolved: HashMap::new(),
            anchor_nodes: FxMap::default(),
            touched_nodes: Vec::new(),
            touched_dom: Vec::new(),
            touched_anchors: Vec::new(),
            dom_anchors: HashMap::new(),
            exact_by_node: HashMap::new(),
            fused_total: 0,
            aged_total: 0,
            next_feed: 0,
            cut_runs: 0,
            sync_valve: None,
            sync_resume: 0,
            #[cfg(debug_assertions)]
            refusions: HashMap::new(),
            s0_computes: 0,
            s0_neighbour_hits: 0,
            s0_resident_nodes: 0,
            s0_resident_bytes: 0,
            rebinds: 0,
            sweep: None,
            source_sweeps: VecDeque::new(),
            dirty: DirtySet::default(),
            focus: Vec::new(),
            visible: Vec::new(),
            prio: HashMap::new(),
            prio_work: 0,
            retained: HashMap::new(),
            silenced: HashSet::new(),
            relations: Relations::default(),
            background: VecDeque::new(),
            in_background: HashSet::new(),
            scratch: Scratch::default(),
            s0_buf: S0Node::default(),
            shrink_list: Vec::new(),
        }
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    pub fn side(&self) -> &SideTable {
        &self.side
    }

    pub fn patches(&self) -> &P {
        &self.patches
    }

    /// The published entry of `node` **without** the silence mask: it may still carry the value
    /// of a silenced source until the bounded re-fusion reaches the node. Speech and every other
    /// reader must use [`Ivm::entry_view`]; this exists for the engine's own tests.
    #[doc(hidden)]
    pub fn entry_unmasked(&self, node: NodeIdx) -> Option<&Arc<SemEntry>> {
        node_of(&self.nodes, node).and_then(|n| n.published.as_ref())
    }

    /// The entry to **read** (key to speech): the published entry with every group produced by a
    /// silenced source suppressed at once (default value, plain `Default` provenance, unknown),
    /// for all nodes and independent of how far the re-fusion has got. O(1) for the common
    /// case; allocates only for an entry that actually carries a silenced source's value.
    pub fn entry_view(&self, node: NodeIdx) -> Option<Arc<SemEntry>> {
        let n = node_of(&self.nodes, node)?;
        let e = n.published.as_ref()?;
        if self.silenced.is_empty() || n.contrib().is_empty() {
            return Some(Arc::clone(e));
        }
        let hit: Vec<Group> = n
            .contrib()
            .iter()
            .filter(|(_, s)| self.silenced.contains(s))
            .map(|(g, _)| *g)
            .collect();
        if hit.is_empty() {
            return Some(Arc::clone(e));
        }
        let mut m = (**e).clone();
        for g in hit {
            crate::value::write_default(&mut m, g, self.profile.default_role);
            m.prov.set(g, gorca_model::sem::ProvCode::Default);
            m.hedge.set(g, 0);
            m.taint.set(g, 0);
            m.known = m.known.without(g);
        }
        m.sem_rev = e.sem_rev.wrapping_add(1);
        Some(Arc::new(m))
    }

    /// Dirty nodes plus background dependents not yet re-fused.
    pub fn pending(&self) -> usize {
        let sweep = self.sweep.map_or(0, |c| self.nodes.len().saturating_sub(c));
        let sources: usize = self
            .source_sweeps
            .iter()
            .map(|s| {
                s.anchors
                    .as_ref()
                    .map_or(1, |a| a.len().saturating_sub(s.cur))
            })
            .sum();
        self.dirty.len() + self.background.len() + sweep + sources
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// The current binding of an anchor (what a writer stamps as the fact's `BindGen`).
    pub fn resolve_anchor(&self, a: &AnchorId) -> Resolved {
        resolve_in(&self.nodes, &self.dom, &self.exact, a)
    }

    /// The node's current `vis_rev` (what a writer stamps as the fact's basis).
    pub fn vis_rev(&self, node: NodeIdx) -> u32 {
        node_of(&self.nodes, node).map_or(0, |n| n.vis)
    }

    // ---- structure --------------------------------------------------------------------------

    /// Add a node under `parent` (parents first). The node starts dirty; its parent is dirtied
    /// (the children list changed).
    pub fn add_node(&mut self, node: NodeIdx, parent: Option<NodeIdx>) -> Result<(), IvmError> {
        if node_of(&self.nodes, node).is_some() {
            return Err(IvmError::NodeExists);
        }
        let (depth, pos) = match parent {
            Some(p) => {
                let pn = node_of(&self.nodes, p).ok_or(IvmError::UnknownParent)?;
                (
                    pn.depth + 1,
                    u32::try_from(pn.children.len()).unwrap_or(u32::MAX),
                )
            }
            None => (0, 0),
        };
        if self.nodes.len() <= slot(node) {
            self.nodes.resize_with(slot(node) + 1, || None);
        }
        self.nodes[slot(node)] = Some(Node {
            parent,
            children: Vec::new(),
            pos,
            depth,
            regs: NodeRegs::default(),
            alive: None,
            vis: 0,
            delta: None,
            published: None,
            cold: None,
        });
        if let Some(p) = parent
            && let Some(Some(pn)) = self.nodes.get_mut(slot(p))
        {
            pn.children.push(node);
        }
        self.mark_dirty(node);
        if let Some(p) = parent {
            self.mark_content_readers(p);
        }
        Ok(())
    }

    /// True when `node` exists in the engine.
    pub fn contains(&self, node: NodeIdx) -> bool {
        node_of(&self.nodes, node).is_some()
    }

    /// The parent of `node` (`None` for a root or an unknown node).
    pub fn parent(&self, node: NodeIdx) -> Option<NodeIdx> {
        node_of(&self.nodes, node).and_then(|n| n.parent)
    }

    /// The ordered children of `node` (empty for an unknown node).
    pub fn children(&self, node: NodeIdx) -> &[NodeIdx] {
        node_of(&self.nodes, node).map_or(&[], |n| n.children.as_slice())
    }

    /// Make `desired` the complete, ordered child list of `parent` (a source's child list is
    /// complete, SL-4). Every node of `desired` must exist already (a new node is added first
    /// with `add_node(node, None)`); listed nodes move under `parent` from wherever they were,
    /// and former children that are not listed become roots (the caller removes them with
    /// [`Ivm::remove_subtree`] when they are gone). A repeated entry keeps its first place. The
    /// call is atomic: on `Err` nothing changed. The parent and every node that left a child
    /// list are re-fused (the content name reads the children).
    pub fn set_children(&mut self, parent: NodeIdx, desired: &[NodeIdx]) -> Result<(), IvmError> {
        let pn = node_of(&self.nodes, parent).ok_or(IvmError::UnknownParent)?;
        let parent_depth = pn.depth;
        let mut seen: HashSet<NodeIdx> = HashSet::with_capacity(desired.len());
        let mut list: Vec<NodeIdx> = Vec::with_capacity(desired.len());
        for &c in desired {
            if node_of(&self.nodes, c).is_none() {
                return Err(IvmError::UnknownNode);
            }
            if c == parent {
                return Err(IvmError::Cycle);
            }
            if seen.insert(c) {
                list.push(c);
            }
        }
        // A listed node may not be an ancestor of `parent` (the walk is bounded by the depth).
        let mut up = node_of(&self.nodes, parent).and_then(|n| n.parent);
        let mut hops = 0u32;
        while let Some(a) = up {
            if seen.contains(&a) {
                return Err(IvmError::Cycle);
            }
            hops += 1;
            if hops > parent_depth + 1 {
                break;
            }
            up = node_of(&self.nodes, a).and_then(|n| n.parent);
        }
        // Former children that are not listed become roots.
        let old: Vec<NodeIdx> = node_of(&self.nodes, parent)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        for c in old {
            if !seen.contains(&c) {
                self.relink(c, None, 0, 0);
            }
        }
        let child_depth = parent_depth + 1;
        for (i, &c) in list.iter().enumerate() {
            let pos = u32::try_from(i).unwrap_or(u32::MAX);
            let (old_parent, old_depth) =
                node_of(&self.nodes, c).map_or((None, 0), |n| (n.parent, n.depth));
            if let Some(op) = old_parent
                && op != parent
                && let Some(Some(opn)) = self.nodes.get_mut(slot(op))
            {
                opn.children.retain(|x| *x != c);
                for (j, k) in opn.children.clone().into_iter().enumerate() {
                    if let Some(Some(kn)) = self.nodes.get_mut(slot(k)) {
                        kn.pos = u32::try_from(j).unwrap_or(u32::MAX);
                    }
                }
                self.mark_content_readers(op);
            }
            if let Some(Some(cn)) = self.nodes.get_mut(slot(c)) {
                cn.parent = Some(parent);
                cn.pos = pos;
            }
            if old_depth != child_depth || old_parent.is_none() {
                self.set_subtree_depth(c, child_depth);
            }
        }
        if let Some(Some(pn)) = self.nodes.get_mut(slot(parent)) {
            pn.children = list;
        }
        self.mark_content_readers(parent);
        Ok(())
    }

    /// Move `node` and its subtree under `parent` as the **last** child, or make it a root with
    /// `None` (a writer that learns a node's parent after the node: a reply for a child arrives
    /// before the parent's own). A node already under `parent` stays where it is. Atomic: on
    /// `Err` nothing changed. The old and the new parent and the node itself are re-fused (a
    /// content name reads the children, and the depth order of the dirty set follows the move).
    pub fn reparent(&mut self, node: NodeIdx, parent: Option<NodeIdx>) -> Result<(), IvmError> {
        let (old_parent, old_depth) = node_of(&self.nodes, node)
            .map(|n| (n.parent, n.depth))
            .ok_or(IvmError::UnknownNode)?;
        if old_parent == parent {
            return Ok(());
        }
        let new_depth = match parent {
            None => 0,
            Some(p) => {
                if p == node {
                    return Err(IvmError::Cycle);
                }
                let pn = node_of(&self.nodes, p).ok_or(IvmError::UnknownParent)?;
                // `node` may not be an ancestor of `p`; depths are kept exact, so the walk is
                // bounded by the depth of `p`.
                let mut up = pn.parent;
                let mut hops = 0u32;
                while let Some(a) = up {
                    if a == node {
                        return Err(IvmError::Cycle);
                    }
                    hops += 1;
                    if hops > pn.depth {
                        break;
                    }
                    up = node_of(&self.nodes, a).and_then(|n| n.parent);
                }
                pn.depth + 1
            }
        };
        if let Some(op) = old_parent
            && let Some(Some(opn)) = self.nodes.get_mut(slot(op))
        {
            opn.children.retain(|x| *x != node);
            for (j, k) in opn.children.clone().into_iter().enumerate() {
                if let Some(Some(kn)) = self.nodes.get_mut(slot(k)) {
                    kn.pos = u32::try_from(j).unwrap_or(u32::MAX);
                }
            }
            self.mark_content_readers(op);
        }
        let mut pos = 0u32;
        if let Some(p) = parent
            && let Some(Some(pn)) = self.nodes.get_mut(slot(p))
        {
            pos = u32::try_from(pn.children.len()).unwrap_or(u32::MAX);
            pn.children.push(node);
            self.mark_content_readers(p);
        }
        if let Some(Some(nd)) = self.nodes.get_mut(slot(node)) {
            nd.parent = parent;
            nd.pos = pos;
        }
        if old_depth != new_depth {
            self.set_subtree_depth(node, new_depth);
        }
        self.mark_dirty(node);
        Ok(())
    }

    /// Detach `c` from its parent's list (renumbering the siblings) and make it a root at
    /// `depth`; used for former children that a new list no longer names.
    fn relink(&mut self, c: NodeIdx, parent: Option<NodeIdx>, pos: u32, depth: u32) {
        if let Some(Some(cn)) = self.nodes.get_mut(slot(c)) {
            cn.parent = parent;
            cn.pos = pos;
        }
        self.set_subtree_depth(c, depth);
    }

    /// Set the depth of `top` and re-derive its descendants' depths (iterative, bounded by the
    /// subtree).
    fn set_subtree_depth(&mut self, top: NodeIdx, depth: u32) {
        let mut stack = vec![(top, depth)];
        while let Some((n, d)) = stack.pop() {
            let Some(Some(nd)) = self.nodes.get_mut(slot(n)) else {
                continue;
            };
            let changed = nd.depth != d;
            nd.depth = d;
            stack.extend(nd.children.iter().map(|k| (*k, d + 1)));
            // The scheduler orders the dirty set by depth (children before the names that read
            // them): a dirty node whose depth changed is re-keyed, or it keeps the order of the
            // depth it had when it was marked and its parent re-fuses once per child.
            if changed && self.dirty.contains(n) {
                self.mark_dirty(n);
            }
        }
    }

    /// Remove `node` and its whole subtree. Returns the removals to publish.
    pub fn remove_subtree(&mut self, node: NodeIdx) -> Result<Vec<Published>, IvmError> {
        let parent = node_of(&self.nodes, node)
            .ok_or(IvmError::UnknownNode)?
            .parent;
        let mut stack = vec![node];
        let mut removed = Vec::new();
        while let Some(n) = stack.pop() {
            let Some(Some(nd)) = self.nodes.get_mut(slot(n)) else {
                continue;
            };
            stack.extend(nd.children.iter().copied());
            if let Some(d) = nd.delta.take() {
                self.s0_resident_nodes = self.s0_resident_nodes.saturating_sub(1);
                self.s0_resident_bytes = self.s0_resident_bytes.saturating_sub(delta_bytes(&d));
            }
            let published = nd.published.take();
            let attr_id = nd.attr().and_then(|a| a.1.clone());
            self.nodes[slot(n)] = None;
            if published.is_some() {
                removed.push(Published {
                    node: n,
                    entry: None,
                });
            }
            self.dirty.remove(n);
            self.relations.forget(n);
            self.focus.retain(|x| *x != n);
            self.visible.retain(|x| *x != n);
            self.prio.remove(&n);
            // Anchors bound to the node, its DOM id and its exact keys re-resolve (per key).
            if let Some(set) = self.anchor_nodes.remove(&n) {
                self.touched_anchors.extend(set);
            }
            if let Some(id) = attr_id {
                if let Some(holders) = self.dom.get_mut(&id) {
                    holders.remove(&n);
                    if holders.is_empty() {
                        self.dom.remove(&id);
                    }
                }
                self.touched_dom.push(id);
            }
            for (space, id) in self.exact_by_node.remove(&n).unwrap_or_default() {
                self.exact.remove(&(space, id));
                self.touched_anchors.push(AnchorId::Exact { space, id });
            }
            self.touched_nodes.push(n);
        }
        if let Some(p) = parent
            && let Some(Some(pn)) = self.nodes.get_mut(slot(p))
        {
            pn.children.retain(|c| *c != node);
            for (i, c) in pn.children.clone().into_iter().enumerate() {
                if let Some(Some(cn)) = self.nodes.get_mut(slot(c)) {
                    cn.pos = u32::try_from(i).unwrap_or(u32::MAX);
                }
            }
            self.mark_content_readers(p);
        }
        self.reindex();
        Ok(removed)
    }

    /// Bind an exact key (for example an AccessKit node id) to a node.
    pub fn bind_exact(&mut self, space: u16, id: u128, node: NodeIdx) {
        if let Some(old) = self.exact.insert((space, id), node)
            && let Some(keys) = self.exact_by_node.get_mut(&old)
        {
            keys.retain(|k| *k != (space, id));
        }
        self.exact_by_node
            .entry(node)
            .or_default()
            .push((space, id));
        self.touched_anchors.push(AnchorId::Exact { space, id });
        self.reindex();
    }

    /// Declare which nodes' texts `source` reads (describedby/labelledby targets).
    pub fn set_relations(&mut self, source: NodeIdx, targets: Vec<NodeIdx>) {
        self.relations.set(source, targets);
    }

    // ---- priorities ---------------------------------------------------------------------------

    /// The focus chain (focused node, its ancestors, the active descendant): fused first.
    pub fn set_focus_chain(&mut self, chain: &[NodeIdx]) {
        self.focus = chain.to_vec();
        self.apply_prio();
    }

    /// The visible window: fused right after the focus chain.
    pub fn set_visible(&mut self, window: &[NodeIdx]) {
        self.visible = window.to_vec();
        self.apply_prio();
    }

    /// The non-primary sources behind the published entry of `node`, group by group (what the
    /// silence mask consults).
    #[doc(hidden)]
    pub fn contrib_of(&self, node: NodeIdx) -> Vec<(Group, SourceId)> {
        node_of(&self.nodes, node).map_or_else(Vec::new, |n| n.contrib().to_vec())
    }

    /// Whether `node` is waiting to be re-fused.
    pub fn is_dirty(&self, node: NodeIdx) -> bool {
        self.dirty.contains(node)
    }

    /// Counters for tuning the S0 storage (resident S0, recompute count, neighbour hits).
    pub fn stats(&self) -> IvmStats {
        IvmStats {
            nodes: self.nodes.iter().flatten().count(),
            s0_resident_nodes: self.s0_resident_nodes,
            s0_resident_bytes: self.s0_resident_bytes,
            node_record_bytes: std::mem::size_of::<Node>(),
            s0_computes: self.s0_computes,
            s0_neighbour_hits: self.s0_neighbour_hits,
            fusions: self.fused_total,
        }
    }

    /// Nodes the global-epoch sweep still has to visit.
    pub fn sweep_remaining(&self) -> usize {
        self.sweep.map_or(0, |c| self.nodes.len().saturating_sub(c))
    }

    /// Anchor re-resolutions performed so far (cost tests).
    pub fn rebind_count(&self) -> u64 {
        self.rebinds
    }

    /// Nodes visited by the last priority-set computation (bounded by `Limits::max_priority`).
    pub fn priority_work(&self) -> usize {
        self.prio_work
    }

    fn key_of(&self, n: NodeIdx) -> Option<DirtyKey> {
        let nd = node_of(&self.nodes, n)?;
        // Deepest first for the classes that must fuse children before the names that read them.
        let deep_first = u32::MAX - nd.depth;
        Some(match self.prio.get(&n) {
            // Children before the names that read them: deepest first in classes 0 and 2. The
            // ancestors (class 1) keep the order given, nearest first, which is deepest first too.
            Some(&(class @ (0 | 2), _)) => (class, deep_first, n.0),
            Some(&(class, pos)) => (class, pos, n.0),
            None => (REST_CLASS, deep_first, n.0),
        })
    }

    /// Whether `p` derives its name from its children (its effective role is named from content).
    fn reads_children(&self, p: NodeIdx) -> bool {
        node_of(&self.nodes, p)
            .and_then(|n| n.published.as_deref())
            .is_some_and(|e| self.profile.rules.content_named.has(e.sem.role))
    }

    /// The priority set, dependency ordered: the focused node (`focus[0]`) and the descendants it
    /// reads (class 0), its ancestors in the order given, nearest first (class 1), then the visible
    /// window, deepest first (class 2). Bounded by
    /// `Limits::max_priority` members (the excess falls to the backlog class), so the cost does
    /// not depend on the tree.
    fn compute_prio(&mut self) -> HashMap<NodeIdx, (u8, u32)> {
        let limit = self.profile.limits.max_priority.max(1);
        let cap = self.profile.limits.max_children.max(1);
        let mut set: HashMap<NodeIdx, (u8, u32)> = HashMap::new();
        let mut work = 0usize;
        let live = |n: &NodeIdx| node_of(&self.nodes, *n).is_some();
        let mut queue: VecDeque<NodeIdx> = VecDeque::new();
        if let Some(&f) = self.focus.first().filter(|n| live(n)) {
            set.insert(f, (0, 0));
            queue.push_back(f);
        }
        // Class 0: what the focused node's name reads, breadth first, bounded.
        while let Some(n) = queue.pop_front() {
            work += 1;
            // A nameless container below a reader is read through (its children feed the name).
            let transparent = Some(&n) != self.focus.first()
                && self.profile.rules.content_depth > 0
                && node_of(&self.nodes, n)
                    .and_then(|nd| nd.published.as_deref())
                    .is_none_or(|e| e.name.as_deref().is_none_or(str::is_empty));
            if !self.reads_children(n) && !transparent {
                continue;
            }
            if let Some(nd) = node_of(&self.nodes, n) {
                for c in nd.children.iter().take(cap) {
                    if set.len() < limit && !set.contains_key(c) {
                        set.insert(*c, (0, 0));
                        queue.push_back(*c);
                    }
                }
            }
        }
        for (i, n) in self.focus.iter().enumerate().skip(1) {
            work += 1;
            if live(n) && set.len() < limit {
                set.entry(*n)
                    .or_insert((1, u32::try_from(i).unwrap_or(u32::MAX)));
            }
        }
        for (i, n) in self.visible.iter().enumerate() {
            work += 1;
            if live(n) && set.len() < limit {
                set.entry(*n)
                    .or_insert((2, u32::try_from(i).unwrap_or(u32::MAX)));
            }
        }
        self.prio_work = work;
        set
    }

    /// Recompute the priority set and re-key only the dirty nodes whose class or position changed.
    fn apply_prio(&mut self) {
        let new = self.compute_prio();
        let moved: Vec<NodeIdx> = self
            .prio
            .iter()
            .filter(|(n, c)| new.get(n) != Some(c))
            .map(|(n, _)| *n)
            .chain(
                new.iter()
                    .filter(|(n, c)| self.prio.get(n) != Some(c))
                    .map(|(n, _)| *n),
            )
            .filter(|n| self.dirty.contains(*n))
            .collect();
        self.prio = new;
        for n in moved {
            self.mark_dirty(n);
        }
    }

    /// Whether `n` is in the focus chain or the visible window (or feeds one of them).
    #[doc(hidden)]
    pub fn priority_class(&self, n: NodeIdx) -> u8 {
        self.prio.get(&n).map_or(REST_CLASS, |c| c.0)
    }

    fn mark_dirty(&mut self, n: NodeIdx) {
        let Some(key) = self.key_of(n) else { return };
        self.dirty.mark(n, key, self.fused_total, self.cut_runs);
    }

    fn take_dirty(&mut self, key: DirtyKey) {
        self.dirty.take(key);
    }

    /// `p`'s children (or their names) changed: re-fuse `p`, and, because a name from content
    /// looks through nameless containers, the content-named ancestors above `p` that can see
    /// through to it (up to the profile's `content_depth` levels, stopping at the first
    /// ancestor that has a name of its own).
    fn mark_content_readers(&mut self, p: NodeIdx) {
        self.mark_dirty(p);
        let depth = self.profile.rules.content_depth;
        let mut cur = p;
        for _ in 0..depth {
            let named = node_of(&self.nodes, cur)
                .and_then(|n| n.published.as_deref())
                .is_some_and(|e| e.name.as_deref().is_some_and(|s| !s.is_empty()));
            if named {
                break;
            }
            let Some(up) = node_of(&self.nodes, cur).and_then(|n| n.parent) else {
                break;
            };
            if self.reads_children(up) {
                self.mark_dirty(up);
            }
            cur = up;
        }
    }

    /// Re-fuse `n` soon: priority nodes (and nodes already dirty) join the dirty set now; any other
    /// node waits in the background queue, which `run` serves with a guaranteed share. This keeps
    /// the cost of issuing a mask or a release independent of how many nodes it touches.
    fn schedule(&mut self, n: NodeIdx) {
        if node_of(&self.nodes, n).is_none() {
            return;
        }
        if self.prio.contains_key(&n) || self.dirty.contains(n) {
            self.mark_dirty(n);
        } else if self.in_background.insert(n) {
            self.background.push_back(n);
        }
    }

    /// Move up to `share` units of background work into the dirty set: queued dependents, then
    /// per-source sweeps, then the global sweep.
    fn feed_background(&mut self, share: usize) {
        let mut left = share;
        while left > 0 {
            if let Some(n) = self.background.pop_front() {
                self.in_background.remove(&n);
                self.mark_dirty(n);
                left -= 1;
                continue;
            }
            if let Some(sw) = self.source_sweeps.front_mut() {
                if sw.anchors.is_none() {
                    sw.anchors = Some(self.side.anchors_of_source(sw.src));
                }
                let next = sw.anchors.as_ref().and_then(|a| a.get(sw.cur)).cloned();
                match next {
                    Some(a) => {
                        sw.cur += 1;
                        self.dirty_anchor(&a);
                        left -= 1;
                    }
                    None => {
                        self.source_sweeps.pop_front();
                    }
                }
                continue;
            }
            if let Some(c) = self.sweep {
                if c < self.nodes.len() {
                    self.sweep = Some(c + 1);
                    if let Some(n) = u32::try_from(c).ok().map(NodeIdx) {
                        self.mark_dirty(n);
                    }
                    left -= 1;
                } else {
                    self.sweep = None;
                }
                continue;
            }
            break;
        }
    }

    // ---- inputs ---------------------------------------------------------------------------------

    /// Apply a batch of inputs: join registers and mark the affected nodes dirty. Nothing is
    /// fused until [`Ivm::run`].
    pub fn apply(&mut self, inputs: impl IntoIterator<Item = Input>) -> ApplyReport {
        let mut rep = ApplyReport::default();
        for i in inputs {
            match self.apply_one(i) {
                Ok(()) => rep.applied += 1,
                Err(e) => rep.errors.push(e),
            }
        }
        self.reindex();
        // A node's registers arrive in a burst (its state read) and rarely grow afterwards: trim
        // the spare capacity of every node written in this batch.
        if !self.shrink_list.is_empty() {
            self.shrink_list.sort_unstable();
            self.shrink_list.dedup();
            for n in std::mem::take(&mut self.shrink_list) {
                if let Some(Some(nd)) = self.nodes.get_mut(slot(n)) {
                    nd.regs.shrink_to_fit();
                }
            }
        }
        rep
    }

    fn node_mut(&mut self, n: NodeIdx) -> Result<&mut Node, IvmError> {
        self.nodes
            .get_mut(slot(n))
            .and_then(Option::as_mut)
            .ok_or(IvmError::UnknownNode)
    }

    fn apply_one(&mut self, input: Input) -> Result<(), IvmError> {
        let max_text = self.profile.limits.max_text;
        match input {
            Input::Obs {
                node,
                src,
                seq,
                write,
            } => {
                if self
                    .node_mut(node)?
                    .regs
                    .src_mut(src)
                    .apply_obs_capped(seq, write, max_text)
                {
                    self.shrink_list.push(node);
                    self.mark_dirty(node);
                }
            }
            Input::Life {
                node,
                src,
                seq,
                alive,
            } => {
                let profile = &self.profile;
                let n = self
                    .nodes
                    .get_mut(slot(node))
                    .and_then(Option::as_mut)
                    .ok_or(IvmError::UnknownNode)?;
                if n.regs.src_mut(src).apply_life(seq, alive) {
                    let now = alive_seq(profile, &n.regs);
                    let moved = now != n.alive;
                    n.alive = now;
                    if moved {
                        self.touched_nodes.push(node);
                    }
                    self.mark_dirty(node);
                }
            }
            Input::Inval { node, src, seq } => {
                if self.node_mut(node)?.regs.src_mut(src).apply_inval(seq) {
                    self.mark_dirty(node);
                }
            }
            Input::User {
                node,
                group,
                seq,
                op,
            } => {
                let op = match op {
                    UserOp::Set(w) => UserOp::Set(w.capped(max_text).0),
                    UserOp::Clear => UserOp::Clear,
                };
                if self.node_mut(node)?.regs.apply_user(group, seq, op) {
                    self.mark_dirty(node);
                }
            }
            Input::Attr { node, seq, id } => {
                let n = self.node_mut(node)?;
                let key = |a: &(Seq, Option<Arc<str>>)| {
                    (a.0, a.1.as_deref().unwrap_or("None").to_owned())
                };
                let new = (seq, id);
                if n.attr().is_some_and(|cur| key(&new) <= key(cur)) {
                    return Ok(());
                }
                let old = n.cold_mut().attr.replace(new.clone());
                if let Some((_, Some(old_id))) = old {
                    if let Some(set) = self.dom.get_mut(&old_id) {
                        set.remove(&node);
                        if set.is_empty() {
                            self.dom.remove(&old_id);
                        }
                    }
                    self.touched_dom.push(old_id);
                }
                if let Some(new_id) = new.1 {
                    self.dom
                        .entry(Arc::clone(&new_id))
                        .or_default()
                        .insert(node);
                    self.touched_dom.push(new_id);
                }
                self.mark_dirty(node);
            }
            Input::Vis { node, rev } => {
                let n = self.node_mut(node)?;
                if rev > n.vis {
                    n.vis = rev;
                    if self.anchor_nodes.get(&node).is_some_and(|s| !s.is_empty()) {
                        self.mark_dirty(node);
                    }
                }
            }
            Input::DocRev { node, rev } => {
                let n = self.node_mut(node)?;
                if rev > n.doc_rev() {
                    n.cold_mut().doc_rev = rev;
                    if self.anchor_nodes.get(&node).is_some_and(|s| !s.is_empty()) {
                        self.mark_dirty(node);
                    }
                }
            }
            Input::Tick(t) => {
                if t > self.clock {
                    let expiring = self.side.expiring(self.clock, t);
                    self.clock = t;
                    for a in expiring {
                        self.dirty_anchor(&a);
                    }
                }
            }
            Input::Fact(f) => {
                let anchor = f.anchor.clone();
                if self.side.insert(*f)? {
                    self.rebind_anchor(&anchor);
                    self.dirty_anchor(&anchor);
                }
            }
        }
        Ok(())
    }

    fn dirty_anchor(&mut self, a: &AnchorId) {
        if let Some(n) = self.resolved.get(a).and_then(|r| r.node) {
            self.mark_dirty(n);
        }
    }

    /// Re-resolve one anchor; dirty the nodes it moved between.
    fn rebind_anchor(&mut self, a: &AnchorId) {
        self.rebinds += 1;
        let new = resolve_in(&self.nodes, &self.dom, &self.exact, a);
        let old = self.resolved.insert(a.clone(), new);
        if old.is_none()
            && let AnchorId::Dom { id, .. } = a
        {
            self.dom_anchors
                .entry(Arc::clone(id))
                .or_default()
                .insert(a.clone());
        }
        if old == Some(new) {
            return;
        }
        if let Some(o) = old.and_then(|r| r.node) {
            if let Some(set) = self.anchor_nodes.get_mut(&o) {
                set.remove(a);
            }
            self.mark_dirty(o);
        }
        if let Some(n) = new.node {
            self.anchor_nodes.entry(n).or_default().insert(a.clone());
            self.mark_dirty(n);
        }
    }

    /// Re-resolve exactly the anchors a lifecycle, DOM-id or exact-key change can move: the node's
    /// own anchor, the anchors bound to it, the anchors of the DOM ids whose holders changed and
    /// the exact keys of the node. Cost is proportional to those, not to all cached anchors.
    fn reindex(&mut self) {
        if self.touched_nodes.is_empty()
            && self.touched_dom.is_empty()
            && self.touched_anchors.is_empty()
        {
            return;
        }
        let mut anchors: BTreeSet<AnchorId> = std::mem::take(&mut self.touched_anchors)
            .into_iter()
            .collect();
        let mut dom_ids: BTreeSet<Arc<str>> =
            std::mem::take(&mut self.touched_dom).into_iter().collect();
        for x in std::mem::take(&mut self.touched_nodes) {
            anchors.insert(AnchorId::Node(x));
            if let Some(set) = self.anchor_nodes.get(&x) {
                anchors.extend(set.iter().cloned());
            }
            for (space, id) in self.exact_by_node.get(&x).into_iter().flatten() {
                anchors.insert(AnchorId::Exact {
                    space: *space,
                    id: *id,
                });
            }
            // A holder of a DOM id going live or dead moves that id's anchors.
            if let Some(Some((_, Some(id)))) = node_of(&self.nodes, x).map(|n| n.attr().cloned()) {
                dom_ids.insert(id);
            }
        }
        for id in dom_ids {
            if let Some(set) = self.dom_anchors.get(&id) {
                anchors.extend(set.iter().cloned());
            }
        }
        for a in anchors {
            // Only anchors that have registers are cached; the rest need no resolution.
            if self.resolved.contains_key(&a) {
                self.rebind_anchor(&a);
            }
        }
    }

    // ---- sources and epochs ---------------------------------------------------------------------

    fn refresh_snapshot(&mut self) {
        self.snap = self.epochs.snapshot(self.side.source_ids());
    }

    /// Register a non-primary source.
    ///
    /// A new source is **masked** until an explicit `Active` epoch arrives
    /// ([`Ivm::set_source_state`]); a source whose stable name was revoked, quarantined or paused
    /// before inherits that state.
    pub fn set_source(&mut self, info: SourceInfo) -> Result<(), SideError> {
        let (id, name) = (info.id, Arc::clone(&info.name));
        self.side.set_source(info)?;
        if let Some(&(n, state)) = self.retained.get(&name) {
            self.epochs.join_source(id, n, state);
        }
        self.refresh_snapshot();
        Ok(())
    }

    /// Remember a silencing epoch under the source's stable name (called before the old id goes).
    fn retain_silence(&mut self, id: SourceId) {
        if let (Some(name), Some((n, state))) = (self.side.source_name(id), self.epochs.take(id))
            && sticky(state)
        {
            self.retained.insert(name, (n, state));
        }
    }

    fn forget_anchors(&mut self, anchors: &[AnchorId]) -> Vec<NodeIdx> {
        let mut touched = Vec::new();
        for a in anchors {
            if !self.side.has_anchor(a)
                && let Some(r) = self.resolved.remove(a)
            {
                if let AnchorId::Dom { id, .. } = a
                    && let Some(set) = self.dom_anchors.get_mut(id)
                {
                    set.remove(a);
                }
                if let Some(n) = r.node {
                    if let Some(set) = self.anchor_nodes.get_mut(&n) {
                        set.remove(a);
                    }
                    touched.push(n);
                }
            } else if let Some(n) = self.resolved.get(a).and_then(|r| r.node) {
                touched.push(n);
            }
        }
        touched
    }

    /// A source disconnects for good: its registration and every register it owns are dropped,
    /// and the focus chain and visible window are re-fused before this returns (bounded; the
    /// rest follows in `run`).
    pub fn release_source(&mut self, id: SourceId) -> Vec<Published> {
        self.retain_silence(id);
        self.silenced.insert(id);
        let anchors = self.side.release_source(id);
        self.refresh_snapshot();
        let touched = self.forget_anchors(&anchors);
        for n in touched {
            self.schedule(n);
        }
        self.drain_priority().published
    }

    /// A source reconnects under its stable name: the old slot is released (registration and
    /// registers) **before** the new registration, which would otherwise be rejected as a
    /// duplicate name. On error nothing changed. A silencing state (revoked, quarantined,
    /// paused) follows the name to the new connection; otherwise the new id is masked until its
    /// first explicit `Active` epoch.
    pub fn reconnect(
        &mut self,
        old: SourceId,
        info: SourceInfo,
    ) -> Result<Vec<Published>, SideError> {
        let (new_id, name) = (info.id, Arc::clone(&info.name));
        let old_state = self.epochs.entry(old);
        let anchors = self.side.reconnect(old, info)?;
        self.silenced.insert(old);
        if let Some(&(n, state)) = old_state.as_ref().filter(|(_, s)| sticky(*s)) {
            self.retained.insert(name.clone(), (n, state));
        }
        self.epochs.take(old);
        if let Some(&(n, state)) = self.retained.get(&name) {
            self.epochs.join_source(new_id, n, state);
        }
        self.refresh_snapshot();
        let touched = self.forget_anchors(&anchors);
        for n in touched {
            self.schedule(n);
        }
        Ok(self.drain_priority().published)
    }

    /// A per-source epoch message (mask, pause, revoke, detach, throttle, resume). Issuing it is
    /// O(priority set): the priority nodes are re-fused synchronously (FS-7, bounded by
    /// `Limits::max_sync_drain`) and the source's other nodes are swept by `run` with a guaranteed
    /// share. Returns what was published.
    pub fn set_source_state(
        &mut self,
        src: SourceId,
        n: u64,
        state: SourceState,
    ) -> Vec<Published> {
        if !self.epochs.join_source(src, n, state) {
            return Vec::new();
        }
        if let Some(name) = self.side.source_name(src) {
            if sticky(state) {
                self.retained.insert(name, (n, state));
            } else if self.retained.get(&name).is_some_and(|(rn, _)| n > *rn) {
                // An explicit later epoch lifts the silence.
                self.retained.remove(&name);
            }
        }
        if silences(state) {
            self.silenced.insert(src);
        } else {
            self.silenced.remove(&src);
        }
        self.refresh_snapshot();
        self.touch_priority_nodes();
        self.source_sweeps.push_back(SourceSweep {
            src,
            anchors: None,
            cur: 0,
        });
        if silences(state) {
            self.drain_priority().published
        } else {
            Vec::new()
        }
    }

    /// Re-fuse the priority nodes that carry non-primary facts: as many as the synchronous drain
    /// can take go to the front of the dirty set; the rest wait in the background queue (the
    /// silence mask already hides their silenced values).
    fn touch_priority_nodes(&mut self) {
        let cap = self.profile.limits.max_sync_drain.max(1);
        // `prio` is a hash map: sort by (class, order, node) before the cap split, so the same
        // nodes are fused synchronously on every engine and in every process.
        let mut nodes: Vec<(u8, u32, NodeIdx)> = self
            .prio
            .iter()
            .filter(|(n, _)| self.anchor_nodes.get(n).is_some_and(|s| !s.is_empty()))
            .map(|(n, (class, order))| (*class, *order, *n))
            .collect();
        nodes.sort_unstable();
        for (i, (_, _, n)) in nodes.into_iter().enumerate() {
            if i < cap {
                self.mark_dirty(n);
            } else if !self.dirty.contains(n) && self.in_background.insert(n) {
                self.background.push_back(n);
            }
        }
    }

    /// The global epoch (patch set changed, host-wide mask): O(priority set) to issue. The
    /// priority nodes are re-fused synchronously (bounded), every other node by the sweep `run`
    /// serves in budgeted chunks.
    pub fn bump_global(&mut self) -> Vec<Published> {
        self.epochs.bump_global();
        self.sweep = Some(0);
        let mut nodes: Vec<NodeIdx> = self.prio.keys().copied().collect();
        nodes.sort_unstable();
        for n in nodes {
            self.mark_dirty(n);
        }
        self.drain_priority().published
    }

    pub fn global_epoch(&self) -> u64 {
        self.epochs.global()
    }

    /// Mutable access to the patch source; call [`Ivm::bump_global`] after changing it.
    pub fn patches_mut(&mut self) -> &mut P {
        &mut self.patches
    }

    // ---- running ---------------------------------------------------------------------------------

    /// Production only: let the synchronous drain return after `valve` of wall time (at least one
    /// fusion is always done). The published content does not depend on where the valve cuts: the
    /// fusions still owed out of the drain's cap are finished by the start of the next `run` in
    /// the same heap order, before any aging or background feed, so the concatenated publication
    /// stream equals the uncut one (as long as no input is applied in between). Replay, tests and
    /// benchmarks leave it `None`.
    pub fn set_sync_valve(&mut self, valve: Option<Duration>) {
        self.sync_valve = valve;
    }

    /// Re-fuse the priority classes synchronously (focus chain first, then the visible window), at
    /// most `Limits::max_sync_drain` fusions; what does not fit stays dirty in front for the next
    /// `run`. Publications go to `sink` the moment they happen. The bound is a count of fusions:
    /// the published prefix is the same on a slow and a fast machine (the read-time silence mask
    /// already hides silenced values on nodes the drain did not reach, and the cap bounds the
    /// latency). With a valve set the cut may come earlier, with the same overall stream.
    fn drain_priority_with(&mut self, sink: &mut dyn FnMut(Published)) -> RunReport {
        let cap = self.profile.limits.max_sync_drain.max(1);
        let mut rep = RunReport::default();
        #[cfg(debug_assertions)]
        self.refusions.clear();
        self.sync_resume = 0;
        self.drain_priority_loop(cap, self.sync_valve, None, &mut rep, sink);
        rep.pending = self.pending();
        rep
    }

    /// The loop of the synchronous drain, shared with the resume at the start of a `run`. At most
    /// `cap` fusions are done by this call; if the `valve` or the `budget` stops it early, the
    /// fusions of the cap still owed are remembered in `sync_resume`.
    fn drain_priority_loop(
        &mut self,
        cap: usize,
        valve: Option<Duration>,
        budget: Option<&Budget>,
        rep: &mut RunReport,
        sink: &mut dyn FnMut(Published),
    ) {
        let (t0, start) = (Instant::now(), rep.fuse_calls);
        'outer: loop {
            let before = rep.fuse_calls;
            while let Some(key) = self.dirty.peek() {
                let done = rep.fuse_calls - start;
                if key.0 >= REST_CLASS || done >= cap {
                    break;
                }
                let valve_cut = done > 0 && valve.is_some_and(|v| t0.elapsed() >= v);
                if valve_cut || budget.is_some_and(|b| b.exhausted(rep.fuse_calls)) {
                    self.sync_resume = cap - done;
                    break 'outer;
                }
                self.take_dirty(key);
                self.process(NodeIdx(key.2), rep, sink);
            }
            // Fusing changes which nodes feed the priority nodes: refresh the set and go on while
            // a priority node is still dirty (keys are scheduling hints, never results).
            if rep.fuse_calls == before || rep.fuse_calls - start >= cap {
                break;
            }
            self.apply_prio();
            if self.dirty.peek().is_none_or(|k| k.0 >= REST_CLASS) {
                break;
            }
        }
    }

    fn drain_priority(&mut self) -> RunReport {
        let mut out = Vec::new();
        let mut rep = self.drain_priority_with(&mut |p| out.push(p));
        rep.published = out;
        rep
    }

    /// Re-fuse dirty nodes until the budget is spent: the focus chain, then the visible window,
    /// then deepest first, with two anti-starvation guarantees. **Aging:** a node dirty for
    /// `AGE_FUSIONS` fusions, or pending through `AGE_CUT_RUNS` budget-cut runs, may be taken ahead
    /// of depth order, in the fusion slots that are a quarter of all fusions. **Background share:**
    /// queued dependents, sweeps and epoch re-fuses get `FEED_SHARE` units every `FEED_EVERY`
    /// fusions even while the dirty set is busy. Both clocks count fusions (and cut runs), never
    /// wall time, so a deadline decides only how the same fusion sequence is batched into runs
    /// (until a node has waited through `AGE_CUT_RUNS` cut runs). Carry-over changes latency only,
    /// never the result.
    pub fn run(&mut self, budget: Budget) -> RunReport {
        let mut out = Vec::new();
        let mut rep = self.run_with(budget, &mut |p| out.push(p));
        rep.published = out;
        rep
    }

    /// As [`Ivm::run`], handing every publication to `sink` **as soon as its node is fused**, so a
    /// result is visible long before a deadline-bounded run returns (`RunReport::published` stays
    /// empty).
    pub fn run_with(&mut self, budget: Budget, sink: &mut dyn FnMut(Published)) -> RunReport {
        let mut rep = RunReport::default();
        #[cfg(debug_assertions)]
        self.refusions.clear();
        if self.sync_resume > 0 {
            // The synchronous drain was cut by its valve: finish its cap first, in the same order.
            let left = std::mem::take(&mut self.sync_resume);
            self.drain_priority_loop(left, None, Some(&budget), &mut rep, sink);
        }
        let mut cut = false;
        loop {
            if budget.exhausted(rep.fuse_calls) {
                cut = true;
                break;
            }
            // The work clock: ticks, background feeds and aging slots are functions of the number
            // of fusions so far, so where a run is cut (a call count, or a wall deadline) changes
            // only how the same fusion sequence is batched into runs.
            if self.fused_total >= self.next_feed {
                self.next_feed = self.fused_total + FEED_EVERY;
                self.feed_background(FEED_SHARE);
            }
            let aged = if self.fused_total % AGE_PERIOD == AGE_SLOT {
                self.dirty
                    .oldest(self.fused_total, AGE_FUSIONS, self.cut_runs, AGE_CUT_RUNS)
            } else {
                None
            };
            let key = match aged {
                Some(k) => {
                    self.aged_total += 1;
                    Some(k)
                }
                None => self.dirty.peek(),
            };
            let Some(key) = key else {
                self.feed_background(BACKGROUND_CHUNK);
                if self.dirty.is_empty() {
                    break;
                }
                continue;
            };
            self.take_dirty(key);
            self.fused_total += 1;
            self.process(NodeIdx(key.2), &mut rep, sink);
        }
        rep.pending = self.pending();
        if cut && rep.pending > 0 {
            self.cut_runs += 1;
        }
        if rep.pending == 0 {
            // Nothing references a released or re-registered id any more.
            let (side, epochs) = (&self.side, &self.epochs);
            self.silenced
                .retain(|s| side.has_source(*s) && epochs.state(*s).is_some_and(silences));
        }
        #[cfg(feature = "fuse_full_check")]
        if rep.pending == 0 {
            rep.check_mismatches = Some(self.verify().err().map_or(0, |v| v.len()));
        }
        rep
    }

    fn process(&mut self, x: NodeIdx, rep: &mut RunReport, sink: &mut dyn FnMut(Published)) {
        #[cfg(debug_assertions)]
        {
            let c = self.refusions.entry(x).or_insert(0);
            *c += 1;
            assert!(
                *c <= MAX_REFUSIONS_PER_DRAIN,
                "node {x:?} fused {c} times in one drain: scheduling loop"
            );
        }
        let mut buf = std::mem::take(&mut self.s0_buf);
        let (alive, fused) = {
            let Some(node) = node_of(&self.nodes, x) else {
                self.s0_buf = buf;
                return;
            };
            let anchors: Vec<AnchorId> = self
                .anchor_nodes
                .get(&x)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            let resolved = &self.resolved;
            let resolve = |a: &AnchorId| resolved.get(a).copied().unwrap_or(Resolved::UNBOUND);
            let (vis, doc) = (node.vis, node.doc_rev());
            let vis_fn = move |_: NodeIdx| vis;
            let doc_fn = move |_: NodeIdx| doc;
            let ctx = FuseCtx {
                epoch: &self.snap,
                clock_ms: self.clock,
                vis_rev: &vis_fn,
                doc_rev: &doc_fn,
            };
            let ranked = if anchors.is_empty() {
                Ranked::default()
            } else {
                self.side.ranked_for(x, anchors.iter(), &resolve, &ctx)
            };
            self.s0_computes += 1;
            let alive = s0_into(
                &self.profile,
                &node.regs,
                ranked.view(),
                &self.quirks,
                &mut NoTrace,
                &mut buf,
            );
            let nodes = &self.nodes;
            let fused = alive.then(|| {
                let reads = self.patches.reads();
                // The children views are built only when something can read them: a selector
                // that reads children, a patch that can re-role the node, or a role named from
                // its content (the effective role is the user override, else S0, else default).
                let role = node
                    .regs
                    .user_value(Group::Role)
                    .and_then(|(_, w)| w.slot().role())
                    .or_else(|| buf.role())
                    .unwrap_or(self.profile.default_role);
                let needs_children = reads.children.0 != 0
                    || self.patches.writes().has(Group::Role)
                    || self.profile.rules.content_named.has(role);
                let cap = self.profile.limits.max_children.max(1);
                let depth = if self.profile.rules.content_named.has(role) {
                    self.profile.rules.content_depth
                } else {
                    0
                };
                let (vs, more) = if needs_children {
                    views(
                        &node.children,
                        cap,
                        depth,
                        |c| node_of(nodes, c).map_or(&[][..], |cn| cn.children.as_slice()),
                        |c| {
                            node_of(nodes, c).map_or((None, None), |cn| {
                                (s0_view_of(nodes, c), cn.published.as_deref())
                            })
                        },
                    )
                } else {
                    (Vec::new(), false)
                };
                let parent_s0 = node.parent.and_then(|p| s0_view_of(nodes, p));
                self.s0_neighbour_hits += u64::from(parent_s0.is_some());
                let nb = Neighbours {
                    parent: parent_s0,
                    children: &vs,
                    more_children: more,
                };
                fuse_node_with(
                    &self.profile,
                    &node.regs,
                    &buf,
                    &nb,
                    ranked.view(),
                    &self.patches,
                    &mut self.scratch,
                    &mut NoTrace,
                )
            });
            (alive, fused)
        };

        // Phase B: store, publish with early cutoff, propagate.
        let (new_entry, mut contrib, content_children) = match fused {
            Some(f) => (Some(f.entry), f.winners, f.content_children),
            None => (None, Vec::new(), Vec::new()),
        };
        // A name built from tainted children carries their sources up the content edge.
        for c in &content_children {
            if let Some(cn) = node_of(&self.nodes, NodeIdx(*c)) {
                contrib.extend(
                    cn.contrib()
                        .iter()
                        .filter(|(g, _)| *g == Group::Name)
                        .copied(),
                );
            }
        }
        contrib.sort_by_key(|(g, s)| (g.slot(), s.slot, s.generation));
        contrib.dedup();
        let reads = self.patches.reads();
        let mask = GroupMask(reads.parent.0 | reads.children.0);
        let max_children = self.profile.limits.max_children.max(1);
        let Some(Some(node)) = self.nodes.get_mut(slot(x)) else {
            self.s0_buf = buf;
            return;
        };
        // Neighbours read the delta form: the published entry plus the exceptions over the
        // selector-read groups. Compare with the old view (old entry + old delta), then store the
        // new delta; the common fusion allocates nothing for S0.
        let old_pub_for_view = node.published.clone();
        let s0_changed = {
            let old_view = old_pub_for_view
                .as_deref()
                .map(|e| S0View::delta(e, node.delta.as_deref().map_or(&[], Vec::as_slice)));
            changed_vs(old_view, alive.then_some(&buf), mask)
        };
        let new_delta: Option<Vec<(Group, Slot)>> = match (&new_entry, alive) {
            (Some(e), true) => {
                let d = delta_of(e, &buf, mask);
                (!d.is_empty()).then_some(d)
            }
            _ => None,
        };
        let old_bytes = node.delta.as_deref().map(|d| delta_bytes(d));
        let new_bytes = new_delta.as_ref().map(|d| delta_bytes(d));
        node.delta = new_delta.map(Box::new);
        self.s0_resident_nodes = (self.s0_resident_nodes + usize::from(new_bytes.is_some()))
            .saturating_sub(usize::from(old_bytes.is_some()));
        self.s0_resident_bytes = (self.s0_resident_bytes + new_bytes.unwrap_or(0))
            .saturating_sub(old_bytes.unwrap_or(0));
        buf.clear();
        self.s0_buf = buf;
        let Some(Some(node)) = self.nodes.get_mut(slot(x)) else {
            return;
        };
        if !contrib.is_empty() || node.cold.is_some() {
            node.cold_mut().contrib = contrib.into_boxed_slice();
        }
        let (parent, pos) = (node.parent, node.pos);
        let old_pub = node.published.clone();
        let published_entry: Option<Arc<SemEntry>> = match (&old_pub, new_entry) {
            (None | Some(_), None) => None,
            (None, Some(mut e)) => {
                e.sem_rev = 1;
                Some(Arc::new(e))
            }
            (Some(old), Some(mut e)) => {
                e.sem_rev = old.sem_rev;
                if e == **old {
                    Some(Arc::clone(old))
                } else {
                    e.sem_rev = old.sem_rev.wrapping_add(1);
                    Some(Arc::new(e))
                }
            }
        };
        let changed = match (&old_pub, &published_entry) {
            (None, None) => false,
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            _ => true,
        };
        node.published.clone_from(&published_entry);
        rep.fuse_calls += 1;
        if changed {
            sink(Published {
                node: x,
                entry: published_entry.clone(),
            });
        } else {
            rep.cutoff += 1;
        }

        // Selector-read propagation (S0 values only).
        if let Some(p) = parent
            && intersects(s0_changed, reads.children)
        {
            self.mark_dirty(p);
        }
        if intersects(s0_changed, reads.parent) {
            let children = node_of(&self.nodes, x).map(|n| n.children.clone());
            for c in children.into_iter().flatten() {
                self.mark_dirty(c);
            }
        }
        if !changed {
            return;
        }
        // Content-name edge: the parent reads this child's name group while within its prefix.
        let before = name_proj(old_pub.as_deref());
        let after = name_proj(published_entry.as_deref());
        if before != after
            && let Some(p) = parent
            && (pos as usize) < max_children
        {
            self.mark_content_readers(p);
        }
        // Relation edge: dependents read this node's text.
        if text_proj(old_pub.as_deref()) != text_proj(published_entry.as_deref()) {
            let deps = self.relations.dependents(x);
            for (i, d) in deps.into_iter().enumerate() {
                if i < RELATION_FANOUT_CAP {
                    self.mark_dirty(d);
                } else {
                    self.defer_stale(d, sink);
                }
            }
        }
    }

    /// Beyond the fan-out cap: publish the dependent with its text groups marked stale now and
    /// re-fuse it in a background chunk.
    fn defer_stale(&mut self, d: NodeIdx, sink: &mut dyn FnMut(Published)) {
        if let Some(Some(nd)) = self.nodes.get_mut(slot(d))
            && let Some(old) = nd.published.clone()
        {
            let mut e = (*old).clone();
            e.stale = e.stale.with(Group::Name).with(Group::Description);
            if e != *old {
                e.sem_rev = old.sem_rev.wrapping_add(1);
                let arc = Arc::new(e);
                nd.published = Some(Arc::clone(&arc));
                sink(Published {
                    node: d,
                    entry: Some(arc),
                });
            }
        }
        if self.in_background.insert(d) {
            self.background.push_back(d);
        }
    }

    // ---- from-scratch check ------------------------------------------------------------------------

    /// FUSE from scratch over the current registers, tree and side table, by an independent code
    /// path (full binding pass, no caches, no dirty set). Entries carry `sem_rev` 0.
    pub fn full_check(&self) -> Vec<Option<SemEntry>> {
        self.full_eval().1
    }

    /// The from-scratch S0 and entry of every node.
    fn full_eval(&self) -> (Vec<Option<S0Node>>, Vec<Option<SemEntry>>) {
        let n = self.nodes.len();
        let live = Live {
            nodes: &self.nodes,
            dom: &self.dom,
            exact: &self.exact,
        };
        let bound = self.side.bind(&live);
        let vis: Vec<u32> = self
            .nodes
            .iter()
            .map(|n| n.as_ref().map_or(0, |n| n.vis))
            .collect();
        let doc: Vec<u64> = self
            .nodes
            .iter()
            .map(|n| n.as_ref().map_or(0, Node::doc_rev))
            .collect();
        let vis_fn = |x: NodeIdx| vis.get(slot(x)).copied().unwrap_or(0);
        let doc_fn = |x: NodeIdx| doc.get(slot(x)).copied().unwrap_or(0);
        let ctx = FuseCtx {
            epoch: &self.snap,
            clock_ms: self.clock,
            vis_rev: &vis_fn,
            doc_rev: &doc_fn,
        };
        let ranked: Vec<Ranked> = (0..n)
            .map(|i| match u32::try_from(i) {
                Ok(i) => bound.node(NodeIdx(i), &ctx),
                Err(_) => Ranked::default(),
            })
            .collect();
        let s0s: Vec<Option<S0Node>> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, nd)| {
                let nd = nd.as_ref()?;
                s0_with(
                    &self.profile,
                    &nd.regs,
                    ranked.get(i)?.view(),
                    &self.quirks,
                    &mut NoTrace,
                )
            })
            .collect();
        let mut order: Vec<usize> = (0..n)
            .filter(|i| self.nodes.get(*i).is_some_and(Option::is_some))
            .collect();
        order.sort_by_key(|i| {
            (
                Reverse(
                    self.nodes
                        .get(*i)
                        .and_then(Option::as_ref)
                        .map_or(0, |n| n.depth),
                ),
                *i,
            )
        });
        let mut out: Vec<Option<SemEntry>> = (0..n).map(|_| None).collect();
        let mut scratch = Scratch::default();
        let cap = self.profile.limits.max_children.max(1);
        for i in order {
            let (Some(Some(nd)), Some(Some(s0))) = (self.nodes.get(i), s0s.get(i)) else {
                continue;
            };
            let role = nd
                .regs
                .user_value(Group::Role)
                .and_then(|(_, w)| w.slot().role())
                .or_else(|| s0.role())
                .unwrap_or(self.profile.default_role);
            let depth = if self.profile.rules.content_named.has(role) {
                self.profile.rules.content_depth
            } else {
                0
            };
            let (vs, more) = views(
                &nd.children,
                cap,
                depth,
                |c| {
                    self.nodes
                        .get(slot(c))
                        .and_then(Option::as_ref)
                        .map_or(&[][..], |cn| cn.children.as_slice())
                },
                |c| {
                    (
                        s0s.get(slot(c)).and_then(Option::as_ref).map(S0View::full),
                        out.get(slot(c)).and_then(Option::as_ref),
                    )
                },
            );
            let parent = nd
                .parent
                .and_then(|p| s0s.get(slot(p)))
                .and_then(Option::as_ref)
                .map(S0View::full);
            let nb = Neighbours {
                parent,
                children: &vs,
                more_children: more,
            };
            let fused = fuse_node_with(
                &self.profile,
                &nd.regs,
                s0,
                &nb,
                ranked.get(i).map(Ranked::view).unwrap_or_default(),
                &self.patches,
                &mut scratch,
                &mut NoTrace,
            );
            drop(vs);
            out[i] = Some(fused.entry);
        }
        (s0s, out)
    }

    /// F7: compare the published entries with [`Ivm::full_check`] (ignoring `sem_rev`). Returns
    /// the nodes that differ. Only meaningful once `pending() == 0`.
    pub fn verify(&self) -> Result<(), Vec<NodeIdx>> {
        let (full_s0, full) = self.full_eval();
        let mut bad = Vec::new();
        // The stored S0 (published entry + delta) must give back the real S0 over the groups
        // selectors read.
        let reads = self.patches.reads();
        let mask = GroupMask(reads.parent.0 | reads.children.0);
        for (i, s0) in full_s0.iter().enumerate() {
            let Ok(idx) = u32::try_from(i) else { continue };
            let view = s0_view_of(&self.nodes, NodeIdx(idx));
            let ok = match (s0, view) {
                (None, None) => true,
                (Some(s0), Some(v)) => Group::ALL
                    .iter()
                    .filter(|g| mask.has(**g))
                    .all(|g| &v.slot(*g) == s0.slot(*g)),
                _ => false,
            };
            if !ok {
                bad.push(NodeIdx(idx));
            }
        }
        for (i, f) in full.iter().enumerate() {
            let have = self
                .nodes
                .get(i)
                .and_then(Option::as_ref)
                .and_then(|n| n.published.as_deref())
                .map(|e| {
                    let mut e = e.clone();
                    e.sem_rev = 0;
                    e
                });
            if &have != f
                && let Ok(i) = u32::try_from(i)
            {
                bad.push(NodeIdx(i));
            }
        }
        if bad.is_empty() { Ok(()) } else { Err(bad) }
    }
}
