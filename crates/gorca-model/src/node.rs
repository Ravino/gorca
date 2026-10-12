//! Node snapshots and freshness (ADR-0001 sections 4.2 and 4.4).

use std::sync::Arc;
use std::time::Instant;

use bitflags::bitflags;
use smallvec::SmallVec;

use crate::geometry::Rect;
use crate::ids::{NodeKey, ReqId};
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
    }
}

impl PropMask {
    /// What the focus presenter needs (name, role, states, value, description).
    pub const PRESENT: PropMask = PropMask::NAME
        .union(PropMask::DESCRIPTION)
        .union(PropMask::VALUE)
        .union(PropMask::ROLE)
        .union(PropMask::STATES);
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
    pub fresh: Freshness,
}

/// A bulk cache fill (`AppPort::push_snapshot`, `MirrorWriter::apply`).
#[derive(Clone, Debug, Default)]
pub struct SnapshotBatch {
    pub nodes: Vec<Arc<NodeSnapshot>>,
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

/// ADR-0001 section 4.4. `Fetching` carries one extra field, `redirtied`, that the ADR table
/// needs ("Fetching (mark re-dirty)" then "Fresh, or Stale if re-dirtied") but its struct omits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Freshness {
    /// Confirmed by connector push or reply.
    Fresh { at: Instant },
    /// Invalidated by an event; still usable for presentation with care.
    Stale { since: Instant },
    Fetching {
        req: ReqId,
        deadline: Instant,
        redirtied: bool,
    },
    /// Connector reported removal or the app is gone; a tombstone is kept briefly.
    Defunct,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FreshnessEvent {
    Invalidate {
        at: Instant,
    },
    FetchIssued {
        req: ReqId,
        deadline: Instant,
    },
    ReplyOk {
        at: Instant,
    },
    /// Timeout or error. The health penalty is applied by the app actor, not here.
    ReplyFailed {
        at: Instant,
    },
    Removed,
}

impl Freshness {
    /// Transition function of the section 4.4 table. Cells marked "n/a" leave the state as is.
    #[must_use]
    pub fn step(self, ev: FreshnessEvent) -> Freshness {
        match self {
            Freshness::Fresh { .. } => match ev {
                FreshnessEvent::Invalidate { at } => Freshness::Stale { since: at },
                FreshnessEvent::FetchIssued { req, deadline } => Freshness::Fetching {
                    req,
                    deadline,
                    redirtied: false,
                },
                FreshnessEvent::ReplyOk { .. } | FreshnessEvent::ReplyFailed { .. } => self,
                FreshnessEvent::Removed => Freshness::Defunct,
            },
            Freshness::Stale { since } => match ev {
                FreshnessEvent::Invalidate { .. } => Freshness::Stale { since },
                FreshnessEvent::FetchIssued { req, deadline } => Freshness::Fetching {
                    req,
                    deadline,
                    redirtied: false,
                },
                FreshnessEvent::ReplyOk { .. } | FreshnessEvent::ReplyFailed { .. } => self,
                FreshnessEvent::Removed => Freshness::Defunct,
            },
            Freshness::Fetching {
                req,
                deadline,
                redirtied,
            } => match ev {
                FreshnessEvent::Invalidate { .. } => Freshness::Fetching {
                    req,
                    deadline,
                    redirtied: true,
                },
                // Deduplicated: a fetch is already in flight.
                FreshnessEvent::FetchIssued { .. } => self,
                FreshnessEvent::ReplyOk { at } => {
                    if redirtied {
                        Freshness::Stale { since: at }
                    } else {
                        Freshness::Fresh { at }
                    }
                }
                FreshnessEvent::ReplyFailed { at } => Freshness::Stale { since: at },
                FreshnessEvent::Removed => Freshness::Defunct,
            },
            // Refused / dropped: a defunct node never comes back under the same key (I-1).
            Freshness::Defunct => match ev {
                FreshnessEvent::Invalidate { .. }
                | FreshnessEvent::FetchIssued { .. }
                | FreshnessEvent::ReplyOk { .. }
                | FreshnessEvent::ReplyFailed { .. }
                | FreshnessEvent::Removed => Freshness::Defunct,
            },
        }
    }

    /// May the node be presented? Stale nodes may, with the `may_refine` tag.
    pub fn presentable(self) -> bool {
        match self {
            Freshness::Fresh { .. } | Freshness::Stale { .. } | Freshness::Fetching { .. } => true,
            Freshness::Defunct => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn fresh_invalidate_fetch_reply_cycle() {
        let t = t0();
        let d = t + Duration::from_millis(120);
        let s = Freshness::Fresh { at: t }.step(FreshnessEvent::Invalidate { at: t });
        assert_eq!(s, Freshness::Stale { since: t });
        let s = s.step(FreshnessEvent::FetchIssued {
            req: ReqId(1),
            deadline: d,
        });
        assert_eq!(
            s,
            Freshness::Fetching {
                req: ReqId(1),
                deadline: d,
                redirtied: false
            }
        );
        assert_eq!(
            s.step(FreshnessEvent::ReplyOk { at: t }),
            Freshness::Fresh { at: t }
        );
    }

    #[test]
    fn redirtied_fetch_ends_stale() {
        let t = t0();
        let s = Freshness::Fetching {
            req: ReqId(1),
            deadline: t,
            redirtied: false,
        }
        .step(FreshnessEvent::Invalidate { at: t })
        .step(FreshnessEvent::ReplyOk { at: t });
        assert_eq!(s, Freshness::Stale { since: t });
    }

    #[test]
    fn duplicate_fetch_is_deduplicated() {
        let t = t0();
        let s = Freshness::Fetching {
            req: ReqId(1),
            deadline: t,
            redirtied: false,
        };
        assert_eq!(
            s.step(FreshnessEvent::FetchIssued {
                req: ReqId(2),
                deadline: t
            }),
            s
        );
    }

    #[test]
    fn failed_reply_goes_stale() {
        let t = t0();
        let s = Freshness::Fetching {
            req: ReqId(1),
            deadline: t,
            redirtied: false,
        };
        assert_eq!(
            s.step(FreshnessEvent::ReplyFailed { at: t }),
            Freshness::Stale { since: t }
        );
    }

    #[test]
    fn defunct_is_absorbing() {
        let t = t0();
        let evs = [
            FreshnessEvent::Invalidate { at: t },
            FreshnessEvent::FetchIssued {
                req: ReqId(1),
                deadline: t,
            },
            FreshnessEvent::ReplyOk { at: t },
            FreshnessEvent::ReplyFailed { at: t },
            FreshnessEvent::Removed,
        ];
        for ev in evs {
            assert_eq!(Freshness::Defunct.step(ev), Freshness::Defunct);
        }
        assert!(!Freshness::Defunct.presentable());
    }

    #[test]
    fn present_mask_contains_name_and_role() {
        assert!(PropMask::PRESENT.contains(PropMask::NAME | PropMask::ROLE));
        assert!(!PropMask::PRESENT.contains(PropMask::BOUNDS));
    }
}
