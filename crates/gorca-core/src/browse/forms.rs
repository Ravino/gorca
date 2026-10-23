// SPDX-License-Identifier: GPL-3.0-or-later
//! The pass-through predicate `P(node, reason)` (browse-mode design 7.2, ADR-0003 11), after
//! NVDA's `shouldPassThrough` (`refs/nvda/source/browseMode.py:326-440`).
//!
//! It decides when focus or activation on a node switches from browse mode to focus mode: text
//! entry, combo boxes, list boxes, trees, sliders, spin buttons, menus, application regions.
//! Quick navigation never switches mode (`QuickNav` is always false), so `E`/`F` through a form
//! cannot throw the user into a field.

use gorca_model::{Role, StateSet};

/// Why the predicate is asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Reason {
    /// Focus landed on the node (Tab, a click, a script).
    Focus,
    /// The user pressed Enter or Space on the node in browse mode. (Quick navigation never
    /// switches mode, so it does not ask.)
    Activate,
}

/// Does `reason` on a node of this role and state pass keys through to the application?
pub fn passes_through(role: Role, states: StateSet, reason: Reason) -> bool {
    match reason {
        Reason::Focus | Reason::Activate => {
            if states.contains(StateSet::DISABLED) {
                return false;
            }
            let editable = states.contains(StateSet::EDITABLE)
                && !states.contains(StateSet::READ_ONLY)
                && !matches!(role, Role::Document | Role::DocumentWeb);
            // D-1 (NVDA `SWITCH_TO_PASS_THROUGH_ON_FOCUS_ROLES`, `ALWAYS_SWITCH...` grid cells):
            // a focus event on a radio button, tab, menu or list item, or on a focusable cell of
            // an interactive grid enters focus mode so the arrows drive the widget. Activation
            // (Enter in browse mode) keeps the browse rules.
            let focusable = states.contains(StateSet::FOCUSABLE);
            let focus_widget = reason == Reason::Focus
                && match role {
                    // The node is the target of a focus event: these roles are widgets whatever
                    // the state set says (roving tabindex items may lack `focusable`).
                    Role::RadioButton | Role::Tab | Role::MenuItem => true,
                    Role::ListItem => focusable || states.contains(StateSet::SELECTABLE),
                    Role::Cell | Role::Row | Role::ColumnHeader | Role::RowHeader => focusable,
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
                    | Role::Table
                    | Role::Button
                    | Role::ToggleButton
                    | Role::Switch
                    | Role::CheckBox
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
                    | Role::TreeItem
                    | Role::Toolbar
                    | Role::StatusBar
                    | Role::Separator
                    | Role::Label
                    | Role::StaticText
                    | Role::Terminal
                    | Role::Landmark
                    | Role::Alert => false,
                };
            editable
                || focus_widget
                || match role {
                    Role::TextEntry
                    | Role::PasswordEntry
                    | Role::ComboBox
                    | Role::Slider
                    | Role::SpinButton
                    | Role::Menu
                    | Role::MenuBar
                    | Role::MenuItem
                    | Role::Application => true,
                    // Lists and trees are passive text in a page; a focusable one is a widget.
                    Role::List
                    | Role::Tree
                    | Role::TreeItem
                    | Role::TabList
                    | Role::Tab
                    | Role::Table => states.contains(StateSet::FOCUSABLE) && widget_role(role),
                    Role::Unknown
                    | Role::Window
                    | Role::Dialog
                    | Role::Panel
                    | Role::Document
                    | Role::DocumentWeb
                    | Role::Heading
                    | Role::Paragraph
                    | Role::Link
                    | Role::Image
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
                    | Role::ProgressBar
                    | Role::Popover
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
    }
}

/// Focusable instances of these roles are interactive widgets (listbox, tree, tab strip), not
/// document structure.
fn widget_role(role: Role) -> bool {
    matches!(
        role,
        Role::List | Role::Tree | Role::TreeItem | Role::TabList | Role::Tab
    )
}

/// `role=application` style regions that only the user's toggle leaves (`Focus{Application}`).
pub fn is_application(role: Role) -> bool {
    role == Role::Application
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(f: StateSet) -> StateSet {
        f | StateSet::ENABLED | StateSet::VISIBLE
    }

    #[test]
    fn editable_and_widget_roles_pass_keys_through_on_focus_and_activation() {
        for r in [Reason::Focus, Reason::Activate] {
            assert!(passes_through(Role::TextEntry, st(StateSet::EDITABLE), r));
            assert!(passes_through(
                Role::PasswordEntry,
                st(StateSet::EDITABLE),
                r
            ));
            assert!(passes_through(Role::ComboBox, st(StateSet::FOCUSABLE), r));
            assert!(passes_through(Role::Slider, st(StateSet::FOCUSABLE), r));
            assert!(
                passes_through(Role::Panel, st(StateSet::EDITABLE), r),
                "contenteditable"
            );
            assert!(
                passes_through(Role::List, st(StateSet::FOCUSABLE), r),
                "listbox"
            );
            assert!(
                !passes_through(Role::List, st(StateSet::empty()), r),
                "plain ul"
            );
            assert!(!passes_through(Role::Link, st(StateSet::FOCUSABLE), r));
            assert!(!passes_through(Role::Button, st(StateSet::FOCUSABLE), r));
            assert!(!passes_through(Role::CheckBox, st(StateSet::FOCUSABLE), r));
        }
    }

    #[test]
    fn disabled_fields_do_not_pass_keys_through_and_read_only_entries_stay_fields() {
        assert!(!passes_through(
            Role::TextEntry,
            StateSet::EDITABLE | StateSet::DISABLED,
            Reason::Focus
        ));
        // A read-only text entry (Role is still an entry): the role rule applies, the state
        // rule does not, so an entry stays a field.
        assert!(passes_through(
            Role::TextEntry,
            st(StateSet::EDITABLE | StateSet::READ_ONLY),
            Reason::Focus
        ));
        // The document itself is editable in designMode pages but is not a field.
        assert!(!passes_through(
            Role::DocumentWeb,
            st(StateSet::EDITABLE),
            Reason::Focus
        ));
    }

    #[test]
    fn d1_radios_tabs_menu_items_and_grid_cells_pass_keys_through_on_focus_only() {
        for r in [
            Role::RadioButton,
            Role::Tab,
            Role::MenuItem,
            Role::ListItem,
            Role::Cell,
        ] {
            assert!(
                passes_through(r, st(StateSet::FOCUSABLE), Reason::Focus),
                "{r:?}"
            );
        }
        assert!(!passes_through(
            Role::RadioButton,
            st(StateSet::FOCUSABLE),
            Reason::Activate
        ));
        // a plain table cell or list item (not focusable) stays document text
        assert!(!passes_through(
            Role::Cell,
            st(StateSet::empty()),
            Reason::Focus
        ));
        assert!(!passes_through(
            Role::ListItem,
            st(StateSet::empty()),
            Reason::Focus
        ));
    }
}
