// SPDX-License-Identifier: GPL-3.0-or-later
//! Attribute normalisation (M22a-B4): one pass over the `GetAttributes` pairs, then typed
//! parsers for level, set position, live regions, `aria-current`, `aria-haspopup`,
//! `aria-invalid`, `aria-sort`, grid indices and the rare strings.
//!
//! Parsing never fails the record: a malformed value is dropped (the group stays at its default
//! for that node) and the record notes say so.

use std::num::NonZeroU32;
use std::sync::Arc;

use gorca_model::event::Politeness;
use gorca_model::sem::{
    Current, GridPos, Invalid, Live, NameFrom, Orientation, Popup, PosSrc, Relevant, SemRole,
    SemText, SetPos, Sort,
};

/// The attributes the normaliser reads, borrowed from the caller's pairs (no allocation).
#[derive(Copy, Clone, Debug, Default)]
pub struct ParsedAttrs<'a> {
    pub xml_roles: Option<&'a str>,
    pub tag: Option<&'a str>,
    pub toolkit: Option<&'a str>,
    pub text_input_type: Option<&'a str>,
    pub level: Option<&'a str>,
    pub posinset: Option<&'a str>,
    pub setsize: Option<&'a str>,
    pub live: Option<&'a str>,
    pub atomic: Option<&'a str>,
    pub relevant: Option<&'a str>,
    pub busy: Option<&'a str>,
    pub container_live: Option<&'a str>,
    pub container_atomic: Option<&'a str>,
    pub container_relevant: Option<&'a str>,
    pub current: Option<&'a str>,
    pub haspopup: Option<&'a str>,
    pub invalid: Option<&'a str>,
    pub sort: Option<&'a str>,
    pub orientation: Option<&'a str>,
    pub roledescription: Option<&'a str>,
    pub keyshortcuts: Option<&'a str>,
    pub valuetext: Option<&'a str>,
    /// The document source reads the `aria-errormessage` target and attaches its text here.
    pub error_text: Option<&'a str>,
    /// Gecko and GTK4 export `placeholder-text`.
    pub placeholder_text: Option<&'a str>,
    /// Blink exports `placeholder`.
    pub placeholder: Option<&'a str>,
    pub rowindex: Option<&'a str>,
    pub colindex: Option<&'a str>,
    pub rowspan: Option<&'a str>,
    pub colspan: Option<&'a str>,
    pub rowindextext: Option<&'a str>,
    pub colindextext: Option<&'a str>,
    pub rowcount: Option<&'a str>,
    pub colcount: Option<&'a str>,
    pub details_roles: Option<&'a str>,
    pub details_from: Option<&'a str>,
    pub layout_guess: Option<&'a str>,
    pub name_from: Option<&'a str>,
    pub explicit_name: Option<&'a str>,
    /// Blink's `pressed` is exported next to the state; Gecko too.
    pub pressed: Option<&'a str>,
}

/// Every attribute name [`ParsedAttrs::parse`] reads. A connector keeps only these from a
/// `GetAttributes` reply (the app's attribute map is untrusted and can be large); a test checks
/// that the list and the parser agree.
pub const WANTED_ATTRS: [&str; 39] = [
    "xml-roles",
    "tag",
    "toolkit",
    "text-input-type",
    "level",
    "posinset",
    "setsize",
    "live",
    "atomic",
    "relevant",
    "busy",
    "container-live",
    "container-atomic",
    "container-relevant",
    "current",
    "haspopup",
    "invalid",
    "sort",
    "orientation",
    "roledescription",
    "keyshortcuts",
    "valuetext",
    "error-text",
    "placeholder-text",
    "placeholder",
    "rowindex",
    "colindex",
    "rowspan",
    "colspan",
    "rowindextext",
    "colindextext",
    "rowcount",
    "colcount",
    "details-roles",
    "details-from",
    "layout-guess",
    "name-from",
    "explicit-name",
    "pressed",
];

/// True when the normaliser reads the attribute `name` ([`WANTED_ATTRS`]).
pub fn wants_attr(name: &str) -> bool {
    WANTED_ATTRS.contains(&name)
}

