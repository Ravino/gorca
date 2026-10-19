// SPDX-License-Identifier: GPL-3.0-or-later
//! Fact value types (ADR-0024 sections 2.1 to 2.6): subjects, anchors, confidence, basis,
//! binding and source identity. Pure data: no tokio, no connector type (I-2). The contract
//! functions (`admit`, `step`) live in `gorca-fact-api`.

use std::num::NonZeroU32;
use std::sync::Arc;

use crate::geometry::Rect;
use crate::ids::NodeKey;
use crate::sem::{
    Avail, BindQuality, Check, ConfClass, Current, EvidenceKind, Expand, GridPos, Group,
    GroupClass, GroupCode, Invalid, Live, Orientation, Popup, Press, Select, SemFlags, SemRole,
    SetPos, Sort,
};

/// Identity of a fact source: a slot recycled with a generation (M-8).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SourceId {
    pub slot: u32,
    pub generation: u32,
}

/// Per-source monotone sequence; orders facts within one source only (Lemma 1).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SrcSeq(pub u64);

/// Binding generation: the primary seq at which the anchor's current target acquired it
/// (ADR-0024 section 2.3.2). Stamped on a fact at admission.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BindGen(pub u32);

/// Namespace of an `AnchorKey::Exact` id (for example the AccessKit node id space).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct KeySpace(pub u16);

/// A frame within a document (`DocRef` frame); opaque to the model.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FrameKey(pub u64);

/// A top-level window reference for region anchors; opaque to the model.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct WindowRef(pub u64);

/// Identifies one capture a region fact was derived from.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CaptureId(pub u64);

/// Host-issued token for extension facts (ADR-0024 section 2.10, W-4).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SnapshotToken(pub u64);

/// Keyed content hash (ADR-0002 section 7.1), 128 bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ContentKey(pub u128);

/// How a non-primary source names a node (ADR-0024 section 2.3.1).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnchorKey {
    /// The same id in both sources (AccessKit node id in the AT-SPI path).
    Exact {
        space: KeySpace,
        id: u128,
    },
    /// An HTML `id`; `dom_count` is the add-on's count of that id in its frame and shadow root.
    DomId {
        frame: FrameKey,
        shadow: u64,
        id: Arc<str>,
        dom_count: u16,
    },
    /// Hashed ancestor path + role + name.
    Structural {
        path: u64,
        role: SemRole,
        name: u64,
    },
    /// A rectangle in window coordinates (GTK4: window only).
    Region {
        window: WindowRef,
        rect: Rect,
        capture: CaptureId,
    },
    Content(ContentKey),
}

/// What a fact is about. The scope of an `Anchor` is **not** in the fact: the transport pins it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Subject {
    /// Minted by a Primary source of this scope.
    Node(NodeKey),
    Anchor(AnchorKey),
}

/// A bound anchor: the node it resolves to, how well, and its generation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Binding {
    pub node: NodeKey,
    pub quality: BindQuality,
    pub generation: BindGen,
}

/// Calibrated confidence of a fact. The writer recomputes `class` from `p_milli` and the grant;
/// a source-supplied class is never trusted.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Confidence {
    pub class: ConfClass,
    /// Calibrated precision estimate, 0..=1000.
    pub p_milli: u16,
}

impl Confidence {
    /// Primary, Patch, User and untainted Derived facts.
    pub const ASSERTED: Confidence = Confidence {
        class: ConfClass::Asserted,
        p_milli: 1000,
    };

    /// Build from a precision estimate with the default thresholds.
    pub const fn from_p_milli(p_milli: u16) -> Confidence {
        Confidence {
            class: ConfClass::from_p_milli(p_milli),
            p_milli,
        }
    }
}

/// What a fact's freshness is judged against (ADR-0024 section 2.4).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Basis {
    None,
    VisRev(u32),
    DocRev(u64),
    Capture(CaptureId, u64),
    Snapshot(SnapshotToken),
}

