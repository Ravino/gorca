//! Keyed, order-preserving coalescing table.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gorca_model::{NodeEvent, NodeKey, PropMask, RawEvent, TextDelta};
use gorca_rt::Notify;
use smallvec::SmallVec;

use crate::dirty::DirtySet;

/// ADR-0001 section 6.3. `ActiveDescendant` is an addition: the ADR's list omits it although
/// `NodeEvent::ActiveDescendantChanged` exists; it is last-writer-wins per container.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CoalesceKey {
    /// Last-writer-wins, one slot per app.
    Focus,
    /// OR the `PropMask`; value fetched lazily.
    Props(NodeKey),
    /// Collapse. TBD: ancestor `ChildrenChanged` subsuming descendants (depth-limited check).
    Children(NodeKey),
    /// `TextDelta` merge.
    Text(NodeKey),
    /// Last-writer-wins.
    Caret(NodeKey),
    Selection(NodeKey),
    ActiveDescendant(NodeKey),
    /// Never coalesced, but rate-capped per drain.
    Announcement(u64),
    Window(NodeKey),
    Document(NodeKey),
    Removed(NodeKey),
}

/// Default [`IngressConfig::hard_cap`].
pub const HARD_CAP: usize = 65_536;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IngressConfig {
    /// Distinct keys per app before the app enters `Flooded` (R-3). ADR: 4096.
    pub max_keys: usize,
    /// Announcements kept per drain; extra ones are dropped and counted.
    pub announcement_cap: usize,
    /// More deltas than this on one node collapse to `Replaced` (ADR: 64).
    pub max_text_deltas: usize,
    /// Hard bound on pending slots, lossless classes included (PC-7, SPEC 4.4 rule H). A new
    /// key beyond it sets the dirty set to "everything" instead of queueing.
    pub hard_cap: usize,
}

impl Default for IngressConfig {
    fn default() -> Self {
        Self {
            max_keys: 4096,
            announcement_cap: 16,
            max_text_deltas: 64,
            hard_cap: HARD_CAP,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct IngressStats {
    pub pushed: u64,
    /// Events merged into an existing slot or superseded.
    pub coalesced: u64,
    /// Events turned into a dirty bit because the app was `Flooded`.
    pub dirtied: u64,
    pub announcements_dropped: u64,
    /// Number of focus events folded into the single focus slot (R-1 statistics).
    pub focus_sequence: u64,
    pub flooded: bool,
}

/// What one drain delivers to the app actor.
#[derive(Debug, Default)]
pub struct Drained {
    /// Surviving events, in order of first occurrence (R-2).
    pub events: Vec<RawEvent>,
    /// Present when the app was `Flooded`: nodes to resync lazily.
    pub dirty: Option<DirtySet>,
    pub stats: IngressStats,
}

#[derive(Debug)]
enum Payload {
    Event(NodeEvent),
    Props {
        node: NodeKey,
        mask: PropMask,
    },
    Text {
        node: NodeKey,
        deltas: SmallVec<[TextDelta; 2]>,
    },
}

#[derive(Debug)]
struct Slot {
    key: CoalesceKey,
    payload: Payload,
}

/// Process-wide bound on pending slots across many ingresses (hostile many-app floods).
/// Shared by every ingress created with [`ingress_with_budget`]; each pending slot is charged
/// once and released when it is drained, superseded or turned into a dirty mark.
#[derive(Debug)]
pub struct SlotBudget {
    used: std::sync::atomic::AtomicUsize,
    limit: usize,
}

/// Default global pending-slot budget (about 25 MB of slots).
pub const GLOBAL_SLOT_BUDGET: usize = 262_144;

impl SlotBudget {
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            used: std::sync::atomic::AtomicUsize::new(0),
            limit,
        })
    }

