// SPDX-License-Identifier: GPL-3.0-or-later
//! `PresIr`: the presentation intermediate representation (ADR-0022 section 2.5).
//!
//! **Frozen at version 1** (`PRES_IR_VERSION = 1`, end of M22b1 slice 2). The slice 1 draft was
//! blocked by review (`docs/reports/m22b1-review.md` P-1..P-5); the corrected shape was exercised by
//! the native corpus replay (0 differences outside `deviations.toml`). This crate is pure data: no
//! runtime, no connector types, so the plugin API crate may depend on it. A change that is not an
//! append of a code (a reshaped record, a removed or reordered field) bumps the version and amends
//! ADR-0022; the `shape_*` tests below fail to compile or run on such a change.
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
//! * **Host-only by type.** `ExtKinds`, `ExtNs` binding and `LangTags` exist only with the `host`
//!   cargo feature, which only host crates (`gorca-present`) enable. The plugin API crate depends
//!   on this crate without it, so a guest-facing build cannot even name them.
//! * **Add-on field kinds are host-allocated** per (namespace, name) by [`ExtKinds`]; a guest never
//!   picks a raw code.
//! * **Reserved bits** of `FieldFlags`/`IrFlags` are zero and preserved by editors.
//! * `Field` and `PresIr` are `#[non_exhaustive]`: build them through the constructors. `Reason`
//!   and `Content` are matched exhaustively; their open arms are `Unknown`/`Opaque`.
//!
//! A `PresIr` is built by `compose` from one fused `SemEntry`, edited by hooks, deduplicated
//! (PR-1) and only then rendered into tokens. `render` is the only producer of text.

#[cfg(feature = "host")]
use std::collections::HashMap;
use std::sync::Arc;

use gorca_i18n::MsgId;
use gorca_model::sem::GroupMask;
use gorca_model::{NodeKey, SegmentKind};
use smallvec::SmallVec;

/// Version of the record shapes below. 1 = frozen (M22b1 slice 2).
pub const PRES_IR_VERSION: u32 = 1;

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

/// Why `ExtKinds` refused.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExtKindError {
    /// The add-on code range is used up.
    #[error("the add-on field kind range is full")]
    Full,
    /// This namespace has used its quota.
    #[error("the namespace has used its field kind quota")]
    Quota,
    /// The namespace or name is empty or too long.
    #[error("empty or over-long field kind namespace or name")]
    BadName,
}

/// A namespace bound by the host to one add-on (its manifest id). Only the host can make one
/// ([`ExtKinds::bind`]); the guest-facing allocation takes this token, never a string, so an add-on
/// cannot claim another add-on's namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtNs(Arc<str>);

impl ExtNs {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Host-side allocator of add-on `FieldKind` codes, one per (namespace, name). Idempotent; codes
/// are stable for the process lifetime. A guest names a kind; it never picks the number, and each
/// namespace has a quota so one add-on cannot use up the range.
#[cfg(feature = "host")]
#[derive(Debug, Default)]
pub struct ExtKinds {
    by_name: HashMap<(Arc<str>, Arc<str>), FieldKind>,
    used: HashMap<Arc<str>, usize>,
    next: u16,
}

#[cfg(feature = "host")]
impl ExtKinds {
    /// Longest namespace or name.
    pub const MAX_NAME: usize = 64;
    /// Kinds one namespace may allocate.
    pub const NS_QUOTA: usize = 256;

    pub fn new() -> ExtKinds {
        ExtKinds::default()
    }

    /// Bind `manifest_id` (host knowledge, never guest input) as a namespace token.
    ///
    /// # Errors
    /// [`ExtKindError::BadName`] for an empty or over-long id.
    pub fn bind(&self, manifest_id: &str) -> Result<ExtNs, ExtKindError> {
        if manifest_id.is_empty() || manifest_id.len() > Self::MAX_NAME {
            return Err(ExtKindError::BadName);
        }
        Ok(ExtNs(Arc::from(manifest_id)))
    }

