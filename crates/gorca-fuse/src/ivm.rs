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
use std::time::Instant;

use gorca_model::fact::{BindGen, SourceId};
use gorca_model::sem::{BindQuality, Group, GroupMask, SemEntry};

use crate::epoch::{Epochs, silences};
use crate::explain::NoTrace;
use crate::profile::Profile;
use crate::register::{NodeRegs, PrimarySlot, UserOp};
use crate::side::{
    AnchorId, AnchorIndex, EpochSnapshot, FactIn, FuseCtx, NodeIdx, Ranked, Resolved, SideError,
    SideTable, SourceInfo, SourceState,
};
use crate::strata::{
    ChildView, Neighbours, PatchSource, Quirks, S0Node, Scratch, fuse_node_with, s0_with,
};
use crate::value::{Seq, Write};

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

/// The result of applying a batch of inputs.
#[derive(Debug, Default)]
pub struct ApplyReport {
    pub applied: usize,
    pub errors: Vec<IvmError>,
}

type DirtyKey = (u8, Reverse<u32>, u32);

#[derive(Debug)]
struct Node {
    parent: Option<NodeIdx>,
    children: Vec<NodeIdx>,
    /// Index in the parent's children list.
    pos: usize,
    depth: u32,
    regs: NodeRegs,
    /// Life seq of the highest-priority source that has an opinion, when alive.
    alive: Option<Seq>,
    attr: Option<(Seq, Option<Arc<str>>)>,
    vis: u32,
    doc_rev: u64,
    s0: Option<S0Node>,
    published: Option<Arc<SemEntry>>,
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
    anchor_nodes: HashMap<NodeIdx, BTreeSet<AnchorId>>,
    index_dirty: bool,
    dirty: BTreeSet<DirtyKey>,
    dirty_of: HashMap<NodeIdx, DirtyKey>,
    focus: HashSet<NodeIdx>,
    visible: HashSet<NodeIdx>,
    relations: Relations,
    background: VecDeque<NodeIdx>,
    in_background: HashSet<NodeIdx>,
    scratch: Scratch,
}

fn slot(n: NodeIdx) -> usize {
    n.0 as usize
}

fn intersects(a: GroupMask, b: GroupMask) -> bool {
    a.0 & b.0 != 0
}

fn bind_gen(seq: Seq) -> BindGen {
    BindGen(u32::try_from(seq).unwrap_or(u32::MAX))
}

fn node_of(nodes: &[Option<Node>], n: NodeIdx) -> Option<&Node> {
    nodes.get(slot(n)).and_then(Option::as_ref)
}

fn by_node(nodes: &[Option<Node>], n: NodeIdx, quality: BindQuality) -> Resolved {
    match node_of(nodes, n).and_then(|nd| nd.alive) {
        Some(seq) => Resolved {
            node: Some(n),
            quality,
            generation: bind_gen(seq),
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
                .and_then(|nd| nd.attr.as_ref())
                .map_or(0, |a| a.0);
            Resolved {
                node: Some(first),
                quality: if *count == 1 && holders.len() == 1 {
                    BindQuality::Unique
                } else {
                    BindQuality::Heuristic
                },
                generation: bind_gen(seq),
            }
        }
        // Region, structural and content anchors need geometry or hashing indices (M24).
        AnchorId::Content(_) | AnchorId::Region { .. } | AnchorId::Structural { .. } => {
            Resolved::UNBOUND
        }
    }
}

