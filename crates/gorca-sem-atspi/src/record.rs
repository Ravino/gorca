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
//! | roledescription, placeholder, keyshortcuts, valuetext | ATTRS |
//! | namefrom | NAME, and ATTRS unless the name is empty |
//!
//! The text fields of [`Semantics::text`] other than the description (placeholder, key shortcuts,
//! value text, role description) are filled whenever ATTRS was fetched, and each has a group of its
//! own (M22a-C3, review W-6), known exactly when ATTRS was fetched; `namefrom` needs NAME, and
//! ATTRS unless the name is empty.

use gorca_model::PropMask;
use gorca_model::sem::{
    Group, GroupMask, Invalid, NameFrom, Orientation, Popup, SemFlags, SemRole, Semantics, Sort,
};
pub use gorca_model::sem::{Notes, RoleSource, SrcRecord, TagName};

use crate::attrs::{
    self, ParsedAttrs, arc_capped, current_of, grid_of, has_details, invalid_of, level_of, live_of,
    name_from_of, orientation_of, popup_of, setpos_of, sort_of, text_of,
};
use crate::roles::{
    EngineRole, TokenPick, engine_role, is_weak_engine_role, pick_token, raw as raw_role,
    resolve_role, tag_role,
};
use crate::states::{StateBits, bit, map_states_for};

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
        let mut cut = false;
        rec.name = arc_capped(raw.name, &mut cut);
        if cut {
            notes.add(Notes::TRUNCATED);
        }
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
        let g = map_states_for(role, engine_toggle, engine_text_field, a.is_web(), st);
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
        // The rare text (below) and the name source come from attributes; the name source also
        // needs the name itself.
        known.set(Group::RoleDescription, true);
        known.set(Group::Placeholder, true);
        known.set(Group::KeyShortcuts, true);
        known.set(Group::ValueText, true);
        known.set(Group::ErrorText, true);
        known.set(Group::NameFrom, have_name);
    } else if have_name && !name_nonempty {
        sem.name_from = NameFrom::None;
        known.set(Group::NameFrom, true);
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
                // Gecko and Blink also raise INVALID_ENTRY on an empty required field and an
                // unchecked required checkbox (HTML validity). Keep the fact, mark the case.
                if required && a.is_web() {
                    notes.add(Notes::INVALID_ENTRY_ON_REQUIRED);
                }
                Invalid::True
            }
            None => Invalid::No,
        };
    }

    // ---- text, flags from attributes --------------------------------------------------------
    if have_attrs || have_desc {
        let desc = if have_desc { raw.description } else { None };
        let (text, cut) = text_of(desc, &a);
        sem.text = text;
        if cut {
            notes.add(Notes::TRUNCATED);
        }
    }

    if have_attrs {
        // The HTML `id` is not in `WANTED_ATTRS` (the app connector keeps only what the groups
        // read); the document source hands over every attribute it holds, `id` among them.
        let id = raw.attrs.iter().find(|(k, _)| *k == "id").map(|(_, v)| *v);
        let mut cut = false;
        rec.dom_id = arc_capped(id, &mut cut);
        if cut {
            notes.add(Notes::TRUNCATED);
        }
    }

    if have_role && rec.role_source == RoleSource::Token {
        sem.flags |= SemFlags::ROLE_AUTHORED;
        // The bit is a fact of the role, not of the states: a node whose states were not fetched
        // (the document source reads states of the stateful roles only) still carries it, so the
        // flags group is written with it. Without the states it holds nothing else.
        known.set(Group::Flags, true);
    }
    // The engine's `label` role is the one static text the model keeps apart from the engine's
    // static text, for object review. A role the author's token replaced is not a label.
    if have_role && rec.raw_role == raw_role::LABEL && sem.role == SemRole::StaticText {
        sem.flags |= SemFlags::TEXT_LABEL;
        known.set(Group::Flags, true);
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
        // The engines export an `<input type=date>` (and time, month, week) as a date editor, which
        // maps to a group; a form input is an editable field, not a group of things.
        if engine == SemRole::Group && a.tag == Some("input") {
            engine = SemRole::TextBox;
        }
        if engine == SemRole::Table && a.layout_guess == Some("true") {
            engine = SemRole::Presentation;
        }
        if is_weak_engine_role(engine)
            && let Some(r) = a.tag.and_then(tag_role)
        {
            engine = r;
        }
        // A web `<option>` is an option whatever list role the engine gave it.
        if engine == SemRole::ListItem && a.tag == Some("option") {
            engine = SemRole::Option;
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
    fn an_authored_role_is_known_even_when_the_states_were_not_fetched() {
        // The document source reads states of the stateful roles only; a tab list (38) is not one.
        let attrs = [("xml-roles", "tablist")];
        let fetched = PropMask::ROLE | PropMask::ATTRS | PropMask::NAME;
        let r = normalise(&node(fetched, 38, StateBits(0), &attrs));
        assert_eq!(r.sem.role, SemRole::TabList);
        assert!(r.sem.flags.contains(SemFlags::ROLE_AUTHORED));
        assert!(
            r.known.has(Group::Flags),
            "the authored bit travels with the role"
        );
        // Without an author role the group stays unknown.
        let r = normalise(&node(fetched, 38, StateBits(0), &[]));
        assert!(!r.known.has(Group::Flags));
        assert!(!r.sem.flags.contains(SemFlags::ROLE_AUTHORED));
    }

    #[test]
    fn the_engines_label_role_is_a_text_label_and_its_static_text_is_not() {
        let fetched = PropMask::ROLE | PropMask::NAME;
        // ATSPI_ROLE_LABEL (29) and ATSPI_ROLE_STATIC (116) are one model role.
        let label = normalise(&node(fetched, raw_role::LABEL, StateBits(0), &[]));
        let run = normalise(&node(fetched, 116, StateBits(0), &[]));
        assert_eq!(label.sem.role, SemRole::StaticText);
        assert_eq!(run.sem.role, SemRole::StaticText);
        assert!(label.sem.flags.contains(SemFlags::TEXT_LABEL));
        assert!(
            label.known.has(Group::Flags),
            "the bit travels with the role"
        );
        assert!(!run.sem.flags.contains(SemFlags::TEXT_LABEL));
        assert!(!run.known.has(Group::Flags));
        // The role was not fetched: nothing is claimed.
        let blind = normalise(&node(PropMask::NAME, raw_role::LABEL, StateBits(0), &[]));
        assert!(!blind.sem.flags.contains(SemFlags::TEXT_LABEL));
        // The author's token replaced the role: it is a button now, not a label.
        let attrs = [("xml-roles", "button")];
        let fetched = PropMask::ROLE | PropMask::ATTRS | PropMask::NAME;
        let tok = normalise(&node(fetched, raw_role::LABEL, StateBits(0), &attrs));
        assert!(!tok.sem.flags.contains(SemFlags::TEXT_LABEL));
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
    fn a_huge_name_is_capped_and_noted() {
        let big = "n".repeat(1_000_000);
        let mut n = node(PropMask::NAME, 43, StateBits(0), &[]);
        n.name = Some(big.as_str());
        let r = normalise(&n);
        assert_eq!(
            r.name.as_deref().map(str::len),
            Some(crate::attrs::PAGE_STRING_CAP)
        );
        assert!(r.notes.contains(Notes::TRUNCATED));
        n.name = Some("short");
        assert!(!normalise(&n).notes.contains(Notes::TRUNCATED));
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
    fn a_web_date_input_is_a_text_box_not_a_group() {
        // AT-SPI role 12 is the date editor; the DOM tag says it is an input.
        let r = normalise(&node(all(), 12, st(&[bit::FOCUSABLE]), &[("tag", "input")]));
        assert_eq!(r.sem.role, SemRole::TextBox);
        let r = normalise(&node(all(), 12, st(&[bit::FOCUSABLE]), &[("tag", "div")]));
        assert_eq!(r.sem.role, SemRole::Group);
    }

    #[test]
    fn state_groups_through_the_record() {
        let r = normalise(&node(
            all(),
            7,
            st(&[
                bit::INDETERMINATE,
                bit::CHECKABLE,
                bit::ENABLED,
                bit::SENSITIVE,
            ]),
            &[],
        ));
        assert_eq!(r.sem.check, Check::Mixed);
        assert_eq!(r.sem.avail, Avail::Enabled);
        // A native node without SENSITIVE is unavailable even when it still reports ENABLED.
        let r = normalise(&node(all(), 7, st(&[bit::CHECKABLE, bit::ENABLED]), &[]));
        assert_eq!(r.sem.avail, Avail::Disabled);
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
            r.sem
                .text
                .as_deref()
                .and_then(|t| t.col_index_text.as_deref()),
            Some("Column C")
        );
        assert_eq!(
            r.sem
                .text
                .as_deref()
                .and_then(|t| t.row_index_text.as_deref()),
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
    fn invalid_entry_on_required_web_fields_stays_invalid_and_is_marked() {
        // A filled `type=email required` holding "abc" raises INVALID_ENTRY and must stay
        // invalid; the note lets presentation suppress only the empty-required case.
        let web = [("tag", "input"), ("text-input-type", "email")];
        let r = normalise(&node(
            all(),
            79,
            st(&[bit::INVALID_ENTRY, bit::REQUIRED]),
            &web,
        ));
        assert_eq!(r.sem.invalid, Invalid::True);
        assert!(r.notes.contains(Notes::INVALID_ENTRY_ON_REQUIRED));
        let r = normalise(&node(all(), 79, st(&[bit::INVALID_ENTRY]), &web));
        assert_eq!(r.sem.invalid, Invalid::True);
        assert!(!r.notes.contains(Notes::INVALID_ENTRY_ON_REQUIRED));
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
    fn the_error_text_of_the_document_source_is_a_known_text_group() {
        let r = normalise(&node(
            all(),
            79,
            st(&[bit::INVALID_ENTRY]),
            &[("tag", "input"), ("error-text", "Enter a valid address")],
        ));
        assert!(r.known.has(Group::ErrorText));
        assert_eq!(
            r.sem.text.as_deref().and_then(|t| t.error_text.as_deref()),
            Some("Enter a valid address")
        );
    }

    #[test]
    fn rare_text_and_name_source_groups_are_known_with_attributes() {
        let rare = [
            Group::RoleDescription,
            Group::Placeholder,
            Group::KeyShortcuts,
            Group::ValueText,
        ];
        let r = normalise(&node(
            all(),
            79,
            StateBits(0),
            &[
                ("placeholder-text", "Search"),
                ("keyshortcuts", "Control+K"),
                ("valuetext", "3 of 10"),
            ],
        ));
        for g in rare {
            assert!(r.known.has(g), "{g:?}");
        }
        assert!(r.known.has(Group::NameFrom));
        // Without ATTRS none of the attribute-derived text is known.
        let r = normalise(&node(
            PropMask::ROLE | PropMask::NAME | PropMask::STATES,
            79,
            StateBits(0),
            &[],
        ));
        for g in rare {
            assert!(!r.known.has(g), "{g:?} needs ATTRS");
        }
        // A fetched empty name has no source even without attributes; a non-empty one needs them.
        let mut n = node(PropMask::ROLE | PropMask::NAME, 79, StateBits(0), &[]);
        n.name = None;
        assert!(normalise(&n).known.has(Group::NameFrom));
        n.name = Some("Save");
        assert!(!normalise(&n).known.has(Group::NameFrom));
    }

    #[test]
    fn the_html_id_is_kept_capped_and_only_when_attributes_were_fetched() {
        let a = [("tag", "p"), ("id", "msg-1")];
        let r = normalise(&node(all(), 62, StateBits(0), &a));
        assert_eq!(r.dom_id.as_deref(), Some("msg-1"));
        // No attribute fetch, no opinion.
        let r = normalise(&node(PropMask::ROLE, 62, StateBits(0), &a));
        assert_eq!(r.dom_id, None);
        // No id, an empty id and a huge id.
        assert_eq!(normalise(&node(all(), 62, StateBits(0), &[])).dom_id, None);
        let e = [("id", "")];
        assert_eq!(normalise(&node(all(), 62, StateBits(0), &e)).dom_id, None);
        let big = "x".repeat(5000);
        let b = [("id", big.as_str())];
        let r = normalise(&node(all(), 62, StateBits(0), &b));
        assert_eq!(
            r.dom_id.as_deref().map(str::len),
            Some(crate::attrs::PAGE_STRING_CAP)
        );
        assert!(r.notes.contains(Notes::TRUNCATED));
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
