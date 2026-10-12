//! The executable specification: a canonical event type, an exact single-threaded model of the
//! ingress coalescer (SPEC section 4), and the consumer-side state fold used to compare an
//! implementation with the ground truth (SPEC section 5).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use gorca_model::{
    AppId, CaretOrigin, DocKind, LocalId, NodeEvent, NodeKey, Politeness, PropMask, TextDelta,
    WindowKind,
};

/// Canonical, comparable form of a `NodeEvent`. Node ids are `LocalId` values.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Ev {
    Focus(u64),
    Active(u64, Option<u64>),
    Props(u64, u32),
    Children(u64),
    TextIns(u64, u32, u32),
    TextDel(u64, u32, u32),
    TextReplaced(u64),
    Caret(u64, u32),
    Selection(u64),
    Announce(Option<u64>, String, bool),
    Window(u64, u8),
    Doc(u64, u8),
    Removed(u64),
}

impl Ev {
    pub fn node(&self) -> Option<u64> {
        match self {
            Ev::Focus(n)
            | Ev::Active(n, _)
            | Ev::Props(n, _)
            | Ev::Children(n)
            | Ev::TextIns(n, _, _)
            | Ev::TextDel(n, _, _)
            | Ev::TextReplaced(n)
            | Ev::Caret(n, _)
            | Ev::Selection(n)
            | Ev::Window(n, _)
            | Ev::Doc(n, _)
            | Ev::Removed(n) => Some(*n),
            Ev::Announce(n, _, _) => *n,
        }
    }

    /// Back to the type the ingress API takes.
    pub fn to_node_event(&self, app: AppId) -> NodeEvent {
        let k = |n: u64| NodeKey {
            app,
            local: LocalId(n),
        };
        match self {
            Ev::Focus(n) => NodeEvent::FocusChanged { node: k(*n) },
            Ev::Active(n, c) => NodeEvent::ActiveDescendantChanged {
                container: k(*n),
                child: c.map(k),
            },
            Ev::Props(n, m) => NodeEvent::PropertiesChanged {
                node: k(*n),
                mask: PropMask::from_bits_truncate(*m),
            },
            Ev::Children(n) => NodeEvent::ChildrenChanged { node: k(*n) },
            Ev::TextIns(n, o, l) => NodeEvent::TextChanged {
                node: k(*n),
                delta: TextDelta::Insert {
                    offset: *o,
                    len: *l,
                    text: None,
                },
            },
            Ev::TextDel(n, o, l) => NodeEvent::TextChanged {
                node: k(*n),
                delta: TextDelta::Delete {
                    offset: *o,
                    len: *l,
                    text: None,
                },
            },
            Ev::TextReplaced(n) => NodeEvent::TextChanged {
                node: k(*n),
                delta: TextDelta::Replaced { text: None },
            },
            Ev::Caret(n, o) => NodeEvent::CaretMoved {
                node: k(*n),
                offset: *o,
                origin: CaretOrigin::Unknown,
            },
            Ev::Selection(n) => NodeEvent::SelectionChanged { node: k(*n) },
            Ev::Announce(n, t, a) => NodeEvent::Announcement {
                node: n.map(k),
                text: Arc::from(t.as_str()),
                politeness: if *a {
                    Politeness::Assertive
                } else {
                    Politeness::Polite
                },
            },
            Ev::Window(n, w) => NodeEvent::WindowEvent {
                node: k(*n),
                kind: match w {
                    0 => WindowKind::Activate,
                    1 => WindowKind::Deactivate,
                    2 => WindowKind::Create,
                    _ => WindowKind::Destroy,
                },
            },
            Ev::Doc(n, d) => NodeEvent::DocumentEvent {
                node: k(*n),
                kind: match d {
                    0 => DocKind::LoadComplete,
                    1 => DocKind::Reload,
                    2 => DocKind::LoadStopped,
                    _ => DocKind::LoadStart,
                },
            },
            Ev::Removed(n) => NodeEvent::NodeRemoved { node: k(*n) },
        }
    }
}

