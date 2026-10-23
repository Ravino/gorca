// SPDX-License-Identifier: GPL-3.0-or-later
//! `PresIr`: the presentation intermediate representation (ADR-0022 section 2.5).
//!
//! **Draft v0** (`PRES_IR_VERSION = 0`). The reviewer blocked the slice 1 freeze
//! (`docs/reports/m22b1-review.md` P-1..P-5); this is the corrected draft. The freeze to version 1
//! happens at the end of M22b1 slice 2. This crate is pure data: no runtime, no connector types,
//! so the plugin API crate may depend on it.
//!
//! Rules that survive the freeze (they are about the wire, not only about Rust):
//!
//! * **Open kinds.** `Reason`, `Content` and `FieldKind` are code based. A consumer that meets a
//!   code it does not know passes the item through **unedited** and never traps (`Reason::Unknown`,
//!   `Content::Opaque`, an unregistered `FieldKind`). Adding a kind is therefore not a break.
//! * **Words travel as catalog key strings** (`MsgId::key`), never as the build-order `MsgId`
//!   number, which moves when a key is added. `Content::from_wire_msg` resolves a key; an unknown key
//!   becomes `Content::Opaque`.
//! * **No prose with numbers baked in.** "2 of 5" is `Content::Msg { id, args }` with numeric
//!   arguments, so speech and braille format separately.
//! * **Add-on text is a catalog reference** (`Content::Ext { ns, key, args }`), resolved through the
//!   extension catalog at render. A `U+001E` reference never reaches a `Text`.
//! * **Add-on field kinds are host-allocated** per (namespace, name) by [`ExtKinds`]; a guest never
//!   picks a raw code.
//! * **Reserved bits** of `FieldFlags`/`IrFlags` are zero and preserved by editors.
//! * `Field` and `PresIr` are `#[non_exhaustive]`: build them through the constructors. `Reason`
//!   and `Content` are matched exhaustively; their open arms are `Unknown`/`Opaque`.
//!
//! A `PresIr` is built by `compose` from one fused `SemEntry`, edited by hooks, deduplicated
//! (PR-1) and only then rendered into tokens. `render` is the only producer of text.

use std::collections::HashMap;
use std::sync::Arc;

use gorca_i18n::MsgId;
use gorca_model::sem::GroupMask;
use gorca_model::{NodeKey, SegmentKind};
use smallvec::SmallVec;

/// Version of the record shapes below. 0 = draft, 1 = frozen (M22b1 slice 2).
pub const PRES_IR_VERSION: u32 = 0;

/// Why an utterance is composed (ADR-0022 section 2.5). Codes are append-only; an unknown code
/// is carried as [`Reason::Unknown`] and passed through.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Reason {
    Focus,
    FocusAncestor,
    /// The browse cursor reached the node's line; `entered` is true when it came from outside.
    BrowseLine {
        entered: bool,
    },
    QuickNav,
    SayAll,
    Review,
    /// A property changed; `changed` are the fused groups that differ.
    Change {
        changed: GroupMask,
    },
    ListEntry,
    WhereAmI,
    /// A reason of a newer host: edits must leave the IR unedited.
    Unknown(u16),
}

impl Reason {
    /// The wire code. Append-only.
    pub const fn code(self) -> u16 {
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
            Reason::Unknown(c) => c,
        }
    }

    /// From a wire code plus the two payloads (`entered` for code 2, `changed` bits for code 6).
    /// An unknown code is kept, not rejected.
    pub const fn from_code(code: u16, entered: bool, changed: u32) -> Reason {
        match code {
            0 => Reason::Focus,
            1 => Reason::FocusAncestor,
            2 => Reason::BrowseLine { entered },
            3 => Reason::QuickNav,
            4 => Reason::SayAll,
            5 => Reason::Review,
            6 => Reason::Change {
                changed: GroupMask(changed),
            },
            7 => Reason::ListEntry,
            8 => Reason::WhereAmI,
            c => Reason::Unknown(c),
        }
    }

    /// Whether a hook may edit an IR of this reason (an unknown reason is passed through).
    pub const fn editable(self) -> bool {
        !matches!(self, Reason::Unknown(_))
    }
}

/// What a field is. An open `u16` registry: codes are never reused or renumbered. Core codes are
/// `1..EXT_BASE`; `>= EXT_BASE` are allocated by the host per add-on namespace ([`ExtKinds`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct FieldKind(pub u16);

