// SPDX-License-Identifier: GPL-3.0-or-later
//! `SrcRecord` and [`normalise`]: raw AT-SPI values to one normalised source record
//! (ADR-0022 section 2.2, ADR-0024 section 2.1, M22a-B4).
//!
//! # Unknown vs Absent
//!
//! The caller says which parts it fetched ([`PropMask`]). A group whose inputs were all fetched is
//! **known**; if the engine gave no value the group holds its default (that is *Absent*: the
//! engine was asked and said nothing). A group with an input that was not fetched is **unknown**
//! (clear bit in [`SrcRecord::known`]) and holds its default only as a placeholder. Group
//! dependencies:
//!
//! | Group | Needs |
//! |---|---|
//! | role | ROLE (the `xml-roles` token is used when ATTRS was fetched too) |
//! | name, description | NAME, DESCRIPTION |
//! | check, press, select, avail | STATES and ROLE |
//! | expand, orientation, invalid, flags | STATES |
//! | popup | ATTRS, or STATES when HAS_POPUP is clear |
//! | current, sort, setpos, grid | ATTRS |
//! | level, live | ATTRS and ROLE |
//!
//! The text fields of [`Semantics::text`] other than the description (placeholder, key shortcuts,
//! value text, role description) are filled whenever ATTRS was fetched; they have no group slot
//! of their own, they ride with `Description`.

use std::sync::Arc;

use gorca_model::PropMask;
use gorca_model::sem::{
    Group, GroupMask, Invalid, NameFrom, Orientation, Popup, SemFlags, SemRole, Semantics, Sort,
};

use crate::attrs::{
    self, ParsedAttrs, current_of, grid_of, has_details, invalid_of, level_of, live_of,
    name_from_of, orientation_of, popup_of, setpos_of, sort_of, text_of,
};
use crate::roles::{
    EngineRole, RoleSource, TokenPick, engine_role, is_weak_engine_role, pick_token,
    raw as raw_role, resolve_role, tag_role,
};
use crate::states::{StateBits, bit, map_states};

/// One raw node as the connector read it. Borrowed: the normaliser copies only what it keeps.
#[derive(Copy, Clone, Debug)]
pub struct RawNode<'a> {
    /// Which parts below were actually fetched. An unfetched part is **Unknown** in the record.
    pub fetched: PropMask,
    /// `AtspiRole` number.
    pub role: u32,
    /// State set.
    pub states: StateBits,
    /// `GetAttributes` pairs, in wire order.
    pub attrs: &'a [(&'a str, &'a str)],
    /// Accessible name; `None` or empty is Absent when NAME was fetched.
    pub name: Option<&'a str>,
    /// Accessible description.
    pub description: Option<&'a str>,
}

/// Diagnostics of one normalisation, for logs, `explain` and the M-10 coverage counters.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Notes(pub u16);

impl Notes {
    /// The raw role has no model role (`SemRole::Unknown` unless a token decided).
    pub const UNMAPPED_ENGINE_ROLE: Notes = Notes(1 << 0);
    /// `xml-roles` held a token that is not a valid author role (skipped).
    pub const INVALID_ROLE_TOKEN: Notes = Notes(1 << 1);
    /// `xml-roles` held a valid ARIA role the model does not carry (skipped).
    pub const UNMAPPED_ROLE_TOKEN: Notes = Notes(1 << 2);
    /// ROLE was fetched without ATTRS: the role is the engine's, a token could not be consulted.
    pub const TOKEN_UNAVAILABLE: Notes = Notes(1 << 3);
    /// `aria-current` had an unrecognised value (treated as `true`).
    pub const INVALID_CURRENT_TOKEN: Notes = Notes(1 << 4);
    /// `aria-haspopup` had an unrecognised value (treated as `menu`).
    pub const INVALID_POPUP_TOKEN: Notes = Notes(1 << 5);
    /// INVALID_ENTRY on a REQUIRED web field without an `invalid` attribute: engines raise it for
    /// an empty required field, so it is not taken as `aria-invalid`.
    pub const INVALID_ENTRY_DISCOUNTED: Notes = Notes(1 << 6);
    /// A numeric attribute (level, posinset, setsize, index) did not parse.
    pub const MALFORMED_NUMBER: Notes = Notes(1 << 7);
    /// A tag longer than [`TagName::CAP`] bytes was truncated.
    pub const TAG_TRUNCATED: Notes = Notes(1 << 8);

