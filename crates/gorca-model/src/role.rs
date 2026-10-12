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

    /// Short English spoken name, used by the hot-path presenter until verbalisation tables exist.
    pub fn spoken(self) -> &'static str {
        match self {
            Role::Unknown => "",
            Role::Application => "application",
            Role::Window => "window",
            Role::Dialog => "dialog",
            Role::Panel => "panel",
            Role::Document => "document",
            Role::Heading => "heading",
            Role::Paragraph => "paragraph",
            Role::Link => "link",
            Role::Image => "image",
            Role::List => "list",
            Role::ListItem => "list item",
            Role::Table => "table",
            Role::Row => "row",
            Role::Cell => "cell",
            Role::ColumnHeader => "column header",
            Role::RowHeader => "row header",
            Role::Button => "button",
            Role::ToggleButton => "toggle button",
            Role::CheckBox => "check box",
            Role::RadioButton => "radio button",
            Role::ComboBox => "combo box",
            Role::TextEntry => "edit",
            Role::PasswordEntry => "password edit",
            Role::SpinButton => "spin button",
            Role::Slider => "slider",
            Role::ProgressBar => "progress bar",
            Role::Menu => "menu",
            Role::MenuBar => "menu bar",
            Role::MenuItem => "menu item",
            Role::Popover => "popover",
            Role::Tab => "tab",
            Role::TabList => "tab list",
            Role::Tree => "tree",
            Role::TreeItem => "tree item",
            Role::Toolbar => "toolbar",
            Role::StatusBar => "status bar",
            Role::Separator => "separator",
            Role::Label => "label",
            Role::StaticText => "text",
            Role::Terminal => "terminal",
            Role::Landmark => "landmark",
            Role::Alert => "alert",
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
