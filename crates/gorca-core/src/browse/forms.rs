// SPDX-License-Identifier: GPL-3.0-or-later
//! The pass-through predicate `P(node, reason)` (browse-mode design 7.2, ADR-0003 11), after
//! NVDA's `shouldPassThrough` (`refs/nvda/source/browseMode.py:326-440`).
//!
//! It decides when focus or activation on a node switches from browse mode to focus mode: text
//! entry, combo boxes, list boxes, trees, sliders, spin buttons, menus, application regions.
//! Quick navigation never switches mode (`QuickNav` is always false), so `E`/`F` through a form
//! cannot throw the user into a field.

use gorca_model::sem::{Select, SemFlags, SemRole, Semantics};

/// Why the predicate is asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    /// Focus landed on the node (Tab, a click, a script).
    Focus,
    /// The user pressed Enter or Space on the node in browse mode. (Quick navigation never
    /// switches mode, so it does not ask.)
    Activate,
}

/// The facts of a node the predicate reads, from its fused semantics.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // the five facts the predicate reads
pub struct PassFacts {
    pub disabled: bool,
    pub editable: bool,
    pub read_only: bool,
    pub focusable: bool,
    pub selectable: bool,
}

impl PassFacts {
    /// The facts of `sem`. A web control that is disabled still passes the keys through (the web
    /// path never had a disabled state), so `honour_disabled` is false for document nodes.
    pub fn of(sem: &Semantics, honour_disabled: bool) -> Self {
        Self {
            disabled: honour_disabled && sem.avail == gorca_model::sem::Avail::Disabled,
            editable: sem.flags.contains(SemFlags::EDITABLE),
            read_only: sem.flags.contains(SemFlags::READ_ONLY),
            focusable: sem.flags.contains(SemFlags::FOCUSABLE),
            selectable: sem.select != Select::NotSelectable,
        }
    }
}