    /// The code of (`ns`, `name`), allocating it on first use.
    ///
    /// # Errors
    /// [`ExtKindError`] for an empty or long name, the namespace quota, or a full range.
    pub fn alloc(&mut self, ns: &ExtNs, name: &str) -> Result<FieldKind, ExtKindError> {
        if name.is_empty() || name.len() > Self::MAX_NAME {
            return Err(ExtKindError::BadName);
        }
        let key: (Arc<str>, Arc<str>) = (Arc::clone(&ns.0), Arc::from(name));
        if let Some(k) = self.by_name.get(&key) {
            return Ok(*k);
        }
        let used = self.used.get(&ns.0).copied().unwrap_or(0);
        if used >= Self::NS_QUOTA {
            return Err(ExtKindError::Quota);
        }
        let code = FieldKind::EXT_BASE
            .checked_add(self.next)
            .ok_or(ExtKindError::Full)?;
        self.next = self.next.checked_add(1).ok_or(ExtKindError::Full)?;
        let kind = FieldKind(code);
        self.by_name.insert(key, kind);
        self.used.insert(Arc::clone(&ns.0), used + 1);
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

/// A host-interned content language. It names the language of a field's *content* (an HTML
/// `lang="de"` paragraph, an application's declared language) as a BCP-47 tag, and is **not** an
/// index into the UI catalog. The number is meaningful only inside one host process; on the wire a
/// language travels as its tag string ([`LangTags::tag`] / [`LangTags::intern`]). Absent = the UI
/// language. An unparsable or unknown tag is absent, never an error.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct LangId(pub u16);

/// Host-side interner of content-language tags.
#[cfg(feature = "host")]
#[derive(Debug, Default)]
pub struct LangTags {
    tags: Vec<Arc<str>>,
}

#[cfg(feature = "host")]
impl LangTags {
    /// Most distinct tags held (a hostile page cannot grow the table).
    pub const MAX_TAGS: usize = 1024;
    /// Longest accepted tag (RFC 5646 recommends supporting at least 35 characters).
    pub const MAX_LEN: usize = 35;

    pub fn new() -> LangTags {
        LangTags::default()
    }

    /// Intern `tag` (case-folded to lower case, `_` read as `-`). `None` for a tag that is empty,
    /// too long, not ASCII alphanumeric subtags, or when the table is full.
    pub fn intern(&mut self, tag: &str) -> Option<LangId> {
        let t = tag.trim();
        let ok = !t.is_empty()
            && t.len() <= Self::MAX_LEN
            && t.split(['-', '_']).all(|sub| {
                (1..=8).contains(&sub.len()) && sub.bytes().all(|b| b.is_ascii_alphanumeric())
            });
        if !ok {
            return None;
        }
        let norm = t.replace('_', "-").to_ascii_lowercase();
        if let Some(i) = self.tags.iter().position(|x| **x == *norm) {
            return u16::try_from(i + 1).ok().map(LangId);
        }
        if self.tags.len() >= Self::MAX_TAGS {
            return None;
        }
        self.tags.push(Arc::from(norm));
        u16::try_from(self.tags.len()).ok().map(LangId)
    }

    /// The tag string of `id` (the wire form), `None` for an id this table did not hand out.
    pub fn tag(&self, id: LangId) -> Option<&str> {
        usize::from(id.0)
            .checked_sub(1)
            .and_then(|i| self.tags.get(i))
            .map(|t| &**t)
    }
}

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
    /// An argument kind of a newer host (a float, a date, a nested message): kept as is, the
    /// argument is left out when formatting. Codes: 0 num, 1 text; anything else is opaque.
    Opaque {
        code: u16,
    },
}

impl ArgVal {
    /// The wire kind code.
    pub const fn code(&self) -> u16 {
        match self {
            ArgVal::Num(_) => 0,
            ArgVal::Text(_) => 1,
            ArgVal::Opaque { code } => *code,
        }
    }
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
    /// The opaque code of a message whose key this host does not know (never a kind code).
    pub const UNKNOWN_KEY: u16 = 0xFFFF;

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
            None => Content::Opaque {
                code: Content::UNKNOWN_KEY,
            },
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
            Content::Opaque {
                code: Content::UNKNOWN_KEY
            }
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

