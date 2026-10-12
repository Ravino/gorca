//! Deterministic AT-SPI event workloads: what the fake apps would emit, as data.
//!
//! A [`Gen`] is a pure function of `(seed, mix, slots)`. The feeder consumes the first `n`
//! events; the ground truth replays the same generator for the same `n`, so no truth has to be
//! computed on the hot path.
//!
//! Node identity: `id = (generation << 32) | slot`. The object path is `BASE + decimal(id)`.
//! A node that is removed (`Defunct`, `ChildrenChanged:remove`) is retired: its slot gets a new
//! generation and the old id is never used again (ADR-0001 I-1), so the harness never produces
//! events for a node after its `Removed`.

use gorca_model::{AppId, CaretOrigin, DocKind, NodeKey, Politeness, PropMask, WindowKind};
use smallvec::SmallVec;

use crate::clock::Rng;
use crate::model::Ev;
use crate::wire::{Any, Enc, Fields, SIGNAL, encode_event_body, encode_message};

pub const BASE: &str = "/org/a11y/atspi/accessible/";
pub const IFACE_OBJECT: &str = "org.a11y.atspi.Event.Object";
pub const IFACE_WINDOW: &str = "org.a11y.atspi.Event.Window";
pub const IFACE_FOCUS: &str = "org.a11y.atspi.Event.Focus";
pub const IFACE_DOCUMENT: &str = "org.a11y.atspi.Event.Document";

/// Focus probe nodes live in their own id space.
pub const PROBE_SPACE: u64 = 1 << 62;
/// The fence node: a final focus event that proves everything before it was drained.
pub const FENCE_ID: u64 = PROBE_SPACE | 0x3fff_ffff;

pub fn probe_id(k: u32) -> u64 {
    PROBE_SPACE | u64::from(k)
}

/// Write `BASE + decimal(id)` into `out` (cleared first).
pub fn node_path(id: u64, out: &mut String) {
    out.clear();
    out.push_str(BASE);
    let mut digits = [0u8; 20];
    let mut n = id;
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    for &d in &digits[i..] {
        out.push(char::from(d));
    }
}