/// A typed value for a semantic group.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PropValue {
    /// Role, including `aria-roledescription`.
    Role {
        role: SemRole,
        role_description: Option<Arc<str>>,
    },
    Name(Arc<str>),
    Description(Arc<str>),
    Check(Check),
    Press(Press),
    Expand(Expand),
    Select(Select),
    Current(Current),
    Invalid(Invalid),
    Popup(Popup),
    Avail(Avail),
    Flags(SemFlags),
    Level(u8),
    SetPos(SetPos),
    Grid(GridPos),
    Live(Option<Live>),
    Orientation(Orientation),
    Sort(Sort),
    /// Evidence payloads (never presented directly).
    Evidence(EvidenceValue),
}

/// Evidence payload shapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EvidenceValue {
    Text(Arc<str>),
    Box(Rect),
    Flag(bool),
}

impl PropValue {
    /// The group code this value belongs to, or `None` for evidence (its kind is in the fact).
    pub const fn group(&self) -> Option<Group> {
        Some(match self {
            PropValue::Role { .. } => Group::Role,
            PropValue::Name(_) => Group::Name,
            PropValue::Description(_) => Group::Description,
            PropValue::Check(_) => Group::Check,
            PropValue::Press(_) => Group::Press,
            PropValue::Expand(_) => Group::Expand,
            PropValue::Select(_) => Group::Select,
            PropValue::Current(_) => Group::Current,
            PropValue::Invalid(_) => Group::Invalid,
            PropValue::Popup(_) => Group::Popup,
            PropValue::Avail(_) => Group::Avail,
            PropValue::Flags(_) => Group::Flags,
            PropValue::Level(_) => Group::Level,
            PropValue::SetPos(_) => Group::SetPos,
            PropValue::Grid(_) => Group::Grid,
            PropValue::Live(_) => Group::Live,
            PropValue::Orientation(_) => Group::Orientation,
            PropValue::Sort(_) => Group::Sort,
            PropValue::Evidence(_) => return None,
        })
    }

    /// True when the value's shape matches the registered group code (admission "well formed"):
    /// a semantic value must belong to exactly that group, an evidence payload must have the
    /// shape its kind requires (text, box or flag).
    pub const fn matches(&self, code: GroupCode) -> bool {
        match (code.classify(), self) {
            (GroupClass::Semantic(want), v) => match v.group() {
                Some(have) => want.code() == have.code(),
                None => false,
            },
            (GroupClass::Evidence(kind), PropValue::Evidence(v)) => v.fits(kind),
            (GroupClass::Evidence(_) | GroupClass::Unregistered(_), _) => false,
        }
    }
}

impl EvidenceValue {
    /// True when this payload has the shape `kind` requires.
    pub const fn fits(&self, kind: EvidenceKind) -> bool {
        match (kind, self) {
            (
                EvidenceKind::HintIconClass
                | EvidenceKind::HintSrcName
                | EvidenceKind::HintTitleAttr
                | EvidenceKind::VisualOcrText
                | EvidenceKind::VisualLabelGuess,
                EvidenceValue::Text(_),
            )
            | (EvidenceKind::LayoutBox, EvidenceValue::Box(_))
            | (EvidenceKind::LayoutCssHidden, EvidenceValue::Flag(_)) => true,
            (
                EvidenceKind::HintIconClass
                | EvidenceKind::HintSrcName
                | EvidenceKind::HintTitleAttr
                | EvidenceKind::VisualOcrText
                | EvidenceKind::VisualLabelGuess
                | EvidenceKind::LayoutBox
                | EvidenceKind::LayoutCssHidden,
                EvidenceValue::Text(_) | EvidenceValue::Box(_) | EvidenceValue::Flag(_),
            ) => false,
        }
    }
}

/// The payload of a fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FactValue {
    /// Observed and the property is not present (a value, unlike Unknown).
    Absent,
    Value(PropValue),
    /// Back to Unknown.
    Retract,
}