// ---- exact model of one app's ingress --------------------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SpecCfg {
    pub max_keys: usize,
    pub announcement_cap: usize,
    pub max_text_deltas: usize,
    /// Dirty nodes before the set degrades to "everything" (`MAX_DIRTY_NODES`).
    pub max_dirty: usize,
    /// Pending slots before even lossless classes degrade to dirty-all (SPEC 4.4 rule H).
    pub hard_cap: usize,
}

impl Default for SpecCfg {
    fn default() -> Self {
        Self {
            max_keys: 4096,
            announcement_cap: 16,
            max_text_deltas: 64,
            max_dirty: 65_536,
            hard_cap: 65_536,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
enum Key {
    Focus,
    Props(u64),
    Children(u64),
    Text(u64),
    Caret(u64),
    Selection(u64),
    Active(u64),
    Announce(u64),
    Window(u64),
    Doc(u64),
    Removed(u64),
}

impl Key {
    fn node(self) -> Option<u64> {
        match self {
            Key::Focus | Key::Announce(_) => None,
            Key::Props(n)
            | Key::Children(n)
            | Key::Text(n)
            | Key::Caret(n)
            | Key::Selection(n)
            | Key::Active(n)
            | Key::Window(n)
            | Key::Doc(n)
            | Key::Removed(n) => Some(n),
        }
    }
}

#[derive(Clone, Debug)]
enum Payload {
    One(Ev),
    Props(u64, u32),
    Text(Vec<Ev>),
}

/// What one drain returns, in canonical form.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Batch {
    pub events: Vec<Ev>,
    pub flooded: bool,
    pub dirty_all: bool,
    pub dirty: BTreeSet<u64>,
    pub pushed: u64,
    pub focus_sequence: u64,
    pub announcements_dropped: u64,
}

#[derive(Debug)]
pub struct SpecModel {
    cfg: SpecCfg,
    slots: Vec<Option<(Key, Payload)>>,
    index: HashMap<Key, usize>,
    by_node: HashMap<u64, Vec<Key>>,
    live: usize,
    announcements: usize,
    focused: Option<u64>,
    flooded: bool,
    dirty_all: bool,
    dirty: BTreeSet<u64>,
    pushed: u64,
    focus_sequence: u64,
    announcements_dropped: u64,
}

impl SpecModel {
    pub fn new(cfg: SpecCfg) -> Self {
        Self {
            cfg,
            slots: Vec::new(),
            index: HashMap::new(),
            by_node: HashMap::new(),
            live: 0,
            announcements: 0,
            focused: None,
            flooded: false,
            dirty_all: false,
            dirty: BTreeSet::new(),
            pushed: 0,
            focus_sequence: 0,
            announcements_dropped: 0,
        }
    }

    pub fn pending(&self) -> usize {
        self.live
    }

    fn key_of(&self, ev: &Ev) -> Key {
        match ev {
            Ev::Focus(_) => Key::Focus,
            Ev::Active(n, _) => Key::Active(*n),
            Ev::Props(n, _) => Key::Props(*n),
            Ev::Children(n) => Key::Children(*n),
            Ev::TextIns(n, _, _) | Ev::TextDel(n, _, _) | Ev::TextReplaced(n) => Key::Text(*n),
            Ev::Caret(n, _) => Key::Caret(*n),
            Ev::Selection(n) => Key::Selection(*n),
            Ev::Announce(..) => Key::Announce(self.pushed),
            Ev::Window(n, _) => Key::Window(*n),
            Ev::Doc(n, _) => Key::Doc(*n),
            Ev::Removed(n) => Key::Removed(*n),
        }
    }

    fn kept_when_flooded(&self, key: Key) -> bool {
        match key {
            Key::Focus | Key::Removed(_) | Key::Window(_) | Key::Announce(_) => true,
            Key::Caret(n) => self.focused == Some(n),
            Key::Props(_)
            | Key::Children(_)
            | Key::Text(_)
            | Key::Selection(_)
            | Key::Active(_)
            | Key::Doc(_) => false,
        }
    }

    fn mark_dirty(&mut self, n: Option<u64>) {
        match n {
            Some(n) if !self.dirty_all => {
                if self.dirty.len() >= self.cfg.max_dirty && !self.dirty.contains(&n) {
                    self.dirty_all = true;
                    self.dirty.clear();
                } else {
                    self.dirty.insert(n);
                }
            }
            Some(_) => {}
            None => {
                self.dirty_all = true;
                self.dirty.clear();
            }
        }
    }

    fn insert(&mut self, key: Key, p: Payload) {
        if let Some(n) = key.node() {
            self.by_node.entry(n).or_default().push(key);
        }
        self.index.insert(key, self.slots.len());
        self.slots.push(Some((key, p)));
        self.live += 1;
    }

    fn enter_flooded(&mut self) {
        self.flooded = true;
        let old = std::mem::take(&mut self.slots);
        self.index.clear();
        self.by_node.clear();
        self.live = 0;
        for (key, p) in old.into_iter().flatten() {
            if self.kept_when_flooded(key) {
                self.insert(key, p);
            } else {
                self.mark_dirty(key.node());
            }
        }
    }

    pub fn push(&mut self, ev: &Ev) {
        self.pushed += 1;
        let key = self.key_of(ev);
        if let Ev::Focus(n) = ev {
            self.focused = Some(*n);
            self.focus_sequence += 1;
        }
        let known = self.index.contains_key(&key);
        if !self.flooded && self.live >= self.cfg.max_keys && !known {
            self.enter_flooded();
        }
        if self.flooded && !self.kept_when_flooded(key) {
            self.mark_dirty(ev.node());
            return;
        }
        // Rule H: the hard cap bounds even the lossless classes. Focus is one slot.
        if !known && key != Key::Focus && self.live >= self.cfg.hard_cap {
            if !self.flooded {
                self.enter_flooded();
            }
            self.dirty_all = true;
            self.dirty.clear();
            return;
        }
        match (key, ev) {
            (Key::Announce(_), _) => {
                if self.announcements >= self.cfg.announcement_cap {
                    self.announcements_dropped += 1;
                    return;
                }
                self.announcements += 1;
                self.insert(key, Payload::One(ev.clone()));
            }
            (Key::Removed(n), _) => {
                // R-1: Removed(n) deletes every pending slot of n.
                for k in self.by_node.remove(&n).unwrap_or_default() {
                    if let Some(i) = self.index.remove(&k) {
                        self.slots[i] = None;
                        self.live -= 1;
                    }
                }
                self.insert(key, Payload::One(ev.clone()));
            }
            (Key::Props(n), Ev::Props(_, m)) => match self.index.get(&key).copied() {
                Some(i) => {
                    if let Some((_, Payload::Props(_, cur))) = self.slots[i].as_mut() {
                        *cur |= m;
                    }
                }
                None => self.insert(key, Payload::Props(n, *m)),
            },
            (Key::Text(n), _) => match self.index.get(&key).copied() {
                Some(i) => {
                    if let Some((_, Payload::Text(deltas))) = self.slots[i].as_mut() {
                        merge_text(deltas, ev.clone(), self.cfg.max_text_deltas, n);
                    }
                }
                None => self.insert(key, Payload::Text(vec![ev.clone()])),
            },
            _ => match self.index.get(&key).copied() {
                // Last writer wins, position of first occurrence kept (R-2).
                Some(i) => self.slots[i] = Some((key, Payload::One(ev.clone()))),
                None => self.insert(key, Payload::One(ev.clone())),
            },
        }
    }

    pub fn drain(&mut self) -> Batch {
        let mut events = Vec::new();
        for (_, p) in std::mem::take(&mut self.slots).into_iter().flatten() {
            match p {
                Payload::One(e) => events.push(e),
                Payload::Props(n, m) => events.push(Ev::Props(n, m)),
                Payload::Text(ds) => events.extend(ds),
            }
        }
        let b = Batch {
            events,
            flooded: self.flooded,
            dirty_all: self.flooded && self.dirty_all,
            dirty: if self.flooded {
                std::mem::take(&mut self.dirty)
            } else {
                BTreeSet::new()
            },
            pushed: self.pushed,
            focus_sequence: self.focus_sequence,
            announcements_dropped: self.announcements_dropped,
        };
        self.index.clear();
        self.by_node.clear();
        self.live = 0;
        self.announcements = 0;
        self.flooded = false;
        self.dirty_all = false;
        self.dirty.clear();
        self.pushed = 0;
        self.focus_sequence = 0;
        self.announcements_dropped = 0;
        b
    }
}

/// SPEC 4.3 text merge: delete followed by insert, more than `max` deltas, or a `Replaced`
/// already present collapse to one `Replaced`.
fn merge_text(deltas: &mut Vec<Ev>, new: Ev, max: usize, node: u64) {
    let collapse = match (deltas.last(), &new) {
        (Some(Ev::TextReplaced(_)), _) => return,
        (Some(Ev::TextDel(..)), Ev::TextIns(..)) => true,
        _ => deltas.len() + 1 > max,
    };
    if collapse {
        deltas.clear();
        deltas.push(Ev::TextReplaced(node));
    } else {
        deltas.push(new);
    }
}

// ---- consumer fold ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeSt {
    pub props: u32,
    pub caret: Option<u32>,
    pub selection: bool,
    pub active: Option<Option<u64>>,
    pub children: bool,
    pub text: bool,
    pub doc: Option<u8>,
    pub window: Option<u8>,
}

/// State a consumer (the app actor) would hold after applying events. Applying the full raw
/// stream gives the ground truth; applying the drained batches gives the implementation's view.
#[derive(Clone, Debug, Default)]
pub struct Fold {
    pub focus: Option<u64>,
    pub nodes: HashMap<u64, NodeSt>,
    pub removed: HashSet<u64>,
    pub dirty_all: bool,
    pub dirty: HashSet<u64>,
    pub announcements: u64,
    pub announcements_dropped: u64,
    pub pushed: u64,
    pub batches: u64,
    pub flooded_batches: u64,
}

impl Fold {
    pub fn apply(&mut self, ev: &Ev) {
        match ev {
            Ev::Focus(n) => self.focus = Some(*n),
            Ev::Active(n, c) => self.nodes.entry(*n).or_default().active = Some(*c),
            Ev::Props(n, m) => self.nodes.entry(*n).or_default().props |= m,
            Ev::Children(n) => self.nodes.entry(*n).or_default().children = true,
            Ev::TextIns(n, _, _) | Ev::TextDel(n, _, _) | Ev::TextReplaced(n) => {
                self.nodes.entry(*n).or_default().text = true;
            }
            Ev::Caret(n, o) => self.nodes.entry(*n).or_default().caret = Some(*o),
            Ev::Selection(n) => self.nodes.entry(*n).or_default().selection = true,
            Ev::Announce(..) => self.announcements += 1,
            Ev::Window(n, k) => self.nodes.entry(*n).or_default().window = Some(*k),
            Ev::Doc(n, k) => self.nodes.entry(*n).or_default().doc = Some(*k),
            Ev::Removed(n) => {
                self.nodes.remove(n);
                self.dirty.remove(n);
                self.removed.insert(*n);
            }
        }
    }