impl FieldKind {
    pub const NAME: FieldKind = FieldKind(1);
    pub const ROLE: FieldKind = FieldKind(2);
    pub const STATE: FieldKind = FieldKind(3);
    pub const VALUE: FieldKind = FieldKind(4);
    pub const DESCRIPTION: FieldKind = FieldKind(5);
    pub const LEVEL: FieldKind = FieldKind(6);
    pub const POSITION: FieldKind = FieldKind(7);
    pub const CURRENT: FieldKind = FieldKind(8);
    pub const ERROR_TEXT: FieldKind = FieldKind(9);
    pub const PLACEHOLDER: FieldKind = FieldKind(10);
    pub const SHORTCUT: FieldKind = FieldKind(11);
    pub const ROLE_DESCRIPTION: FieldKind = FieldKind(12);
    /// A label inferred from layout (lowest-trust fused fallback, C3b / M22b).
    pub const INFERRED_LABEL: FieldKind = FieldKind(13);
    /// First host-allocated add-on code.
    pub const EXT_BASE: u16 = 0x8000;

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

    /// How the speech lane tags the field's token. An unknown or add-on kind is plain text.
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

/// Why `ExtKinds::alloc` refused.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExtKindError {
    /// The add-on code range is used up.
    #[error("the add-on field kind range is full")]
    Full,
    /// The namespace or name is empty or too long.
    #[error("empty or over-long field kind namespace or name")]
    BadName,
}

/// Host-side allocator of add-on `FieldKind` codes, one per (namespace, name). Idempotent; codes
/// are stable for the process lifetime. A guest names a kind; it never picks the number.
#[derive(Debug, Default)]
pub struct ExtKinds {
    by_name: HashMap<(Arc<str>, Arc<str>), FieldKind>,
    next: u16,
}

impl ExtKinds {
    /// Longest namespace or name.
    pub const MAX_NAME: usize = 64;

    pub fn new() -> ExtKinds {
        ExtKinds::default()
    }

    /// The code of (`ns`, `name`), allocating it on first use.
    ///
    /// # Errors
    /// [`ExtKindError`] for an empty or long name, or when the range is full.
    pub fn alloc(&mut self, ns: &str, name: &str) -> Result<FieldKind, ExtKindError> {
        if ns.is_empty()
            || name.is_empty()
            || ns.len() > Self::MAX_NAME
            || name.len() > Self::MAX_NAME
        {
            return Err(ExtKindError::BadName);
        }
        let key: (Arc<str>, Arc<str>) = (Arc::from(ns), Arc::from(name));
        if let Some(k) = self.by_name.get(&key) {
            return Ok(*k);
        }
        let code = FieldKind::EXT_BASE
            .checked_add(self.next)
            .ok_or(ExtKindError::Full)?;
        self.next = self.next.checked_add(1).ok_or(ExtKindError::Full)?;
        let kind = FieldKind(code);
        self.by_name.insert(key, kind);
        Ok(kind)
    }

    /// Registry rows (`ns:name`, code), sorted by code, for the registry export.
    pub fn rows(&self) -> Vec<(String, u32)> {
        let mut v: Vec<_> = self
            .by_name
            .iter()
            .map(|((ns, n), k)| (format!("{ns}:{n}"), u32::from(k.0)))
            .collect();
        v.sort_by_key(|(_, c)| *c);
        v
    }
}

/// The rule-table row that produced a field (ADR-0022 section 2.5).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct RowId(pub u16);

/// Where a field came from: the "why was this said" chain starts here.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct Origin {
    pub row: RowId,
    /// The fused group slot that supplied the content (`Group` code), `0xFF` = none.
    pub group: u8,
    /// The patch, hook or quirk id that contributed or edited the field; `0` = none.
    pub via: u32,
}

impl Origin {
    pub const NO_GROUP: u8 = 0xFF;
}

/// An interned language (`Lang` index + 1; absent = the UI language).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LangId(pub u16);

/// Per-field flags. Reserved bits are zero and preserved by editors.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct FieldFlags(pub u16);

impl FieldFlags {
    pub const NONE: FieldFlags = FieldFlags(0);
    /// The content is secret (a password value): never shown to a plugin, a log or a braille
    /// display in clear.
    pub const SECRET: FieldFlags = FieldFlags(1);
    /// Spoken only at higher verbosity (dropped by the terse presets).
    pub const VERBOSE_ONLY: FieldFlags = FieldFlags(2);

    pub const fn contains(self, other: FieldFlags) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn with(self, other: FieldFlags) -> FieldFlags {
        FieldFlags(self.0 | other.0)
    }
}

/// Utterance-level flags. Reserved bits are zero.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct IrFlags(pub u16);

impl IrFlags {
    pub const NONE: IrFlags = IrFlags(0);
    /// The whole utterance is about a secret node.
    pub const SECRET: IrFlags = IrFlags(1);

    pub const fn contains(self, other: IrFlags) -> bool {
        self.0 & other.0 == other.0
    }
}