/// One fact: a value for one group of one subject, from one source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fact {
    pub subject: Subject,
    pub group: GroupCode,
    pub value: FactValue,
    pub seq: SrcSeq,
    pub conf: Confidence,
    pub basis: Basis,
    pub ttl_ms: Option<u32>,
}

impl Fact {
    /// Shape check before admission looks at grants: the group code is registered and the value
    /// (when there is one) belongs to it. `Absent` and `Retract` carry no payload.
    pub const fn is_well_formed(&self) -> bool {
        match &self.value {
            FactValue::Absent | FactValue::Retract => self.group.is_registered(),
            FactValue::Value(v) => v.matches(self.group),
        }
    }
}

/// Convenience: a non-zero set size.
pub const fn set_size(n: u32) -> Option<NonZeroU32> {
    NonZeroU32::new(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AppId, ConnectorId, LocalId};

    fn node() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 7,
            },
            local: LocalId(9),
        }
    }

    #[test]
    fn prop_value_group_is_total_and_matches_its_code() {
        let cases: Vec<(PropValue, Group)> = vec![
            (
                PropValue::Role {
                    role: SemRole::Button,
                    role_description: None,
                },
                Group::Role,
            ),
            (PropValue::Name(Arc::from("x")), Group::Name),
            (PropValue::Description(Arc::from("d")), Group::Description),
            (PropValue::Check(Check::On), Group::Check),
            (PropValue::Press(Press::Off), Group::Press),
            (PropValue::Expand(Expand::Expanded), Group::Expand),
            (PropValue::Select(Select::On), Group::Select),
            (PropValue::Current(Current::Page), Group::Current),
            (PropValue::Invalid(Invalid::True), Group::Invalid),
            (PropValue::Popup(Popup::Menu), Group::Popup),
            (PropValue::Avail(Avail::Disabled), Group::Avail),
            (PropValue::Flags(SemFlags::BUSY), Group::Flags),
            (PropValue::Level(2), Group::Level),
            (
                PropValue::SetPos(SetPos {
                    pos: 1,
                    size: set_size(3),
                    src: crate::sem::PosSrc::Author,
                }),
                Group::SetPos,
            ),
            (
                PropValue::Grid(GridPos {
                    row: 1,
                    col: 1,
                    row_span: 1,
                    col_span: 1,
                }),
                Group::Grid,
            ),
            (PropValue::Live(None), Group::Live),
            (
                PropValue::Orientation(Orientation::Vertical),
                Group::Orientation,
            ),
            (PropValue::Sort(Sort::Ascending), Group::Sort),
        ];
        assert_eq!(
            cases.len(),
            Group::ALL.len(),
            "one case per registered group"
        );
        for (v, g) in cases {
            assert_eq!(v.group(), Some(g));
            assert!(v.matches(g.group_code()), "{g:?}");
            for other in Group::ALL.iter().filter(|o| **o != g) {
                assert!(!v.matches(other.group_code()), "{g:?} vs {other:?}");
            }
            assert!(!v.matches(EvidenceKind::LayoutBox.group_code()));
        }
        let ev = PropValue::Evidence(EvidenceValue::Flag(true));
        assert!(ev.matches(EvidenceKind::LayoutCssHidden.group_code()));
        assert!(!ev.matches(Group::Name.group_code()));
        assert!(!ev.matches(GroupCode(0x8123)));
    }

    fn fact(group: GroupCode, value: FactValue) -> Fact {
        Fact {
            subject: Subject::Node(node()),
            group,
            value,
            seq: SrcSeq(1),
            conf: Confidence::ASSERTED,
            basis: Basis::None,
            ttl_ms: None,
        }
    }

    #[test]
    fn evidence_payload_must_fit_its_kind() {
        let text = || PropValue::Evidence(EvidenceValue::Text(Arc::from("Close")));
        let flag = || PropValue::Evidence(EvidenceValue::Flag(true));
        let boxed = || PropValue::Evidence(EvidenceValue::Box(Rect::default()));
        for k in EvidenceKind::ALL {
            let (good, bad): (PropValue, [PropValue; 2]) = match k {
                EvidenceKind::LayoutBox => (boxed(), [text(), flag()]),
                EvidenceKind::LayoutCssHidden => (flag(), [text(), boxed()]),
                EvidenceKind::HintIconClass
                | EvidenceKind::HintSrcName
                | EvidenceKind::HintTitleAttr
                | EvidenceKind::VisualOcrText
                | EvidenceKind::VisualLabelGuess => (text(), [flag(), boxed()]),
            };
            assert!(good.matches(k.group_code()), "{k:?}");
            for b in bad {
                assert!(!b.matches(k.group_code()), "{k:?} accepted a wrong shape");
            }
        }
    }

    #[test]
    fn malformed_facts_are_rejected() {
        let name = || FactValue::Value(PropValue::Name(Arc::from("x")));
        assert!(fact(Group::Name.group_code(), name()).is_well_formed());
        // Value of another group.
        assert!(!fact(Group::Description.group_code(), name()).is_well_formed());
        // Semantic value under an evidence code and vice versa.
        assert!(!fact(EvidenceKind::HintTitleAttr.group_code(), name()).is_well_formed());
        let flag = FactValue::Value(PropValue::Evidence(EvidenceValue::Flag(true)));
        assert!(!fact(EvidenceKind::HintTitleAttr.group_code(), flag.clone()).is_well_formed());
        assert!(!fact(Group::Name.group_code(), flag.clone()).is_well_formed());
        assert!(fact(EvidenceKind::LayoutCssHidden.group_code(), flag).is_well_formed());
        // Unregistered codes, with or without payload.
        assert!(!fact(GroupCode(99), name()).is_well_formed());
        assert!(!fact(GroupCode(0x8100), FactValue::Retract).is_well_formed());
        assert!(fact(Group::Name.group_code(), FactValue::Absent).is_well_formed());
        assert!(fact(Group::Name.group_code(), FactValue::Retract).is_well_formed());
    }

    #[test]
    fn every_anchor_case_and_basis_constructs() {
        let anchors = [
            AnchorKey::Exact {
                space: KeySpace(1),
                id: 5,
            },
            AnchorKey::DomId {
                frame: FrameKey(1),
                shadow: 0,
                id: Arc::from("save"),
                dom_count: 1,
            },
            AnchorKey::Structural {
                path: 1,
                role: SemRole::Button,
                name: 2,
            },
            AnchorKey::Region {
                window: WindowRef(3),
                rect: Rect::default(),
                capture: CaptureId(4),
            },
            AnchorKey::Content(ContentKey(6)),
        ];
        for a in &anchors {
            assert_eq!(Subject::Anchor(a.clone()), Subject::Anchor(a.clone()));
        }
        let bases = [
            Basis::None,
            Basis::VisRev(1),
            Basis::DocRev(2),
            Basis::Capture(CaptureId(3), 4),
            Basis::Snapshot(SnapshotToken(5)),
        ];
        assert_eq!(bases.len(), 5);
        let f = Fact {
            subject: Subject::Node(node()),
            group: Group::Name.group_code(),
            value: FactValue::Value(PropValue::Name(Arc::from("Close"))),
            seq: SrcSeq(1),
            conf: Confidence::from_p_milli(950),
            basis: Basis::Snapshot(SnapshotToken(1)),
            ttl_ms: None,
        };
        assert_eq!(f.conf.class, ConfClass::Probable);
        assert_eq!(f.clone(), f);
    }

    #[test]
    fn source_id_distinguishes_generations() {
        let a = SourceId {
            slot: 2,
            generation: 0,
        };
        let b = SourceId {
            slot: 2,
            generation: 1,
        };
        assert_ne!(a, b);
        assert!(a < b);
    }
}