    #[cfg(feature = "host")]
    #[test]
    fn add_on_field_kinds_are_host_allocated_per_bound_namespace() {
        let mut k = ExtKinds::new();
        let tg = k.bind("telegram").expect("bind");
        let sg = k.bind("signal").expect("bind");
        let a = k.alloc(&tg, "unread").expect("alloc");
        let b = k.alloc(&sg, "unread").expect("alloc");
        assert_ne!(a, b, "the same name in two namespaces never merges");
        assert_eq!(k.alloc(&tg, "unread"), Ok(a), "idempotent");
        assert!(a.is_extension() && b.is_extension());
        assert_eq!(k.bind(""), Err(ExtKindError::BadName));
        assert_eq!(k.alloc(&tg, ""), Err(ExtKindError::BadName));
        assert_eq!(k.alloc(&tg, &"n".repeat(65)), Err(ExtKindError::BadName));
        assert_eq!(k.rows().len(), 2);
    }

    #[cfg(feature = "host")]
    #[test]
    fn one_namespace_cannot_use_up_the_range() {
        let mut k = ExtKinds::new();
        let greedy = k.bind("greedy").expect("bind");
        for i in 0..ExtKinds::NS_QUOTA {
            assert!(k.alloc(&greedy, &format!("k{i}")).is_ok());
        }
        assert_eq!(k.alloc(&greedy, "one-more"), Err(ExtKindError::Quota));
        // An existing name is still served, and another namespace is unaffected.
        assert!(k.alloc(&greedy, "k0").is_ok());
        let other = k.bind("other").expect("bind");
        assert!(k.alloc(&other, "x").is_ok());
    }

    #[cfg(feature = "host")]
    #[test]
    fn a_content_language_is_an_interned_bcp47_tag_that_travels_as_a_string() {
        let mut t = LangTags::new();
        let de = t.intern("de").expect("de");
        let de_ch = t.intern("de_CH").expect("de-CH");
        assert_ne!(de, de_ch);
        assert_eq!(t.tag(de), Some("de"));
        assert_eq!(
            t.tag(de_ch),
            Some("de-ch"),
            "case-folded, underscore read as a hyphen"
        );
        assert_eq!(t.intern("DE"), Some(de), "interning is idempotent");
        assert_eq!(t.intern(""), None);
        assert_eq!(t.intern("not a tag"), None);
        assert_eq!(t.intern(&"a".repeat(36)), None);
        assert_eq!(t.tag(LangId(0)), None);
        assert_eq!(
            t.tag(LangId(999)),
            None,
            "an id this table did not hand out is absent"
        );
        // `ru` is not special: a content language is not a UI catalog index.
        assert_eq!(t.intern("ru").and_then(|id| t.tag(id)), Some("ru"));
    }

