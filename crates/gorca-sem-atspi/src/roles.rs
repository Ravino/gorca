// SPDX-License-Identifier: GPL-3.0-or-later
//! Role normalisation: raw `AtspiRole` numbers, `xml-roles` tokens and the engine-vs-ARIA rule
//! (ADR-0022 SM-4, M22a-B4).
//!
//! Three pieces:
//! 1. [`engine_role`]: the raw role number (values of `AtspiRole`, at-spi2-core 2.56, stable ABI)
//!    to the closest [`SemRole`]. The numbers are copied, the `atspi` crate is not linked.
//! 2. [`token_kind`] and [`pick_token`]: the `xml-roles` attribute is a space-separated ARIA role
//!    token list. The first token that maps to a role wins (multi-token fallback); abstract and
//!    unknown tokens are skipped and reported in the record notes.
//! 3. [`resolve_role`]: the engine-vs-ARIA rule. The author's token wins, except where the ARIA
//!    spec says the native semantics survive (presentational conflict resolution, a secret field,
//!    `generic` on a semantic element).

use gorca_model::sem::{RoleSource, SemRole};

/// Raw `AtspiRole` numbers used by name elsewhere in this crate.
pub mod raw {
    pub const INVALID: u32 = 0;
    pub const PUSH_BUTTON: u32 = 43;
    pub const TOGGLE_BUTTON: u32 = 62;
    pub const ENTRY: u32 = 79;
    pub const TEXT: u32 = 61;
    pub const PASSWORD_TEXT: u32 = 40;
    pub const LAST_DEFINED: u32 = 131;
}

/// How a raw AT-SPI role maps to the semantic model.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EngineRole {
    /// Mapped to a role (possibly the weak `Generic`).
    Role(SemRole),
    /// No equivalent in the model; the record gets `SemRole::Unknown` and a note.
    Unmapped,
}

/// Map a raw `AtspiRole` number. Total: out-of-range numbers are `Unmapped`.
// The explicit `Unmapped` arms document which roles were looked at and left out (M-10).
#[allow(clippy::match_same_arms)]
pub const fn engine_role(raw: u32) -> EngineRole {
    use EngineRole::{Role, Unmapped};
    use SemRole as R;
    match raw {
        0 | 67 | 70 => Unmapped, // invalid, unknown, extended
        1 | 29 | 116 => Role(R::StaticText),
        2 | 101 => Role(R::Alert),
        3 | 4 | 18 | 21 | 24 | 25 | 30 | 36 | 46 | 49 | 53 | 68 | 71 | 72 | 76 | 84 | 86 | 100
        | 104 => Role(R::Generic),
        // calendar, desktop icon, ruler: no model role.
        5 | 13 | 74 => Unmapped,
        6 | 26 | 27 | 80 => Role(R::Img),
        7 => Role(R::Checkbox),
        8 => Role(R::MenuItemCheckbox),
        9 | 12 | 17 | 22 | 99 | 106 | 107 => Role(R::Group),
        10 | 57 => Role(R::ColumnHeader),
        11 => Role(R::Combobox),
        14 | 23 | 69 | 89 => Role(R::Window),
        15 | 51 | 114 => Role(R::Slider),
        16 | 19 => Role(R::Dialog),
        20 | 39 | 85 => Role(R::Generic),
        28 => Role(R::Iframe),
        31 => Role(R::List),
        32 => Role(R::ListItem),
        33 | 41 => Role(R::Menu),
        34 => Role(R::Menubar),
        35 | 59 => Role(R::MenuItem),
        37 => Role(R::Tab),
        38 => Role(R::TabList),
        40 => Role(R::PasswordBox),
        42 => Role(R::ProgressBar),
        43 | 62 | 129 => Role(R::Button),
        44 => Role(R::Radio),
        45 => Role(R::MenuItemRadio),
        47 | 58 => Role(R::RowHeader),
        48 => Role(R::Scrollbar),
        50 => Role(R::Separator),
        52 => Role(R::SpinButton),
        54 | 102 => Role(R::Status),
        55 => Role(R::Table),
        56 => Role(R::Cell),
        60 => Role(R::Terminal),
        61 | 77 | 79 => Role(R::TextBox),
        63 => Role(R::Toolbar),
        64 => Role(R::Tooltip),
        65 => Role(R::Tree),
        66 => Role(R::TreeGrid),
        73 => Role(R::Paragraph),
        75 | 78 => Role(R::Application),
        81 => Role(R::Caption),
        82 | 92..=94 | 96 => Role(R::Document),
        83 => Role(R::Heading),
        87 => Role(R::Form),
        88 => Role(R::Link),
        90 => Role(R::Row),
        91 => Role(R::TreeItem),
        95 => Role(R::DocumentWeb),
        97 => Role(R::Comment),
        98 => Role(R::Listbox),
        103 => Role(R::Meter),
        105 => Role(R::Blockquote),
        108 | 123 => Role(R::Definition),
        109 => Role(R::Article),
        110 => Role(R::Region),
        111 => Role(R::Log),
        112 => Role(R::Marquee),
        113 | 117 | 118 => Role(R::Math),
        115 => Role(R::Timer),
        119 => Role(R::Subscript),
        120 => Role(R::Superscript),
        121 => Role(R::DescriptionList),
        122 => Role(R::Term),
        124 => Role(R::Note),
        125 => Role(R::Deletion),
        126 => Role(R::Insertion),
        127 => Role(R::Mark),
        128 => Role(R::Suggestion),
        130 => Role(R::Switch),
        _ => Unmapped,
    }
}