/// `LocalId` value the drain must compute for node `id` (FNV-1a 64 of its path).
pub fn local_of(id: u64) -> u64 {
    let mut s = String::with_capacity(48);
    node_path(id, &mut s);
    gorca_dbus::drain::local_id(&s).0
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    FocusSignal,
    FocusState,
    FocusLost,
    State,
    Defunct,
    PropName,
    PropDesc,
    PropRole,
    PropValue,
    PropOther,
    ChildAdd { child: u64 },
    ChildRemove { child: u64 },
    TextIns { off: i32 },
    TextDel { off: i32 },
    Caret { off: i32 },
    Selection,
    TextSelection,
    Active { child: Option<u64> },
    Announce { assertive: bool },
    Window(u8),
    Doc(u8),
    /// A valid AT-SPI signal that maps to nothing (`Object:BoundsChanged`).
    Ignored,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GenEvent {
    pub node: u64,
    pub kind: Kind,
    /// Per-app event counter, carried in the `seq` property.
    pub seq: u64,
}

/// Reusable buffers for encoding.
#[derive(Debug, Default)]
pub struct Scratch {
    body: Enc,
    path: String,
    child: String,
}

impl GenEvent {
    /// Append the wire message of this event to `out`. `sender` is set for the mock broker
    /// (a real broker adds it).
    pub fn encode(&self, out: &mut Vec<u8>, sc: &mut Scratch, serial: u32, sender: Option<&str>) {
        node_path(self.node, &mut sc.path);
        let bus = sender.unwrap_or(":1.1");
        let (iface, member, detail, d1, d2) = self.shape();
        sc.body.buf.clear();
        let any = match self.kind {
            Kind::PropName | Kind::PropDesc | Kind::PropRole | Kind::PropValue | Kind::PropOther => {
                Any::Str("name")
            }
            Kind::ChildAdd { child } | Kind::ChildRemove { child } => {
                node_path(child, &mut sc.child);
                Any::Ref(bus, &sc.child)
            }
            Kind::Active { child: Some(c) } => {
                node_path(c, &mut sc.child);
                Any::Ref(bus, &sc.child)
            }
            Kind::TextIns { .. } | Kind::TextDel { .. } => Any::Str("x"),
            Kind::Announce { .. } => Any::Str("hello world"),
            _ => Any::Int(0),
        };
        encode_event_body(&mut sc.body, detail, d1, d2, any, Some(self.seq));
        encode_message(
            out,
            SIGNAL,
            serial,
            &Fields {
                path: Some(&sc.path),
                interface: Some(iface),
                member: Some(member),
                sender,
                signature: Some("siiva{sv}"),
                ..Fields::default()
            },
            &sc.body.buf,
        );
    }

    fn shape(&self) -> (&'static str, &'static str, &'static str, i32, i32) {
        let s = (self.seq & 0x7fff_ffff) as i32;
        match self.kind {
            Kind::FocusSignal => (IFACE_FOCUS, "Focus", "", 0, 0),
            Kind::FocusState => (IFACE_OBJECT, "StateChanged", "focused", 1, 0),
            Kind::FocusLost => (IFACE_OBJECT, "StateChanged", "focused", 0, 0),
            Kind::State => (IFACE_OBJECT, "StateChanged", "showing", i32::from(self.seq & 1 == 1), 0),
            Kind::Defunct => (IFACE_OBJECT, "StateChanged", "defunct", 1, 0),
            Kind::PropName => (IFACE_OBJECT, "PropertyChange", "accessible-name", 0, 0),
            Kind::PropDesc => (IFACE_OBJECT, "PropertyChange", "accessible-description", 0, 0),
            Kind::PropRole => (IFACE_OBJECT, "PropertyChange", "accessible-role", 0, 0),
            Kind::PropValue => (IFACE_OBJECT, "PropertyChange", "accessible-value", 0, 0),
            Kind::PropOther => (IFACE_OBJECT, "PropertyChange", "accessible-table-caption", 0, 0),
            Kind::ChildAdd { .. } => (IFACE_OBJECT, "ChildrenChanged", "add", s & 7, 0),
            Kind::ChildRemove { .. } => (IFACE_OBJECT, "ChildrenChanged", "remove", s & 7, 0),
            Kind::TextIns { off } => (IFACE_OBJECT, "TextChanged", "insert", off, 1),
            Kind::TextDel { off } => (IFACE_OBJECT, "TextChanged", "delete", off, 1),
            Kind::Caret { off } => (IFACE_OBJECT, "TextCaretMoved", "", off, 0),
            Kind::Selection => (IFACE_OBJECT, "SelectionChanged", "", 0, 0),
            Kind::TextSelection => (IFACE_OBJECT, "TextSelectionChanged", "", 0, 0),
            Kind::Active { .. } => (IFACE_OBJECT, "ActiveDescendantChanged", "", 0, 0),
            Kind::Announce { assertive } => {
                (IFACE_OBJECT, "Announcement", "", if assertive { 2 } else { 1 }, 0)
            }
            Kind::Window(k) => (
                IFACE_WINDOW,
                ["Activate", "Deactivate", "Create", "Destroy"][usize::from(k & 3)],
                "",
                0,
                0,
            ),
            Kind::Doc(k) => (
                IFACE_DOCUMENT,
                ["LoadComplete", "Reload", "LoadStopped"][usize::from(k % 3)],
                "",
                0,
                0,
            ),
            Kind::Ignored => (IFACE_OBJECT, "BoundsChanged", "", 0, 0),
        }
    }

    /// The `NodeEvent`s the SPEC table prescribes for this event, in canonical form.
    pub fn expected(&self, app: AppId) -> SmallVec<[Ev; 2]> {
        let n = local_of(self.node);
        let _ = app;
        let mut v = SmallVec::new();
        match self.kind {
            Kind::FocusSignal | Kind::FocusState => v.push(Ev::Focus(n)),
            Kind::FocusLost | Kind::State => v.push(Ev::Props(n, PropMask::STATES.bits())),
            Kind::Defunct => v.push(Ev::Removed(n)),
            Kind::PropName => v.push(Ev::Props(n, PropMask::NAME.bits())),
            Kind::PropDesc => v.push(Ev::Props(n, PropMask::DESCRIPTION.bits())),
            Kind::PropRole => v.push(Ev::Props(n, PropMask::ROLE.bits())),
            Kind::PropValue => v.push(Ev::Props(n, PropMask::VALUE.bits())),
            Kind::PropOther => v.push(Ev::Props(n, PropMask::ATTRS.bits())),
            Kind::ChildAdd { .. } => v.push(Ev::Children(n)),
            Kind::ChildRemove { child } => {
                v.push(Ev::Children(n));
                v.push(Ev::Removed(local_of(child)));
            }
            Kind::TextIns { off } => v.push(Ev::TextIns(n, off.max(0) as u32, 1)),
            Kind::TextDel { off } => v.push(Ev::TextDel(n, off.max(0) as u32, 1)),
            Kind::Caret { off } => v.push(Ev::Caret(n, off.max(0) as u32)),
            Kind::Selection | Kind::TextSelection => v.push(Ev::Selection(n)),
            Kind::Active { child } => v.push(Ev::Active(n, child.map(local_of))),
            Kind::Announce { assertive } => {
                v.push(Ev::Announce(Some(n), "hello world".to_owned(), assertive));
            }
            Kind::Window(k) => v.push(Ev::Window(n, k & 3)),
            Kind::Doc(k) => v.push(Ev::Doc(n, k % 3)),
            Kind::Ignored => {}
        }
        v
    }
}

/// Convert a `NodeEvent` to canonical form (exhaustive on purpose: a new variant must be
/// handled here).
pub fn canon(ev: &gorca_model::NodeEvent) -> Ev {
    use gorca_model::NodeEvent as N;
    use gorca_model::TextDelta as T;
    let id = |k: &NodeKey| k.local.0;
    match ev {
        N::FocusChanged { node } => Ev::Focus(id(node)),
        N::ActiveDescendantChanged { container, child } => {
            Ev::Active(id(container), child.as_ref().map(id))
        }
        N::PropertiesChanged { node, mask } => Ev::Props(id(node), mask.bits()),
        N::ChildrenChanged { node } => Ev::Children(id(node)),
        N::TextChanged { node, delta } => match delta {
            T::Insert { offset, len, .. } => Ev::TextIns(id(node), *offset, *len),
            T::Delete { offset, len, .. } => Ev::TextDel(id(node), *offset, *len),
            T::Replaced { .. } => Ev::TextReplaced(id(node)),
        },
        N::CaretMoved {
            node,
            offset,
            origin,
        } => {
            let _: CaretOrigin = *origin;
            Ev::Caret(id(node), *offset)
        }
        N::SelectionChanged { node } => Ev::Selection(id(node)),
        N::Announcement {
            node,
            text,
            politeness,
        } => Ev::Announce(
            node.as_ref().map(id),
            text.to_string(),
            *politeness == Politeness::Assertive,
        ),
        N::WindowEvent { node, kind } => Ev::Window(
            id(node),
            match kind {
                WindowKind::Activate => 0,
                WindowKind::Deactivate => 1,
                WindowKind::Create => 2,
                WindowKind::Destroy => 3,
            },
        ),
        N::DocumentEvent { node, kind } => Ev::Doc(
            id(node),
            match kind {
                DocKind::LoadComplete => 0,
                DocKind::Reload => 1,
                DocKind::LoadStopped => 2,
                DocKind::LoadStart => 3,
            },
        ),
        N::NodeRemoved { node } => Ev::Removed(id(node)),
    }
}

/// Which node a canonical event belongs to, for the `app` check.
pub fn node_app(ev: &gorca_model::NodeEvent) -> Option<AppId> {
    ev.node().map(|k| k.app)
}

// ---- generators ------------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mix {
    /// `tests/harness/fakeapp.py` storm default: children, text, state, focus-state, 1:1:1:1.
    Fakeapp,
    /// VTE-like: delete+insert pairs on a few nodes, caret moves (F-4).
    Terminal,
    /// Every event a new key: `ChildrenChanged` on never-repeated nodes. Forces `Flooded`.
    Distinct,
    /// A weighted mix of every event class over skewed nodes.
    Realistic,
    /// Mostly lossless classes (Removed, Window, Focus, Announcement) plus noise.
    Lossless,
}

