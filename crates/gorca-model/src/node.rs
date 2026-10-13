//! Node snapshots and freshness (ADR-0001 sections 4.2 and 4.4).

use std::sync::Arc;
use std::time::Instant;

use bitflags::bitflags;
use smallvec::SmallVec;

use crate::geometry::Rect;
use crate::ids::NodeKey;
use crate::role::Role;

bitflags! {
    /// Connector-normalised state bits.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct StateSet: u64 {
        const FOCUSED    = 1 << 0;
        const FOCUSABLE  = 1 << 1;
        const SELECTED   = 1 << 2;
        const CHECKED    = 1 << 3;
        const PRESSED    = 1 << 4;
        const EXPANDED   = 1 << 5;
        const COLLAPSED  = 1 << 6;
        const DISABLED   = 1 << 7;
        const EDITABLE   = 1 << 8;
        const READ_ONLY  = 1 << 9;
        const SHOWING    = 1 << 10;
        const VISIBLE    = 1 << 11;
        const BUSY       = 1 << 12;
        const MODAL      = 1 << 13;
        const MULTILINE  = 1 << 14;
        const REQUIRED   = 1 << 15;
        const INVALID    = 1 << 16;
        const HAS_POPUP  = 1 << 17;
        const ACTIVE     = 1 << 18;
        const DEFUNCT    = 1 << 19;
    }
}

bitflags! {
    /// Advisory interface bits. Browsers advertise these too broadly (WEB 1.2, 2.2), so
    /// generators branch on role and states, never on interface presence.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct Ifaces: u32 {
        const TEXT          = 1 << 0;
        const EDITABLE_TEXT = 1 << 1;
        const HYPERTEXT     = 1 << 2;
        const TABLE         = 1 << 3;
        const ACTION        = 1 << 4;
        const SELECTION     = 1 << 5;
        const DOCUMENT      = 1 << 6;
        const VALUE         = 1 << 7;
        const COLLECTION    = 1 << 8;
    }
}

bitflags! {
    /// Which properties changed or are wanted (`NodeEvent::PropertiesChanged`, `BackendOp::Fetch`).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct PropMask: u32 {
        const NAME        = 1 << 0;
        const DESCRIPTION = 1 << 1;
        const VALUE       = 1 << 2;
        const ROLE        = 1 << 3;
        const STATES      = 1 << 4;
        const BOUNDS      = 1 << 5;
        const ATTRS       = 1 << 6;
        const RELATIONS   = 1 << 7;
        const CHILDREN    = 1 << 8;
        /// Hint on `PropertiesChanged` only: the node just *became* selected
        /// (`state-changed:selected(1)`). Never part of a fetch mask.
        const SELECTED_NOW = 1 << 9;
        /// The node's parent reference (B-1 PW-2). Part of `ALL`, never of `PRESENT`.
        const PARENT      = 1 << 10;
    }
}

impl PropMask {
    /// What the focus presenter needs (name, role, states, value, description).
    pub const PRESENT: PropMask = PropMask::NAME
        .union(PropMask::DESCRIPTION)
        .union(PropMask::VALUE)
        .union(PropMask::ROLE)
        .union(PropMask::STATES);

    /// Every field a snapshot can carry (no hint bits). `invalidate(k, ALL)` and the app-wide
    /// stale epoch mean this.
    pub const ALL: PropMask = PropMask::NAME
        .union(PropMask::DESCRIPTION)
        .union(PropMask::VALUE)
        .union(PropMask::ROLE)
        .union(PropMask::STATES)
        .union(PropMask::BOUNDS)
        .union(PropMask::ATTRS)
        .union(PropMask::RELATIONS)
        .union(PropMask::CHILDREN)
        .union(PropMask::PARENT);

    /// What a `Cache.GetItems` row carries (B-1 F-6): name, description, role, states, child
    /// count and parent. No value, relations, attributes or bounds.
    pub const CACHE_ITEM: PropMask = PropMask::NAME
        .union(PropMask::DESCRIPTION)
        .union(PropMask::ROLE)
        .union(PropMask::STATES)
        .union(PropMask::CHILDREN)
        .union(PropMask::PARENT);
}