/// Raw role numbers that map to no model role (the M-10 list for engine roles).
pub fn unmapped_engine_roles() -> Vec<u32> {
    (0..raw::LAST_DEFINED)
        .filter(|r| matches!(engine_role(*r), EngineRole::Unmapped))
        .collect()
}

/// What an `xml-roles` token is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TokenKind {
    /// A concrete role the model has.
    Role(SemRole),
    /// A valid ARIA role the model does not carry yet (DPUB and graphics subsets). Skipped,
    /// and listed by [`unmapped_tokens`].
    Unmapped,
    /// An abstract role: never valid in author content.
    Abstract,
    /// Not an ARIA role at all.
    Invalid,
}

/// Classify one token (ARIA role names are lower case; the match is case-sensitive on the
/// lower-cased ASCII form, which is what `xml-roles` carries).
pub fn token_kind(token: &str) -> TokenKind {
    use SemRole as R;
    use TokenKind::{Abstract, Invalid, Role, Unmapped};
    match token {
        "alert" => Role(R::Alert),
        "alertdialog" => Role(R::AlertDialog),
        "application" => Role(R::Application),
        "article" => Role(R::Article),
        "banner" => Role(R::Banner),
        "blockquote" => Role(R::Blockquote),
        "button" => Role(R::Button),
        "caption" => Role(R::Caption),
        "cell" => Role(R::Cell),
        "checkbox" => Role(R::Checkbox),
        "code" => Role(R::Code),
        "columnheader" => Role(R::ColumnHeader),
        "combobox" => Role(R::Combobox),
        "comment" => Role(R::Comment),
        "complementary" => Role(R::Complementary),
        "contentinfo" => Role(R::ContentInfo),
        "definition" => Role(R::Definition),
        "deletion" => Role(R::Deletion),
        "dialog" => Role(R::Dialog),
        // Deprecated in ARIA 1.2 (use list); browsers still export it.
        "directory" | "list" => Role(R::List),
        "document" => Role(R::Document),
        "emphasis" => Role(R::Emphasis),
        "feed" => Role(R::Feed),
        "figure" => Role(R::Figure),
        "form" => Role(R::Form),
        "generic" => Role(R::Generic),
        "grid" => Role(R::Grid),
        "gridcell" => Role(R::GridCell),
        "group" => Role(R::Group),
        "heading" => Role(R::Heading),
        // `image` is the ARIA 1.3 synonym of `img`.
        "img" | "image" => Role(R::Img),
        "insertion" => Role(R::Insertion),
        "link" => Role(R::Link),
        "listbox" => Role(R::Listbox),
        "listitem" => Role(R::ListItem),
        "log" => Role(R::Log),
        "main" => Role(R::Main),
        "mark" => Role(R::Mark),
        "marquee" => Role(R::Marquee),
        "math" => Role(R::Math),
        "menu" => Role(R::Menu),
        "menubar" => Role(R::Menubar),
        "menuitem" => Role(R::MenuItem),
        "menuitemcheckbox" => Role(R::MenuItemCheckbox),
        "menuitemradio" => Role(R::MenuItemRadio),
        "meter" => Role(R::Meter),
        "navigation" => Role(R::Navigation),
        "none" | "presentation" => Role(R::Presentation),
        "note" => Role(R::Note),
        "option" => Role(R::Option),
        "paragraph" => Role(R::Paragraph),
        "progressbar" => Role(R::ProgressBar),
        "radio" => Role(R::Radio),
        "radiogroup" => Role(R::RadioGroup),
        "region" => Role(R::Region),
        "row" => Role(R::Row),
        "rowgroup" => Role(R::RowGroup),
        "rowheader" => Role(R::RowHeader),
        "scrollbar" => Role(R::Scrollbar),
        "search" => Role(R::Search),
        "searchbox" => Role(R::SearchBox),
        "separator" => Role(R::Separator),
        "slider" => Role(R::Slider),
        "spinbutton" => Role(R::SpinButton),
        "status" => Role(R::Status),
        "strong" => Role(R::Strong),
        "subscript" => Role(R::Subscript),
        "suggestion" => Role(R::Suggestion),
        "superscript" => Role(R::Superscript),
        "switch" => Role(R::Switch),
        "tab" => Role(R::Tab),
        "table" => Role(R::Table),
        "tablist" => Role(R::TabList),
        "tabpanel" => Role(R::TabPanel),
        "term" => Role(R::Term),
        "textbox" => Role(R::TextBox),
        "time" => Role(R::Time),
        "timer" => Role(R::Timer),
        "toolbar" => Role(R::Toolbar),
        "tooltip" => Role(R::Tooltip),
        "tree" => Role(R::Tree),
        "treegrid" => Role(R::TreeGrid),
        "treeitem" => Role(R::TreeItem),
        "command" | "composite" | "input" | "landmark" | "range" | "roletype" | "section"
        | "sectionhead" | "select" | "structure" | "widget" | "window" => Abstract,
        _ => {
            if is_deferred_token(token) {
                Unmapped
            } else {
                Invalid
            }
        }
    }
}

