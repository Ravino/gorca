// SPDX-License-Identifier: GPL-3.0-or-later
//! `PresIr`: the presentation intermediate representation (ADR-0022 section 2.5).
//!
//! **Frozen at M22b1 slice 1 (`PRES_IR_VERSION` 1).** ADR-0026 (`present-edit` hook, WIT 0.9)
//! builds on exactly these records. The freeze rules:
//!
//! * a record is never reshaped; a field is never removed or reordered;
//! * a new `FieldKind` code, `Reason` or `Content` variant is an append (new codes only, never
//!   reused); the registries are open on the extension side (codes `>= EXT_BASE`);
//! * every change that is not an append bumps `PRES_IR_VERSION` and needs an ADR amendment.
//!
//! A `PresIr` is built by `compose` from one fused `SemEntry`, edited by hooks (add, drop,
//! reorder fields), deduplicated (PR-1) and only then rendered into tokens. Words are catalog
//! ids; `render` is the only producer of text.

use std::sync::Arc;

use gorca_i18n::MsgId;
use gorca_model::sem::GroupMask;
use gorca_model::{NodeKey, SegmentKind};
use smallvec::SmallVec;

/// Version of the frozen record shapes below.
pub const PRES_IR_VERSION: u32 = 1;

/// Why an utterance is composed (ADR-0022 section 2.5). Codes are append-only.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Reason {
    /// The focus moved to the node.
    Focus,
    /// An ancestor entered or left on a focus move (container context).
    FocusAncestor,
    /// The browse cursor reached the node's line; `entered` is true when it came from outside.
    BrowseLine { entered: bool },
    /// Structural quick navigation.
    QuickNav,
    /// Continuous reading.
    SayAll,
    /// Review cursor or object navigation.
    Review,
    /// A property of the node changed; `changed` are the fused groups that differ.
    Change { changed: GroupMask },
    /// A list row, composed lazily.
    ListEntry,
    /// The where-am-I / object-info key.
    WhereAmI,
}

impl Reason {
    /// The wire code (WIT `reason` variant discriminant). Append-only.
    pub const fn code(self) -> u8 {
        match self {
            Reason::Focus => 0,
            Reason::FocusAncestor => 1,
            Reason::BrowseLine { .. } => 2,
            Reason::QuickNav => 3,
            Reason::SayAll => 4,
            Reason::Review => 5,
            Reason::Change { .. } => 6,
            Reason::ListEntry => 7,
            Reason::WhereAmI => 8,
        }
    }
}

/// What a field is. An open `u16` code registry: codes are never reused or renumbered, and
/// `>= EXT_BASE` is reserved for add-ons (their codes render through `Content::Text` only).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FieldKind(pub u16);

impl FieldKind {
    /// The accessible name (PR-1: at most one per node and utterance).
    pub const NAME: FieldKind = FieldKind(1);
    /// The role word (PR-1: at most one per node and utterance).
    pub const ROLE: FieldKind = FieldKind(2);
    /// One state word (`checked`, `expanded`, ...).
    pub const STATE: FieldKind = FieldKind(3);
    /// The value (range current value, value text).
    pub const VALUE: FieldKind = FieldKind(4);
    /// The accessible description or a field's help text.
    pub const DESCRIPTION: FieldKind = FieldKind(5);
    /// A heading or tree level phrase.
    pub const LEVEL: FieldKind = FieldKind(6);
    /// Position in a set ("2 of 5").
    pub const POSITION: FieldKind = FieldKind(7);
    /// `aria-current`.
    pub const CURRENT: FieldKind = FieldKind(8);
    /// Error text of an invalid field.
    pub const ERROR_TEXT: FieldKind = FieldKind(9);
    /// Placeholder text.
    pub const PLACEHOLDER: FieldKind = FieldKind(10);
    /// Key shortcut.
    pub const SHORTCUT: FieldKind = FieldKind(11);
    /// Author role description (`aria-roledescription`, PR-5).
    pub const ROLE_DESCRIPTION: FieldKind = FieldKind(12);
    /// A label inferred from layout (lowest-trust fused fallback, C3b / M22b).
    pub const INFERRED_LABEL: FieldKind = FieldKind(13);
    /// First code of the add-on range.
    pub const EXT_BASE: u16 = 0x8000;

    /// Whether the code belongs to the add-on range.
    pub const fn is_extension(self) -> bool {
        self.0 >= FieldKind::EXT_BASE
    }

    /// The registry name of a core code.
    pub const fn name(self) -> Option<&'static str> {
        Some(match self.0 {
            1 => "name",
            2 => "role",
            3 => "state",
            4 => "value",
            5 => "description",
            6 => "level",
            7 => "position",
            8 => "current",
            9 => "error-text",
            10 => "placeholder",
            11 => "shortcut",
            12 => "role-description",
            13 => "inferred-label",
            _ => return None,
        })
    }

    /// How the speech lane tags the field's token. `description_pause` is the
    /// `browse.description_pause` setting (a separate sentence for descriptions).
    pub const fn segment_kind(self, description_pause: bool) -> SegmentKind {
        match self.0 {
            2 | 12 => SegmentKind::Role,
            3 | 8 => SegmentKind::State,
            4 => SegmentKind::Value,
            5 if description_pause => SegmentKind::Description,
            _ => SegmentKind::Text,
        }
    }
}