    /// True when every bit of `other` is set.
    pub const fn contains(self, other: Notes) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no note was raised.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn add(&mut self, other: Notes) {
        self.0 |= other.0;
    }
}

/// An HTML tag name stored inline (no allocation). Longer names (custom elements) are truncated
/// at a character boundary; [`Notes::TAG_TRUNCATED`] records it.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct TagName {
    len: u8,
    bytes: [u8; TagName::CAP],
}

impl TagName {
    /// Capacity in bytes.
    pub const CAP: usize = 15;

    /// Build from a tag, truncating at a character boundary. Returns the tag and whether it was
    /// truncated.
    pub fn new(tag: &str) -> (TagName, bool) {
        let mut end = tag.len().min(Self::CAP);
        while !tag.is_char_boundary(end) {
            end -= 1;
        }
        let mut bytes = [0u8; Self::CAP];
        bytes[..end].copy_from_slice(&tag.as_bytes()[..end]);
        let len = u8::try_from(end).unwrap_or(0);
        (TagName { len, bytes }, end < tag.len())
    }

    /// The tag; empty when the node has none.
    pub fn as_str(&self) -> &str {
        let n = usize::from(self.len).min(Self::CAP);
        std::str::from_utf8(&self.bytes[..n]).unwrap_or("")
    }

    /// True when the node had no tag.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// `aria-colindextext` / `aria-rowindextext`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexText {
    pub col: Option<Arc<str>>,
    pub row: Option<Arc<str>>,
}

/// The normalised source record of one node from the AT-SPI source: per-group values with the
/// `known` mask (Unknown vs Absent), plus the pieces S3 and the quirk stratum read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SrcRecord {
    /// Per-group values. A group with a clear `known` bit holds its default.
    pub sem: Semantics,
    /// The accessible name when known and non-empty.
    pub name: Option<Arc<str>>,
    /// Groups this record has an opinion on (fetched), including Absent ones.
    pub known: GroupMask,
    /// The raw `AtspiRole` the record was built from (kept for quirk rows and logs).
    pub raw_role: u32,
    /// Which side decided the role.
    pub role_source: RoleSource,
    /// The DOM tag, when the engine exports it.
    pub tag: TagName,
    /// Column and row index texts, when exported.
    pub index_text: Option<Arc<IndexText>>,
    /// Diagnostics.
    pub notes: Notes,
}