impl Mix {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fakeapp" => Self::Fakeapp,
            "terminal" => Self::Terminal,
            "distinct" => Self::Distinct,
            "realistic" => Self::Realistic,
            "lossless" => Self::Lossless,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Fakeapp => "fakeapp",
            Self::Terminal => "terminal",
            Self::Distinct => "distinct",
            Self::Realistic => "realistic",
            Self::Lossless => "lossless",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Gen {
    rng: Rng,
    mix: Mix,
    gens: Vec<u32>,
    n: u64,
    distinct: u64,
}

impl Gen {
    pub fn new(seed: u64, mix: Mix, slots: u32) -> Self {
        Self {
            rng: Rng::new(seed),
            mix,
            gens: vec![0; slots.max(2) as usize],
            n: 0,
            distinct: 0,
        }
    }

    fn id_at(&self, slot: usize) -> u64 {
        (u64::from(self.gens[slot]) << 32) | slot as u64
    }

    fn pick(&mut self, skew: bool) -> usize {
        let len = self.gens.len() as u64;
        if skew {
            let u = self.rng.unit();
            ((u * u * u) * len as f64) as usize % self.gens.len()
        } else {
            self.rng.below(len) as usize
        }
    }

    fn retire(&mut self, slot: usize) {
        self.gens[slot] = self.gens[slot].wrapping_add(1);
    }

