//! The consumer-side coalescing table.
//!
//! Pending events are stored **in output order** in one `Vec<NodeEvent>` (`events`), with a
//! parallel `meta` vector and a per-node index of positions. `drain()` hands the vector over
//! without copying (SPEC AU-6: the hand-over of 65 536 events is O(1)). Superseded slots become
//! placeholders (`Kind::Dead`) and are removed either by incremental compaction (a few steps per
//! push, SPEC AU-6) or, at the latest, when the batch is handed over.
//!
//! Single-threaded by construction: the owner (`coalesce::Shared`) holds it behind a mutex that
//! the consumer takes to drain and that producers take (with `try_lock`, or for a bounded chunk
//! when the ring is full) to fold records in. All work under that mutex is bounded CPU work that
//! does not depend on consumer speed.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use gorca_model::{AppId, ConnectorId, LocalId, NodeEvent, NodeKey, RawEvent, TextDelta};
use smallvec::SmallVec;

use crate::coalesce::{Drained, Garbage, IngressConfig, IngressStats, SlotBudget};
use crate::dirty::DirtySet;
use crate::hash::FastState;

/// "No slot" marker in the per-node index and in `Meta::aux`.
const NONE: u32 = u32::MAX;

/// Compaction steps (one position each) done per push while a compaction is in progress.
const COMPACT_STEPS: usize = 64;

/// An app holding fewer slots than this may take slots back from a larger holder when the
/// global budget is exhausted (SPEC AU-3). Beyond the flat per-app reserve of the
/// [`SlotBudget`], which never competes for the pool, this is how a small app grows to this size.
pub(crate) const PREEMPT_BELOW: usize = 64;

/// Buffers a preempted table gave up. Freed off the critical section (SPEC AU-6): dropping
/// tens of thousands of events and unmapping their tables costs hundreds of microseconds.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct Shed {
    events: Vec<NodeEvent>,
    meta: Vec<Meta>,
    aux: Vec<SmallVec<[TextDelta; 2]>>,
    nodes: NodeMap,
}

impl Shed {
    /// Worth a trip to the reaper thread (a few cache lines are cheaper to free in place).
    pub(crate) fn is_large(&self) -> bool {
        self.events.capacity() + self.nodes.capacity() > 256
    }
}

/// A table that grew beyond this many node entries is rebuilt on drain instead of keeping its
/// capacity (SPEC AU-5).
const KEEP_NODES: usize = 4096;
/// Spare vectors above this many elements are dropped on drain (SPEC AU-5).
const KEEP_SLOTS: usize = 4096;

/// Keys that belong to a node first, in the order of their column in [`NodeIdx`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Props = 0,
    Children = 1,
    Text = 2,
    Caret = 3,
    Selection = 4,
    Active = 5,
    Window = 6,
    Document = 7,
    Removed = 8,
    /// One slot per app, not in the node index.
    Focus = 9,
    /// Never merged, not in the node index.
    Announcement = 10,
    /// Superseded; holds a placeholder event until compaction.
    Dead = 11,
}

const NODE_KINDS: usize = 9;
/// Columns that are not kept when the app is `Flooded` (`Caret` is decided per node).
const DROPPED_WHEN_FLOODED: [usize; 6] = [
    Kind::Props as usize,
    Kind::Children as usize,
    Kind::Text as usize,
    Kind::Selection as usize,
    Kind::Active as usize,
    Kind::Document as usize,
];

type NodeIdx = [u32; NODE_KINDS];
pub(crate) type NodeMap = HashMap<NodeKey, NodeIdx, FastState>;
const EMPTY_IDX: NodeIdx = [NONE; NODE_KINDS];

#[derive(Copy, Clone, Debug)]
pub(crate) struct Meta {
    kind: Kind,
    /// Index into `aux` of the deltas after the first one (text slots only).
    aux: u32,
}