/// A message argument: numbers stay numbers so speech and braille format on their own.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Arg {
    pub name: Arc<str>,
    pub val: ArgVal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgVal {
    Num(i64),
    Text(Arc<str>),
}

impl Arg {
    pub fn num(name: &str, n: i64) -> Arg {
        Arg {
            name: Arc::from(name),
            val: ArgVal::Num(n),
        }
    }

    pub fn text(name: &str, t: &str) -> Arg {
        Arg {
            name: Arc::from(name),
            val: ArgVal::Text(Arc::from(t)),
        }
    }
}

/// An add-on's catalog reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtRef {
    pub ns: Arc<str>,
    pub key: Arc<str>,
    pub args: Box<[Arg]>,
}

/// The content of a field. Wire kinds: 0 text, 1 msg, 2 ext; any other code is [`Content::Opaque`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Content {
    /// Literal text: page or application content.
    Text(Arc<str>),
    /// A core catalog message with arguments.
    Msg { id: MsgId, args: Box<[Arg]> },
    /// An add-on catalog message.
    Ext(Box<ExtRef>),
    /// Content of a newer host: kept as is, renders as nothing.
    Opaque { code: u16 },
}

impl Content {
    /// A core word without arguments.
    pub fn word(id: MsgId) -> Content {
        Content::Msg {
            id,
            args: Box::default(),
        }
    }

    /// A core message with arguments.
    pub fn msg(id: MsgId, args: Vec<Arg>) -> Content {
        Content::Msg {
            id,
            args: args.into_boxed_slice(),
        }
    }

    /// An add-on message.
    pub fn ext(ns: &str, key: &str, args: Vec<Arg>) -> Content {
        Content::Ext(Box::new(ExtRef {
            ns: Arc::from(ns),
            key: Arc::from(key),
            args: args.into_boxed_slice(),
        }))
    }

    /// The wire kind code.
    pub const fn code(&self) -> u16 {
        match self {
            Content::Text(_) => 0,
            Content::Msg { .. } => 1,
            Content::Ext(_) => 2,
            Content::Opaque { code } => *code,
        }
    }

    /// The catalog key of a core message: the wire form of a word.
    pub fn msg_key(&self) -> Option<&'static str> {
        match self {
            Content::Msg { id, .. } => Some(id.key()),
            Content::Text(_) | Content::Ext(_) | Content::Opaque { .. } => None,
        }
    }

    /// A core message from its wire key. An unknown key (a newer host's) is opaque, not an error.
    pub fn from_wire_msg(key: &str, args: Vec<Arg>) -> Content {
        match MsgId::from_key(key) {
            Some(id) => Content::msg(id, args),
            None => Content::Opaque { code: 1 },
        }
    }
}

/// A range of the node's flat text a field was taken from (braille routing, M10).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct FlatSpan {
    pub start: u32,
    pub end: u32,
}

/// One part of an utterance.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Field {
    pub kind: FieldKind,
    pub flags: FieldFlags,
    pub lang: Option<LangId>,
    pub content: Content,
    pub origin: Origin,
    pub span: Option<FlatSpan>,
}

impl Field {
    pub fn new(kind: FieldKind, content: Content, origin: Origin) -> Field {
        Field {
            kind,
            flags: FieldFlags::NONE,
            lang: None,
            content,
            origin,
            span: None,
        }
    }

    #[must_use]
    pub fn with_flags(mut self, flags: FieldFlags) -> Field {
        self.flags = flags;
        self
    }

    #[must_use]
    pub fn with_lang(mut self, lang: LangId) -> Field {
        self.lang = Some(lang);
        self
    }

    pub fn is_secret(&self) -> bool {
        self.flags.contains(FieldFlags::SECRET)
    }
}

/// The presentation IR of one node and reason.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PresIr {
    pub reason: Reason,
    pub node: NodeKey,
    /// The `sem_rev` of the entry composed from (act-on-what-was-spoken guard, cache key).
    pub sem_rev: u32,
    /// The content was composed from stale data; a refine may follow (ADR-0001 section 8.1).
    pub may_refine: bool,
    pub flags: IrFlags,
    pub fields: SmallVec<[Field; 6]>,
}

impl PresIr {
    pub fn new(
        reason: Reason,
        node: NodeKey,
        sem_rev: u32,
        fields: SmallVec<[Field; 6]>,
    ) -> PresIr {
        PresIr {
            reason,
            node,
            sem_rev,
            may_refine: false,
            flags: IrFlags::NONE,
            fields,
        }
    }

    /// Fields of `kind`, in order.
    pub fn of_kind(&self, kind: FieldKind) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(move |f| f.kind == kind)
    }

    /// The copy a plugin, a log or a braille display may see: every secret field's content is
    /// replaced by empty text.
    #[must_use]
    pub fn redacted(&self) -> PresIr {
        let mut out = self.clone();
        for f in &mut out.fields {
            if f.is_secret() {
                f.content = Content::Text(Arc::from(""));
            }
        }
        out
    }
}