    pub fn next_event(&mut self) -> GenEvent {
        self.n += 1;
        let seq = self.n;
        let s = (seq & 0x3fff_ffff) as i32;
        let (slot, kind) = match self.mix {
            Mix::Fakeapp => self.fakeapp(seq, s),
            Mix::Terminal => self.terminal(s),
            Mix::Distinct => {
                self.distinct += 1;
                let node = PROBE_SPACE >> 1 | self.distinct;
                return GenEvent {
                    node,
                    kind: Kind::ChildAdd { child: node },
                    seq,
                };
            }
            Mix::Realistic => self.realistic(seq, s),
            Mix::Lossless => self.lossless(seq, s),
        };
        let node = self.id_at(slot);
        // Retire after naming: ChildRemove retires the *child* slot, Defunct the node itself.
        let kind = match kind {
            Kind::ChildRemove { child } => Kind::ChildRemove { child },
            k => k,
        };
        if matches!(kind, Kind::Defunct) {
            self.retire(slot);
        }
        GenEvent { node, kind, seq }
    }

    fn child_for(&mut self, parent: usize) -> usize {
        let c = self.pick(false);
        if c == parent {
            (c + 1) % self.gens.len()
        } else {
            c
        }
    }

    fn fakeapp(&mut self, seq: u64, s: i32) -> (usize, Kind) {
        let slot = self.pick(false);
        let kind = match self.rng.below(4) {
            0 => {
                let c = self.child_for(slot);
                if seq % 2 == 1 {
                    Kind::ChildAdd { child: self.id_at(c) }
                } else {
                    let child = self.id_at(c);
                    self.retire(c);
                    Kind::ChildRemove { child }
                }
            }
            1 => Kind::TextIns { off: s.wrapping_mul(3) & 0x7fff_fffe },
            2 => Kind::State,
            _ => {
                if seq % 2 == 1 {
                    Kind::FocusState
                } else {
                    Kind::FocusLost
                }
            }
        };
        (slot, kind)
    }

    fn terminal(&mut self, s: i32) -> (usize, Kind) {
        let slot = self.pick(false) % 4;
        let off = s.wrapping_mul(3) & 0x7fff_fffe;
        let kind = match self.rng.below(100) {
            0..=44 => Kind::TextDel { off: off + 1 },
            45..=89 => Kind::TextIns { off },
            90..=97 => Kind::Caret { off: s },
            _ => Kind::State,
        };
        (slot, kind)
    }