/// Does `reason` on a node of this role and with these facts pass keys through to the
/// application?
pub fn passes_through(role: SemRole, facts: PassFacts, reason: Reason) -> bool {
    match reason {
        Reason::Focus | Reason::Activate => {
            if facts.disabled {
                return false;
            }
            let editable = facts.editable
                && !facts.read_only
                && !matches!(role, SemRole::Document | SemRole::DocumentWeb);
            // D-1 (NVDA `SWITCH_TO_PASS_THROUGH_ON_FOCUS_ROLES`, `ALWAYS_SWITCH...` grid cells):
            // a focus event on a radio button, tab, menu or list item, or on a focusable cell of
            // an interactive grid enters focus mode so the arrows drive the widget. Activation
            // (Enter in browse mode) keeps the browse rules.
            let focus_widget = reason == Reason::Focus
                && match role {
                    // The node is the target of a focus event: these roles are widgets whatever
                    // the state set says (roving tabindex items may lack `focusable`).
                    SemRole::Radio | SemRole::MenuItemRadio | SemRole::Tab | SemRole::MenuItem => {
                        true
                    }
                    SemRole::ListItem | SemRole::Option => facts.focusable || facts.selectable,
                    SemRole::Cell
                    | SemRole::GridCell
                    | SemRole::Row
                    | SemRole::ColumnHeader
                    | SemRole::RowHeader => facts.focusable,
                    SemRole::Unknown
                    | SemRole::Generic
                    | SemRole::Alert
                    | SemRole::AlertDialog
                    | SemRole::Application
                    | SemRole::Article
                    | SemRole::Banner
                    | SemRole::Blockquote
                    | SemRole::Button
                    | SemRole::Caption
                    | SemRole::Checkbox
                    | SemRole::Code
                    | SemRole::Combobox
                    | SemRole::Comment
                    | SemRole::Complementary
                    | SemRole::ContentInfo
                    | SemRole::Definition
                    | SemRole::Deletion
                    | SemRole::DescriptionList
                    | SemRole::Details
                    | SemRole::Dialog
                    | SemRole::Document
                    | SemRole::DocumentWeb
                    | SemRole::Emphasis
                    | SemRole::Feed
                    | SemRole::Figure
                    | SemRole::Form
                    | SemRole::Grid
                    | SemRole::Group
                    | SemRole::Heading
                    | SemRole::Iframe
                    | SemRole::Img
                    | SemRole::Insertion
                    | SemRole::Link
                    | SemRole::List
                    | SemRole::Listbox
                    | SemRole::Log
                    | SemRole::Main
                    | SemRole::Mark
                    | SemRole::Marquee
                    | SemRole::Math
                    | SemRole::Menu
                    | SemRole::Menubar
                    | SemRole::MenuItemCheckbox
                    | SemRole::Meter
                    | SemRole::Navigation
                    | SemRole::Note
                    | SemRole::Paragraph
                    | SemRole::PasswordBox
                    | SemRole::Popover
                    | SemRole::Presentation
                    | SemRole::ProgressBar
                    | SemRole::RadioGroup
                    | SemRole::Region
                    | SemRole::RowGroup
                    | SemRole::Scrollbar
                    | SemRole::Search
                    | SemRole::SearchBox
                    | SemRole::Separator
                    | SemRole::Slider
                    | SemRole::SpinButton
                    | SemRole::StaticText
                    | SemRole::Status
                    | SemRole::Strong
                    | SemRole::Subscript
                    | SemRole::Suggestion
                    | SemRole::Summary
                    | SemRole::Superscript
                    | SemRole::Switch
                    | SemRole::Table
                    | SemRole::TabList
                    | SemRole::TabPanel
                    | SemRole::Term
                    | SemRole::Terminal
                    | SemRole::TextBox
                    | SemRole::Time
                    | SemRole::Timer
                    | SemRole::Toolbar
                    | SemRole::Tooltip
                    | SemRole::Tree
                    | SemRole::TreeGrid
                    | SemRole::TreeItem
                    | SemRole::Window => false,
                };
            editable
                || focus_widget
                || match role {
                    SemRole::TextBox
                    | SemRole::SearchBox
                    | SemRole::PasswordBox
                    | SemRole::Combobox
                    | SemRole::Slider
                    | SemRole::SpinButton
                    | SemRole::Menu
                    | SemRole::Menubar
                    | SemRole::MenuItem
                    | SemRole::Application => true,
                    // Lists and trees are passive text in a page; a focusable one is a widget.
                    SemRole::List
                    | SemRole::Listbox
                    | SemRole::Tree
                    | SemRole::TreeItem
                    | SemRole::TabList
                    | SemRole::Tab
                    | SemRole::Table
                    | SemRole::Grid
                    | SemRole::TreeGrid => facts.focusable && widget_role(role),
                    SemRole::Unknown
                    | SemRole::Generic
                    | SemRole::Alert
                    | SemRole::AlertDialog
                    | SemRole::Article
                    | SemRole::Banner
                    | SemRole::Blockquote
                    | SemRole::Button
                    | SemRole::Caption
                    | SemRole::Cell
                    | SemRole::Checkbox
                    | SemRole::Code
                    | SemRole::ColumnHeader
                    | SemRole::Comment
                    | SemRole::Complementary
                    | SemRole::ContentInfo
                    | SemRole::Definition
                    | SemRole::Deletion
                    | SemRole::DescriptionList
                    | SemRole::Details
                    | SemRole::Dialog
                    | SemRole::Document
                    | SemRole::DocumentWeb
                    | SemRole::Emphasis
                    | SemRole::Feed
                    | SemRole::Figure
                    | SemRole::Form
                    | SemRole::GridCell
                    | SemRole::Group
                    | SemRole::Heading
                    | SemRole::Iframe
                    | SemRole::Img
                    | SemRole::Insertion
                    | SemRole::Link
                    | SemRole::ListItem
                    | SemRole::Log
                    | SemRole::Main
                    | SemRole::Mark
                    | SemRole::Marquee
                    | SemRole::Math
                    | SemRole::MenuItemCheckbox
                    | SemRole::MenuItemRadio
                    | SemRole::Meter
                    | SemRole::Navigation
                    | SemRole::Note
                    | SemRole::Option
                    | SemRole::Paragraph
                    | SemRole::Popover
                    | SemRole::Presentation
                    | SemRole::ProgressBar
                    | SemRole::Radio
                    | SemRole::RadioGroup
                    | SemRole::Region
                    | SemRole::Row
                    | SemRole::RowGroup
                    | SemRole::RowHeader
                    | SemRole::Scrollbar
                    | SemRole::Search
                    | SemRole::Separator
                    | SemRole::StaticText
                    | SemRole::Status
                    | SemRole::Strong
                    | SemRole::Subscript
                    | SemRole::Suggestion
                    | SemRole::Summary
                    | SemRole::Superscript
                    | SemRole::Switch
                    | SemRole::TabPanel
                    | SemRole::Term
                    | SemRole::Terminal
                    | SemRole::Time
                    | SemRole::Timer
                    | SemRole::Toolbar
                    | SemRole::Tooltip
                    | SemRole::Window => false,
                }
        }
    }
}