impl<'a> ParsedAttrs<'a> {
    /// One pass; the first occurrence of a key wins.
    pub fn parse(pairs: &[(&'a str, &'a str)]) -> ParsedAttrs<'a> {
        let mut a = ParsedAttrs::default();
        for &(k, v) in pairs {
            let slot = match k {
                "xml-roles" => &mut a.xml_roles,
                "tag" => &mut a.tag,
                "toolkit" => &mut a.toolkit,
                "text-input-type" => &mut a.text_input_type,
                "level" => &mut a.level,
                "posinset" => &mut a.posinset,
                "setsize" => &mut a.setsize,
                "live" => &mut a.live,
                "atomic" => &mut a.atomic,
                "relevant" => &mut a.relevant,
                "busy" => &mut a.busy,
                "container-live" => &mut a.container_live,
                "container-atomic" => &mut a.container_atomic,
                "container-relevant" => &mut a.container_relevant,
                "current" => &mut a.current,
                "haspopup" => &mut a.haspopup,
                "invalid" => &mut a.invalid,
                "sort" => &mut a.sort,
                "orientation" => &mut a.orientation,
                "roledescription" => &mut a.roledescription,
                "keyshortcuts" => &mut a.keyshortcuts,
                "valuetext" => &mut a.valuetext,
                "error-text" => &mut a.error_text,
                "placeholder-text" => &mut a.placeholder_text,
                "placeholder" => &mut a.placeholder,
                "rowindex" => &mut a.rowindex,
                "colindex" => &mut a.colindex,
                "rowspan" => &mut a.rowspan,
                "colspan" => &mut a.colspan,
                "rowindextext" => &mut a.rowindextext,
                "colindextext" => &mut a.colindextext,
                "rowcount" => &mut a.rowcount,
                "colcount" => &mut a.colcount,
                "details-roles" => &mut a.details_roles,
                "details-from" => &mut a.details_from,
                "layout-guess" => &mut a.layout_guess,
                "name-from" => &mut a.name_from,
                "explicit-name" => &mut a.explicit_name,
                "pressed" => &mut a.pressed,
                _ => continue,
            };
            if slot.is_none() {
                *slot = Some(v);
            }
        }
        a
    }

    /// True for an HTML-derived node (Gecko and Blink export the DOM `tag`).
    pub fn is_web(&self) -> bool {
        self.tag.is_some()
    }
}