/// DPUB-ARIA and graphics-ARIA tokens: valid, not in the model yet (SemRole docs: appended later).
fn is_deferred_token(token: &str) -> bool {
    DEFERRED_TOKENS.contains(&token)
}

const DEFERRED_TOKENS: &[&str] = &[
    "doc-abstract",
    "doc-acknowledgments",
    "doc-afterword",
    "doc-appendix",
    "doc-backlink",
    "doc-biblioentry",
    "doc-bibliography",
    "doc-biblioref",
    "doc-chapter",
    "doc-colophon",
    "doc-conclusion",
    "doc-cover",
    "doc-credit",
    "doc-credits",
    "doc-dedication",
    "doc-endnote",
    "doc-endnotes",
    "doc-epigraph",
    "doc-epilogue",
    "doc-errata",
    "doc-example",
    "doc-footnote",
    "doc-foreword",
    "doc-glossary",
    "doc-glossref",
    "doc-index",
    "doc-introduction",
    "doc-noteref",
    "doc-notice",
    "doc-pagebreak",
    "doc-pagelist",
    "doc-part",
    "doc-preface",
    "doc-prologue",
    "doc-pullquote",
    "doc-qna",
    "doc-subtitle",
    "doc-tip",
    "doc-toc",
    "graphics-document",
    "graphics-object",
    "graphics-symbol",
];

/// The valid-but-unmapped tokens (M-10 list for roles).
pub fn unmapped_tokens() -> &'static [&'static str] {
    DEFERRED_TOKENS
}

/// Every role of ARIA 1.2 section 5.4 (concrete roles). The coverage test asserts that each is
/// mapped by [`token_kind`].
pub const ARIA_12_ROLES: &[&str] = &[
    "alert",
    "alertdialog",
    "application",
    "article",
    "banner",
    "blockquote",
    "button",
    "caption",
    "cell",
    "checkbox",
    "code",
    "columnheader",
    "combobox",
    "complementary",
    "contentinfo",
    "definition",
    "deletion",
    "dialog",
    "directory",
    "document",
    "emphasis",
    "feed",
    "figure",
    "form",
    "generic",
    "grid",
    "gridcell",
    "group",
    "heading",
    "img",
    "insertion",
    "link",
    "list",
    "listbox",
    "listitem",
    "log",
    "main",
    "marquee",
    "math",
    "menu",
    "menubar",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "meter",
    "navigation",
    "none",
    "note",
    "option",
    "paragraph",
    "presentation",
    "progressbar",
    "radio",
    "radiogroup",
    "region",
    "row",
    "rowgroup",
    "rowheader",
    "scrollbar",
    "search",
    "searchbox",
    "separator",
    "slider",
    "spinbutton",
    "status",
    "strong",
    "subscript",
    "superscript",
    "switch",
    "tab",
    "table",
    "tablist",
    "tabpanel",
    "term",
    "textbox",
    "time",
    "timer",
    "toolbar",
    "tooltip",
    "tree",
    "treegrid",
    "treeitem",
];