    /// Slots currently charged.
    pub fn used(&self) -> usize {
        self.used.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn full(&self) -> bool {
        self.used() >= self.limit
    }

    fn charge(&self, n: usize) {
        self.used.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    fn release(&self, n: usize) {
        self.used.fetch_sub(n, std::sync::atomic::Ordering::Relaxed);
    }
}

#[derive(Debug, Default)]
struct Inner {
    budget: Option<Arc<SlotBudget>>,
    /// Order of first occurrence; `None` = superseded.
    slots: Vec<Option<Slot>>,
    index: HashMap<CoalesceKey, usize>,
    by_node: HashMap<NodeKey, SmallVec<[usize; 4]>>,
    live: usize,
    announcements: usize,
    focused: Option<NodeKey>,
    flooded: bool,
    dirty: DirtySet,
    stats: IngressStats,
}

#[derive(Debug)]
struct Shared {
    cfg: IngressConfig,
    inner: Mutex<Inner>,
    wake: Notify,
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    // A panic while holding the lock cannot leave the table in a state worse than a stale slot.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Create a connected sender and receiver pair for one app.
pub fn ingress(cfg: IngressConfig) -> (IngressSender, IngressReceiver) {
    ingress_inner(cfg, None)
}

/// Like [`ingress`], charging every pending slot to a budget shared with other ingresses.
pub fn ingress_with_budget(
    cfg: IngressConfig,
    budget: &Arc<SlotBudget>,
) -> (IngressSender, IngressReceiver) {
    ingress_inner(cfg, Some(Arc::clone(budget)))
}

fn ingress_inner(
    cfg: IngressConfig,
    budget: Option<Arc<SlotBudget>>,
) -> (IngressSender, IngressReceiver) {
    let shared = Arc::new(Shared {
        cfg,
        inner: Mutex::new(Inner {
            budget,
            ..Inner::default()
        }),
        wake: Notify::new(),
    });
    (
        IngressSender {
            shared: Arc::clone(&shared),
        },
        IngressReceiver { shared },
    )
}

/// Producer side, held by the drain / connector. Cloneable.
#[derive(Clone, Debug)]
pub struct IngressSender {
    shared: Arc<Shared>,
}

/// Consumer side, held by the app actor.
#[derive(Debug)]
pub struct IngressReceiver {
    shared: Arc<Shared>,
}

impl IngressSender {
    /// O(1) amortised, never awaits (C-1). May coalesce or mark dirty.
    pub fn push(&self, ev: RawEvent) {
        let was_empty = {
            let mut g = lock(&self.shared.inner);
            let was_empty = g.live == 0 && g.dirty.is_empty();
            g.push(&self.shared.cfg, ev);
            // O(n): sampled so debug-build tests stay fast.
            debug_assert!(
                !g.stats.pushed.is_multiple_of(509) || g.check().is_ok(),
                "{:?}",
                g.check()
            );
            was_empty
        };
        if was_empty {
            self.shared.wake.notify_one();
        }
    }
}

impl IngressReceiver {
    /// Take everything pending. Non-blocking. Leaves the ingress empty and out of `Flooded`.
    pub fn drain(&self) -> Drained {
        let mut g = lock(&self.shared.inner);
        let d = g.take();
        debug_assert_eq!(g.check(), Ok(()));
        d
    }

    /// Verify that every cap counter equals the real size of its structure (tests).
    pub fn check_invariants(&self) -> Result<(), String> {
        lock(&self.shared.inner).check()
    }

    /// Resolves when something was pushed into an empty ingress.
    pub async fn notified(&self) {
        self.shared.wake.notified().await;
    }

    pub fn pending(&self) -> usize {
        lock(&self.shared.inner).live
    }
}

impl Inner {
    fn kept_when_flooded(&self, key: CoalesceKey) -> bool {
        match key {
            CoalesceKey::Focus
            | CoalesceKey::Removed(_)
            | CoalesceKey::Window(_)
            | CoalesceKey::Announcement(_) => true,
            CoalesceKey::Caret(n) => self.focused == Some(n),
            CoalesceKey::Props(_)
            | CoalesceKey::Children(_)
            | CoalesceKey::Text(_)
            | CoalesceKey::Selection(_)
            | CoalesceKey::ActiveDescendant(_)
            | CoalesceKey::Document(_) => false,
        }
    }

    fn key_of(&mut self, ev: &NodeEvent) -> CoalesceKey {
        match ev {
            NodeEvent::FocusChanged { .. } => CoalesceKey::Focus,
            NodeEvent::ActiveDescendantChanged { container, .. } => {
                CoalesceKey::ActiveDescendant(*container)
            }
            NodeEvent::PropertiesChanged { node, .. } => CoalesceKey::Props(*node),
            NodeEvent::ChildrenChanged { node } => CoalesceKey::Children(*node),
            NodeEvent::TextChanged { node, .. } => CoalesceKey::Text(*node),
            NodeEvent::CaretMoved { node, .. } => CoalesceKey::Caret(*node),
            NodeEvent::SelectionChanged { node } => CoalesceKey::Selection(*node),
            NodeEvent::Announcement { .. } => {
                let seq = self.stats.pushed;
                CoalesceKey::Announcement(seq)
            }
            NodeEvent::WindowEvent { node, .. } => CoalesceKey::Window(*node),
            NodeEvent::DocumentEvent { node, .. } => CoalesceKey::Document(*node),
            NodeEvent::NodeRemoved { node } => CoalesceKey::Removed(*node),
        }
    }

    fn push(&mut self, cfg: &IngressConfig, ev: NodeEvent) {
        self.stats.pushed += 1;
        let key = self.key_of(&ev);
        if let NodeEvent::FocusChanged { node } = &ev {
            self.focused = Some(*node);
            self.stats.focus_sequence += 1;
        }

        if !self.flooded && self.live >= cfg.max_keys && !self.index.contains_key(&key) {
            self.enter_flooded();
        }
        if self.flooded && !self.kept_when_flooded(key) {
            if let Some(n) = ev.node() {
                self.dirty.mark(n);
            } else {
                self.dirty.mark_all();
            }
            self.stats.dirtied += 1;
            return;
        }

        // Rule H: even the lossless classes are bounded. Focus is a single slot.
        if key != CoalesceKey::Focus
            && (self.live >= cfg.hard_cap || self.budget.as_ref().is_some_and(|b| b.full()))
            && !self.index.contains_key(&key)
        {
            if !self.flooded {
                self.enter_flooded();
            }
            if matches!(key, CoalesceKey::Announcement(_)) {
                self.stats.announcements_dropped += 1;
            } else {
                self.dirty.mark_all();
                self.stats.dirtied += 1;
            }
            return;
        }

        match key {
            CoalesceKey::Announcement(_) => {
                if self.announcements >= cfg.announcement_cap {
                    self.stats.announcements_dropped += 1;
                    return;
                }
                self.announcements += 1;
                self.insert(key, Payload::Event(ev));
            }
            CoalesceKey::Removed(n) => {
                // R-1: Removed(n) deletes every pending slot for n.
                self.supersede_node(n);
                self.insert(key, Payload::Event(ev));
            }
            CoalesceKey::Props(n) => {
                let mask = match &ev {
                    NodeEvent::PropertiesChanged { mask, .. } => *mask,
                    _ => PropMask::empty(),
                };
                if let Some(&i) = self.index.get(&key) {
                    if let Some(Slot {
                        payload: Payload::Props { mask: m, .. },
                        ..
                    }) = self.slots.get_mut(i).and_then(Option::as_mut)
                    {
                        *m |= mask;
                    }
                    self.stats.coalesced += 1;
                } else {
                    self.insert(key, Payload::Props { node: n, mask });
                }
            }
            CoalesceKey::Text(n) => {
                let NodeEvent::TextChanged { delta, .. } = ev else {
                    return;
                };
                if let Some(&i) = self.index.get(&key) {
                    if let Some(Slot {
                        payload: Payload::Text { deltas, .. },
                        ..
                    }) = self.slots.get_mut(i).and_then(Option::as_mut)
                    {
                        merge_text(deltas, delta, cfg.max_text_deltas);
                    }
                    self.stats.coalesced += 1;
                } else {
                    let mut deltas = SmallVec::new();
                    deltas.push(delta);
                    self.insert(key, Payload::Text { node: n, deltas });
                }
            }
            CoalesceKey::Focus
            | CoalesceKey::Children(_)
            | CoalesceKey::Caret(_)
            | CoalesceKey::Selection(_)
            | CoalesceKey::ActiveDescendant(_)
            | CoalesceKey::Window(_)
            | CoalesceKey::Document(_) => {
                // Last-writer-wins in place; order of first occurrence is kept (R-2).
                if let Some(&i) = self.index.get(&key) {
                    if let Some(slot) = self.slots.get_mut(i).and_then(Option::as_mut) {
                        slot.payload = Payload::Event(ev);
                    }
                    self.stats.coalesced += 1;
                } else {
                    self.insert(key, Payload::Event(ev));
                }
            }
        }
    }

    fn insert(&mut self, key: CoalesceKey, payload: Payload) {
        let i = self.slots.len();
        if let Some(n) = node_of(key) {
            self.by_node.entry(n).or_default().push(i);
        }
        self.index.insert(key, i);
        self.slots.push(Some(Slot { key, payload }));
        self.live += 1;
        if let Some(b) = &self.budget {
            b.charge(1);
        }
    }

    fn supersede_node(&mut self, n: NodeKey) {
        let Some(idxs) = self.by_node.remove(&n) else {
            return;
        };
        for i in idxs {
            if let Some(slot) = self.slots.get_mut(i).and_then(Option::take) {
                self.index.remove(&slot.key);
                debug_assert!(self.live > 0, "live counter underflow");
                self.live -= 1;
                if let Some(b) = &self.budget {
                    b.release(1);
                }
                self.stats.coalesced += 1;
            }
        }
        self.compact_if_sparse();
    }

    /// Superseded slots leave tombstones in `slots`; without this a stream of `Removed(n)` for
    /// one node grows the vector without bound while `live` stays at 1.
    fn compact_if_sparse(&mut self) {
        if self.slots.len() <= 2 * self.live + 256 {
            return;
        }
        let old = std::mem::take(&mut self.slots);
        self.index.clear();
        self.by_node.clear();
        for slot in old.into_iter().flatten() {
            let i = self.slots.len();
            if let Some(n) = node_of(slot.key) {
                self.by_node.entry(n).or_default().push(i);
            }
            self.index.insert(slot.key, i);
            self.slots.push(Some(slot));
        }
    }

    /// The counters the caps compare against must equal the real structure sizes.
    fn check(&self) -> Result<(), String> {
        let present = self.slots.iter().flatten().count();
        if self.live != present || self.live != self.index.len() {
            return Err(format!(
                "live={} slots(some)={} index={}",
                self.live,
                present,
                self.index.len()
            ));
        }
        let anns = self
            .slots
            .iter()
            .flatten()
            .filter(|s| matches!(s.key, CoalesceKey::Announcement(_)))
            .count();
        if anns != self.announcements {
            return Err(format!(
                "announcements={} counted={anns}",
                self.announcements
            ));
        }
        let by_node: usize = self.by_node.values().map(SmallVec::len).sum();
        let nodal = self
            .slots
            .iter()
            .flatten()
            .filter(|s| node_of(s.key).is_some())
            .count();
        if by_node != nodal {
            return Err(format!("by_node entries={by_node} nodal slots={nodal}"));
        }
        if let Some(b) = &self.budget
            && b.used() < self.live
        {
            return Err(format!("budget used {} < live {}", b.used(), self.live));
        }
        Ok(())
    }

    /// R-3: keep Focus, Caret on the focused node, Removed, Window and announcements; every
    /// other pending slot becomes a dirty bit.
    fn enter_flooded(&mut self) {
        self.flooded = true;
        let slots = std::mem::take(&mut self.slots);
        self.index.clear();
        self.by_node.clear();
        if let Some(b) = &self.budget {
            b.release(self.live);
        }
        self.live = 0;
        for slot in slots.into_iter().flatten() {
            if self.kept_when_flooded(slot.key) {
                self.insert(slot.key, slot.payload);
            } else {
                if let Some(n) = node_of(slot.key) {
                    self.dirty.mark(n);
                }
                self.stats.dirtied += 1;
            }
        }
    }

    fn take(&mut self) -> Drained {
        let mut stats = self.stats;
        stats.flooded = self.flooded;
        let mut events = Vec::with_capacity(self.live);
        for slot in std::mem::take(&mut self.slots).into_iter().flatten() {
            match slot.payload {
                Payload::Event(e) => events.push(e),
                Payload::Props { node, mask } => {
                    events.push(NodeEvent::PropertiesChanged { node, mask });
                }
                Payload::Text { node, deltas } => {
                    events.extend(
                        deltas
                            .into_iter()
                            .map(|delta| NodeEvent::TextChanged { node, delta }),
                    );
                }
            }
        }
        let dirty = if self.flooded {
            Some(std::mem::take(&mut self.dirty))
        } else {
            None
        };
        self.index.clear();
        self.by_node.clear();
        if let Some(b) = &self.budget {
            b.release(self.live);
        }
        self.live = 0;
        self.announcements = 0;
        self.flooded = false;
        self.stats = IngressStats::default();
        Drained {
            events,
            dirty,
            stats,
        }
    }
}

fn node_of(key: CoalesceKey) -> Option<NodeKey> {
    match key {
        CoalesceKey::Focus | CoalesceKey::Announcement(_) => None,
        CoalesceKey::Props(n)
        | CoalesceKey::Children(n)
        | CoalesceKey::Text(n)
        | CoalesceKey::Caret(n)
        | CoalesceKey::Selection(n)
        | CoalesceKey::ActiveDescendant(n)
        | CoalesceKey::Window(n)
        | CoalesceKey::Document(n)
        | CoalesceKey::Removed(n) => Some(n),
    }
}

/// T-1 / R-5: a `Delete` followed by an `Insert` becomes `Replaced`; more than `max` deltas
/// become `Replaced`; `Replaced` absorbs everything after it.
fn merge_text(deltas: &mut SmallVec<[TextDelta; 2]>, new: TextDelta, max: usize) {
    let collapse = match (deltas.last(), &new) {
        (Some(TextDelta::Replaced { .. }), _) => return,
        (Some(TextDelta::Delete { .. }), TextDelta::Insert { .. }) => true,
        _ => deltas.len() + 1 > max,
    };
    if collapse {
        let text = match new {
            TextDelta::Insert { text, .. } | TextDelta::Replaced { text } => text,
            TextDelta::Delete { .. } => None,
        };
        deltas.clear();
        deltas.push(TextDelta::Replaced { text });
    } else {
        deltas.push(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, CaretOrigin, ConnectorId, LocalId, Politeness};
    use std::sync::Arc;

    fn key(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 0,
            },
            local: LocalId(n),
        }
    }

    fn cfg(max_keys: usize) -> IngressConfig {
        IngressConfig {
            max_keys,
            ..IngressConfig::default()
        }
    }

    fn ann(i: u32) -> NodeEvent {
        NodeEvent::Announcement {
            node: None,
            text: Arc::from(format!("a{i}")),
            politeness: Politeness::Polite,
        }
    }

    fn ins(offset: u32) -> NodeEvent {
        NodeEvent::TextChanged {
            node: key(1),
            delta: TextDelta::Insert {
                offset,
                len: 1,
                text: None,
            },
        }
    }

    fn del(offset: u32) -> NodeEvent {
        NodeEvent::TextChanged {
            node: key(1),
            delta: TextDelta::Delete {
                offset,
                len: 1,
                text: None,
            },
        }
    }

    #[test]
    fn focus_is_last_writer_wins_and_keeps_position() {
        let (tx, rx) = ingress(IngressConfig::default());
        tx.push(NodeEvent::SelectionChanged { node: key(9) });
        tx.push(NodeEvent::FocusChanged { node: key(1) });
        tx.push(NodeEvent::FocusChanged { node: key(2) });
        tx.push(NodeEvent::FocusChanged { node: key(3) });
        let d = rx.drain();
        assert_eq!(d.events.len(), 2);
        assert!(matches!(d.events[0], NodeEvent::SelectionChanged { .. }));
        assert!(matches!(d.events[1], NodeEvent::FocusChanged { node } if node == key(3)));
        assert_eq!(d.stats.focus_sequence, 3);
        assert_eq!(rx.pending(), 0);
    }

    #[test]
    fn property_masks_are_ored() {
        let (tx, rx) = ingress(IngressConfig::default());
        for mask in [PropMask::NAME, PropMask::STATES, PropMask::NAME] {
            tx.push(NodeEvent::PropertiesChanged { node: key(1), mask });
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), 1);
        assert!(matches!(
            d.events[0],
            NodeEvent::PropertiesChanged { mask, .. } if mask == (PropMask::NAME | PropMask::STATES)
        ));
    }

    #[test]
    fn removed_supersedes_every_pending_slot_of_the_node() {
        let (tx, rx) = ingress(IngressConfig::default());
        tx.push(NodeEvent::PropertiesChanged {
            node: key(1),
            mask: PropMask::NAME,
        });
        tx.push(NodeEvent::SelectionChanged { node: key(1) });
        tx.push(NodeEvent::SelectionChanged { node: key(2) });
        tx.push(NodeEvent::NodeRemoved { node: key(1) });
        let d = rx.drain();
        assert_eq!(d.events.len(), 2);
        assert!(matches!(d.events[0], NodeEvent::SelectionChanged { node } if node == key(2)));
        assert!(matches!(d.events[1], NodeEvent::NodeRemoved { node } if node == key(1)));
    }

    #[test]
    fn delete_then_insert_collapses_to_replaced() {
        let (tx, rx) = ingress(IngressConfig::default());
        tx.push(del(0));
        tx.push(ins(0));
        tx.push(ins(5));
        let d = rx.drain();
        assert_eq!(d.events.len(), 1);
        assert!(matches!(
            d.events[0],
            NodeEvent::TextChanged {
                delta: TextDelta::Replaced { .. },
                ..
            }
        ));
    }

    #[test]
    fn more_than_64_deltas_collapse_to_replaced() {
        let (tx, rx) = ingress(IngressConfig::default());
        for i in 0..64 {
            tx.push(ins(i));
        }
        assert_eq!(rx.pending(), 1);
        let d = rx.drain();
        assert_eq!(d.events.len(), 64);
        for i in 0..65 {
            tx.push(ins(i));
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), 1);
    }

