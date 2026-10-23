// SPDX-License-Identifier: GPL-3.0-or-later
//! `SemRole`: the fine-grained, ARIA 1.2 based role (ADR-0022 section 2.1).

use super::coded::coded_enum;
use crate::role::Role;

coded_enum! {
    /// ARIA 1.2 core roles, the ADR-0022 r2 additions and native-only roles. The DPUB and
    /// graphics-aria subsets are appended later (B4 coverage work).
    ///
    /// Append-only `u16` codes: a code is never reused or renumbered (it is a WIT code at the
    /// extension boundary, ADR-0019 V-3). `Unknown` (0) is "not observed or not mapped".
    pub enum SemRole: u16 {
        Unknown = 0 => "unknown",
        Generic = 1 => "generic",
        Alert = 2 => "alert",
        AlertDialog = 3 => "alertdialog",
        Application = 4 => "application",
        Article = 5 => "article",
        Banner = 6 => "banner",
        Blockquote = 7 => "blockquote",
        Button = 8 => "button",
        Caption = 9 => "caption",
        Cell = 10 => "cell",
        Checkbox = 11 => "checkbox",
        Code = 12 => "code",
        ColumnHeader = 13 => "columnheader",
        Combobox = 14 => "combobox",
        Complementary = 15 => "complementary",
        ContentInfo = 16 => "contentinfo",
        Definition = 17 => "definition",
        Deletion = 18 => "deletion",
        Dialog = 19 => "dialog",
        Document = 20 => "document",
        DocumentWeb = 21 => "document-web",
        Feed = 22 => "feed",
        Figure = 23 => "figure",
        Form = 24 => "form",
        Grid = 25 => "grid",
        GridCell = 26 => "gridcell",
        Group = 27 => "group",
        Heading = 28 => "heading",
        Img = 29 => "img",
        Insertion = 30 => "insertion",
        Link = 31 => "link",
        List = 32 => "list",
        Listbox = 33 => "listbox",
        ListItem = 34 => "listitem",
        Log = 35 => "log",
        Main = 36 => "main",
        Mark = 37 => "mark",
        Marquee = 38 => "marquee",
        Math = 39 => "math",
        Menu = 40 => "menu",
        Menubar = 41 => "menubar",
        MenuItem = 42 => "menuitem",
        MenuItemCheckbox = 43 => "menuitemcheckbox",
        MenuItemRadio = 44 => "menuitemradio",
        Meter = 45 => "meter",
        Navigation = 46 => "navigation",
        Note = 47 => "note",
        Option = 48 => "option",
        Paragraph = 49 => "paragraph",
        ProgressBar = 50 => "progressbar",
        Radio = 51 => "radio",
        RadioGroup = 52 => "radiogroup",
        Region = 53 => "region",
        Row = 54 => "row",
        RowGroup = 55 => "rowgroup",
        RowHeader = 56 => "rowheader",
        Scrollbar = 57 => "scrollbar",
        Search = 58 => "search",
        SearchBox = 59 => "searchbox",
        Separator = 60 => "separator",
        Slider = 61 => "slider",
        SpinButton = 62 => "spinbutton",
        Status = 63 => "status",
        Switch = 64 => "switch",
        Tab = 65 => "tab",
        Table = 66 => "table",
        TabList = 67 => "tablist",
        TabPanel = 68 => "tabpanel",
        Term = 69 => "term",
        TextBox = 70 => "textbox",
        PasswordBox = 71 => "passwordbox",
        Timer = 72 => "timer",
        Toolbar = 73 => "toolbar",
        Tooltip = 74 => "tooltip",
        Tree = 75 => "tree",
        TreeGrid = 76 => "treegrid",
        TreeItem = 77 => "treeitem",
        Terminal = 78 => "terminal",
        Window = 79 => "window",
        Popover = 80 => "popover",
        StaticText = 81 => "statictext",
        Comment = 82 => "comment",
        Suggestion = 83 => "suggestion",
        Details = 84 => "details",
        Summary = 85 => "summary",
        DescriptionList = 86 => "descriptionlist",
        Time = 87 => "time",
        Iframe = 88 => "iframe",
        Emphasis = 89 => "emphasis",
        Strong = 90 => "strong",
        Subscript = 91 => "subscript",
        Superscript = 92 => "superscript",
        Presentation = 93 => "presentation",
    }
}

impl SemRole {
    /// The legacy role a model role stands for (what the legacy mapping gives for the engine role the
    /// model role comes from). `Unknown` for model roles the legacy set has no word for.
    pub fn legacy_family(self) -> Role {
        use SemRole as S;
        match self {
            S::Application => Role::Application,
            S::Window => Role::Window,
            S::Dialog | S::AlertDialog => Role::Dialog,
            S::Alert => Role::Alert,
            S::Status => Role::StatusBar,
            S::Generic | S::Group | S::Presentation => Role::Panel,
            S::Document => Role::Document,
            S::DocumentWeb => Role::DocumentWeb,
            S::Heading => Role::Heading,
            S::Paragraph => Role::Paragraph,
            S::Link => Role::Link,
            S::Img => Role::Image,
            S::List | S::Listbox | S::Feed | S::DescriptionList => Role::List,
            S::ListItem | S::Option | S::Term | S::Definition => Role::ListItem,
            S::Table | S::Grid | S::TreeGrid => Role::Table,
            S::Row => Role::Row,
            S::Cell | S::GridCell => Role::Cell,
            S::ColumnHeader => Role::ColumnHeader,
            S::RowHeader => Role::RowHeader,
            S::Button => Role::Button,
            S::Switch => Role::ToggleButton,
            S::Checkbox | S::MenuItemCheckbox => Role::CheckBox,
            S::Radio | S::MenuItemRadio => Role::RadioButton,
            S::Combobox => Role::ComboBox,
            S::TextBox | S::SearchBox => Role::TextEntry,
            S::PasswordBox => Role::PasswordEntry,
            S::SpinButton => Role::SpinButton,
            S::Slider | S::Scrollbar => Role::Slider,
            S::ProgressBar | S::Meter => Role::ProgressBar,
            S::Menu => Role::Menu,
            S::Menubar => Role::MenuBar,
            S::MenuItem => Role::MenuItem,
            S::Popover | S::Tooltip => Role::Popover,
            S::Tab => Role::Tab,
            S::TabList => Role::TabList,
            S::Tree => Role::Tree,
            S::TreeItem => Role::TreeItem,
            S::Toolbar => Role::Toolbar,
            S::Separator => Role::Separator,
            S::Caption => Role::Label,
            S::StaticText => Role::StaticText,
            S::Terminal => Role::Terminal,
            S::Region
            | S::Banner
            | S::Complementary
            | S::ContentInfo
            | S::Main
            | S::Navigation
            | S::Search
            | S::Article => Role::Landmark,
            // The legacy set has no word for these: an unlisted new role must be placed here.
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
            | S::Superscript => Role::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_dense_and_round_trip() {
        for (i, r) in SemRole::ALL.iter().enumerate() {
            assert_eq!(usize::from(r.code()), i, "{r:?}");
            assert_eq!(SemRole::from_code(r.code()), Some(*r));
            assert_eq!(SemRole::from_name(r.name()), Some(*r));
        }
        assert_eq!(SemRole::from_code(u16::MAX), None);
        assert_eq!(SemRole::default(), SemRole::Unknown);
    }
}
