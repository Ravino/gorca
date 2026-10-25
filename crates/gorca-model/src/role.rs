// SPDX-License-Identifier: GPL-3.0-or-later
//! Core role enum: a superset mapping of ARIA 1.2, AT-SPI and AccessKit roles (ADR-0001 4.2).
//!
//! TBD: the full table is generated from the connector mapping work. Unknown raw roles map to
//! `Role::Unknown` plus a raw attribute (C-4). Generators branch on role and states, never on
//! interface presence.

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Role {
    Unknown,
    Application,
    Window,
    Dialog,
    Panel,
    Document,
    /// A top-level or nested web document (`DOCUMENT_WEB`, ADR-0003 AM-12).
    DocumentWeb,
    Heading,
    Paragraph,
    Link,
    Image,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    ColumnHeader,
    RowHeader,
    Button,
    ToggleButton,
    CheckBox,
    RadioButton,
    ComboBox,
    TextEntry,
    PasswordEntry,
    SpinButton,
    Slider,
    ProgressBar,
    Menu,
    MenuBar,
    MenuItem,
    Popover,
    Tab,
    TabList,
    Tree,
    TreeItem,
    Toolbar,
    StatusBar,
    Separator,
    Label,
    StaticText,
    Terminal,
    Landmark,
    Alert,
    /// A toggle that says on or off (GTK switch): `switch`, `on`, `off`.
    Switch,
}

impl Role {
    /// Roles whose focus is known to flap (ADR-0001 section 7.1: 30 ms settle delay).
    pub fn flaps_on_focus(self) -> bool {
        match self {
            Role::Menu | Role::MenuItem | Role::Popover => true,
            Role::Unknown
            | Role::Application
            | Role::Window
            | Role::Dialog
            | Role::Panel
            | Role::Document
            | Role::DocumentWeb
            | Role::Heading
            | Role::Paragraph
            | Role::Link
            | Role::Image
            | Role::List
            | Role::ListItem
            | Role::Table
            | Role::Row
            | Role::Cell
            | Role::ColumnHeader
            | Role::RowHeader
            | Role::Button
            | Role::ToggleButton
            | Role::Switch
            | Role::CheckBox
            | Role::RadioButton
            | Role::ComboBox
            | Role::TextEntry
            | Role::PasswordEntry
            | Role::SpinButton
            | Role::Slider
            | Role::ProgressBar
            | Role::MenuBar
            | Role::Tab
            | Role::TabList
            | Role::Tree
            | Role::TreeItem
            | Role::Toolbar
            | Role::StatusBar
            | Role::Separator
            | Role::Label
            | Role::StaticText
            | Role::Terminal
            | Role::Landmark
            | Role::Alert => false,
        }
    }