/// Parse an ARIA boolean-ish attribute: `true`/`false` only.
pub fn parse_bool(v: Option<&str>) -> Option<bool> {
    match v {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

/// Parse a decimal that must fit `u32` (no sign, no whitespace).
pub fn parse_u32(v: &str) -> Option<u32> {
    v.parse::<u32>().ok()
}

/// Heading or tree level: roles that carry one. Blink exports `level=1` on every list item, so
/// the level is ignored outside these roles. `StaticText` is allowed because GTK labels carry a
/// LEVEL accessible property.
pub const fn has_level(role: SemRole) -> bool {
    matches!(
        role,
        SemRole::Heading
            | SemRole::TreeItem
            | SemRole::Row
            | SemRole::Comment
            | SemRole::StaticText
    )
}

/// The level of a node (0 = none). Heading falls back to the DOM tag (`h1`..`h6`) and then to
/// the ARIA default of 2 for `role=heading` without `aria-level`.
pub fn level_of(role: SemRole, a: &ParsedAttrs<'_>) -> u8 {
    if !has_level(role) {
        return 0;
    }
    if let Some(n) = a.level.and_then(parse_u32) {
        return u8::try_from(n.min(255)).unwrap_or(u8::MAX);
    }
    if role == SemRole::Heading {
        if let Some(n) = a.tag.and_then(heading_tag_level) {
            return n;
        }
        // ARIA 1.2 default for aria-level on heading.
        return 2;
    }
    0
}

/// `h1`..`h6` to 1..6.
pub fn heading_tag_level(tag: &str) -> Option<u8> {
    match tag.as_bytes() {
        [b'h', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    }
}

/// Position in a set from `posinset`/`setsize`. A set size of 0 or -1 is "unknown" (`None`);
/// `posinset` must be at least 1. Returns `None` without a usable position.
pub fn setpos_of(a: &ParsedAttrs<'_>) -> Option<SetPos> {
    let pos = a.posinset.and_then(parse_u32).filter(|p| *p >= 1)?;
    let size = a.setsize.and_then(parse_u32).and_then(NonZeroU32::new);
    Some(SetPos {
        pos,
        size,
        src: PosSrc::Engine,
    })
}

/// Grid position from `rowindex`/`colindex`/`rowspan`/`colspan`. A missing index is 0 ("not
/// exported"); spans default to 1 and are capped at `u16::MAX`.
pub fn grid_of(a: &ParsedAttrs<'_>) -> Option<GridPos> {
    // A table carries its declared size (`rowcount`, `colcount`) where a cell carries its index.
    let row = a.rowindex.or(a.rowcount).and_then(parse_u32);
    let col = a.colindex.or(a.colcount).and_then(parse_u32);
    let rs = a.rowspan.and_then(parse_u32);
    let cs = a.colspan.and_then(parse_u32);
    if row.is_none() && col.is_none() && rs.is_none() && cs.is_none() {
        return None;
    }
    let span = |v: Option<u32>| u16::try_from(v.unwrap_or(1).max(1)).unwrap_or(u16::MAX);
    Some(GridPos {
        row: row.unwrap_or(0),
        col: col.unwrap_or(0),
        row_span: span(rs),
        col_span: span(cs),
    })
}

/// `aria-current`: the five tokens, `true`, `false`; an empty value is `false`; any other
/// non-empty token is `true` (ARIA 1.2: unrecognised values are treated as `true`). The flag is
/// set when the value was unrecognised.
pub fn current_of(v: Option<&str>) -> (Current, bool) {
    match v {
        None | Some("" | "false") => (Current::No, false),
        Some("true") => (Current::True, false),
        Some("page") => (Current::Page, false),
        Some("step") => (Current::Step, false),
        Some("location") => (Current::Location, false),
        Some("date") => (Current::Date, false),
        Some("time") => (Current::Time, false),
        Some(_) => (Current::True, true),
    }
}

/// `aria-haspopup`: `true` is `menu`; `false` or empty is none; an unknown token is `menu`
/// (ARIA 1.2: unrecognised values are `true`). The flag is set when the value was unrecognised.
pub fn popup_of(v: &str) -> (Popup, bool) {
    match v {
        "" | "false" => (Popup::No, false),
        "true" | "menu" => (Popup::Menu, false),
        "listbox" => (Popup::Listbox, false),
        "tree" => (Popup::Tree, false),
        "grid" => (Popup::Grid, false),
        "dialog" => (Popup::Dialog, false),
        _ => (Popup::Menu, true),
    }
}

/// `aria-invalid` token. `None` for a value that says nothing.
pub fn invalid_of(v: &str) -> Option<Invalid> {
    match v {
        "false" | "" => Some(Invalid::No),
        "grammar" => Some(Invalid::Grammar),
        "spelling" => Some(Invalid::Spelling),
        // `true`, and unrecognised tokens, which ARIA 1.2 treats as `true`.
        _ => Some(Invalid::True),
    }
}

/// `aria-sort`.
pub fn sort_of(v: &str) -> Sort {
    match v {
        "ascending" => Sort::Ascending,
        "descending" => Sort::Descending,
        "other" => Sort::Other,
        _ => Sort::None,
    }
}

/// `aria-orientation` fallback when the HORIZONTAL/VERTICAL states are absent.
pub fn orientation_of(v: &str) -> Orientation {
    match v {
        "horizontal" => Orientation::Horizontal,
        "vertical" => Orientation::Vertical,
        _ => Orientation::NotApplicable,
    }
}

/// An `aria-live` value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LiveAttr {
    Off,
    On(Politeness),
    /// Not a recognised token: treated like an absent attribute.
    Bad,
}

fn live_attr(v: Option<&str>) -> Option<LiveAttr> {
    Some(match v? {
        "polite" => LiveAttr::On(Politeness::Polite),
        "assertive" => LiveAttr::On(Politeness::Assertive),
        "off" | "" => LiveAttr::Off,
        _ => LiveAttr::Bad,
    })
}

/// `aria-relevant` tokens; `all` is the three kinds; the default is `additions text`.
pub fn relevant_of(v: Option<&str>) -> Relevant {
    let Some(v) = v else {
        return Relevant::ADDITIONS | Relevant::TEXT;
    };
    let mut r = Relevant::empty();
    for t in v.split_ascii_whitespace() {
        match t {
            "additions" => r |= Relevant::ADDITIONS,
            "removals" => r |= Relevant::REMOVALS,
            "text" => r |= Relevant::TEXT,
            "all" => r |= Relevant::all(),
            _ => {}
        }
    }
    if r.is_empty() {
        Relevant::ADDITIONS | Relevant::TEXT
    } else {
        r
    }
}

/// ARIA implicit live settings of a live-region role: (politeness, atomic). `None` for a role
/// that is not a live region.
pub const fn implicit_live(role: SemRole) -> Option<(Politeness, bool)> {
    match role {
        SemRole::Alert => Some((Politeness::Assertive, true)),
        SemRole::Status => Some((Politeness::Polite, true)),
        SemRole::Log => Some((Politeness::Polite, false)),
        _ => None,
    }
}

/// The live-region group of a node.
///
/// * An own `live` attribute makes the node a live-region root (`root: true`); `off` turns it
///   off.
/// * Otherwise a role with implicit live behaviour (alert, status, log) is a root.
/// * Otherwise `container-live` makes the node a member of an ancestor's region (`root: false`).
///
/// `aria-atomic` and `aria-relevant` come from the own attribute, then from the container, then
/// from the role default.
pub fn live_of(role: SemRole, a: &ParsedAttrs<'_>) -> Option<Live> {
    let implicit = implicit_live(role);
    match live_attr(a.live) {
        Some(LiveAttr::Off) => return None,
        Some(LiveAttr::On(politeness)) => {
            let atomic = parse_bool(a.atomic).unwrap_or(implicit.is_some_and(|(_, at)| at));
            return Some(Live {
                politeness,
                atomic,
                relevant: relevant_of(a.relevant),
                root: true,
            });
        }
        Some(LiveAttr::Bad) | None => {}
    }
    if let Some((politeness, atomic)) = implicit {
        return Some(Live {
            politeness,
            atomic: parse_bool(a.atomic).unwrap_or(atomic),
            relevant: relevant_of(a.relevant),
            root: true,
        });
    }
    let LiveAttr::On(politeness) = live_attr(a.container_live)? else {
        return None;
    };
    Some(Live {
        politeness,
        atomic: parse_bool(a.container_atomic).unwrap_or(false),
        relevant: relevant_of(a.container_relevant),
        root: false,
    })
}

/// Where the accessible name came from, from the engine's own hints.
///
/// Blink exports `name-from` (`attribute`, `related-element`, `contents`, `tooltip`, `caption`,
/// `value`, `placeholder`); Gecko exports `explicit-name=true` for authored names. GTK exports
/// neither, so a non-empty name there is `Content`.
pub fn name_from_of(a: &ParsedAttrs<'_>, name_empty: bool) -> NameFrom {
    if name_empty {
        return NameFrom::None;
    }
    match a.name_from {
        Some("contents") => NameFrom::Content,
        Some("placeholder") => NameFrom::Placeholder,
        Some(_) => NameFrom::Author,
        None => {
            if a.explicit_name == Some("true") {
                NameFrom::Author
            } else {
                NameFrom::Content
            }
        }
    }
}

/// Cap on `aria-roledescription` (ADR-0022 section 2.1).
pub const ROLE_DESCRIPTION_CAP: usize = 64;

/// Longest page-derived string the normaliser keeps, in bytes (ADR-0024 section 2.6: page strings
/// longer than 1 KiB are truncated and counted). Applies to name, description, placeholder, key
/// shortcuts, value text and the index texts; `aria-roledescription` has its own 64 scalar cap.
pub const PAGE_STRING_CAP: usize = 1024;

/// Truncate to at most [`PAGE_STRING_CAP`] bytes on a scalar boundary. The flag says it cut.
pub fn cap_page_string(s: &str) -> (&str, bool) {
    if s.len() <= PAGE_STRING_CAP {
        return (s, false);
    }
    let mut end = PAGE_STRING_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (&s[..end], true)
}

/// A non-empty, capped string as an `Arc`; sets `cut` when it truncated.
pub fn arc_capped(v: Option<&str>, cut: &mut bool) -> Option<Arc<str>> {
    let (s, c) = cap_page_string(v.filter(|s| !s.is_empty())?);
    *cut |= c;
    Some(Arc::from(s))
}

/// Truncate to at most `cap` Unicode scalars, on a scalar boundary.
pub fn cap_scalars(s: &str, cap: usize) -> &str {
    match s.char_indices().nth(cap) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Assemble the rare strings; `None` when every field is empty. The flag says a string was
/// truncated to [`PAGE_STRING_CAP`].
pub fn text_of(description: Option<&str>, a: &ParsedAttrs<'_>) -> (Option<Arc<SemText>>, bool) {
    let mut cut = false;
    let t = SemText {
        description: arc_capped(description, &mut cut),
        role_description: arc_capped(
            a.roledescription
                .map(|s| cap_scalars(s, ROLE_DESCRIPTION_CAP)),
            &mut cut,
        ),
        error_text: arc_capped(a.error_text, &mut cut),
        placeholder: arc_capped(a.placeholder_text.or(a.placeholder), &mut cut),
        key_shortcuts: arc_capped(a.keyshortcuts, &mut cut),
        value_text: arc_capped(a.valuetext, &mut cut),
        col_index_text: arc_capped(a.colindextext, &mut cut),
        row_index_text: arc_capped(a.rowindextext, &mut cut),
        details_target: None,
        dom_id: None,
    };
    if t.is_empty() {
        (None, cut)
    } else {
        (Some(Arc::new(t)), cut)
    }
}

/// `aria-details` is present (Blink and Gecko export `details-roles` or `details-from`).
pub fn has_details(a: &ParsedAttrs<'_>) -> bool {
    a.details_roles.is_some() || a.details_from == Some("aria-details")
}

/// How an ARIA property is handled (the M-10 list for properties and states).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AriaUse {
    /// Carried by a semantic group, flag or text field; the text names it.
    Mapped(&'static str),
    /// Not a semantic group; the text says where it lives or why it is out of scope.
    Unmapped(&'static str),
}

/// Every WAI-ARIA 1.2 state and property and what the model does with it.
pub const ARIA_PROPERTIES: &[(&str, AriaUse)] = &[
    (
        "aria-activedescendant",
        AriaUse::Unmapped("relation / active-descendant event, not a register"),
    ),
    ("aria-atomic", AriaUse::Mapped("live.atomic")),
    (
        "aria-autocomplete",
        AriaUse::Unmapped("autocomplete kind: raw attribute, not a semantic group"),
    ),
    (
        "aria-braillelabel",
        AriaUse::Unmapped("ARIA 1.3, braille path reads the raw attribute"),
    ),
    (
        "aria-brailleroledescription",
        AriaUse::Unmapped("ARIA 1.3, braille path reads the raw attribute"),
    ),
    ("aria-busy", AriaUse::Mapped("flags BUSY")),
    ("aria-checked", AriaUse::Mapped("check")),
    (
        "aria-colcount",
        AriaUse::Mapped("grid.col (table: declared size)"),
    ),
    ("aria-colindex", AriaUse::Mapped("grid.col")),
    (
        "aria-colindextext",
        AriaUse::Mapped("text.col_index_text / row_index_text"),
    ),
    ("aria-colspan", AriaUse::Mapped("grid.col_span")),
    (
        "aria-controls",
        AriaUse::Unmapped("relation (controller-for), not a register"),
    ),
    ("aria-current", AriaUse::Mapped("current")),
    (
        "aria-describedby",
        AriaUse::Mapped("description (engine computed) and S3 relation text"),
    ),
    ("aria-description", AriaUse::Mapped("description")),
    ("aria-details", AriaUse::Mapped("flags HAS_DETAILS")),
    ("aria-disabled", AriaUse::Mapped("avail")),
    (
        "aria-dropeffect",
        AriaUse::Unmapped("deprecated in ARIA 1.1"),
    ),
    (
        "aria-errormessage",
        AriaUse::Unmapped("relation: flags HAS_ERROR_MESSAGE and error_text come from S3"),
    ),
    ("aria-expanded", AriaUse::Mapped("expand")),
    (
        "aria-flowto",
        AriaUse::Unmapped("relation (flows-to), not a register"),
    ),
    ("aria-grabbed", AriaUse::Unmapped("deprecated in ARIA 1.1")),
    ("aria-haspopup", AriaUse::Mapped("popup")),
    (
        "aria-hidden",
        AriaUse::Unmapped("engines prune hidden subtrees; absence is the signal"),
    ),
    (
        "aria-invalid",
        AriaUse::Mapped("invalid (kind only when the engine exports it)"),
    ),
    ("aria-keyshortcuts", AriaUse::Mapped("text.key_shortcuts")),
    ("aria-label", AriaUse::Mapped("name")),
    ("aria-labelledby", AriaUse::Mapped("name (engine computed)")),
    ("aria-level", AriaUse::Mapped("level")),
    ("aria-live", AriaUse::Mapped("live")),
    ("aria-modal", AriaUse::Mapped("flags MODAL")),
    ("aria-multiline", AriaUse::Mapped("flags MULTILINE")),
    (
        "aria-multiselectable",
        AriaUse::Mapped("flags MULTISELECTABLE"),
    ),
    ("aria-orientation", AriaUse::Mapped("orientation")),
    (
        "aria-owns",
        AriaUse::Unmapped("tree structure: the engine reparents children"),
    ),
    ("aria-placeholder", AriaUse::Mapped("text.placeholder")),
    ("aria-posinset", AriaUse::Mapped("setpos.pos")),
    ("aria-pressed", AriaUse::Mapped("press")),
    (
        "aria-readonly",
        AriaUse::Mapped("flags READ_ONLY (Gecko: inexpressible on non-text roles)"),
    ),
    ("aria-relevant", AriaUse::Mapped("live.relevant")),
    ("aria-required", AriaUse::Mapped("flags REQUIRED")),
    (
        "aria-roledescription",
        AriaUse::Mapped("text.role_description"),
    ),
    (
        "aria-rowcount",
        AriaUse::Mapped("grid.row (table: declared size)"),
    ),
    ("aria-rowindex", AriaUse::Mapped("grid.row")),
    (
        "aria-rowindextext",
        AriaUse::Mapped("text.col_index_text / row_index_text"),
    ),
    ("aria-rowspan", AriaUse::Mapped("grid.row_span")),
    ("aria-selected", AriaUse::Mapped("select")),
    ("aria-setsize", AriaUse::Mapped("setpos.size")),
    ("aria-sort", AriaUse::Mapped("sort")),
    (
        "aria-valuemax",
        AriaUse::Unmapped("Value interface, read on demand"),
    ),
    (
        "aria-valuemin",
        AriaUse::Unmapped("Value interface, read on demand"),
    ),
    (
        "aria-valuenow",
        AriaUse::Unmapped("Value interface, read on demand"),
    ),
    ("aria-valuetext", AriaUse::Mapped("text.value_text")),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn pa<'a>(p: &[(&'a str, &'a str)]) -> ParsedAttrs<'a> {
        ParsedAttrs::parse(p)
    }

    #[test]
    fn wanted_attrs_are_exactly_the_parsed_ones() {
        let empty = format!("{:?}", ParsedAttrs::default());
        for n in WANTED_ATTRS {
            let got = format!("{:?}", pa(&[(n, "x")]));
            assert_ne!(got, empty, "{n} is listed but not parsed");
            assert!(wants_attr(n));
        }
        assert!(!wants_attr("class") && !wants_attr("id"));
    }

    #[test]
    fn parse_is_first_wins_and_ignores_unknown_keys() {
        let a = pa(&[
            ("level", "2"),
            ("level", "5"),
            ("class", "x"),
            ("tag", "h2"),
        ]);
        assert_eq!(a.level, Some("2"));
        assert_eq!(a.tag, Some("h2"));
        assert!(a.is_web());
        assert!(!pa(&[("toolkit", "GTK")]).is_web());
    }

    #[test]
    fn level_rules() {
        let h = pa(&[("tag", "h4"), ("level", "4")]);
        assert_eq!(level_of(SemRole::Heading, &h), 4);
        assert_eq!(
            level_of(SemRole::Heading, &pa(&[("tag", "h3")])),
            3,
            "tag fallback"
        );
        assert_eq!(
            level_of(SemRole::Heading, &pa(&[("tag", "div")])),
            2,
            "ARIA default"
        );
        assert_eq!(
            level_of(SemRole::Heading, &pa(&[("level", "999")])),
            255,
            "clamped"
        );
        assert_eq!(
            level_of(SemRole::Heading, &pa(&[("level", "x")])),
            2,
            "malformed: default"
        );
        // Blink exports level=1 on every list item.
        assert_eq!(level_of(SemRole::ListItem, &pa(&[("level", "1")])), 0);
        assert_eq!(level_of(SemRole::TreeItem, &pa(&[("level", "3")])), 3);
        assert_eq!(level_of(SemRole::TreeItem, &pa(&[])), 0);
        assert_eq!(heading_tag_level("h6"), Some(6));
        assert_eq!(heading_tag_level("h7"), None);
        assert_eq!(heading_tag_level("hr"), None);
    }

    #[test]
    fn set_position() {
        let p = setpos_of(&pa(&[("posinset", "2"), ("setsize", "10")]));
        assert_eq!(
            p.map(|p| (p.pos, p.size.map(NonZeroU32::get))),
            Some((2, Some(10)))
        );
        // setsize 0 and -1 are "unknown size".
        let p = setpos_of(&pa(&[("posinset", "1"), ("setsize", "-1")]));
        assert_eq!(p.map(|p| p.size), Some(None));
        let p = setpos_of(&pa(&[("posinset", "1"), ("setsize", "0")]));
        assert_eq!(p.map(|p| p.size), Some(None));
        assert_eq!(setpos_of(&pa(&[("posinset", "0"), ("setsize", "3")])), None);
        assert_eq!(setpos_of(&pa(&[("setsize", "3")])), None);
        assert_eq!(setpos_of(&pa(&[("posinset", "x")])), None);
    }

    #[test]
    fn grid_position() {
        let g = grid_of(&pa(&[("colindex", "3"), ("colspan", "2")]));
        assert_eq!(
            g,
            Some(GridPos {
                row: 0,
                col: 3,
                row_span: 1,
                col_span: 2
            })
        );
        assert_eq!(grid_of(&pa(&[])), None);
        let g = grid_of(&pa(&[
            ("rowindex", "50"),
            ("colindex", "1"),
            ("colspan", "0"),
        ]));
        assert_eq!(g.map(|g| (g.row, g.col, g.col_span)), Some((50, 1, 1)));
        let g = grid_of(&pa(&[("colspan", "999999")]));
        assert_eq!(g.map(|g| g.col_span), Some(u16::MAX));
    }

    #[test]
    fn current_tokens_and_invalid_token_is_true() {
        assert_eq!(current_of(None), (Current::No, false));
        assert_eq!(current_of(Some("false")), (Current::No, false));
        assert_eq!(current_of(Some("")), (Current::No, false));
        assert_eq!(current_of(Some("page")), (Current::Page, false));
        assert_eq!(current_of(Some("date")), (Current::Date, false));
        assert_eq!(current_of(Some("bogus")), (Current::True, true));
    }

    #[test]
    fn popup_and_invalid_and_sort() {
        assert_eq!(popup_of("true"), (Popup::Menu, false));
        assert_eq!(popup_of("grid"), (Popup::Grid, false));
        assert_eq!(popup_of("false"), (Popup::No, false));
        assert_eq!(popup_of("bogus"), (Popup::Menu, true));
        assert_eq!(invalid_of("spelling"), Some(Invalid::Spelling));
        assert_eq!(invalid_of("grammar"), Some(Invalid::Grammar));
        assert_eq!(invalid_of("false"), Some(Invalid::No));
        assert_eq!(invalid_of("zzz"), Some(Invalid::True));
        assert_eq!(sort_of("ascending"), Sort::Ascending);
        assert_eq!(sort_of("other"), Sort::Other);
        assert_eq!(sort_of("none"), Sort::None);
        assert_eq!(orientation_of("vertical"), Orientation::Vertical);
    }

    #[test]
    fn relevant_tokens() {
        assert_eq!(relevant_of(None), Relevant::ADDITIONS | Relevant::TEXT);
        assert_eq!(relevant_of(Some("all")), Relevant::all());
        assert_eq!(relevant_of(Some("removals")), Relevant::REMOVALS);
        assert_eq!(
            relevant_of(Some("additions removals")),
            Relevant::ADDITIONS | Relevant::REMOVALS
        );
        assert_eq!(
            relevant_of(Some("bogus")),
            Relevant::ADDITIONS | Relevant::TEXT
        );
    }

    #[test]
    fn live_root_member_off_and_implicit() {
        let own = pa(&[
            ("live", "assertive"),
            ("atomic", "true"),
            ("relevant", "additions text"),
        ]);
        let l = live_of(SemRole::Generic, &own);
        assert_eq!(
            l,
            Some(Live {
                politeness: Politeness::Assertive,
                atomic: true,
                relevant: Relevant::ADDITIONS | Relevant::TEXT,
                root: true
            })
        );
        // Own off: no live region even if a container says polite.
        assert_eq!(
            live_of(
                SemRole::Generic,
                &pa(&[("live", "off"), ("container-live", "polite")])
            ),
            None
        );
        // Member of an ancestor's region.
        let m = live_of(
            SemRole::Paragraph,
            &pa(&[("container-live", "polite"), ("container-atomic", "true")]),
        );
        assert_eq!(
            m.map(|l| (l.politeness, l.atomic, l.root)),
            Some((Politeness::Polite, true, false))
        );
        // Implicit roles, with the atomic default.
        let a = live_of(SemRole::Alert, &pa(&[]));
        assert_eq!(
            a.map(|l| (l.politeness, l.atomic, l.root)),
            Some((Politeness::Assertive, true, true))
        );
        let s = live_of(SemRole::Status, &pa(&[("atomic", "false")]));
        assert_eq!(
            s.map(|l| l.atomic),
            Some(false),
            "explicit aria-atomic wins over the role default"
        );
        let log = live_of(SemRole::Log, &pa(&[]));
        assert_eq!(
            log.map(|l| (l.politeness, l.atomic)),
            Some((Politeness::Polite, false))
        );
        assert_eq!(live_of(SemRole::Generic, &pa(&[])), None);
        // Malformed value: ignored like absent.
        assert_eq!(live_of(SemRole::Generic, &pa(&[("live", "loud")])), None);
    }

    #[test]
    fn name_source_hints() {
        assert_eq!(name_from_of(&pa(&[]), true), NameFrom::None);
        assert_eq!(
            name_from_of(&pa(&[("name-from", "contents")]), false),
            NameFrom::Content
        );
        assert_eq!(
            name_from_of(&pa(&[("name-from", "attribute")]), false),
            NameFrom::Author
        );
        assert_eq!(
            name_from_of(&pa(&[("name-from", "related-element")]), false),
            NameFrom::Author
        );
        assert_eq!(
            name_from_of(&pa(&[("name-from", "placeholder")]), false),
            NameFrom::Placeholder
        );
        assert_eq!(
            name_from_of(&pa(&[("explicit-name", "true")]), false),
            NameFrom::Author
        );
        assert_eq!(name_from_of(&pa(&[]), false), NameFrom::Content);
    }

    #[test]
    fn rare_strings_placeholder_alias_and_caps() {
        let a = pa(&[
            ("placeholder", "Hint"),
            ("keyshortcuts", "Alt+K"),
            ("valuetext", "five"),
        ]);
        let (t, cut) = text_of(Some("desc"), &a);
        assert!(!cut);
        let t = t.as_deref();
        assert_eq!(
            t.and_then(|t| t.placeholder.as_deref()),
            Some("Hint"),
            "Blink key"
        );
        assert_eq!(t.and_then(|t| t.key_shortcuts.as_deref()), Some("Alt+K"));
        assert_eq!(t.and_then(|t| t.value_text.as_deref()), Some("five"));
        assert_eq!(t.and_then(|t| t.description.as_deref()), Some("desc"));
        let g = pa(&[("placeholder-text", "Gecko hint")]);
        assert_eq!(
            text_of(None, &g)
                .0
                .and_then(|t| t.placeholder.clone())
                .as_deref(),
            Some("Gecko hint")
        );
        assert_eq!(
            text_of(Some(""), &pa(&[])),
            (None, false),
            "empty strings store no Arc"
        );
        let long = "é".repeat(100);
        let rd = pa(&[("roledescription", long.as_str())]);
        let n = text_of(None, &rd)
            .0
            .and_then(|t| t.role_description.clone())
            .map(|s| s.chars().count());
        assert_eq!(n, Some(ROLE_DESCRIPTION_CAP));
    }

    #[test]
    fn page_strings_are_capped_on_a_scalar_boundary() {
        assert_eq!(cap_page_string("short"), ("short", false));
        let exact = "a".repeat(PAGE_STRING_CAP);
        assert!(!cap_page_string(&exact).1);
        // 2-byte scalars: 1024 bytes is a boundary, 1025 would split one.
        let wide = "é".repeat(600);
        let (s, cut) = cap_page_string(&wide);
        assert!(cut && s.len() == PAGE_STRING_CAP && s.chars().all(|c| c == 'é'));
        let odd = format!("x{}", "é".repeat(600));
        let (s, cut) = cap_page_string(&odd);
        assert!(
            cut && s.len() == PAGE_STRING_CAP - 1,
            "never splits a scalar"
        );
        // Every text field is capped and the flag is set; index texts live in SemText.
        let big = "k".repeat(5000);
        let a = pa(&[
            ("keyshortcuts", big.as_str()),
            ("valuetext", big.as_str()),
            ("placeholder", big.as_str()),
            ("colindextext", big.as_str()),
            ("rowindextext", "Row 3"),
        ]);
        let (t, cut) = text_of(Some(big.as_str()), &a);
        assert!(cut);
        let t = t.expect("text");
        for s in [
            &t.description,
            &t.key_shortcuts,
            &t.value_text,
            &t.placeholder,
            &t.col_index_text,
        ] {
            assert_eq!(s.as_deref().map(str::len), Some(PAGE_STRING_CAP));
        }
        assert_eq!(t.row_index_text.as_deref(), Some("Row 3"));
    }

    #[test]
    fn details_detection() {
        assert!(has_details(&pa(&[("details-roles", "comment")])));
        assert!(has_details(&pa(&[("details-from", "aria-details")])));
        assert!(!has_details(&pa(&[])));
    }

    #[test]
    fn property_coverage_is_complete_and_sorted() {
        // 50 ARIA 1.2 states and properties (incl. the deprecated dropeffect and grabbed) plus
        // the ARIA 1.3 aria-braillelabel, aria-brailleroledescription and aria-description.
        assert_eq!(ARIA_PROPERTIES.len(), 53);
        let mut names: Vec<&str> = ARIA_PROPERTIES.iter().map(|(n, _)| *n).collect();
        let before = names.clone();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names, before, "sorted, no duplicates");
        assert!(names.iter().all(|n| n.starts_with("aria-")));
    }
}