/// Result of scanning an `xml-roles` value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TokenPick {
    /// The first token that maps to a role.
    pub role: Option<SemRole>,
    /// At least one token was skipped as invalid or abstract.
    pub saw_invalid: bool,
    /// At least one token was a valid role the model does not carry.
    pub saw_unmapped: bool,
}

/// Scan a space-separated token list: invalid tokens fall through to the next one (ARIA 1.2
/// section 5.4 fallback roles), the first mapped role wins.
pub fn pick_token(value: &str) -> TokenPick {
    let mut pick = TokenPick {
        role: None,
        saw_invalid: false,
        saw_unmapped: false,
    };
    for tok in value.split_ascii_whitespace() {
        match token_kind_ci(tok) {
            TokenKind::Role(r) => {
                pick.role = Some(r);
                return pick;
            }
            TokenKind::Unmapped => pick.saw_unmapped = true,
            TokenKind::Abstract | TokenKind::Invalid => pick.saw_invalid = true,
        }
    }
    pick
}

/// ARIA tokens are ASCII case-insensitive in HTML attributes; engines usually lower-case them.
fn token_kind_ci(tok: &str) -> TokenKind {
    if tok.bytes().any(|b| b.is_ascii_uppercase()) {
        // Rare: allocate only here.
        token_kind(&tok.to_ascii_lowercase())
    } else {
        token_kind(tok)
    }
}

/// HTML-AAM roles of elements that engines export with a weak role (`section`, `static`): Blink
/// exports `<code>`, `<time>` and `<mark>` as static text and `<figure>` and `<fieldset>` as
/// plain containers. Applied only when the author gave no role and the engine role is weak
/// ([`is_weak_engine_role`]).
pub fn tag_role(tag: &str) -> Option<SemRole> {
    match tag {
        "code" => Some(SemRole::Code),
        "time" => Some(SemRole::Time),
        "mark" => Some(SemRole::Mark),
        "figure" => Some(SemRole::Figure),
        "fieldset" => Some(SemRole::Group),
        "em" => Some(SemRole::Emphasis),
        "strong" => Some(SemRole::Strong),
        _ => None,
    }
}

/// Engine roles that carry no semantics of their own, so the DOM tag may refine them.
pub const fn is_weak_engine_role(r: SemRole) -> bool {
    matches!(r, SemRole::Generic | SemRole::StaticText | SemRole::Unknown)
}

/// Roles that survive `role=presentation|none` because they are interactive (ARIA 1.2 section
/// 5.2.7, presentational roles conflict resolution).
const fn survives_presentation(r: SemRole) -> bool {
    use SemRole as R;
    matches!(
        r,
        R::Button
            | R::Link
            | R::TextBox
            | R::SearchBox
            | R::PasswordBox
            | R::Checkbox
            | R::Radio
            | R::Switch
            | R::Combobox
            | R::Slider
            | R::SpinButton
            | R::Listbox
            | R::Option
            | R::MenuItem
            | R::MenuItemCheckbox
            | R::MenuItemRadio
            | R::Tab
            | R::TreeItem
    )
}