#[derive(Debug)]
pub(crate) struct Table {
    id: usize,
    budget: Option<Arc<SlotBudget>>,
    /// Pending events in output order; dead entries hold a placeholder.
    events: Vec<NodeEvent>,
    meta: Vec<Meta>,
    /// Second and later deltas of text slots.
    aux: Vec<SmallVec<[TextDelta; 2]>>,
    aux_free: Vec<u32>,
    nodes: NodeMap,
    focus: u32,
    /// Live slots (the quantity `max_keys` and `hard_cap` compare against).
    live: usize,
    dead: usize,
    /// Deltas held in `aux` (charged like slots, SPEC AU-4).
    extra: usize,
    announcements: usize,
    /// Budget units this table holds: non-focus slots + extra deltas + the focus slot.
    held: usize,
    focused: Option<NodeKey>,
    flooded: bool,
    dirty: DirtySet,
    stats: IngressStats,
    /// Incremental compaction cursors: `[0, cw)` compacted, `[cw, cr)` gap, `[cr, len)` pending.
    compacting: bool,
    cw: usize,
    cr: usize,
    /// Dead entries behind `cw` while compacting.
    dead_behind: usize,
    capacity_hint: usize,
    released: bool,
    /// Reserve units this app holds, table and ring together (shared with the ring's credit).
    app_reserve: Arc<AtomicUsize>,
    /// How many of `held` came from the reserve; the rest are pool units.
    res_units: usize,
}

/// Everything `Table::take` hands over; the heavy conversion happens outside the lock.
#[derive(Debug)]
pub(crate) struct Taken {
    events: Vec<NodeEvent>,
    meta: Vec<Meta>,
    aux: Vec<SmallVec<[TextDelta; 2]>>,
    dead: usize,
    dirty: Option<DirtySet>,
    stats: IngressStats,
    /// Old node index to drop after the lock is released.
    garbage: Option<NodeMap>,
}

fn placeholder() -> NodeEvent {
    NodeEvent::SelectionChanged {
        node: NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 0,
            },
            local: LocalId(0),
        },
    }
}

fn classify(ev: &NodeEvent) -> (Kind, Option<NodeKey>) {
    match ev {
        NodeEvent::FocusChanged { .. } => (Kind::Focus, None),
        NodeEvent::ActiveDescendantChanged { container, .. } => (Kind::Active, Some(*container)),
        NodeEvent::PropertiesChanged { node, .. } => (Kind::Props, Some(*node)),
        NodeEvent::ChildrenChanged { node } => (Kind::Children, Some(*node)),
        NodeEvent::TextChanged { node, .. } => (Kind::Text, Some(*node)),
        NodeEvent::CaretMoved { node, .. } => (Kind::Caret, Some(*node)),
        NodeEvent::SelectionChanged { node } => (Kind::Selection, Some(*node)),
        NodeEvent::Announcement { .. } => (Kind::Announcement, None),
        NodeEvent::WindowEvent { node, .. } => (Kind::Window, Some(*node)),
        NodeEvent::DocumentEvent { node, .. } => (Kind::Document, Some(*node)),
        NodeEvent::NodeRemoved { node } => (Kind::Removed, Some(*node)),
    }
}

fn to_u32(i: usize) -> u32 {
    // Positions are bounded by about 3 * hard_cap; saturating keeps the marker unreachable.
    u32::try_from(i).unwrap_or(NONE - 1)
}