    #[test]
    fn announcements_are_never_coalesced_but_capped() {
        let (tx, rx) = ingress(IngressConfig {
            announcement_cap: 3,
            ..IngressConfig::default()
        });
        for i in 0..5 {
            tx.push(ann(i));
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), 3);
        assert_eq!(d.stats.announcements_dropped, 2);
        // The cap is per drain.
        tx.push(ann(9));
        assert_eq!(rx.drain().events.len(), 1);
    }

    #[test]
    fn overload_floods_and_keeps_only_the_essentials() {
        let (tx, rx) = ingress(cfg(4));
        tx.push(NodeEvent::FocusChanged { node: key(100) });
        for n in 0..4 {
            tx.push(NodeEvent::PropertiesChanged {
                node: key(n),
                mask: PropMask::NAME,
            });
        }
        // The fifth distinct key trips the bound.
        tx.push(NodeEvent::PropertiesChanged {
            node: key(50),
            mask: PropMask::NAME,
        });
        tx.push(NodeEvent::CaretMoved {
            node: key(100),
            offset: 3,
            origin: CaretOrigin::Unknown,
        });
        tx.push(NodeEvent::CaretMoved {
            node: key(7),
            offset: 3,
            origin: CaretOrigin::Unknown,
        });
        tx.push(NodeEvent::NodeRemoved { node: key(3) });
        let d = rx.drain();
        assert!(d.stats.flooded);
        let kinds: Vec<_> = d.events.iter().map(std::mem::discriminant).collect();
        assert_eq!(
            kinds.len(),
            3,
            "focus, focused caret, removed: {:?}",
            d.events
        );
        let dirty = d.dirty.unwrap();
        for n in [0, 1, 2, 50, 7] {
            assert!(dirty.contains(key(n)), "node {n} must be dirty");
        }
        // Back to normal after the drain.
        tx.push(NodeEvent::SelectionChanged { node: key(1) });
        let d = rx.drain();
        assert!(!d.stats.flooded && d.dirty.is_none() && d.events.len() == 1);
    }

    #[test]
    fn memory_is_bounded_under_a_storm_of_distinct_keys() {
        let (tx, rx) = ingress(cfg(64));
        for n in 0..100_000u64 {
            tx.push(NodeEvent::ChildrenChanged { node: key(n) });
        }
        assert!(rx.pending() <= 64);
    }

    #[tokio::test]
    async fn receiver_is_woken_by_the_first_push() {
        let (tx, rx) = ingress(IngressConfig::default());
        let waiter = tokio::spawn(async move {
            rx.notified().await;
            rx.drain().events.len()
        });
        tokio::task::yield_now().await;
        tx.push(NodeEvent::FocusChanged { node: key(1) });
        assert_eq!(waiter.await.unwrap(), 1);
    }
}