/// The engine-vs-ARIA rule. `focusable` is the FOCUSABLE state (a focusable element is never
/// presentational); `engine` is the already mapped engine role.
pub fn resolve_role(
    engine: SemRole,
    token: Option<SemRole>,
    focusable: bool,
) -> (SemRole, RoleSource) {
    let Some(tok) = token else {
        return (engine, RoleSource::Engine);
    };
    // A secret field is never relabelled by the author.
    if engine == SemRole::PasswordBox {
        return (engine, RoleSource::EngineOverToken);
    }
    // The document root keeps its role whatever the page claims.
    if engine == SemRole::DocumentWeb {
        return (engine, RoleSource::EngineOverToken);
    }
    match tok {
        SemRole::Presentation => {
            if focusable || survives_presentation(engine) {
                (engine, RoleSource::EngineOverToken)
            } else {
                (SemRole::Presentation, RoleSource::Token)
            }
        }
        // `generic` never downgrades a semantic engine role.
        SemRole::Generic if engine != SemRole::Generic && engine != SemRole::Unknown => {
            (engine, RoleSource::EngineOverToken)
        }
        other => (other, RoleSource::Token),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_aria_12_role_is_mapped() {
        for name in ARIA_12_ROLES {
            assert!(
                matches!(token_kind(name), TokenKind::Role(_)),
                "ARIA 1.2 role {name} is not mapped"
            );
        }
    }

    #[test]
    fn mapped_token_names_round_trip_with_the_registry() {
        // Tokens that share the registry name map to the same role.
        for r in SemRole::ALL {
            let n = r.name();
            if let TokenKind::Role(back) = token_kind(n) {
                assert_eq!(back, *r, "{n}");
            }
        }
    }

    #[test]
    fn deferred_and_abstract_tokens_are_listed_not_mapped() {
        assert_eq!(token_kind("doc-toc"), TokenKind::Unmapped);
        assert_eq!(token_kind("graphics-symbol"), TokenKind::Unmapped);
        assert_eq!(token_kind("widget"), TokenKind::Abstract);
        assert_eq!(token_kind("landmark"), TokenKind::Abstract);
        assert_eq!(token_kind("bogus"), TokenKind::Invalid);
        assert_eq!(token_kind(""), TokenKind::Invalid);
        assert!(
            unmapped_tokens()
                .iter()
                .all(|t| token_kind(t) == TokenKind::Unmapped)
        );
    }

    #[test]
    fn multi_token_fallback_skips_invalid_tokens() {
        let p = pick_token("bogus widget button");
        assert_eq!(p.role, Some(SemRole::Button));
        assert!(p.saw_invalid);
        let p = pick_token("doc-toc navigation");
        assert_eq!(p.role, Some(SemRole::Navigation));
        assert!(p.saw_unmapped && !p.saw_invalid);
        let p = pick_token("  Button   link ");
        assert_eq!(
            p.role,
            Some(SemRole::Button),
            "case-insensitive, extra spaces"
        );
        let p = pick_token("bogus");
        assert_eq!(p.role, None);
        assert!(p.saw_invalid);
        assert_eq!(pick_token("").role, None);
    }

    #[test]
    fn engine_role_table_is_total_and_lists_its_gaps() {
        for raw in 0..raw::LAST_DEFINED + 5 {
            let _ = engine_role(raw);
        }
        assert_eq!(engine_role(43), EngineRole::Role(SemRole::Button));
        assert_eq!(engine_role(95), EngineRole::Role(SemRole::DocumentWeb));
        assert_eq!(engine_role(130), EngineRole::Role(SemRole::Switch));
        assert_eq!(engine_role(9999), EngineRole::Unmapped);
        assert_eq!(unmapped_engine_roles(), vec![0, 5, 13, 67, 70, 74]);
    }

    #[test]
    fn html_tags_refine_only_weak_engine_roles() {
        assert_eq!(tag_role("code"), Some(SemRole::Code));
        assert_eq!(tag_role("fieldset"), Some(SemRole::Group));
        assert_eq!(tag_role("div"), None);
        assert!(is_weak_engine_role(SemRole::StaticText));
        assert!(is_weak_engine_role(SemRole::Generic));
        assert!(!is_weak_engine_role(SemRole::Button));
    }

    #[test]
    fn engine_vs_aria_rule() {
        use SemRole as R;
        assert_eq!(
            resolve_role(R::Generic, None, false),
            (R::Generic, RoleSource::Engine)
        );
        assert_eq!(
            resolve_role(R::Generic, Some(R::Banner), false),
            (R::Banner, RoleSource::Token)
        );
        // Presentation on a plain container wins; on a focusable or interactive node it loses.
        assert_eq!(
            resolve_role(R::Img, Some(R::Presentation), false),
            (R::Presentation, RoleSource::Token)
        );
        assert_eq!(
            resolve_role(R::Img, Some(R::Presentation), true),
            (R::Img, RoleSource::EngineOverToken)
        );
        assert_eq!(
            resolve_role(R::Button, Some(R::Presentation), false),
            (R::Button, RoleSource::EngineOverToken)
        );
        // A password field stays a password field.
        assert_eq!(
            resolve_role(R::PasswordBox, Some(R::TextBox), false),
            (R::PasswordBox, RoleSource::EngineOverToken)
        );
        // generic never downgrades.
        assert_eq!(
            resolve_role(R::Heading, Some(R::Generic), false),
            (R::Heading, RoleSource::EngineOverToken)
        );
        assert_eq!(
            resolve_role(R::Unknown, Some(R::Generic), false),
            (R::Generic, RoleSource::Token)
        );
    }
}