/// Normalise one raw node. Pure, total, allocation only for the strings the record keeps.
pub fn normalise(raw: &RawNode<'_>) -> SrcRecord {
    let f = raw.fetched;
    let have_role = f.contains(PropMask::ROLE);
    let have_states = f.contains(PropMask::STATES);
    let have_attrs = f.contains(PropMask::ATTRS);
    let have_name = f.contains(PropMask::NAME);
    let have_desc = f.contains(PropMask::DESCRIPTION);

    let a = if have_attrs {
        ParsedAttrs::parse(raw.attrs)
    } else {
        ParsedAttrs::default()
    };
    let st = if have_states {
        raw.states
    } else {
        StateBits::default()
    };

    let mut rec = SrcRecord {
        raw_role: raw.role,
        ..SrcRecord::default()
    };
    let mut known = GroupMask::EMPTY;
    let mut notes = Notes::default();

    // ---- role -------------------------------------------------------------------------------
    let mut role = SemRole::Unknown;
    if have_role {
        known.set(Group::Role, true);
        let (r, source, role_notes) = resolve_node_role(raw.role, &a, st, have_attrs);
        role = r;
        rec.role_source = source;
        notes.add(role_notes);
    }

    // ---- name, description ------------------------------------------------------------------
    let name_nonempty = raw.name.is_some_and(|n| !n.is_empty());
    if have_name {
        known.set(Group::Name, true);
        rec.name = raw.name.filter(|n| !n.is_empty()).map(Arc::from);
    }
    if have_desc {
        known.set(Group::Description, true);
    }

    // ---- states -----------------------------------------------------------------------------
    let mut sem = Semantics {
        role,
        ..Semantics::default()
    };
    let engine_toggle = raw.role == raw_role::TOGGLE_BUTTON;
    let engine_text_field = matches!(
        raw.role,
        raw_role::ENTRY | raw_role::TEXT | raw_role::PASSWORD_TEXT
    );
    let mut has_popup_state = false;
    let mut invalid_entry = false;
    let mut required = false;
    if have_states {
        let g = map_states(role, engine_toggle, engine_text_field, st);
        sem.expand = g.expand;
        sem.orientation = g.orientation;
        sem.flags = g.flags;
        known.set(Group::Expand, true);
        known.set(Group::Orientation, true);
        known.set(Group::Flags, true);
        if have_role {
            sem.check = g.check;
            sem.press = g.press;
            sem.select = g.select;
            sem.avail = g.avail;
            known.set(Group::Check, true);
            known.set(Group::Press, true);
            known.set(Group::Select, true);
            known.set(Group::Avail, true);
        }
        has_popup_state = g.has_popup;
        invalid_entry = g.invalid_entry;
        required = g.required;
        // `invalid` is decided below; the state alone is known.
        known.set(Group::Invalid, true);
    }

    // ---- attributes -------------------------------------------------------------------------
    if have_attrs {
        apply_attrs(&mut sem, &a, role, have_role, have_states, &mut notes);
        if let Some(t) = a.tag {
            let (tag, truncated) = TagName::new(t);
            rec.tag = tag;
            if truncated {
                notes.add(Notes::TAG_TRUNCATED);
            }
        }
        if a.rowindextext.is_some() || a.colindextext.is_some() {
            rec.index_text = Some(Arc::new(IndexText {
                col: a.colindextext.filter(|s| !s.is_empty()).map(Arc::from),
                row: a.rowindextext.filter(|s| !s.is_empty()).map(Arc::from),
            }));
        }
        known.set(Group::Current, true);
        known.set(Group::Sort, true);
        known.set(Group::SetPos, true);
        known.set(Group::Grid, true);
        if have_role {
            known.set(Group::Level, true);
            known.set(Group::Live, true);
        }
        // Orientation falls back to the attribute when the states say nothing.
        if have_states
            && sem.orientation == Orientation::NotApplicable
            && let Some(o) = a.orientation
        {
            sem.orientation = orientation_of(o);
        }
        sem.name_from = if have_name {
            name_from_of(&a, !name_nonempty)
        } else {
            NameFrom::None
        };
    } else if have_name && !name_nonempty {
        sem.name_from = NameFrom::None;
    }

    // ---- popup ------------------------------------------------------------------------------
    let popup_attr = if have_attrs { a.haspopup } else { None };
    if let Some(v) = popup_attr {
        let (p, bad) = popup_of(v);
        sem.popup = p;
        if bad {
            notes.add(Notes::INVALID_POPUP_TOKEN);
        }
        known.set(Group::Popup, true);
    } else if have_attrs {
        // Fetched, no attribute: the state alone means the ARIA default `true` (= menu).
        sem.popup = if has_popup_state && have_states {
            Popup::Menu
        } else {
            Popup::No
        };
        known.set(Group::Popup, true);
    } else if have_states && !has_popup_state {
        sem.popup = Popup::No;
        known.set(Group::Popup, true);
    }

    // ---- invalid ----------------------------------------------------------------------------
    if have_states {
        let attr_kind = if have_attrs {
            a.invalid.and_then(invalid_of)
        } else {
            None
        };
        sem.invalid = match attr_kind {
            Some(k) => k,
            None if invalid_entry => {
                // Gecko and Blink raise INVALID_ENTRY on an empty required field and an
                // unchecked required checkbox (HTML validity), not only for aria-invalid.
                if required && a.is_web() {
                    notes.add(Notes::INVALID_ENTRY_DISCOUNTED);
                    Invalid::No
                } else {
                    Invalid::True
                }
            }
            None => Invalid::No,
        };
    }

    // ---- text, flags from attributes --------------------------------------------------------
    if have_attrs || have_desc {
        let desc = if have_desc { raw.description } else { None };
        sem.text = text_of(desc, &a);
    }

    rec.sem = sem;
    rec.known = known;
    rec.notes = notes;
    rec
}