    fn realistic(&mut self, seq: u64, s: i32) -> (usize, Kind) {
        let slot = self.pick(true);
        let off = s.wrapping_mul(3) & 0x7fff_fffe;
        let kind = match self.rng.below(100) {
            0..=29 => Kind::State,
            30..=39 => Kind::TextIns { off },
            40..=49 => Kind::TextDel { off: off + 1 },
            50..=64 => Kind::Caret { off: s },
            65..=69 => {
                let c = self.child_for(slot);
                Kind::ChildAdd { child: self.id_at(c) }
            }
            70..=74 => {
                let c = self.child_for(slot);
                let child = self.id_at(c);
                self.retire(c);
                Kind::ChildRemove { child }
            }
            75..=79 => Kind::Selection,
            80..=83 => {
                if self.rng.below(2) == 0 {
                    let c = self.child_for(slot);
                    Kind::Active { child: Some(self.id_at(c)) }
                } else {
                    Kind::Active { child: None }
                }
            }
            84..=87 => Kind::PropName,
            88..=89 => Kind::PropValue,
            90..=91 => Kind::PropOther,
            92..=93 => Kind::FocusSignal,
            94 => Kind::Window((seq & 3) as u8),
            95 => Kind::Doc((seq % 3) as u8),
            96 => Kind::Announce { assertive: seq % 5 == 0 },
            97 => Kind::Defunct,
            98 => Kind::TextSelection,
            _ => Kind::Ignored,
        };
        (slot, kind)
    }

    fn lossless(&mut self, seq: u64, s: i32) -> (usize, Kind) {
        let slot = self.pick(false);
        let kind = match self.rng.below(100) {
            0..=14 => Kind::Defunct,
            15..=29 => Kind::Window((seq & 3) as u8),
            30..=44 => Kind::FocusSignal,
            45..=54 => Kind::Announce { assertive: seq % 3 == 0 },
            55..=69 => Kind::Caret { off: s },
            70..=84 => Kind::State,
            85..=92 => Kind::Doc((seq % 3) as u8),
            _ => {
                let c = self.child_for(slot);
                let child = self.id_at(c);
                self.retire(c);
                Kind::ChildRemove { child }
            }
        };
        (slot, kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Parse, parse};

    #[test]
    fn generator_is_deterministic() {
        for mix in [Mix::Fakeapp, Mix::Terminal, Mix::Distinct, Mix::Realistic, Mix::Lossless] {
            let mut a = Gen::new(7, mix, 100);
            let mut b = Gen::new(7, mix, 100);
            for _ in 0..1000 {
                assert_eq!(a.next_event(), b.next_event());
            }
        }
    }

    #[test]
    fn removed_nodes_are_never_reused() {
        let mut g = Gen::new(3, Mix::Lossless, 50);
        let mut dead = std::collections::HashSet::new();
        for _ in 0..20_000 {
            let e = g.next_event();
            assert!(!dead.contains(&e.node), "event for a retired node");
            match e.kind {
                Kind::Defunct => {
                    dead.insert(e.node);
                }
                Kind::ChildRemove { child } => {
                    dead.insert(child);
                }
                _ => {}
            }
        }
    }

    #[test]
    fn node_path_is_decimal() {
        let mut s = String::new();
        node_path(0, &mut s);
        assert_eq!(s, format!("{BASE}0"));
        node_path(1234567890123, &mut s);
        assert_eq!(s, format!("{BASE}1234567890123"));
    }

    #[test]
    fn wire_and_expected_agree_with_the_reference_mapping() {
        use gorca_dbus::drain::{SignalView, map_signal};
        use gorca_model::{AppId, ConnectorId};
        let app = AppId {
            connector: ConnectorId(0),
            serial: 1,
        };
        let mut sc = Scratch::default();
        for mix in [Mix::Fakeapp, Mix::Terminal, Mix::Realistic, Mix::Lossless] {
            let mut g = Gen::new(11, mix, 64);
            for i in 0..5000u32 {
                let ev = g.next_event();
                let mut out = Vec::new();
                ev.encode(&mut out, &mut sc, i + 1, Some(":1.7"));
                let Parse::Msg(m, used) = parse(&out) else {
                    panic!("parse failed for {ev:?}");
                };
                assert_eq!(used, out.len());
                let view = SignalView {
                    interface: m.interface.unwrap_or_default(),
                    member: m.member.unwrap_or_default(),
                    path: m.path.unwrap_or_default(),
                    body: m.body,
                    big_endian: m.big,
                };
                let mut mapped = SmallVec::new();
                map_signal(app, &view, &mut mapped).unwrap();
                let got: Vec<Ev> = mapped.iter().map(canon).collect();
                let want: Vec<Ev> = ev.expected(app).into_iter().collect();
                assert_eq!(got, want, "mismatch for {ev:?}");
            }
        }
    }
}