/// Registry rows (name, code) of the core `FieldKind` codes.
pub fn field_kind_rows() -> Vec<(&'static str, u32)> {
    (1..=13u16)
        .filter_map(|c| FieldKind(c).name().map(|n| (n, u32::from(c))))
        .collect()
}

#[cfg(test)]
mod tests {
    use gorca_model::{AppId, ConnectorId, LocalId};

    use super::*;

    fn node() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            local: LocalId(1),
        }
    }

    #[test]
    fn field_kind_codes_are_frozen() {
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
    fn reason_codes_round_trip_and_an_unknown_code_is_kept_not_rejected() {
        let all = [
            Reason::Focus,
            Reason::FocusAncestor,
            Reason::BrowseLine { entered: true },
            Reason::QuickNav,
            Reason::SayAll,
            Reason::Review,
            Reason::Change {
                changed: GroupMask(0b101),
            },
            Reason::ListEntry,
            Reason::WhereAmI,
        ];
        let codes: Vec<u16> = all.iter().map(|r| r.code()).collect();
        assert_eq!(codes, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
        for r in all {
            assert_eq!(Reason::from_code(r.code(), true, 0b101), r);
        }
        let u = Reason::from_code(999, false, 0);
        assert_eq!(u, Reason::Unknown(999));
        assert_eq!(u.code(), 999);
        assert!(!u.editable() && Reason::Focus.editable());
    }

    #[test]
    fn words_travel_as_catalog_key_strings_and_an_unknown_key_passes_through() {
        let c = Content::word(MsgId::StateChecked);
        let key = c.msg_key().expect("a core message has a key");
        assert_eq!(key, "state-checked");
        assert_eq!(Content::from_wire_msg(key, vec![]), c);
        assert_eq!(
            Content::from_wire_msg("no-such-key-from-a-newer-host", vec![]),
            Content::Opaque { code: 1 }
        );
        assert_eq!(c.code(), 1);
        assert_eq!(Content::Text("x".into()).code(), 0);
        assert_eq!(Content::ext("a", "b", vec![]).code(), 2);
        assert_eq!(Content::Opaque { code: 77 }.code(), 77);
    }

    #[test]
    fn parameterised_words_keep_numbers_as_numbers() {
        let c = Content::msg(MsgId::StateLevel, vec![Arg::num("l", 3)]);
        assert!(matches!(&c, Content::Msg { args, .. } if args[0].val == ArgVal::Num(3)));
    }

    #[test]
    fn add_on_field_kinds_are_host_allocated_per_namespace() {
        let mut k = ExtKinds::new();
        let a = k.alloc("telegram", "unread").expect("alloc");
        let b = k.alloc("signal", "unread").expect("alloc");
        assert_ne!(a, b, "the same name in two namespaces never merges");
        assert_eq!(k.alloc("telegram", "unread"), Ok(a), "idempotent");
        assert!(a.is_extension() && b.is_extension());
        assert_eq!(k.alloc("", "x"), Err(ExtKindError::BadName));
        assert_eq!(k.alloc("a", &"n".repeat(65)), Err(ExtKindError::BadName));
        assert_eq!(k.rows().len(), 2);
    }

    #[test]
    fn secret_fields_are_redacted_for_plugins() {
        let f = Field::new(
            FieldKind::VALUE,
            Content::Text("hunter2".into()),
            Origin::default(),
        )
        .with_flags(FieldFlags::SECRET);
        let name = Field::new(
            FieldKind::NAME,
            Content::Text("Password".into()),
            Origin::default(),
        );
        let ir = PresIr::new(Reason::Focus, node(), 1, [name, f].into_iter().collect());
        let r = ir.redacted();
        assert_eq!(r.fields[0].content, Content::Text("Password".into()));
        assert_eq!(r.fields[1].content, Content::Text("".into()));
        assert!(r.fields[1].is_secret(), "the flag survives redaction");
        assert_eq!(ir.fields[1].content, Content::Text("hunter2".into()));
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
    fn record_sizes_budget() {
        // Re-budgeted for flags (2 B), lang (4 B) and the boxed message arguments: a field is
        // copied into hook inputs. Measured 56 B (was 48 B in the first draft): kind 2, flags 2,
        // lang 4, content 24 (message arguments are boxed), origin 12, span 12.
        let f = std::mem::size_of::<Field>();
        let c = std::mem::size_of::<Content>();
        eprintln!("size_of Field = {f} B, Content = {c} B");
        assert!(c <= 32, "{c}");
        assert!(f <= 56, "{f}");
    }
}