impl Table {
    pub(crate) fn new(
        id: usize,
        budget: Option<Arc<SlotBudget>>,
        app_reserve: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            id,
            budget,
            events: Vec::new(),
            meta: Vec::new(),
            aux: Vec::new(),
            aux_free: Vec::new(),
            nodes: HashMap::with_hasher(FastState::default()),
            focus: NONE,
            live: 0,
            dead: 0,
            extra: 0,
            announcements: 0,
            held: 0,
            focused: None,
            flooded: false,
            dirty: DirtySet::default(),
            stats: IngressStats::default(),
            compacting: false,
            cw: 0,
            cr: 0,
            dead_behind: 0,
            capacity_hint: 0,
            released: false,
            app_reserve,
            res_units: 0,
        }
    }

    pub(crate) fn pushed_total(&self) -> u64 {
        self.stats.pushed
    }

    pub(crate) fn live(&self) -> usize {
        self.live
    }

    /// Budget units held (for the shared registry's view of the largest holder).
    pub(crate) fn held(&self) -> usize {
        self.held
    }

    fn kept_when_flooded(&self, kind: Kind, node: Option<NodeKey>) -> bool {
        match kind {
            Kind::Focus | Kind::Removed | Kind::Window | Kind::Announcement => true,
            Kind::Caret => node.is_some() && self.focused == node,
            Kind::Props
            | Kind::Children
            | Kind::Text
            | Kind::Selection
            | Kind::Active
            | Kind::Document
            | Kind::Dead => false,
        }
    }

    fn present(&self, kind: Kind, node: Option<NodeKey>) -> bool {
        match kind {
            Kind::Focus => self.focus != NONE,
            Kind::Announcement | Kind::Dead => false,
            Kind::Props
            | Kind::Children
            | Kind::Text
            | Kind::Caret
            | Kind::Selection
            | Kind::Active
            | Kind::Window
            | Kind::Document
            | Kind::Removed => node
                .and_then(|n| self.nodes.get(&n))
                .is_some_and(|e| e[kind as usize] != NONE),
        }
    }

    /// Take one budget unit. The first `R` units of an app (table and ring together) come from
    /// its reserve, outside the pool; above that the pool is tried, and when it is exhausted a
    /// small app takes slots back from a larger holder, of any size above `R` (SPEC AU-3).
    fn acquire(&mut self, cfg: &IngressConfig) -> bool {
        let Some(b) = self.budget.clone() else {
            self.held += 1;
            return true;
        };
        let reserve = cfg.reserved_slots;
        if b.take_reserve(&self.app_reserve, reserve, 1) == 1 {
            self.res_units += 1;
            self.held += 1;
            return true;
        }
        if b.try_charge()
            || (self.held < PREEMPT_BELOW
                && b.preempt(self.id, self.held.max(reserve), 1)
                && b.try_charge())
        {
            self.held += 1;
            return true;
        }
        false
    }

    fn release(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        debug_assert!(self.held >= n, "held counter underflow");
        // Pool units go back first: the app keeps its reserve for as long as it holds anything.
        let pool = n.min(self.held - self.res_units);
        let reserve = n - pool;
        self.res_units -= reserve;
        self.held -= n;
        if let Some(b) = &self.budget {
            b.release_split(pool, reserve, &self.app_reserve);
        }
    }

    /// Rule H outcome for a key that cannot get a slot.
    fn overflow(&mut self, kind: Kind) {
        if !self.flooded {
            self.enter_flooded();
        }
        if kind == Kind::Announcement {
            self.stats.announcements_dropped += 1;
        } else {
            self.dirty.mark_all();
            self.stats.dirtied += 1;
        }
    }

    /// Apply one record. Mirrors `model.rs::SpecModel::push` exactly for a single producer.
    pub(crate) fn push(&mut self, cfg: &IngressConfig, ev: RawEvent) {
        self.stats.pushed += 1;
        if self.compacting {
            self.compact_steps(COMPACT_STEPS);
        }
        let (kind, knode) = classify(&ev);
        if let NodeEvent::FocusChanged { node } = &ev {
            self.focused = Some(*node);
            self.stats.focus_sequence += 1;
        }

        if !self.flooded && self.live >= cfg.max_keys && !self.present(kind, knode) {
            self.enter_flooded();
        }
        if self.flooded && !self.kept_when_flooded(kind, knode) {
            if let Some(n) = ev.node() {
                self.dirty.mark(n);
            } else {
                self.dirty.mark_all();
            }
            self.stats.dirtied += 1;
            return;
        }

        // Rule H: even the lossless classes are bounded. Focus is a single slot.
        if kind != Kind::Focus && self.live >= cfg.hard_cap && !self.present(kind, knode) {
            self.overflow(kind);
            return;
        }

        match kind {
            Kind::Announcement => {
                if self.announcements >= cfg.announcement_cap {
                    self.stats.announcements_dropped += 1;
                    return;
                }
                if !self.acquire(cfg) {
                    self.overflow(kind);
                    return;
                }
                self.announcements += 1;
                self.append(Kind::Announcement, ev);
            }
            Kind::Focus => {
                if self.focus == NONE {
                    // The newest focus is lossless: from the reserve, else the pool; when the
                    // pool is exhausted it takes a slot back from a larger holder (at most
                    // once: a failed preemption is not repeated); only if nobody can give one is
                    // the limit exceeded, by one unit per app.
                    let own = self.held;
                    self.focus = to_u32(self.events.len());
                    self.append(Kind::Focus, ev);
                    self.held += 1;
                    if let Some(b) = &self.budget {
                        if b.take_reserve(&self.app_reserve, cfg.reserved_slots, 1) == 1 {
                            self.res_units += 1;
                        } else {
                            let mut got = false;
                            for _ in 0..32 {
                                if b.try_charge() {
                                    got = true;
                                    break;
                                }
                                if !b.preempt(self.id, own.max(cfg.reserved_slots), 256) {
                                    break;
                                }
                            }
                            if !got {
                                b.force_charge();
                            }
                        }
                    }
                } else {
                    let at = self.focus as usize;
                    if let Some(slot) = self.events.get_mut(at) {
                        *slot = ev;
                    }
                    self.stats.coalesced += 1;
                }
            }
            Kind::Removed => {
                if let Some(n) = knode {
                    // R-1: Removed(n) deletes every pending slot for n.
                    self.supersede_node(n);
                    if self.acquire(cfg) {
                        self.new_node_slot(Kind::Removed, n, ev);
                    } else {
                        self.overflow(kind);
                    }
                }
            }
            Kind::Props
            | Kind::Children
            | Kind::Text
            | Kind::Caret
            | Kind::Selection
            | Kind::Active
            | Kind::Window
            | Kind::Document => {
                if let Some(n) = knode {
                    self.merge_node_event(cfg, kind, n, ev);
                }
            }
            Kind::Dead => {}
        }
    }

    fn append(&mut self, kind: Kind, ev: NodeEvent) {
        self.events.push(ev);
        self.meta.push(Meta { kind, aux: NONE });
        self.live += 1;
    }

    fn new_node_slot(&mut self, kind: Kind, n: NodeKey, ev: NodeEvent) {
        let at = to_u32(self.events.len());
        self.nodes.entry(n).or_insert(EMPTY_IDX)[kind as usize] = at;
        self.append(kind, ev);
    }

    fn merge_node_event(&mut self, cfg: &IngressConfig, kind: Kind, n: NodeKey, ev: NodeEvent) {
        let pos = self.nodes.get(&n).map_or(NONE, |e| e[kind as usize]);
        if pos == NONE {
            if self.acquire(cfg) {
                self.new_node_slot(kind, n, ev);
            } else {
                self.overflow(kind);
            }
            return;
        }
        self.stats.coalesced += 1;
        let p = pos as usize;
        match (kind, ev) {
            (Kind::Props, NodeEvent::PropertiesChanged { mask, .. }) => {
                if let Some(NodeEvent::PropertiesChanged { mask: m, .. }) = self.events.get_mut(p) {
                    *m |= mask;
                }
            }
            (Kind::Text, NodeEvent::TextChanged { delta, .. }) => {
                self.merge_text(cfg, p, n, delta);
            }
            (_, ev) => {
                if let Some(slot) = self.events.get_mut(p) {
                    *slot = ev;
                }
            }
        }
    }

    /// T-1 / R-5: a `Delete` followed by an `Insert` becomes `Replaced`; more than `max` deltas
    /// become `Replaced`; `Replaced` absorbs everything after it. The deltas of a node are the
    /// one in `events[p]` followed by those in `aux`.
    fn merge_text(&mut self, cfg: &IngressConfig, p: usize, n: NodeKey, new: TextDelta) {
        let aux_idx = self.meta.get(p).map_or(NONE, |m| m.aux);
        let count = 1 + if aux_idx == NONE {
            0
        } else {
            self.aux.get(aux_idx as usize).map_or(0, SmallVec::len)
        };
        let last = if aux_idx == NONE {
            match self.events.get(p) {
                Some(NodeEvent::TextChanged { delta, .. }) => Some(delta),
                _ => None,
            }
        } else {
            self.aux.get(aux_idx as usize).and_then(|a| a.last())
        };
        let mut collapse = match (last, &new) {
            (Some(TextDelta::Replaced { .. }), _) => return,
            (Some(TextDelta::Delete { .. }), TextDelta::Insert { .. }) => true,
            _ => count + 1 > cfg.max_text_deltas,
        };
        if !collapse {
            // The new delta is a queued event: it is charged and bounded (AU-4).
            if self.extra >= cfg.hard_cap || !self.acquire(cfg) {
                collapse = true;
            } else {
                let idx = if aux_idx == NONE {
                    let i = self.aux_free.pop().unwrap_or_else(|| {
                        self.aux.push(SmallVec::new());
                        to_u32(self.aux.len() - 1)
                    });
                    if let Some(m) = self.meta.get_mut(p) {
                        m.aux = i;
                    }
                    i
                } else {
                    aux_idx
                };
                if let Some(a) = self.aux.get_mut(idx as usize) {
                    a.push(new);
                    self.extra += 1;
                }
                return;
            }
        }
        if collapse {
            let text = match new {
                TextDelta::Insert { text, .. } | TextDelta::Replaced { text } => text,
                TextDelta::Delete { .. } => None,
            };
            self.free_aux(p);
            if let Some(slot) = self.events.get_mut(p) {
                *slot = NodeEvent::TextChanged {
                    node: n,
                    delta: TextDelta::Replaced { text },
                };
            }
        }
    }

    /// Drop the extra deltas of the text slot at `p` and return their charge.
    fn free_aux(&mut self, p: usize) {
        let Some(m) = self.meta.get_mut(p) else {
            return;
        };
        if m.aux == NONE {
            return;
        }
        let i = m.aux;
        m.aux = NONE;
        if let Some(a) = self.aux.get_mut(i as usize) {
            let n = a.len();
            a.clear();
            self.extra -= n;
            self.release(n);
        }
        self.aux_free.push(i);
    }

    /// Turn the slot at `pos` into a tombstone. The caller maintains the node index.
    fn kill(&mut self, pos: usize) {
        let Some(kind) = self.meta.get(pos).map(|m| m.kind) else {
            return;
        };
        if kind == Kind::Dead {
            return;
        }
        self.free_aux(pos);
        if kind == Kind::Focus {
            self.focus = NONE;
        } else if kind == Kind::Announcement {
            self.announcements = self.announcements.saturating_sub(1);
        }
        self.release(1);
        if let Some(slot) = self.events.get_mut(pos) {
            *slot = placeholder();
        }
        if let Some(m) = self.meta.get_mut(pos) {
            m.kind = Kind::Dead;
        }
        debug_assert!(self.live > 0, "live counter underflow");
        self.live -= 1;
        self.dead += 1;
        if self.compacting {
            if pos < self.cw {
                self.dead_behind += 1;
            }
        } else if pos + 1 == self.events.len() {
            while self.meta.last().is_some_and(|m| m.kind == Kind::Dead) {
                self.meta.pop();
                self.events.pop();
                self.dead -= 1;
            }
        }
    }

    fn supersede_node(&mut self, n: NodeKey) {
        let Some(idx) = self.nodes.remove(&n) else {
            return;
        };
        for i in idx {
            if i != NONE {
                self.kill(i as usize);
                self.stats.coalesced += 1;
            }
        }
        self.maybe_start_compaction();
    }

    /// Tombstones are bounded by `2 * live + 256`; they are removed a few positions per push so
    /// no single operation does O(n) work (SPEC AU-6).
    fn maybe_start_compaction(&mut self) {
        if !self.compacting && self.dead > 2 * self.live + 256 {
            self.compacting = true;
            self.cw = 0;
            self.cr = 0;
            self.dead_behind = 0;
        }
        if self.compacting {
            self.compact_steps(COMPACT_STEPS);
        }
    }

    fn compact_steps(&mut self, mut steps: usize) {
        while steps > 0 && self.cr < self.events.len() {
            steps -= 1;
            let r = self.cr;
            let dead = self.meta.get(r).is_none_or(|m| m.kind == Kind::Dead);
            self.cr += 1;
            if dead {
                continue;
            }
            let w = self.cw;
            if w != r {
                self.events.swap(w, r);
                self.meta.swap(w, r);
                self.fix_index(w);
            }
            self.cw += 1;
        }
        if self.cr >= self.events.len() {
            self.events.truncate(self.cw);
            self.meta.truncate(self.cw);
            self.compacting = false;
            self.dead = self.dead_behind;
            self.dead_behind = 0;
            self.cw = 0;
            self.cr = 0;
        }
    }

    /// The slot now at `p` moved there: point the index at it.
    fn fix_index(&mut self, p: usize) {
        let Some(kind) = self.meta.get(p).map(|m| m.kind) else {
            return;
        };
        match kind {
            Kind::Focus => self.focus = to_u32(p),
            Kind::Announcement | Kind::Dead => {}
            Kind::Props
            | Kind::Children
            | Kind::Text
            | Kind::Caret
            | Kind::Selection
            | Kind::Active
            | Kind::Window
            | Kind::Document
            | Kind::Removed => {
                if let Some(n) = self.events.get(p).and_then(NodeEvent::node)
                    && let Some(e) = self.nodes.get_mut(&n)
                {
                    e[kind as usize] = to_u32(p);
                }
            }
        }
    }

    /// R-3: keep Focus, Caret on the focused node, Removed, Window and announcements; every
    /// other pending slot becomes a dirty bit.
    fn enter_flooded(&mut self) {
        self.flooded = true;
        let mut kills: Vec<u32> = Vec::new();
        let focused = self.focused;
        for (key, idx) in &mut self.nodes {
            for col in DROPPED_WHEN_FLOODED {
                if idx[col] != NONE {
                    kills.push(idx[col]);
                    idx[col] = NONE;
                    self.dirty.mark(*key);
                    self.stats.dirtied += 1;
                }
            }
            let caret = Kind::Caret as usize;
            if idx[caret] != NONE && focused != Some(*key) {
                kills.push(idx[caret]);
                idx[caret] = NONE;
                self.dirty.mark(*key);
                self.stats.dirtied += 1;
            }
        }
        self.nodes.retain(|_, idx| idx.iter().any(|&i| i != NONE));
        for p in kills {
            self.kill(p as usize);
        }
        self.maybe_start_compaction();
    }

    /// Give back every slot but the focus one (global budget preemption, SPEC AU-3): the app
    /// is flooded and fully dirty, so its actor resyncs; the newest focus survives.
    pub(crate) fn shed(&mut self, newer: Option<NodeEvent>) -> (usize, Shed) {
        let before = self.held;
        let old_focus = if self.focus == NONE {
            None
        } else {
            self.events
                .get_mut(self.focus as usize)
                .map(|e| std::mem::replace(e, placeholder()))
        };
        self.stats.announcements_dropped += self.announcements as u64;
        let had_focus = old_focus.is_some();
        // A Focus that was still in the ring is newer than the one in the table.
        if let Some(NodeEvent::FocusChanged { node }) = &newer {
            self.focused = Some(*node);
            self.stats.focus_sequence += 1;
        }
        let focus = newer.or(old_focus);
        // The buffers are moved out, not freed: the caller drops them after releasing the lock.
        let garbage = Shed {
            events: std::mem::take(&mut self.events),
            meta: std::mem::take(&mut self.meta),
            aux: std::mem::take(&mut self.aux),
            nodes: std::mem::replace(&mut self.nodes, HashMap::with_hasher(FastState::default())),
        };
        self.aux_free.clear();
        self.focus = NONE;
        self.live = 0;
        self.dead = 0;
        self.extra = 0;
        self.announcements = 0;
        self.compacting = false;
        self.cw = 0;
        self.cr = 0;
        self.dead_behind = 0;
        self.release(self.held - usize::from(had_focus));
        self.flooded = true;
        self.dirty.mark_all();
        self.stats.dirtied += 1;
        if let Some(ev) = focus {
            self.focus = 0;
            self.append(Kind::Focus, ev);
            if !had_focus {
                // A Focus that came from the ring has no unit yet; it is always admitted.
                self.held += 1;
                if let Some(b) = &self.budget {
                    b.force_charge();
                }
            }
        }
        (before.saturating_sub(self.held), garbage)
    }

    /// Hand over the pending events (moved out, not copied) and reset to a fresh drain. Vectors
    /// of a drained table that grew large are not kept (SPEC AU-5).
    pub(crate) fn take(&mut self, spare_meta: Vec<Meta>) -> Taken {
        let mut stats = self.stats;
        stats.flooded = self.flooded;
        let cap = self.capacity_hint.min(KEEP_SLOTS);
        let events = std::mem::replace(&mut self.events, Vec::with_capacity(cap));
        let meta = std::mem::replace(&mut self.meta, spare_meta);
        let aux = std::mem::take(&mut self.aux);
        self.aux_free.clear();
        let dead = self.dead;
        self.capacity_hint = events.len();
        let dirty = self.flooded.then(|| std::mem::take(&mut self.dirty));
        let garbage = if self.nodes.capacity() > KEEP_NODES {
            Some(std::mem::replace(
                &mut self.nodes,
                HashMap::with_hasher(FastState::default()),
            ))
        } else {
            self.nodes.clear();
            None
        };
        self.focus = NONE;
        // Everything but the focus charge (returned below) is released with the events.
        self.release(self.held);
        self.live = 0;
        self.dead = 0;
        self.extra = 0;
        self.announcements = 0;
        self.flooded = false;
        self.compacting = false;
        self.cw = 0;
        self.cr = 0;
        self.dead_behind = 0;
        self.stats = IngressStats::default();
        Taken {
            events,
            meta,
            aux,
            dead,
            dirty,
            stats,
            garbage,
        }
    }

    /// Return every charge and stop accepting work (receiver dropped, SPEC AU-1/AU-2).
    pub(crate) fn teardown(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.events = Vec::new();
        self.meta = Vec::new();
        self.aux = Vec::new();
        self.nodes = HashMap::with_hasher(FastState::default());
        self.dirty = DirtySet::default();
        self.focus = NONE;
        self.live = 0;
        self.dead = 0;
        self.extra = 0;
        self.announcements = 0;
        self.compacting = false;
        self.release(self.held);
    }

    /// The counters the caps compare against must equal the real structure sizes.
    pub(crate) fn check(&self) -> Result<(), String> {
        let alive = self.meta.iter().filter(|m| m.kind != Kind::Dead).count();
        let dead = self.meta.len() - alive;
        if self.events.len() != self.meta.len() {
            return Err(format!(
                "events={} meta={}",
                self.events.len(),
                self.meta.len()
            ));
        }
        if self.live != alive {
            return Err(format!("live={} alive slots={alive}", self.live));
        }
        if !self.compacting && self.dead != dead {
            return Err(format!("dead={} counted={dead}", self.dead));
        }
        let anns = self
            .meta
            .iter()
            .filter(|m| m.kind == Kind::Announcement)
            .count();
        if anns != self.announcements {
            return Err(format!(
                "announcements={} counted={anns}",
                self.announcements
            ));
        }
        let mut foci = 0;
        let mut nodal = 0;
        let mut extra = 0;
        for (i, m) in self.meta.iter().enumerate() {
            if m.aux != NONE {
                extra += self.aux.get(m.aux as usize).map_or(0, SmallVec::len);
            }
            match m.kind {
                Kind::Focus => {
                    foci += 1;
                    if self.focus != to_u32(i) {
                        return Err(format!("focus index {} but slot at {i}", self.focus));
                    }
                }
                Kind::Announcement | Kind::Dead => {}
                k => {
                    nodal += 1;
                    let n = self.events.get(i).and_then(NodeEvent::node);
                    let at = n.and_then(|n| self.nodes.get(&n)).map(|e| e[k as usize]);
                    if at != Some(to_u32(i)) {
                        return Err(format!("node index of {k:?} is {at:?}, slot at {i}"));
                    }
                }
            }
        }
        if extra != self.extra {
            return Err(format!("extra={} counted={extra}", self.extra));
        }
        if foci > 1 || (foci == 0 && self.focus != NONE) {
            return Err(format!("focus slots {foci}, index {}", self.focus));
        }
        let entries: usize = self
            .nodes
            .values()
            .map(|e| e.iter().filter(|&&i| i != NONE).count())
            .sum();
        if entries != nodal {
            return Err(format!("node index entries={entries} nodal slots={nodal}"));
        }
        // Budget units held: every slot plus every extra delta. The focus slot is a slot.
        if self.held != self.live + self.extra {
            return Err(format!(
                "held={} but live={} + extra={}",
                self.held, self.live, self.extra
            ));
        }
        Ok(())
    }
}

