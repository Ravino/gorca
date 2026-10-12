//! Role normalisation table (C-4). TBD: complete mapping; unknown roles must also carry a raw
//! attribute so they are not lost.

use atspi::Role as Raw;
use gorca_model::Role;

pub fn map_role(raw: Raw) -> Role {
    match raw {
        Raw::Application => Role::Application,
        Raw::Frame | Raw::Window => Role::Window,
        Raw::Dialog => Role::Dialog,
        Raw::Alert => Role::Alert,
        Raw::Panel | Raw::Filler | Raw::Grouping | Raw::Section => Role::Panel,
        Raw::DocumentFrame
        | Raw::DocumentWeb
        | Raw::DocumentText
        | Raw::DocumentEmail
        | Raw::DocumentSpreadsheet
        | Raw::DocumentPresentation => Role::Document,
        Raw::Heading => Role::Heading,
        Raw::Paragraph => Role::Paragraph,
        Raw::Link => Role::Link,
        Raw::Image | Raw::Icon => Role::Image,
        Raw::List | Raw::ListBox => Role::List,
        Raw::ListItem => Role::ListItem,
        Raw::Table | Raw::TreeTable => Role::Table,
        Raw::TableRow => Role::Row,
        Raw::TableCell => Role::Cell,
        Raw::ColumnHeader | Raw::TableColumnHeader => Role::ColumnHeader,
        Raw::RowHeader | Raw::TableRowHeader => Role::RowHeader,
        Raw::Button => Role::Button,
        Raw::ToggleButton => Role::ToggleButton,
        Raw::CheckBox | Raw::CheckMenuItem => Role::CheckBox,
        Raw::RadioButton | Raw::RadioMenuItem => Role::RadioButton,
        Raw::ComboBox => Role::ComboBox,
        Raw::Entry | Raw::Text => Role::TextEntry,
        Raw::PasswordText => Role::PasswordEntry,
        Raw::SpinButton => Role::SpinButton,
        Raw::Slider => Role::Slider,
        Raw::ProgressBar => Role::ProgressBar,
        Raw::Menu | Raw::PopupMenu => Role::Menu,
        Raw::MenuBar => Role::MenuBar,
        Raw::MenuItem => Role::MenuItem,
        Raw::PageTab => Role::Tab,
        Raw::PageTabList => Role::TabList,
        Raw::Tree => Role::Tree,
        Raw::TreeItem => Role::TreeItem,
        Raw::ToolBar => Role::Toolbar,
        Raw::StatusBar => Role::StatusBar,
        Raw::Separator => Role::Separator,
        Raw::Label | Raw::Caption => Role::Label,
        Raw::Static => Role::StaticText,
        Raw::Terminal => Role::Terminal,
        Raw::Landmark => Role::Landmark,
        // C-4: everything else, including roles added by future atspi releases.
        _ => Role::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_roles_map() {
        assert_eq!(map_role(Raw::Button), Role::Button);
        assert_eq!(map_role(Raw::Frame), Role::Window);
        assert_eq!(map_role(Raw::DocumentWeb), Role::Document);
        assert_eq!(map_role(Raw::Terminal), Role::Terminal);
        assert_eq!(map_role(Raw::PasswordText), Role::PasswordEntry);
    }

    #[test]
    fn dialog_and_alert_stay_distinct() {
        assert_eq!(map_role(Raw::Dialog), Role::Dialog);
        assert_eq!(map_role(Raw::Alert), Role::Alert);
    }

    #[test]
    fn unmapped_roles_are_unknown_not_a_panic() {
        assert_eq!(map_role(Raw::Invalid), Role::Unknown);
        assert_eq!(map_role(Raw::Ruler), Role::Unknown);
    }
}