    /// Dirty marks first, then the events (a `Removed` clears a dirty mark of its node).
    pub fn apply_batch(&mut self, events: &[Ev], dirty_all: bool, dirty: impl IntoIterator<Item = u64>, pushed: u64, announcements_dropped: u64, flooded: bool) {
        self.batches += 1;
        self.pushed += pushed;
        self.announcements_dropped += announcements_dropped;
        if flooded {
            self.flooded_batches += 1;
        }
        if dirty_all {
            self.dirty_all = true;
        }
        self.dirty.extend(dirty);
        for e in events {
            self.apply(e);
        }
    }

    fn is_dirty(&self, n: u64) -> bool {
        self.dirty_all || self.dirty.contains(&n)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Verdict {
    pub violations: Vec<String>,
    /// Nodes whose state was compared field by field.
    pub compared: u64,
    /// Nodes skipped because the candidate reported them dirty.
    pub skipped_dirty: u64,
}

impl Verdict {
    pub fn ok(&self) -> bool {
        self.violations.is_empty()
    }

    fn bad(&mut self, s: String) {
        if self.violations.len() < 20 {
            self.violations.push(s);
        }
    }
}

/// SPEC 5.2: compare an implementation's fold with the ground truth after settle.
pub fn compare(truth: &Fold, got: &Fold) -> Verdict {
    let mut v = Verdict::default();
    if truth.focus != got.focus {
        v.bad(format!("final focus: want {:?}, got {:?}", truth.focus, got.focus));
    }
    if truth.removed != got.removed {
        let lost: Vec<_> = truth.removed.difference(&got.removed).take(5).collect();
        let phantom: Vec<_> = got.removed.difference(&truth.removed).take(5).collect();
        v.bad(format!(
            "removed set differs: {} lost (e.g. {lost:?}), {} phantom (e.g. {phantom:?})",
            truth.removed.difference(&got.removed).count(),
            got.removed.difference(&truth.removed).count()
        ));
    }
    if truth.announcements != got.announcements + got.announcements_dropped {
        v.bad(format!(
            "announcements: pushed {}, delivered {} + counted dropped {}",
            truth.announcements, got.announcements, got.announcements_dropped
        ));
    }
    if truth.pushed != got.pushed {
        v.bad(format!(
            "stats.pushed: ingress saw {} pushes, expected {}",
            got.pushed, truth.pushed
        ));
    }
    let sorted: BTreeMap<_, _> = truth.nodes.iter().collect();
    for (n, tn) in sorted {
        if got.is_dirty(*n) {
            v.skipped_dirty += 1;
            continue;
        }
        v.compared += 1;
        match got.nodes.get(n) {
            Some(gn) if gn == tn => {}
            Some(gn) => v.bad(format!("node {n:#x}: want {tn:?}, got {gn:?}")),
            None => v.bad(format!("node {n:#x}: lost entirely, want {tn:?}")),
        }
    }
    for n in got.nodes.keys() {
        if !truth.nodes.contains_key(n) && !got.is_dirty(*n) {
            v.bad(format!("node {n:#x}: phantom state"));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(m: &mut SpecModel, evs: &[Ev]) -> Batch {
        for e in evs {
            m.push(e);
        }
        m.drain()
    }

    #[test]
    fn focus_keeps_first_position_and_last_value() {
        let mut m = SpecModel::new(SpecCfg::default());
        let b = feed(
            &mut m,
            &[Ev::Selection(9), Ev::Focus(1), Ev::Focus(2), Ev::Focus(3)],
        );
        assert_eq!(b.events, vec![Ev::Selection(9), Ev::Focus(3)]);
        assert_eq!(b.focus_sequence, 3);
    }

    #[test]
    fn removed_supersedes_and_later_events_survive() {
        let mut m = SpecModel::new(SpecCfg::default());
        let b = feed(
            &mut m,
            &[
                Ev::Props(1, 1),
                Ev::Selection(1),
                Ev::Selection(2),
                Ev::Removed(1),
                Ev::Props(1, 2),
            ],
        );
        assert_eq!(
            b.events,
            vec![Ev::Selection(2), Ev::Removed(1), Ev::Props(1, 2)]
        );
    }

    #[test]
    fn flooding_dirties_and_keeps_lossless() {
        let cfg = SpecCfg {
            max_keys: 3,
            ..SpecCfg::default()
        };
        let mut m = SpecModel::new(cfg);
        let b = feed(
            &mut m,
            &[
                Ev::Focus(100),
                Ev::Children(1),
                Ev::Children(2),
                Ev::Children(3),
                Ev::Children(4),
                Ev::Caret(100, 5),
                Ev::Caret(7, 5),
                Ev::Removed(2),
            ],
        );
        assert!(b.flooded);
        assert_eq!(b.events, vec![Ev::Focus(100), Ev::Caret(100, 5), Ev::Removed(2)]);
        assert_eq!(b.dirty, BTreeSet::from([1, 2, 3, 4, 7]));
    }

    #[test]
    fn fold_equivalence_for_simple_sequence() {
        let evs = vec![
            Ev::Props(1, 1),
            Ev::Props(1, 2),
            Ev::Caret(1, 3),
            Ev::Caret(1, 9),
            Ev::Focus(4),
            Ev::Focus(5),
        ];
        let mut truth = Fold::default();
        truth.pushed = evs.len() as u64;
        for e in &evs {
            truth.apply(e);
        }
        let mut m = SpecModel::new(SpecCfg::default());
        let b = feed(&mut m, &evs);
        let mut got = Fold::default();
        got.apply_batch(&b.events, b.dirty_all, b.dirty.iter().copied(), b.pushed, b.announcements_dropped, b.flooded);
        let v = compare(&truth, &got);
        assert!(v.ok(), "{:?}", v.violations);
    }
}