/// Resolve the role of a node: engine role, optional `xml-roles` token, the engine-vs-ARIA rule,
/// and the two web refinements (search box, layout table).
fn resolve_node_role(
    raw: u32,
    a: &ParsedAttrs<'_>,
    st: StateBits,
    have_attrs: bool,
) -> (SemRole, RoleSource, Notes) {
    let mut notes = Notes::default();
    let mut engine = match engine_role(raw) {
        EngineRole::Role(r) => r,
        EngineRole::Unmapped => {
            notes.add(Notes::UNMAPPED_ENGINE_ROLE);
            SemRole::Unknown
        }
    };
    let pick = if have_attrs {
        a.xml_roles.map(pick_token)
    } else {
        notes.add(Notes::TOKEN_UNAVAILABLE);
        None
    };
    let token = match pick {
        Some(TokenPick {
            role,
            saw_invalid,
            saw_unmapped,
        }) => {
            if saw_invalid {
                notes.add(Notes::INVALID_ROLE_TOKEN);
            }
            if saw_unmapped {
                notes.add(Notes::UNMAPPED_ROLE_TOKEN);
            }
            role
        }
        None => None,
    };
    if token.is_none() {
        // Without an author role, the DOM gives two refinements the AT-SPI role cannot.
        if engine == SemRole::TextBox && a.text_input_type == Some("search") {
            engine = SemRole::SearchBox;
        }
        if engine == SemRole::Table && a.layout_guess == Some("true") {
            engine = SemRole::Presentation;
        }
        if is_weak_engine_role(engine)
            && let Some(r) = a.tag.and_then(tag_role)
        {
            engine = r;
        }
        // A selectable GTK list row is an option of its listbox (web list items are not).
        if engine == SemRole::ListItem && !a.is_web() && st.has(bit::SELECTABLE) {
            engine = SemRole::Option;
        }
    }
    let (role, source) = resolve_role(engine, token, st.has(bit::FOCUSABLE));
    (role, source, notes)
}