    /// Stable machine identifier (snake_case of the variant), used by the presentation log.
    pub fn log_name(self) -> &'static str {
        match self {
            Role::Unknown => "unknown",
            Role::Application => "application",
            Role::Window => "window",
            Role::Dialog => "dialog",
            Role::Panel => "panel",
            Role::Document => "document",
            Role::DocumentWeb => "document_web",
            Role::Heading => "heading",
            Role::Paragraph => "paragraph",
            Role::Link => "link",
            Role::Image => "image",
            Role::List => "list",
            Role::ListItem => "list_item",
            Role::Table => "table",
            Role::Row => "row",
            Role::Cell => "cell",
            Role::ColumnHeader => "column_header",
            Role::RowHeader => "row_header",
            Role::Button => "button",
            Role::ToggleButton => "toggle_button",
            Role::Switch => "switch",
            Role::CheckBox => "check_box",
            Role::RadioButton => "radio_button",
            Role::ComboBox => "combo_box",
            Role::TextEntry => "text_entry",
            Role::PasswordEntry => "password_entry",
            Role::SpinButton => "spin_button",
            Role::Slider => "slider",
            Role::ProgressBar => "progress_bar",
            Role::Menu => "menu",
            Role::MenuBar => "menu_bar",
            Role::MenuItem => "menu_item",
            Role::Popover => "popover",
            Role::Tab => "tab",
            Role::TabList => "tab_list",
            Role::Tree => "tree",
            Role::TreeItem => "tree_item",
            Role::Toolbar => "toolbar",
            Role::StatusBar => "status_bar",
            Role::Separator => "separator",
            Role::Label => "label",
            Role::StaticText => "static_text",
            Role::Terminal => "terminal",
            Role::Landmark => "landmark",
            Role::Alert => "alert",
        }
    }

    /// Roles whose accessible name is computed from their content (B-1 P9-3): a name change of a
    /// descendant label changes the name of such an ancestor without an event on it.
    pub fn takes_name_from_content(self) -> bool {
        match self {
            Role::Heading
            | Role::Link
            | Role::ListItem
            | Role::Row
            | Role::Cell
            | Role::ColumnHeader
            | Role::RowHeader
            | Role::Button
            | Role::ToggleButton
            | Role::Switch
            | Role::CheckBox
            | Role::RadioButton
            | Role::MenuItem
            | Role::Tab
            | Role::TreeItem => true,
            Role::Unknown
            | Role::Application
            | Role::Window
            | Role::Dialog
            | Role::Panel
            | Role::Document
            | Role::DocumentWeb
            | Role::Paragraph
            | Role::Image
            | Role::List
            | Role::Table
            | Role::ComboBox
            | Role::TextEntry
            | Role::PasswordEntry
            | Role::SpinButton
            | Role::Slider
            | Role::ProgressBar
            | Role::Menu
            | Role::MenuBar
            | Role::Popover
            | Role::TabList
            | Role::Tree
            | Role::Toolbar
            | Role::StatusBar
            | Role::Separator
            | Role::Label
            | Role::StaticText
            | Role::Terminal
            | Role::Landmark
            | Role::Alert => false,
        }
    }

    /// The spoken name of the role in the UI language (catalog `role-*`).
    pub fn spoken(self) -> &'static str {
        gorca_i18n::t(self.msg())
    }

    /// The catalog message of the role word (`role-*`).
    pub fn msg(self) -> gorca_i18n::MsgId {
        use gorca_i18n::MsgId as M;
        match self {
            Role::Unknown => M::RoleUnknown,
            Role::Application => M::RoleApplication,
            Role::Window => M::RoleWindow,
            Role::Dialog => M::RoleDialog,
            Role::Panel => M::RolePanel,
            Role::Document | Role::DocumentWeb => M::RoleDocument,
            Role::Heading => M::RoleHeading,
            Role::Paragraph => M::RoleParagraph,
            Role::Link => M::RoleLink,
            Role::Image => M::RoleImage,
            Role::List => M::RoleList,
            Role::ListItem => M::RoleListItem,
            Role::Table => M::RoleTable,
            Role::Row => M::RoleRow,
            Role::Cell => M::RoleCell,
            Role::ColumnHeader => M::RoleColumnHeader,
            Role::RowHeader => M::RoleRowHeader,
            Role::Button => M::RoleButton,
            Role::ToggleButton => M::RoleToggleButton,
            Role::Switch => M::RoleSwitch,
            Role::CheckBox => M::RoleCheckBox,
            Role::RadioButton => M::RoleRadioButton,
            Role::ComboBox => M::RoleComboBox,
            Role::TextEntry => M::RoleTextEntry,
            Role::PasswordEntry => M::RolePasswordEntry,
            Role::SpinButton => M::RoleSpinButton,
            Role::Slider => M::RoleSlider,
            Role::ProgressBar => M::RoleProgressBar,
            Role::Menu => M::RoleMenu,
            Role::MenuBar => M::RoleMenuBar,
            Role::MenuItem => M::RoleMenuItem,
            Role::Popover => M::RolePopover,
            Role::Tab => M::RoleTab,
            Role::TabList => M::RoleTabList,
            Role::Tree => M::RoleTree,
            Role::TreeItem => M::RoleTreeItem,
            Role::Toolbar => M::RoleToolbar,
            Role::StatusBar => M::RoleStatusBar,
            Role::Separator => M::RoleSeparator,
            Role::Label => M::RoleLabel,
            Role::StaticText => M::RoleStaticText,
            Role::Terminal => M::RoleTerminal,
            Role::Landmark => M::RoleLandmark,
            Role::Alert => M::RoleAlert,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_menus_and_popovers_flap() {
        assert!(Role::Menu.flaps_on_focus());
        assert!(Role::MenuItem.flaps_on_focus());
        assert!(Role::Popover.flaps_on_focus());
        assert!(!Role::Button.flaps_on_focus());
    }

    #[test]
    fn unknown_role_has_no_spoken_name() {
        assert_eq!(Role::Unknown.spoken(), "");
        assert_eq!(Role::Button.spoken(), "button");
    }
}