/// The views of the first `cap` children, from a getter of their current values.
fn views<'a>(
    children: &[NodeIdx],
    cap: usize,
    get: impl Fn(NodeIdx) -> (Option<&'a S0Node>, Option<&'a SemEntry>),
) -> (Vec<ChildView<'a>>, bool) {
    let out = children
        .iter()
        .take(cap)
        .map(|c| {
            let (s0, fused) = get(*c);
            ChildView { id: c.0, s0, fused }
        })
        .collect();
    (out, children.len() > cap)
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
            anchor_nodes: HashMap::new(),
            index_dirty: false,
            dirty: BTreeSet::new(),
            dirty_of: HashMap::new(),
            focus: HashSet::new(),
            visible: HashSet::new(),
            relations: Relations::default(),
            background: VecDeque::new(),
            in_background: HashSet::new(),
            scratch: Scratch::default(),
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

    /// The published entry of `node`.
    pub fn entry(&self, node: NodeIdx) -> Option<&Arc<SemEntry>> {
        node_of(&self.nodes, node).and_then(|n| n.published.as_ref())
    }

    /// Dirty nodes plus background dependents not yet re-fused.
    pub fn pending(&self) -> usize {
        self.dirty.len() + self.background.len()
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
                (pn.depth + 1, pn.children.len())
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
            attr: None,
            vis: 0,
            doc_rev: 0,
            s0: None,
            published: None,
        });
        if let Some(p) = parent
            && let Some(Some(pn)) = self.nodes.get_mut(slot(p))
        {
            pn.children.push(node);
        }
        self.mark_dirty(node);
        if let Some(p) = parent {
            self.mark_dirty(p);
        }
        Ok(())
    }

    /// Remove `node` and its whole subtree. Returns the removals to publish.
    pub fn remove_subtree(&mut self, node: NodeIdx) -> Result<Vec<Published>, IvmError> {
        let parent = node_of(&self.nodes, node)
            .ok_or(IvmError::UnknownNode)?
            .parent;
        let mut stack = vec![node];
        let mut removed = Vec::new();
        while let Some(n) = stack.pop() {
            if let Some(Some(nd)) = self.nodes.get_mut(slot(n)) {
                stack.extend(nd.children.iter().copied());
                let published = nd.published.take();
                if let Some(old) = self.dirty_of.remove(&n) {
                    self.dirty.remove(&old);
                }
                if published.is_some() {
                    removed.push(Published {
                        node: n,
                        entry: None,
                    });
                }
                self.nodes[slot(n)] = None;
                self.relations.forget(n);
                self.focus.remove(&n);
                self.visible.remove(&n);
                self.anchor_nodes.remove(&n);
            }
        }
        if let Some(p) = parent
            && let Some(Some(pn)) = self.nodes.get_mut(slot(p))
        {
            pn.children.retain(|c| *c != node);
            for (i, c) in pn.children.clone().into_iter().enumerate() {
                if let Some(Some(cn)) = self.nodes.get_mut(slot(c)) {
                    cn.pos = i;
                }
            }
            self.mark_dirty(p);
        }
        // Dom holders and exact keys of removed nodes disappear; anchors re-resolve.
        self.dom
            .values_mut()
            .for_each(|s| s.retain(|n| self.nodes.get(slot(*n)).is_some_and(Option::is_some)));
        self.exact
            .retain(|_, n| self.nodes.get(slot(*n)).is_some_and(Option::is_some));
        self.index_dirty = true;
        self.reindex();
        Ok(removed)
    }

    /// Bind an exact key (for example an AccessKit node id) to a node.
    pub fn bind_exact(&mut self, space: u16, id: u128, node: NodeIdx) {
        self.exact.insert((space, id), node);
        self.index_dirty = true;
        self.reindex();
    }

    /// Declare which nodes' texts `source` reads (describedby/labelledby targets).
    pub fn set_relations(&mut self, source: NodeIdx, targets: Vec<NodeIdx>) {
        self.relations.set(source, targets);
    }

    // ---- priorities ---------------------------------------------------------------------------

    /// The focus chain (focused node, its ancestors, the active descendant): fused first.
    pub fn set_focus_chain(&mut self, chain: &[NodeIdx]) {
        self.focus = chain.iter().copied().collect();
        self.rekey();
    }

    /// The visible window: fused right after the focus chain.
    pub fn set_visible(&mut self, window: &[NodeIdx]) {
        self.visible = window.iter().copied().collect();
        self.rekey();
    }

    fn key_of(&self, n: NodeIdx) -> Option<DirtyKey> {
        let nd = node_of(&self.nodes, n)?;
        let class = u8::from(!self.is_priority(n));
        Some((class, Reverse(nd.depth), n.0))
    }

    /// Whether `p` derives its name from its children (its effective role is named from content).
    fn reads_children(&self, p: NodeIdx) -> bool {
        node_of(&self.nodes, p)
            .and_then(|n| n.published.as_deref())
            .is_some_and(|e| self.profile.rules.content_named.has(e.sem.role))
    }

    /// True for the focus chain, the visible window and every node that feeds one of them through
    /// the content-name edge (a descendant reached through first-`max_children` positions): a
    /// priority node is complete only when the nodes it reads are fused too (FS-7).
    fn is_priority(&self, n: NodeIdx) -> bool {
        let cap = self.profile.limits.max_children.max(1);
        let mut cur = n;
        loop {
            if self.focus.contains(&cur) || self.visible.contains(&cur) {
                return true;
            }
            let Some(nd) = node_of(&self.nodes, cur) else {
                return false;
            };
            match nd.parent {
                Some(p) if nd.pos < cap && self.reads_children(p) => cur = p,
                _ => return false,
            }
        }
    }

    fn rekey(&mut self) {
        let nodes: Vec<NodeIdx> = self.dirty_of.keys().copied().collect();
        self.dirty.clear();
        self.dirty_of.clear();
        for n in nodes {
            self.mark_dirty(n);
        }
    }

    fn mark_dirty(&mut self, n: NodeIdx) {
        let Some(key) = self.key_of(n) else { return };
        if let Some(old) = self.dirty_of.insert(n, key) {
            self.dirty.remove(&old);
        }
        self.dirty.insert(key);
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
        if self.index_dirty {
            self.reindex();
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
                        self.index_dirty = true;
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
                if n.attr.as_ref().is_some_and(|cur| key(&new) <= key(cur)) {
                    return Ok(());
                }
                let old = n.attr.replace(new.clone());
                if let Some((_, Some(old_id))) = old
                    && let Some(set) = self.dom.get_mut(&old_id)
                {
                    set.remove(&node);
                    if set.is_empty() {
                        self.dom.remove(&old_id);
                    }
                }
                if let Some(new_id) = new.1 {
                    self.dom.entry(new_id).or_default().insert(node);
                }
                self.index_dirty = true;
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
                if rev > n.doc_rev {
                    n.doc_rev = rev;
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
        let new = resolve_in(&self.nodes, &self.dom, &self.exact, a);
        let old = self.resolved.insert(a.clone(), new);
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

    /// A lifecycle or DOM-id change can move any anchor: re-resolve every cached one (the cost is
    /// the number of anchors with registers, paid only on those rare inputs).
    fn reindex(&mut self) {
        self.index_dirty = false;
        let anchors: Vec<AnchorId> = self.resolved.keys().cloned().collect();
        for a in anchors {
            self.rebind_anchor(&a);
        }
    }

    // ---- sources and epochs ---------------------------------------------------------------------

    fn refresh_snapshot(&mut self) {
        self.snap = self.epochs.snapshot(self.side.source_ids());
    }

    /// Register a non-primary source.
    pub fn set_source(&mut self, info: SourceInfo) -> Result<(), SideError> {
        self.side.set_source(info)?;
        self.refresh_snapshot();
        Ok(())
    }

    fn forget_anchors(&mut self, anchors: &[AnchorId]) -> Vec<NodeIdx> {
        let mut touched = Vec::new();
        for a in anchors {
            if !self.side.has_anchor(a)
                && let Some(r) = self.resolved.remove(a)
                && let Some(n) = r.node
            {
                if let Some(set) = self.anchor_nodes.get_mut(&n) {
                    set.remove(a);
                }
                touched.push(n);
            } else if let Some(n) = self.resolved.get(a).and_then(|r| r.node) {
                touched.push(n);
            }
        }
        touched
    }

    /// A source disconnects for good: its registration and every register it owns are dropped,
    /// and the focus chain and visible window are re-fused before this returns.
    pub fn release_source(&mut self, id: SourceId) -> Vec<Published> {
        let anchors = self.side.release_source(id);
        self.epochs.forget(id);
        self.refresh_snapshot();
        let touched = self.forget_anchors(&anchors);
        for n in touched {
            self.mark_dirty(n);
        }
        self.drain_priority().published
    }

    /// A source reconnects under its stable name: the old slot is released (registration and
    /// registers) **before** the new registration, which would otherwise be rejected as a
    /// duplicate name. On error nothing changed.
    pub fn reconnect(
        &mut self,
        old: SourceId,
        info: SourceInfo,
    ) -> Result<Vec<Published>, SideError> {
        let anchors = self.side.reconnect(old, info)?;
        self.epochs.forget(old);
        self.refresh_snapshot();
        let touched = self.forget_anchors(&anchors);
        for n in touched {
            self.mark_dirty(n);
        }
        Ok(self.drain_priority().published)
    }

    /// A per-source epoch message (mask, pause, revoke, detach, throttle, resume). A silencing
    /// state re-fuses the focus chain and the visible window **synchronously** (FS-7); the rest
    /// of the affected nodes stay dirty for the next `run`. Returns what was published.
    pub fn set_source_state(
        &mut self,
        src: SourceId,
        n: u64,
        state: SourceState,
    ) -> Vec<Published> {
        if !self.epochs.join_source(src, n, state) {
            return Vec::new();
        }
        self.refresh_snapshot();
        for a in self.side.anchors_of_source(src) {
            self.dirty_anchor(&a);
        }
        if silences(state) {
            self.drain_priority().published
        } else {
            Vec::new()
        }
    }

    /// The global epoch (patch set changed, host-wide mask): every node is re-fused, focus chain
    /// and visible window synchronously, the rest in budgeted chunks by `run`.
    pub fn bump_global(&mut self) -> Vec<Published> {
        self.epochs.bump_global();
        let all: Vec<NodeIdx> = (0..self.nodes.len())
            .filter_map(|i| u32::try_from(i).ok().map(NodeIdx))
            .filter(|n| node_of(&self.nodes, *n).is_some())
            .collect();
        for n in all {
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

    /// Re-fuse the focus chain and visible window (class 0) until none of them is dirty.
    fn drain_priority(&mut self) -> RunReport {
        let mut rep = RunReport::default();
        loop {
            let before = rep.fuse_calls;
            while let Some(&key) = self.dirty.first() {
                if key.0 != 0 {
                    break;
                }
                self.dirty.remove(&key);
                self.dirty_of.remove(&NodeIdx(key.2));
                self.process(NodeIdx(key.2), &mut rep);
            }
            // Fusing changes which nodes feed the priority nodes: refresh the keys and go on while
            // a priority node is still dirty (keys are scheduling hints, never results).
            if rep.fuse_calls == before {
                break;
            }
            self.rekey();
            if self.dirty.first().is_none_or(|k| k.0 != 0) {
                break;
            }
        }
        rep.pending = self.pending();
        rep
    }

    /// Re-fuse dirty nodes: focus chain and visible window first, then deepest first, until the
    /// budget is spent. Background dependents are re-fused in chunks once the dirty set is empty.
    pub fn run(&mut self, budget: Budget) -> RunReport {
        let mut rep = RunReport::default();
        loop {
            if budget.exhausted(rep.fuse_calls) {
                break;
            }
            let Some(&key) = self.dirty.first() else {
                if self.background.is_empty() {
                    break;
                }
                for _ in 0..BACKGROUND_CHUNK {
                    let Some(n) = self.background.pop_front() else {
                        break;
                    };
                    self.in_background.remove(&n);
                    self.mark_dirty(n);
                }
                continue;
            };
            self.dirty.remove(&key);
            self.dirty_of.remove(&NodeIdx(key.2));
            self.process(NodeIdx(key.2), &mut rep);
        }
        rep.pending = self.pending();
        #[cfg(feature = "fuse_full_check")]
        if rep.pending == 0 {
            rep.check_mismatches = Some(self.verify().err().map_or(0, |v| v.len()));
        }
        rep
    }

    fn process(&mut self, x: NodeIdx, rep: &mut RunReport) {
        let (new_s0, fused) = {
            let Some(node) = node_of(&self.nodes, x) else {
                return;
            };
            let anchors: Vec<AnchorId> = self
                .anchor_nodes
                .get(&x)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            let resolved = &self.resolved;
            let resolve = |a: &AnchorId| resolved.get(a).copied().unwrap_or(Resolved::UNBOUND);
            let (vis, doc) = (node.vis, node.doc_rev);
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
            let new_s0 = s0_with(
                &self.profile,
                &node.regs,
                ranked.view(),
                &self.quirks,
                &mut NoTrace,
            );
            let nodes = &self.nodes;
            let fused = new_s0.as_ref().map(|s0| {
                let cap = self.profile.limits.max_children.max(1);
                let (vs, more) = views(&node.children, cap, |c| {
                    node_of(nodes, c)
                        .map_or((None, None), |cn| (cn.s0.as_ref(), cn.published.as_deref()))
                });
                let parent_s0 = node
                    .parent
                    .and_then(|p| node_of(nodes, p))
                    .and_then(|pn| pn.s0.as_ref());
                let nb = Neighbours {
                    parent: parent_s0,
                    children: &vs,
                    more_children: more,
                };
                fuse_node_with(
                    &self.profile,
                    &node.regs,
                    s0,
                    &nb,
                    ranked.view(),
                    &self.patches,
                    &mut self.scratch,
                    &mut NoTrace,
                )
            });
            (new_s0, fused)
        };

        // Phase B: store, publish with early cutoff, propagate.
        let reads = self.patches.reads();
        let max_children = self.profile.limits.max_children.max(1);
        let Some(Some(node)) = self.nodes.get_mut(slot(x)) else {
            return;
        };
        let s0_changed = S0Node::changed_groups(node.s0.as_ref(), new_s0.as_ref());
        node.s0 = new_s0;
        let (parent, pos, children) = (node.parent, node.pos, node.children.clone());
        let old_pub = node.published.clone();
        let new_entry = fused.map(|f| f.entry);
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
            rep.published.push(Published {
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
            for c in children {
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
            && pos < max_children
        {
            self.mark_dirty(p);
        }
        // Relation edge: dependents read this node's text.
        if text_proj(old_pub.as_deref()) != text_proj(published_entry.as_deref()) {
            let deps = self.relations.dependents(x);
            for (i, d) in deps.into_iter().enumerate() {
                if i < RELATION_FANOUT_CAP {
                    self.mark_dirty(d);
                } else {
                    self.defer_stale(d, rep);
                }
            }
        }
    }

    /// Beyond the fan-out cap: publish the dependent with its text groups marked stale now and
    /// re-fuse it in a background chunk.
    fn defer_stale(&mut self, d: NodeIdx, rep: &mut RunReport) {
        if let Some(Some(nd)) = self.nodes.get_mut(slot(d))
            && let Some(old) = nd.published.clone()
        {
            let mut e = (*old).clone();
            e.stale = e.stale.with(Group::Name).with(Group::Description);
            if e != *old {
                e.sem_rev = old.sem_rev.wrapping_add(1);
                let arc = Arc::new(e);
                nd.published = Some(Arc::clone(&arc));
                rep.published.push(Published {
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
            .map(|n| n.as_ref().map_or(0, |n| n.doc_rev))
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
            let (vs, more) = views(&nd.children, cap, |c| {
                (
                    s0s.get(slot(c)).and_then(Option::as_ref),
                    out.get(slot(c)).and_then(Option::as_ref),
                )
            });
            let parent = nd
                .parent
                .and_then(|p| s0s.get(slot(p)))
                .and_then(Option::as_ref);
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
        out
    }

    /// F7: compare the published entries with [`Ivm::full_check`] (ignoring `sem_rev`). Returns
    /// the nodes that differ. Only meaningful once `pending() == 0`.
    pub fn verify(&self) -> Result<(), Vec<NodeIdx>> {
        let full = self.full_check();
        let mut bad = Vec::new();
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