/// Interned attribute key. The interner lives in the connector; the core only compares keys.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AttrKey(pub u16);

/// Small attribute map (xml-roles, level, tag, live-*, ...).
pub type AttrMap = SmallVec<[(AttrKey, Arc<str>); 4]>;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RelationKind {
    LabelledBy,
    LabelFor,
    Controls,
    ControlledBy,
    FlowsTo,
    FlowsFrom,
    DescribedBy,
    MemberOf,
}

pub type Relations = SmallVec<[(RelationKind, NodeKey); 2]>;

/// Numeric range or text value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Range { current: f64, min: f64, max: f64 },
    Text(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChildList {
    Known(SmallVec<[NodeKey; 8]>),
    Count(u32),
    Unknown,
}

/// Immutable node snapshot. Text is deliberately not part of it (see `TextQuery`).
#[derive(Clone, Debug, PartialEq)]
pub struct NodeSnapshot {
    pub key: NodeKey,
    pub role: Role,
    pub states: StateSet,
    pub name: Option<Arc<str>>,
    pub description: Option<Arc<str>>,
    pub value: Option<Value>,
    pub parent: Option<NodeKey>,
    pub children: ChildList,
    pub interfaces: Ifaces,
    pub attrs: AttrMap,
    pub relations: Relations,
    pub bounds: Option<Rect>,
    /// Fields this snapshot actually carries. A `GetItems` node has `PropMask::CACHE_ITEM`; a
    /// full fetch has what was asked for.
    pub have: PropMask,
    /// Mirror write sequence at which the app confirmed this content: the `issued_at` of the
    /// request whose reply produced it (late-reply and re-dirty rules, B-1 5.4).
    pub confirmed_seq: u64,
    pub fresh: Freshness,
}

/// Result of a mirror read with the app-wide stale epoch applied (B-1 5.2).
#[derive(Clone, Debug, PartialEq)]
pub struct NodeView {
    pub snap: Arc<NodeSnapshot>,
    /// Fields that are no longer trustworthy: the node's own stale mask, plus `ALL` when the
    /// node was confirmed before the app-wide stale epoch.
    pub stale: PropMask,
    /// The last confirmation is older than the app's TTL (B-1 2.2, evaluated on read): the node
    /// may still be presented, tagged `may_refine`, but speaking it is followed by a revalidation.
    pub aged: bool,
}

impl NodeView {
    /// A read is a hit for `need` when the snapshot carries it and none of it is stale.
    pub fn satisfies(&self, need: PropMask) -> bool {
        self.snap.have.contains(need) && !self.stale.intersects(need)
    }
}

/// A bulk cache fill (`AppPort::push_snapshot`, `MirrorWriter::apply`).
#[derive(Clone, Debug, Default)]
pub struct SnapshotBatch {
    pub nodes: Vec<Arc<NodeSnapshot>>,
    /// The producer stopped at a bound (`ChildSnapshots`, `Ancestry`, `Subtree`, `CacheDump`).
    pub truncated: bool,
}

impl SnapshotBatch {
    pub fn new(nodes: Vec<Arc<NodeSnapshot>>) -> Self {
        Self {
            nodes,
            truncated: false,
        }
    }
}

/// Result of a mirror lookup. I-3: `Gone` never panics and never blocks.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Lookup<T> {
    Hit(T),
    Miss,
    Gone,
}

impl<T> Lookup<T> {
    pub fn hit(self) -> Option<T> {
        match self {
            Lookup::Hit(v) => Some(v),
            Lookup::Miss | Lookup::Gone => None,
        }
    }
}

/// ADR-0001 section 4.4 (B-1 amendment): `Fetching` lives in the driver's in-flight table, not in
/// the published snapshot, which is immutable. `Stale` carries the fields an event said are no
/// longer trustworthy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// Confirmed by connector push or reply.
    Fresh { at: Instant },
    /// Invalidated by an event; still usable for presentation with care.
    Stale { since: Instant, mask: PropMask },
    /// Connector reported removal or the app is gone; a tombstone is kept briefly.
    Defunct,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FreshnessEvent {
    Invalidate {
        at: Instant,
        mask: PropMask,
    },
    /// A reply (or push) confirmed the content that was asked for.
    Confirm {
        at: Instant,
    },
    /// The content was re-dirtied by an event issued after the request that produced it.
    ConfirmRedirtied {
        at: Instant,
        mask: PropMask,
    },
    Removed,
}