impl Taken {
    /// Convert to the batch of events (outside the lock) and give the meta vector back for
    /// reuse. Dead entries are removed and text slots expanded only when there are any.
    pub(crate) fn finish(self) -> (Drained, Vec<Meta>) {
        let Taken {
            mut events,
            mut meta,
            mut aux,
            dead,
            dirty,
            stats,
            garbage,
        } = self;
        let has_aux = aux.iter().any(|a| !a.is_empty());
        if dead != 0 || has_aux {
            let mut out = Vec::with_capacity(events.len() - dead.min(events.len()));
            for (ev, m) in events.drain(..).zip(meta.iter()) {
                if m.kind == Kind::Dead {
                    continue;
                }
                let node = ev.node();
                out.push(ev);
                if m.aux != NONE
                    && let Some(n) = node
                    && let Some(a) = aux.get_mut(m.aux as usize)
                {
                    out.extend(
                        a.drain(..)
                            .map(|delta| NodeEvent::TextChanged { node: n, delta }),
                    );
                }
            }
            events = out;
        }
        meta.clear();
        let mut spare = Vec::new();
        let mut junk = Vec::new();
        if meta.capacity() > KEEP_SLOTS {
            // Freed when the batch is dropped, not on the draining thread's critical path.
            junk = meta;
        } else {
            spare = meta;
        }
        (
            Drained {
                events,
                dirty,
                stats,
                garbage: Garbage {
                    nodes: garbage,
                    meta: junk,
                    aux,
                    shed: None,
                },
            },
            spare,
        )
    }
}