    #[test]
    fn an_unknown_argument_kind_is_opaque_and_keeps_its_code() {
        assert_eq!(ArgVal::Num(1).code(), 0);
        assert_eq!(ArgVal::Text("x".into()).code(), 1);
        let o = ArgVal::Opaque { code: 9 };
        assert_eq!(o.code(), 9);
        assert_eq!(o.clone(), o);
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
    fn shape_of_the_records_is_frozen() {
        // Every record is destructured exhaustively and each binding is pinned to its type: a new,
        // removed or renamed field, or a widened integer, fails to compile. That is the signal to
        // bump PRES_IR_VERSION and amend ADR-0022.
        let f = Field::new(
            FieldKind::NAME,
            Content::Text("x".into()),
            Origin::default(),
        );
        let Field {
            kind,
            flags,
            lang,
            content,
            origin,
            span,
        } = f;
        let _: FieldKind = kind;
        let _: FieldFlags = flags;
        let _: Option<LangId> = lang;
        let _: Content = content;
        let _: Option<FlatSpan> = span;
        let Origin { row, group, via } = origin;
        let _: RowId = row;
        let _: u8 = group;
        let _: u32 = via;
        let ir = PresIr::new(Reason::Focus, node(), 0, SmallVec::new());
        let PresIr {
            reason,
            node: n,
            sem_rev,
            may_refine,
            flags,
            fields,
        } = ir;
        let _: Reason = reason;
        let _: NodeKey = n;
        let _: u32 = sem_rev;
        let _: bool = may_refine;
        let _: IrFlags = flags;
        let _: SmallVec<[Field; 6]> = fields;
        let Arg { name, val } = Arg::num("n", 1);
        let _: Arc<str> = name;
        let _: ArgVal = val;
        let FlatSpan { start, end } = FlatSpan { start: 0, end: 1 };
        let _: (u32, u32) = (start, end);
        let LangId(l) = LangId(1);
        let _: u16 = l;
        let RowId(r) = RowId(1);
        let _: u16 = r;
        let FieldFlags(ff) = FieldFlags::NONE;
        let _: u16 = ff;
        let IrFlags(irf) = IrFlags::NONE;
        let _: u16 = irf;
        let FieldKind(k) = FieldKind::NAME;
        let _: u16 = k;
        let ExtRef { ns, key, args } = *match Content::ext("a", "b", vec![]) {
            Content::Ext(r) => r,
            Content::Text(_) | Content::Msg { .. } | Content::Opaque { .. } => {
                unreachable!("built as Ext above")
            }
        };
        let _: (Arc<str>, Arc<str>, Box<[Arg]>) = (ns, key, args);
        // The variants of the open enums, exhaustively (no wildcard).
        let _ = |r: Reason| match r {
            Reason::Focus
            | Reason::FocusAncestor
            | Reason::BrowseLine { entered: _ }
            | Reason::QuickNav
            | Reason::SayAll
            | Reason::Review
            | Reason::Change { changed: _ }
            | Reason::ListEntry
            | Reason::WhereAmI
            | Reason::Unknown(_) => (),
        };
        let _ = |a: ArgVal| match a {
            ArgVal::Num(_) | ArgVal::Text(_) | ArgVal::Opaque { code: _ } => (),
        };
        // Exact sizes, not bounds: any growth is a deliberate decision.
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::size_of::<Field>(), 56);
            assert_eq!(std::mem::size_of::<Content>(), 24);
            assert_eq!(std::mem::size_of::<Arg>(), 40);
            assert_eq!(std::mem::size_of::<ArgVal>(), 24);
        }
        assert_eq!(std::mem::size_of::<FlatSpan>(), 8);
        assert_eq!(std::mem::size_of::<LangId>(), 2);
        assert_eq!(std::mem::size_of::<Origin>(), 8);
        assert_eq!(std::mem::size_of::<FieldFlags>(), 2);
        if cfg!(target_pointer_width = "64") {}
        assert_eq!(PRES_IR_VERSION, 1);
    }

    #[test]
    fn an_unregistered_field_kind_and_opaque_content_pass_through_unchanged() {
        let odd = Field::new(
            FieldKind(0x7123),
            Content::Opaque { code: 40 },
            Origin::default(),
        );
        let ir = PresIr::new(
            Reason::Unknown(77),
            node(),
            3,
            [odd.clone()].into_iter().collect(),
        );
        assert_eq!(ir.redacted(), ir, "nothing secret: the copy is identical");
        assert_eq!(ir.fields[0], odd);
        assert_eq!(FieldKind(0x7123).name(), None);
        assert!(!ir.reason.editable());
    }

    #[test]
    fn record_sizes_budget() {
        // Re-budgeted for flags (2 B), lang (4 B) and the boxed message arguments: a field is
        // copied into hook inputs. Measured 56 B (was 48 B in the first draft): kind 2, flags 2,
        // lang 4, content 24 (message arguments are boxed), origin 12, span 12.
        let f = std::mem::size_of::<Field>();
        let c = std::mem::size_of::<Content>();
        eprintln!("size_of Field = {f} B, Content = {c} B");
        assert!(c <= 24, "{c}");
        assert!(f <= 56, "{f}");
    }
}