impl Freshness {
    /// Transition function of the section 4.4 table. A defunct node never comes back under the
    /// same snapshot (I-1); revival is a new snapshot (LR-3).
    #[must_use]
    pub fn step(self, ev: FreshnessEvent) -> Freshness {
        match self {
            Freshness::Fresh { .. } => match ev {
                FreshnessEvent::Invalidate { at, mask } => Freshness::Stale { since: at, mask },
                FreshnessEvent::Confirm { at } => Freshness::Fresh { at },
                FreshnessEvent::ConfirmRedirtied { at, mask } => {
                    Freshness::Stale { since: at, mask }
                }
                FreshnessEvent::Removed => Freshness::Defunct,
            },
            Freshness::Stale { since, mask } => match ev {
                FreshnessEvent::Invalidate { mask: m, .. }
                | FreshnessEvent::ConfirmRedirtied { mask: m, .. } => Freshness::Stale {
                    since,
                    mask: mask | m,
                },
                FreshnessEvent::Confirm { at } => Freshness::Fresh { at },
                FreshnessEvent::Removed => Freshness::Defunct,
            },
            Freshness::Defunct => Freshness::Defunct,
        }
    }

    /// Fields known to be untrustworthy by this snapshot's own marking.
    pub fn stale_mask(self) -> PropMask {
        match self {
            Freshness::Fresh { .. } => PropMask::empty(),
            Freshness::Stale { mask, .. } => mask,
            Freshness::Defunct => PropMask::ALL,
        }
    }

    /// May the node be presented? Stale nodes may, with the `may_refine` tag.
    pub fn presentable(self) -> bool {
        match self {
            Freshness::Fresh { .. } | Freshness::Stale { .. } => true,
            Freshness::Defunct => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn invalidate_accumulates_and_confirm_clears() {
        let t = t0();
        let s = Freshness::Fresh { at: t }.step(FreshnessEvent::Invalidate {
            at: t,
            mask: PropMask::NAME,
        });
        assert_eq!(
            s,
            Freshness::Stale {
                since: t,
                mask: PropMask::NAME
            }
        );
        let s = s.step(FreshnessEvent::Invalidate {
            at: t,
            mask: PropMask::STATES,
        });
        assert_eq!(s.stale_mask(), PropMask::NAME | PropMask::STATES);
        assert_eq!(
            s.step(FreshnessEvent::Confirm { at: t }),
            Freshness::Fresh { at: t }
        );
    }

    #[test]
    fn a_redirtied_confirmation_stays_stale() {
        let t = t0();
        let s = Freshness::Fresh { at: t }.step(FreshnessEvent::ConfirmRedirtied {
            at: t,
            mask: PropMask::NAME,
        });
        assert_eq!(s.stale_mask(), PropMask::NAME);
        assert!(s.presentable());
    }

    #[test]
    fn defunct_is_absorbing() {
        let t = t0();
        let evs = [
            FreshnessEvent::Invalidate {
                at: t,
                mask: PropMask::ALL,
            },
            FreshnessEvent::Confirm { at: t },
            FreshnessEvent::ConfirmRedirtied {
                at: t,
                mask: PropMask::ALL,
            },
            FreshnessEvent::Removed,
        ];
        for ev in evs {
            assert_eq!(Freshness::Defunct.step(ev), Freshness::Defunct);
        }
        assert!(!Freshness::Defunct.presentable());
        assert_eq!(Freshness::Defunct.stale_mask(), PropMask::ALL);
    }

    #[test]
    fn present_mask_contains_name_and_role() {
        assert!(PropMask::PRESENT.contains(PropMask::NAME | PropMask::ROLE));
        assert!(!PropMask::PRESENT.contains(PropMask::BOUNDS));
        assert!(!PropMask::ALL.contains(PropMask::SELECTED_NOW));
        assert!(PropMask::ALL.contains(PropMask::PARENT));
        assert!(!PropMask::CACHE_ITEM.contains(PropMask::VALUE));
    }
}
