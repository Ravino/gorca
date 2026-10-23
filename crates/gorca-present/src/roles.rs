// SPDX-License-Identifier: GPL-3.0-or-later
//! The native role table (ADR-0022 section 2.5, M-10): `SemRole` to the catalog word the native
//! focus row speaks, and the class that drives the state words. One exhaustive `match`, so a new
//! model role cannot be added without deciding what native focus says for it. It replaces the
//! detour through the legacy `Role` set (`SemRole::legacy_family`).
//!
//! Web content has its own table (slice 3: NVDA words); this one is for native toolkits.

use gorca_i18n::MsgId as M;
use gorca_model::sem::{Press, SemRole};

/// What a role does to the state words of the native row.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Class {
    /// Check box, radio button, check/radio menu item: says "not checked" when off.
    Checkable,
    /// A toggle button: "pressed"/"not pressed", "half pressed" when mixed.
    Toggle,
    /// A switch: "on" or "off" (never checked or pressed).
    Switch,
    /// A progress bar: "indeterminate" when mixed.
    Progress,
    /// A field whose description is its help or error text.
    Field,
    Other,
}

/// The word and class of a role.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RoleInfo {
    /// `None`: the native row says no role word for it (listed in [`UNMAPPED`]).
    pub word: Option<M>,
    pub class: Class,
}

const fn w(word: M, class: Class) -> RoleInfo {
    RoleInfo {
        word: Some(word),
        class,
    }
}

const fn plain(word: M) -> RoleInfo {
    w(word, Class::Other)
}

const NONE: RoleInfo = RoleInfo {
    word: None,
    class: Class::Other,
};

/// The role info of `role`. `press` turns a button with a pressed state into a toggle button.
pub fn native_role(role: SemRole, press: Press) -> RoleInfo {
    use SemRole as S;
    match role {
        S::Application => plain(M::RoleApplication),
        S::Window => plain(M::RoleWindow),
        S::Dialog | S::AlertDialog => plain(M::RoleDialog),
        S::Alert => plain(M::RoleAlert),
        S::Status => plain(M::RoleStatusBar),
        S::Generic | S::Group | S::Presentation => plain(M::RolePanel),
        S::Document | S::DocumentWeb => plain(M::RoleDocument),
        S::Heading => plain(M::RoleHeading),
        S::Paragraph => plain(M::RoleParagraph),
        S::Link => plain(M::RoleLink),
        S::Img => plain(M::RoleImage),
        S::List | S::Listbox | S::Feed | S::DescriptionList => plain(M::RoleList),
        S::ListItem | S::Option | S::Term | S::Definition => plain(M::RoleListItem),
        S::Table | S::Grid | S::TreeGrid => plain(M::RoleTable),
        S::Row => plain(M::RoleRow),
        S::Cell | S::GridCell => plain(M::RoleCell),
        S::ColumnHeader => plain(M::RoleColumnHeader),
        S::RowHeader => plain(M::RoleRowHeader),
        S::Button if press != Press::NotToggle => w(M::RoleToggleButton, Class::Toggle),
        S::Button => plain(M::RoleButton),
        S::Switch => w(M::RoleSwitch, Class::Switch),
        S::Checkbox | S::MenuItemCheckbox => w(M::RoleCheckBox, Class::Checkable),
        S::Radio | S::MenuItemRadio => w(M::RoleRadioButton, Class::Checkable),
        S::Combobox => w(M::RoleComboBox, Class::Field),
        S::TextBox | S::SearchBox => w(M::RoleTextEntry, Class::Field),
        S::PasswordBox => w(M::RolePasswordEntry, Class::Field),
        S::SpinButton => w(M::RoleSpinButton, Class::Field),
        S::Slider => plain(M::RoleSlider),
        S::Scrollbar => plain(M::RoleScrollBar),
        S::ProgressBar | S::Meter => w(M::RoleProgressBar, Class::Progress),
        S::Menu => plain(M::RoleMenu),
        S::Menubar => plain(M::RoleMenuBar),
        S::MenuItem => plain(M::RoleMenuItem),
        S::Popover => plain(M::RolePopover),
        S::Tooltip => plain(M::RoleTooltip),
        S::Tab => plain(M::RoleTab),
        S::TabList => plain(M::RoleTabList),
        S::Tree => plain(M::RoleTree),
        S::TreeItem => plain(M::RoleTreeItem),
        S::Toolbar => plain(M::RoleToolbar),
        S::Separator => plain(M::RoleSeparator),
        // A native label is spoken "label"; the web row (slice 3) says "text" for static text.
        S::Caption | S::StaticText => plain(M::RoleLabel),
        S::Terminal => plain(M::RoleTerminal),
        S::Region
        | S::Banner
        | S::Complementary
        | S::ContentInfo
        | S::Main
        | S::Navigation
        | S::Search
        | S::Article => plain(M::RoleLandmark),
        // No catalog word yet: the native row says none (the M-10 unmapped list).
        S::Unknown
        | S::Blockquote
        | S::Code
        | S::Deletion
        | S::Figure
        | S::Form
        | S::Insertion
        | S::Log
        | S::Mark
        | S::Marquee
        | S::Math
        | S::Note
        | S::RadioGroup
        | S::RowGroup
        | S::TabPanel
        | S::Timer
        | S::Comment
        | S::Suggestion
        | S::Details
        | S::Summary
        | S::Time
        | S::Iframe
        | S::Emphasis
        | S::Strong
        | S::Subscript
        | S::Superscript => NONE,
    }
}