/// Attribute-derived groups.
fn apply_attrs(
    sem: &mut Semantics,
    a: &ParsedAttrs<'_>,
    role: SemRole,
    have_role: bool,
    have_states: bool,
    notes: &mut Notes,
) {
    let (cur, bad) = current_of(a.current);
    sem.current = cur;
    if bad {
        notes.add(Notes::INVALID_CURRENT_TOKEN);
    }
    sem.sort = a.sort.map_or(Sort::None, sort_of);
    sem.pos = setpos_of(a);
    sem.grid = grid_of(a);
    if have_role {
        sem.level = level_of(role, a);
        sem.live = live_of(role, a);
    }
    for v in [a.level, a.posinset, a.rowindex, a.colindex] {
        if v.is_some_and(|s| attrs::parse_u32(s).is_none()) {
            notes.add(Notes::MALFORMED_NUMBER);
        }
    }
    if have_states {
        if attrs::parse_bool(a.busy) == Some(true) {
            sem.flags |= SemFlags::BUSY;
        }
        if has_details(a) {
            sem.flags |= SemFlags::HAS_DETAILS;
        }
        if role == SemRole::Presentation && a.layout_guess == Some("true") {
            sem.flags |= SemFlags::LAYOUT_TABLE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::sem::{Avail, Check, Current, Expand, Press, Select};

    fn st(list: &[u8]) -> StateBits {
        StateBits(list.iter().fold(0u64, |a, b| a | (1u64 << b)))
    }

    fn all() -> PropMask {
        PropMask::ROLE | PropMask::STATES | PropMask::ATTRS | PropMask::NAME | PropMask::DESCRIPTION
    }

    fn node<'a>(
        fetched: PropMask,
        role: u32,
        states: StateBits,
        attrs: &'a [(&'a str, &'a str)],
    ) -> RawNode<'a> {
        RawNode {
            fetched,
            role,
            states,
            attrs,
            name: Some("N"),
            description: None,
        }
    }

    #[test]
    fn unfetched_parts_are_unknown_not_absent() {
        // Only ROLE fetched: every state/attr group is unknown.
        let r = normalise(&node(PropMask::ROLE, 43, st(&[bit::ENABLED]), &[]));
        assert!(r.known.has(Group::Role));
        for g in [
            Group::Check,
            Group::Expand,
            Group::Avail,
            Group::Flags,
            Group::Level,
            Group::Live,
            Group::Current,
        ] {
            assert!(!r.known.has(g), "{g:?} must be unknown");
        }
        assert!(!r.known.has(Group::Name), "NAME not fetched");
        assert!(r.notes.contains(Notes::TOKEN_UNAVAILABLE));
        assert_eq!(r.sem.role, SemRole::Button);

        // All fetched, nothing exported: known + default (Absent).
        let r = normalise(&node(all(), 43, StateBits(0), &[]));
        for g in [
            Group::Check,
            Group::Press,
            Group::Expand,
            Group::Current,
            Group::Level,
            Group::Popup,
            Group::Invalid,
            Group::Sort,
            Group::SetPos,
            Group::Grid,
            Group::Live,
            Group::Flags,
        ] {
            assert!(r.known.has(g), "{g:?} must be known");
        }
        assert_eq!(r.sem.check, Check::NotCheckable);
        assert_eq!(r.sem.current, Current::No);
        assert_eq!(r.sem.live, None);

        // STATES without ROLE: role-gated groups stay unknown, role-free ones are known.
        let r = normalise(&node(PropMask::STATES, 43, st(&[bit::CHECKED]), &[]));
        assert!(
            !r.known.has(Group::Role) && !r.known.has(Group::Check) && !r.known.has(Group::Avail)
        );
        assert!(r.known.has(Group::Expand) && r.known.has(Group::Flags));
        assert_eq!(r.sem.role, SemRole::Unknown);
    }

    #[test]
    fn name_known_even_when_empty() {
        let mut n = node(PropMask::NAME, 43, StateBits(0), &[]);
        n.name = Some("");
        let r = normalise(&n);
        assert!(r.known.has(Group::Name));
        assert_eq!(r.name, None);
        assert_eq!(r.sem.name_from, NameFrom::None);
        n.name = Some("Save");
        let r = normalise(&n);
        assert_eq!(r.name.as_deref(), Some("Save"));
    }

    #[test]
    fn token_wins_and_multi_token_falls_back() {
        let attrs = [("xml-roles", "bogus switch"), ("tag", "div")];
        let r = normalise(&node(
            all(),
            62,
            st(&[bit::CHECKED, bit::FOCUSABLE]),
            &attrs,
        ));
        assert_eq!(r.sem.role, SemRole::Switch);
        assert_eq!(r.role_source, RoleSource::Token);
        assert!(r.notes.contains(Notes::INVALID_ROLE_TOKEN));
        assert_eq!(r.sem.check, Check::On);
        assert_eq!(r.tag.as_str(), "div");

        let attrs = [("xml-roles", "widget")];
        let r = normalise(&node(all(), 7, StateBits(0), &attrs));
        assert_eq!(r.sem.role, SemRole::Checkbox, "abstract token ignored");
        assert_eq!(r.role_source, RoleSource::Engine);
    }

    #[test]
    fn invalid_and_unmapped_tokens_fall_back_to_the_engine_role() {
        let r = normalise(&node(all(), 31, StateBits(0), &[("xml-roles", "doc-toc")]));
        assert_eq!(r.sem.role, SemRole::List);
        assert!(r.notes.contains(Notes::UNMAPPED_ROLE_TOKEN));
        let r = normalise(&node(all(), 9999, StateBits(0), &[]));
        assert_eq!(r.sem.role, SemRole::Unknown);
        assert!(r.notes.contains(Notes::UNMAPPED_ENGINE_ROLE));
    }

    #[test]
    fn presentation_conflict_and_password() {
        let a = [("xml-roles", "presentation")];
        let r = normalise(&node(all(), 27, StateBits(0), &a));
        assert_eq!(r.sem.role, SemRole::Presentation);
        let r = normalise(&node(all(), 27, st(&[bit::FOCUSABLE]), &a));
        assert_eq!(r.sem.role, SemRole::Img);
        assert_eq!(r.role_source, RoleSource::EngineOverToken);
        let r = normalise(&node(all(), 40, StateBits(0), &[("xml-roles", "textbox")]));
        assert_eq!(r.sem.role, SemRole::PasswordBox);
    }

    #[test]
    fn search_box_and_layout_table_refinements() {
        let r = normalise(&node(
            all(),
            79,
            st(&[bit::EDITABLE]),
            &[("text-input-type", "search"), ("tag", "input")],
        ));
        assert_eq!(r.sem.role, SemRole::SearchBox);
        let r = normalise(&node(
            all(),
            79,
            st(&[bit::EDITABLE]),
            &[("text-input-type", "text")],
        ));
        assert_eq!(r.sem.role, SemRole::TextBox);
        let r = normalise(&node(all(), 55, StateBits(0), &[("layout-guess", "true")]));
        assert_eq!(r.sem.role, SemRole::Presentation);
        assert!(r.sem.flags.contains(SemFlags::LAYOUT_TABLE));
        let r = normalise(&node(
            all(),
            55,
            StateBits(0),
            &[("layout-guess", "true"), ("xml-roles", "table")],
        ));
        assert_eq!(
            r.sem.role,
            SemRole::Table,
            "an authored table is never a layout table"
        );
    }

    #[test]
    fn state_groups_through_the_record() {
        let r = normalise(&node(
            all(),
            7,
            st(&[bit::INDETERMINATE, bit::CHECKABLE, bit::ENABLED]),
            &[],
        ));
        assert_eq!(r.sem.check, Check::Mixed);
        assert_eq!(r.sem.avail, Avail::Enabled);
        let r = normalise(&node(all(), 62, st(&[bit::PRESSED]), &[]));
        assert_eq!((r.sem.role, r.sem.press), (SemRole::Button, Press::On));
        let r = normalise(&node(all(), 37, st(&[bit::SELECTABLE, bit::SELECTED]), &[]));
        assert_eq!((r.sem.role, r.sem.select), (SemRole::Tab, Select::On));
        let r = normalise(&node(all(), 43, st(&[bit::EXPANDABLE, bit::EXPANDED]), &[]));
        assert_eq!(r.sem.expand, Expand::Expanded);
    }

    #[test]
    fn attribute_groups_through_the_record() {
        let a = [
            ("tag", "h3"),
            ("level", "3"),
            ("posinset", "2"),
            ("setsize", "7"),
            ("current", "step"),
            ("haspopup", "dialog"),
            ("sort", "descending"),
            ("keyshortcuts", "Alt+S"),
            ("colindextext", "Column C"),
            ("rowindextext", "Row 3"),
            ("busy", "true"),
        ];
        let r = normalise(&node(all(), 83, st(&[bit::ENABLED]), &a));
        assert_eq!(r.sem.role, SemRole::Heading);
        assert_eq!(r.sem.level, 3);
        assert_eq!(r.sem.pos.map(|p| p.pos), Some(2));
        assert_eq!(r.sem.current, Current::Step);
        assert_eq!(r.sem.popup, Popup::Dialog);
        assert_eq!(r.sem.sort, Sort::Descending);
        assert_eq!(
            r.sem
                .text
                .as_deref()
                .and_then(|t| t.key_shortcuts.as_deref()),
            Some("Alt+S")
        );
        assert_eq!(
            r.index_text.as_deref().and_then(|t| t.col.as_deref()),
            Some("Column C")
        );
        assert_eq!(
            r.index_text.as_deref().and_then(|t| t.row.as_deref()),
            Some("Row 3")
        );
        assert!(r.sem.flags.contains(SemFlags::BUSY));
    }

    #[test]
    fn popup_from_state_defaults_to_menu() {
        let r = normalise(&node(
            all(),
            43,
            st(&[bit::HAS_POPUP]),
            &[("tag", "button")],
        ));
        assert_eq!(r.sem.popup, Popup::Menu);
        // STATES only: a set HAS_POPUP leaves the kind unknown, a clear one is a definite No.
        let r = normalise(&node(
            PropMask::STATES | PropMask::ROLE,
            43,
            st(&[bit::HAS_POPUP]),
            &[],
        ));
        assert!(!r.known.has(Group::Popup));
        let r = normalise(&node(
            PropMask::STATES | PropMask::ROLE,
            43,
            StateBits(0),
            &[],
        ));
        assert!(r.known.has(Group::Popup));
        assert_eq!(r.sem.popup, Popup::No);
    }

    #[test]
    fn current_invalid_token_is_true_and_noted() {
        let r = normalise(&node(all(), 88, StateBits(0), &[("current", "bogus")]));
        assert_eq!(r.sem.current, Current::True);
        assert!(r.notes.contains(Notes::INVALID_CURRENT_TOKEN));
    }

    #[test]
    fn invalid_entry_is_not_aria_invalid_on_required_web_fields() {
        let web = [("tag", "input")];
        let r = normalise(&node(
            all(),
            79,
            st(&[bit::INVALID_ENTRY, bit::REQUIRED]),
            &web,
        ));
        assert_eq!(r.sem.invalid, Invalid::No);
        assert!(r.notes.contains(Notes::INVALID_ENTRY_DISCOUNTED));
        let r = normalise(&node(all(), 79, st(&[bit::INVALID_ENTRY]), &web));
        assert_eq!(r.sem.invalid, Invalid::True);
        // Not web (GTK): the state is the whole truth.
        let r = normalise(&node(
            all(),
            79,
            st(&[bit::INVALID_ENTRY, bit::REQUIRED]),
            &[("toolkit", "GTK")],
        ));
        assert_eq!(r.sem.invalid, Invalid::True);
        // An exported kind wins.
        let r = normalise(&node(
            all(),
            79,
            st(&[bit::INVALID_ENTRY, bit::REQUIRED]),
            &[("tag", "input"), ("invalid", "spelling")],
        ));
        assert_eq!(r.sem.invalid, Invalid::Spelling);
    }

    #[test]
    fn live_roles_through_the_record() {
        let r = normalise(&node(all(), 2, StateBits(0), &[("xml-roles", "alert")]));
        assert_eq!(r.sem.live.map(|l| (l.atomic, l.root)), Some((true, true)));
    }

    #[test]
    fn orientation_attribute_fills_in_for_missing_states() {
        let r = normalise(&node(
            all(),
            63,
            StateBits(0),
            &[("orientation", "vertical")],
        ));
        assert_eq!(r.sem.orientation, Orientation::Vertical);
    }

    #[test]
    fn long_tags_truncate_on_a_char_boundary() {
        let (t, cut) = TagName::new("my-custom-element-with-long-name");
        assert!(cut);
        assert_eq!(t.as_str(), "my-custom-eleme");
        let (t, cut) = TagName::new("h1");
        assert!(!cut);
        assert_eq!(t.as_str(), "h1");
        let (t, cut) = TagName::new("ééééééééé"); // 18 bytes
        assert!(cut);
        assert_eq!(
            t.as_str().chars().count(),
            7,
            "14 bytes: never split a scalar"
        );
        assert!(TagName::default().is_empty());
    }

    #[test]
    fn description_and_text_fields() {
        let mut n = node(
            all(),
            43,
            StateBits(0),
            &[("roledescription", "slide toggle")],
        );
        n.description = Some("Hint");
        let r = normalise(&n);
        assert!(r.known.has(Group::Description));
        assert_eq!(r.sem.description(), Some("Hint"));
        assert_eq!(r.sem.role_description(), Some("slide toggle"));
        // Description not fetched: not known, not stored, even if the caller passed a value.
        let mut n = node(PropMask::ROLE | PropMask::ATTRS, 43, StateBits(0), &[]);
        n.description = Some("Hint");
        let r = normalise(&n);
        assert!(!r.known.has(Group::Description));
        assert_eq!(r.sem.description(), None);
    }

    #[test]
    fn normalise_is_deterministic() {
        let a = [
            ("xml-roles", "button"),
            ("tag", "button"),
            ("pressed", "true"),
        ];
        let n = node(all(), 62, st(&[bit::PRESSED, bit::ENABLED]), &a);
        assert_eq!(normalise(&n), normalise(&n));
    }
}