/// The rule-table row that produced a field (ADR-0022 section 2.5).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct RowId(pub u16);

/// Where a field came from: the "why was this said" chain starts here.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Origin {
    /// The rule row.
    pub row: RowId,
    /// The fused group slot that supplied the content (`Group` code), `0xFF` = none.
    pub group: u8,
    /// The patch, hook or quirk id that contributed or edited the field; `0` = none.
    pub via: u32,
}

impl Origin {
    /// `group` value of a field not backed by one fused group.
    pub const NO_GROUP: u8 = 0xFF;
}

/// The content of a field: a catalog word or literal text. Append-only.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Content {
    /// A catalog message without arguments.
    Word(MsgId),
    /// Literal text: page or application content, or an add-on's string.
    Text(Arc<str>),
}

/// A range of the node's flat text a field was taken from (braille routing, M10).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FlatSpan {
    pub start: u32,
    pub end: u32,
}

/// One part of an utterance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    pub kind: FieldKind,
    pub content: Content,
    pub origin: Origin,
    /// `None` until the flat-text builder supplies spans (M10).
    pub span: Option<FlatSpan>,
}

impl Field {
    /// A field of a core `kind` with `content` and `origin`, without a span.
    pub fn new(kind: FieldKind, content: Content, origin: Origin) -> Field {
        Field {
            kind,
            content,
            origin,
            span: None,
        }
    }
}

/// The presentation IR of one node and reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PresIr {
    pub reason: Reason,
    pub node: NodeKey,
    /// The `sem_rev` of the entry composed from (act-on-what-was-spoken guard, cache key).
    pub sem_rev: u32,
    /// The content was composed from stale data; a refine may follow (ADR-0001 section 8.1).
    pub may_refine: bool,
    pub fields: SmallVec<[Field; 6]>,
}

impl PresIr {
    /// Fields of `kind`, in order.
    pub fn of_kind(&self, kind: FieldKind) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(move |f| f.kind == kind)
    }
}

/// Registry rows (name, code) of the core `FieldKind` codes, for the append-only snapshot test
/// and the WIT registry export.
pub fn field_kind_rows() -> Vec<(&'static str, u32)> {
    (1..=13u16)
        .filter_map(|c| FieldKind(c).name().map(|n| (n, u32::from(c))))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_kind_codes_are_frozen() {
        // The append-only snapshot: a code is never reused or renumbered.
        let want: &[(&str, u32)] = &[
            ("name", 1),
            ("role", 2),
            ("state", 3),
            ("value", 4),
            ("description", 5),
            ("level", 6),
            ("position", 7),
            ("current", 8),
            ("error-text", 9),
            ("placeholder", 10),
            ("shortcut", 11),
            ("role-description", 12),
            ("inferred-label", 13),
        ];
        assert_eq!(field_kind_rows(), want);
        assert_eq!(FieldKind(14).name(), None);
        assert!(FieldKind(0x8000).is_extension());
        assert!(!FieldKind::INFERRED_LABEL.is_extension());
    }

    #[test]
    fn reason_codes_are_frozen() {
        let codes = [
            Reason::Focus,
            Reason::FocusAncestor,
            Reason::BrowseLine { entered: true },
            Reason::QuickNav,
            Reason::SayAll,
            Reason::Review,
            Reason::Change {
                changed: GroupMask::default(),
            },
            Reason::ListEntry,
            Reason::WhereAmI,
        ]
        .map(Reason::code);
        assert_eq!(codes, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            Reason::BrowseLine { entered: false }.code(),
            Reason::BrowseLine { entered: true }.code()
        );
    }

    #[test]
    fn segment_kinds_follow_the_field_kind() {
        assert_eq!(FieldKind::NAME.segment_kind(false), SegmentKind::Text);
        assert_eq!(FieldKind::ROLE.segment_kind(false), SegmentKind::Role);
        assert_eq!(FieldKind::STATE.segment_kind(false), SegmentKind::State);
        assert_eq!(FieldKind::VALUE.segment_kind(false), SegmentKind::Value);
        assert_eq!(
            FieldKind::DESCRIPTION.segment_kind(false),
            SegmentKind::Text
        );
        assert_eq!(
            FieldKind::DESCRIPTION.segment_kind(true),
            SegmentKind::Description
        );
        assert_eq!(FieldKind(0x8001).segment_kind(true), SegmentKind::Text);
    }

    #[test]
    fn record_sizes_stay_small() {
        // A field is copied into hook inputs: keep it a few words.
        assert!(
            std::mem::size_of::<Field>() <= 48,
            "{}",
            std::mem::size_of::<Field>()
        );
    }
}