/// The roles the native row has no word for (M-10 "unmapped" list).
pub const UNMAPPED: &[SemRole] = &[
    SemRole::Unknown,
    SemRole::Blockquote,
    SemRole::Code,
    SemRole::Deletion,
    SemRole::Figure,
    SemRole::Form,
    SemRole::Insertion,
    SemRole::Log,
    SemRole::Mark,
    SemRole::Marquee,
    SemRole::Math,
    SemRole::Note,
    SemRole::RadioGroup,
    SemRole::RowGroup,
    SemRole::TabPanel,
    SemRole::Timer,
    SemRole::Comment,
    SemRole::Suggestion,
    SemRole::Details,
    SemRole::Summary,
    SemRole::Time,
    SemRole::Iframe,
    SemRole::Emphasis,
    SemRole::Strong,
    SemRole::Subscript,
    SemRole::Superscript,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unmapped_list_is_exactly_the_roles_without_a_word() {
        let mut got = 0;
        for code in 0..=u16::MAX {
            let Some(role) = SemRole::from_code(code) else {
                continue;
            };
            let none = native_role(role, Press::NotToggle).word.is_none();
            assert_eq!(none, UNMAPPED.contains(&role), "{role:?}");
            got += 1;
        }
        assert!(got > 80, "the role registry was walked ({got})");
    }

    #[test]
    fn a_button_with_a_pressed_state_is_a_toggle_button() {
        let plain = native_role(SemRole::Button, Press::NotToggle);
        assert_eq!(plain.word, Some(M::RoleButton));
        let toggle = native_role(SemRole::Button, Press::Off);
        assert_eq!(
            (toggle.word, toggle.class),
            (Some(M::RoleToggleButton), Class::Toggle)
        );
    }

    #[test]
    fn a_native_label_says_label() {
        assert_eq!(
            native_role(SemRole::StaticText, Press::NotToggle).word,
            Some(M::RoleLabel)
        );
    }

    #[test]
    fn every_role_word_agrees_with_the_legacy_family_unless_improved() {
        // The table started as `legacy_family`; the differences are the documented improvements.
        for code in 0..=u16::MAX {
            let Some(role) = SemRole::from_code(code) else {
                continue;
            };
            let fam = role.legacy_family();
            let got = native_role(role, Press::NotToggle).word;
            let want = (fam != gorca_model::Role::Unknown).then(|| fam.msg());
            if got != want {
                assert!(
                    matches!(
                        role,
                        SemRole::StaticText
                            | SemRole::Scrollbar
                            | SemRole::Tooltip
                            | SemRole::Switch
                    ),
                    "{role:?}: table {got:?} vs legacy {want:?}"
                );
            }
        }
    }
}