/// Focusable instances of these roles are interactive widgets (listbox, tree, tab strip), not
/// document structure.
fn widget_role(role: SemRole) -> bool {
    matches!(
        role,
        SemRole::List
            | SemRole::Listbox
            | SemRole::Tree
            | SemRole::TreeItem
            | SemRole::TabList
            | SemRole::Tab
    )
}

/// `role=application` style regions that only the user's toggle leaves (`Focus{Application}`).
pub fn is_application(role: SemRole) -> bool {
    role == SemRole::Application
}

#[cfg(test)]
mod tests {
    use super::*;

    const EDITABLE: PassFacts = PassFacts {
        disabled: false,
        editable: true,
        read_only: false,
        focusable: false,
        selectable: false,
    };
    const FOCUSABLE: PassFacts = PassFacts {
        disabled: false,
        editable: false,
        read_only: false,
        focusable: true,
        selectable: false,
    };
    const PLAIN: PassFacts = PassFacts {
        disabled: false,
        editable: false,
        read_only: false,
        focusable: false,
        selectable: false,
    };

    #[test]
    fn editable_and_widget_roles_pass_keys_through_on_focus_and_activation() {
        for r in [Reason::Focus, Reason::Activate] {
            assert!(passes_through(SemRole::TextBox, EDITABLE, r));
            assert!(passes_through(SemRole::PasswordBox, EDITABLE, r));
            assert!(passes_through(SemRole::Combobox, FOCUSABLE, r));
            assert!(passes_through(SemRole::Slider, FOCUSABLE, r));
            assert!(
                passes_through(SemRole::Generic, EDITABLE, r),
                "contenteditable"
            );
            assert!(passes_through(SemRole::List, FOCUSABLE, r), "listbox");
            assert!(passes_through(SemRole::Listbox, FOCUSABLE, r), "listbox");
            assert!(!passes_through(SemRole::List, PLAIN, r), "plain ul");
            assert!(!passes_through(SemRole::Link, FOCUSABLE, r));
            assert!(!passes_through(SemRole::Button, FOCUSABLE, r));
            assert!(!passes_through(SemRole::Checkbox, FOCUSABLE, r));
        }
    }

    #[test]
    fn disabled_fields_do_not_pass_keys_through_and_read_only_entries_stay_fields() {
        let disabled = PassFacts {
            disabled: true,
            ..EDITABLE
        };
        assert!(!passes_through(SemRole::TextBox, disabled, Reason::Focus));
        // A read-only text entry (the role is still an entry): the role rule applies, the state
        // rule does not, so an entry stays a field.
        assert!(passes_through(
            SemRole::TextBox,
            PassFacts {
                read_only: true,
                ..EDITABLE
            },
            Reason::Focus
        ));
        // The document itself is editable in designMode pages but is not a field.
        assert!(!passes_through(
            SemRole::DocumentWeb,
            EDITABLE,
            Reason::Focus
        ));
    }

    #[test]
    fn d1_radios_tabs_menu_items_and_grid_cells_pass_keys_through_on_focus_only() {
        for r in [
            SemRole::Radio,
            SemRole::Tab,
            SemRole::MenuItem,
            SemRole::ListItem,
            SemRole::Cell,
        ] {
            assert!(passes_through(r, FOCUSABLE, Reason::Focus), "{r:?}");
        }
        assert!(!passes_through(SemRole::Radio, FOCUSABLE, Reason::Activate));
        // a plain table cell or list item (not focusable) stays document text
        assert!(!passes_through(SemRole::Cell, PLAIN, Reason::Focus));
        assert!(!passes_through(SemRole::ListItem, PLAIN, Reason::Focus));
    }

    #[test]
    fn the_facts_follow_the_fused_semantics() {
        let mut sem = Semantics::default();
        sem.flags |= SemFlags::EDITABLE | SemFlags::FOCUSABLE;
        sem.avail = gorca_model::sem::Avail::Disabled;
        let web = PassFacts::of(&sem, false);
        assert!(web.editable && web.focusable && !web.disabled && !web.selectable);
        assert!(
            PassFacts::of(&sem, true).disabled,
            "a native control is disabled"
        );
        sem.select = Select::Off;
        assert!(PassFacts::of(&sem, false).selectable);
    }
}
