// SPDX-License-Identifier: GPL-3.0-or-later
//! M8b: ARIA roles, states and descriptions as words (NVDA parity).
//!
//! One place that turns what the web source normalised (`role`, `states`, and the attributes
//! `xml-roles`, `level`, `current`, `haspopup`, `explicit-name`, `error-text`) into the words a
//! screen reader says. Browse-mode lines and quick navigation (`browse::present`) and focus
//! speech (`present::utterance`) both read it, so a switch is "on" and a tab "selected" wherever
//! the user meets one.
//!
//! The tables follow NVDA: `refs/nvda/source/controlTypes/role.py` and `state.py` (labels),
//! `controlTypes/processAndLabelStates.py` (which states are spoken for which role),
//! `speech/speech.py` `getPropertiesSpeech` / `getControlFieldSpeech` (order), `aria.py` (the
//! `aria-haspopup` values) and `virtualBuffers/gecko_ia2.py` (`role=switch`). The engine
//! differences were already folded away by the source (`WebQuirks`, ADR-0008): this module never
//! asks which browser it is.

use std::sync::Arc;

use gorca_doc_source_api::web_attrs;
use gorca_i18n::{MsgId as M, msg, t};
use gorca_model::{AttrKey, Role, StateSet};
use smallvec::SmallVec;

/// A node's facts as the words need them: borrowed from a document node or a snapshot.
#[derive(Copy, Clone, Debug)]
pub struct Node<'a> {
    pub role: Role,
    pub states: StateSet,
    pub attrs: &'a [(AttrKey, Arc<str>)],
}

/// The ARIA roles whose NVDA word differs from the coarse `Role` word, or that `Role` cannot
/// tell apart (a switch is a toggle button, a menu item check box a check box).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    Plain,
    Switch,
    Tab,
    TabList,
    TabPanel,
    Tree,
    TreeItem,
    Menu,
    MenuBar,
    MenuItem,
    MenuItemCheckBox,
    MenuItemRadio,
    Toolbar,
    Group,
}

impl Kind {
    fn of_token(t: &str) -> Option<Kind> {
        Some(match t {
            "switch" => Kind::Switch,
            "tab" => Kind::Tab,
            "tablist" => Kind::TabList,
            "tabpanel" => Kind::TabPanel,
            "tree" => Kind::Tree,
            "treeitem" => Kind::TreeItem,
            "menu" => Kind::Menu,
            "menubar" => Kind::MenuBar,
            "menuitem" => Kind::MenuItem,
            "menuitemcheckbox" => Kind::MenuItemCheckBox,
            "menuitemradio" => Kind::MenuItemRadio,
            "toolbar" => Kind::Toolbar,
            "group" | "radiogroup" => Kind::Group,
            _ => return None,
        })
    }

    /// NVDA's label (`controlTypes/role.py`).
    pub fn word(self) -> Option<&'static str> {
        Some(match self {
            Kind::Plain => return None,
            Kind::Switch => t(M::AriaSwitch),
            Kind::Tab => t(M::AriaTab),
            Kind::TabList => t(M::AriaTabList),
            Kind::TabPanel => t(M::AriaTabPanel),
            Kind::Tree => t(M::AriaTree),
            Kind::TreeItem => t(M::AriaTreeItem),
            Kind::Menu => t(M::AriaMenu),
            Kind::MenuBar => t(M::AriaMenuBar),
            Kind::MenuItem => t(M::AriaMenuItem),
            Kind::MenuItemCheckBox => t(M::AriaCheckMenuItem),
            Kind::MenuItemRadio => t(M::AriaRadioMenuItem),
            Kind::Toolbar => t(M::AriaToolbar),
            Kind::Group => t(M::AriaGroup),
        })
    }

    /// A role that holds other controls: spoken when the cursor enters it, with its name.
    pub fn is_container(self) -> bool {
        matches!(
            self,
            Kind::TabList
                | Kind::TabPanel
                | Kind::Tree
                | Kind::Menu
                | Kind::MenuBar
                | Kind::Toolbar
                | Kind::Group
        )
    }

    /// A role that is spoken by itself on its line (like a link or a button).
    pub fn is_item(self) -> bool {
        matches!(
            self,
            Kind::Switch
                | Kind::Tab
                | Kind::TreeItem
                | Kind::MenuItem
                | Kind::MenuItemCheckBox
                | Kind::MenuItemRadio
        )
    }
}

impl<'a> Node<'a> {
    pub fn attr(&self, k: AttrKey) -> Option<&'a str> {
        self.attrs.iter().find(|(a, _)| *a == k).map(|(_, v)| &**v)
    }

    /// The first ARIA role token that has a word.
    pub fn kind(&self) -> Kind {
        self.attr(web_attrs::XML_ROLES)
            .and_then(|r| r.split_ascii_whitespace().find_map(Kind::of_token))
            .unwrap_or(Kind::Plain)
    }

    /// The author named the node (`aria-label`, `aria-labelledby`, `alt`, `title`, a `<label>`):
    /// its name is not just its content.
    pub fn authored_name(&self) -> bool {
        self.attr(web_attrs::EXPLICIT_NAME) == Some("true")
    }

    /// `aria-level` / heading level as the engine exposes it.
    pub fn level(&self) -> Option<u32> {
        self.attr(web_attrs::LEVEL)
            .and_then(|l| l.parse::<u32>().ok())
            .filter(|l| *l > 0)
    }

    /// The effective check-style kind of the node, for the on/off and checked words.
    fn check_style(&self) -> CheckStyle {
        match (self.kind(), self.role) {
            (Kind::Switch, _) => CheckStyle::OnOff,
            (Kind::MenuItemCheckBox | Kind::MenuItemRadio, _)
            | (_, Role::CheckBox | Role::RadioButton) => CheckStyle::Checked,
            (_, Role::ToggleButton) => CheckStyle::Pressed,
            _ => CheckStyle::None,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum CheckStyle {
    None,
    Checked,
    Pressed,
    OnOff,
}

/// The word of the role: NVDA's label for the ARIA role, `None` when the coarse `Role` word is
/// right (`Role::spoken`).
pub fn role_word(n: &Node<'_>) -> Option<&'static str> {
    n.kind().word()
}

/// The state words of a control, in NVDA's order (`State` enum order: unavailable, selected,
/// busy, pressed, checked, half checked, read only, expanded, collapsed, popup kind, required,
/// invalid entry). `unavailable` is decided by the caller: the document mirror and the app mirror
/// carry "not enabled" differently. `focus` is true when the node just took the focus (NVDA's
/// `OutputReason.FOCUS`): an unselected list or tree item then says "not selected".
pub fn state_words(n: &Node<'_>, unavailable: bool, focus: bool) -> SmallVec<[&'static str; 6]> {
    let st = n.states;
    let kind = n.kind();
    let mut out: SmallVec<[&'static str; 6]> = SmallVec::new();
    // States NVDA sorts after `required` and `invalid entry` (bit values 44 to 50 of its `State`).
    let mut late: SmallVec<[&'static str; 3]> = SmallVec::new();
    if unavailable {
        out.push(t(M::StateUnavailable));
    }
    // NVDA drops `selected` for list, tree and menu items (selection follows the focus there);
    // a tab says it.
    // NVDA's `_processNegativeStates` never says "not selected" for a menu item.
    let menu_item = n.role == Role::MenuItem
        || matches!(
            kind,
            Kind::MenuItem | Kind::MenuItemCheckBox | Kind::MenuItemRadio
        );
    let quiet_selected = matches!(n.role, Role::ListItem | Role::TreeItem | Role::MenuItem)
        || matches!(
            kind,
            Kind::TreeItem | Kind::MenuItem | Kind::MenuItemCheckBox | Kind::MenuItemRadio
        );
    if st.contains(StateSet::SELECTED) && !(quiet_selected && st.contains(StateSet::SELECTABLE)) {
        out.push(t(M::StateSelected));
    } else if focus
        && quiet_selected
        && !menu_item
        && !st.contains(StateSet::SELECTED)
        && st.contains(StateSet::SELECTABLE | StateSet::FOCUSABLE)
    {
        // NVDA says what is not selected only when it takes the focus.
        out.push(t(M::StateNotSelected));
    }
    if st.contains(StateSet::BUSY) {
        out.push(t(M::StateBusy));
    }
    let mixed = st.contains(StateSet::MIXED);
    match n.check_style() {
        CheckStyle::OnOff => late.push(if st.contains(StateSet::CHECKED) {
            t(M::StateOn)
        } else {
            t(M::StateOff)
        }),
        CheckStyle::Checked => out.push(if mixed {
            t(M::StateHalfChecked)
        } else if st.contains(StateSet::CHECKED) {
            t(M::StateChecked)
        } else {
            t(M::StateNotChecked)
        }),
        CheckStyle::Pressed => {
            if mixed {
                late.push(t(M::StateHalfPressed));
            } else if st.contains(StateSet::PRESSED) || st.contains(StateSet::CHECKED) {
                out.push(t(M::StatePressed));
            } else {
                out.push(t(M::StateNotPressed));
            }
        }
        CheckStyle::None => {
            if st.contains(StateSet::PRESSED) {
                out.push(t(M::StatePressed));
            }
            if st.contains(StateSet::CHECKED) {
                out.push(t(M::StateChecked));
            }
            if mixed && n.role == Role::ProgressBar {
                late.push(t(M::StateIndeterminate));
            }
        }
    }
    if st.contains(StateSet::READ_ONLY)
        && matches!(
            n.role,
            Role::TextEntry | Role::PasswordEntry | Role::CheckBox
        )
    {
        out.push(t(M::StateReadOnly));
    }
    let popup = popup_word(n);
    let menu_with_popup = matches!(n.role, Role::MenuItem) || kind == Kind::MenuItem;
    if !(menu_with_popup && popup.is_some()) {
        if st.contains(StateSet::EXPANDED) {
            out.push(t(M::StateExpanded));
        } else if st.contains(StateSet::COLLAPSED) {
            out.push(t(M::StateCollapsed));
        }
    }
    if let Some(p) = popup {
        // `submenu` is HASPOPUP (13); the dialog, grid, list and tree kinds are 47 to 50.
        if p == M::StateSubmenu {
            out.push(t(p));
        } else {
            late.push(t(p));
        }
    }
    if st.contains(StateSet::REQUIRED) {
        out.push(t(M::StateRequired));
    }
    if st.contains(StateSet::INVALID) && n.role != Role::Landmark && n.role != Role::Panel {
        out.push(t(M::StateInvalidEntry));
    }
    out.extend(late);
    out
}

/// `aria-haspopup` (`aria.py` `ariaHaspopupValuesToNVDAStates`, state labels of `state.py`). A
/// combo box inherently opens a list, so that is not said for it.
fn popup_word(n: &Node<'_>) -> Option<M> {
    let kind = match n.attr(web_attrs::HASPOPUP) {
        Some(v) => v,
        None if n.states.contains(StateSet::HAS_POPUP) => "true",
        None => return None,
    };
    match kind {
        "true" | "menu" => (n.role != Role::ComboBox).then_some(M::StateSubmenu),
        "dialog" => Some(M::StateOpensDialog),
        "grid" => Some(M::StateOpensGrid),
        "listbox" => (n.role != Role::ComboBox).then_some(M::StateOpensList),
        "tree" => Some(M::StateOpensTree),
        _ => None,
    }
}

/// `aria-current` (`controlTypes/isCurrent.py`): "current page", "current step", ...
pub fn current_word(n: &Node<'_>) -> Option<&'static str> {
    match n.attr(web_attrs::CURRENT)? {
        "" | "false" => None,
        "page" => Some(t(M::StateCurrentPage)),
        "step" => Some(t(M::StateCurrentStep)),
        "location" => Some(t(M::StateCurrentLocation)),
        "date" => Some(t(M::StateCurrentDate)),
        "time" => Some(t(M::StateCurrentTime)),
        _ => Some(t(M::StateCurrent)),
    }
}

/// "level 2" for the roles that have a level of their own (tree items; headings carry it in
/// their role phrase).
pub fn level_phrase(n: &Node<'_>) -> Option<String> {
    (n.kind() == Kind::TreeItem || n.role == Role::TreeItem)
        .then(|| n.level())
        .flatten()
        .map(|l| msg!(StateLevel, l = l))
}

/// The error message of an invalid node.
pub fn error_text<'a>(n: &Node<'a>) -> Option<&'a str> {
    if !n.states.contains(StateSet::INVALID) {
        return None;
    }
    n.attr(web_attrs::ERROR_TEXT)
        .filter(|t| !t.trim().is_empty())
}

/// `browse.description_pause = false`: the description follows the other words like any segment.
pub fn strip_pause(u: &mut gorca_model::Utterance) {
    for seg in &mut u.segments {
        if seg.kind == gorca_model::SegmentKind::Description {
            seg.kind = gorca_model::SegmentKind::Text;
        }
    }
}

/// A description worth saying: not empty and not a repeat of the name.
pub fn speakable_description<'a>(desc: Option<&'a str>, name: Option<&str>) -> Option<&'a str> {
    let d = desc?.trim();
    if d.is_empty() {
        return None;
    }
    if name.is_some_and(|n| n.trim().eq_ignore_ascii_case(d)) {
        return None;
    }
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(v: &[(AttrKey, &str)]) -> Vec<(AttrKey, Arc<str>)> {
        v.iter().map(|(k, x)| (*k, Arc::from(*x))).collect()
    }

    fn node(role: Role, st: StateSet, attrs: &[(AttrKey, Arc<str>)]) -> Node<'_> {
        Node {
            role,
            states: st,
            attrs,
        }
    }

    #[test]
    fn a_switch_is_on_or_off_not_checked() {
        let a = at(&[(web_attrs::XML_ROLES, "switch"), (web_attrs::TAG, "button")]);
        let on = node(Role::ToggleButton, StateSet::CHECKED, &a);
        assert_eq!(role_word(&on), Some("switch"));
        assert_eq!(state_words(&on, false, false).as_slice(), ["on"]);
        let off = node(Role::ToggleButton, StateSet::empty(), &a);
        assert_eq!(state_words(&off, false, false).as_slice(), ["off"]);
    }

    #[test]
    fn toggle_buttons_say_pressed_not_pressed_and_half_pressed() {
        let a = at(&[(web_attrs::TAG, "button")]);
        assert_eq!(
            state_words(
                &node(Role::ToggleButton, StateSet::PRESSED, &a),
                false,
                false
            )
            .as_slice(),
            ["pressed"]
        );
        assert_eq!(
            state_words(
                &node(Role::ToggleButton, StateSet::empty(), &a),
                false,
                false
            )
            .as_slice(),
            ["not pressed"]
        );
        assert_eq!(
            state_words(&node(Role::ToggleButton, StateSet::MIXED, &a), false, false).as_slice(),
            ["half pressed"]
        );
    }

    #[test]
    fn tabs_say_selected_tree_items_do_not_and_menu_items_check() {
        let tab = at(&[(web_attrs::XML_ROLES, "tab"), (web_attrs::TAG, "button")]);
        let n = node(Role::Tab, StateSet::SELECTED | StateSet::SELECTABLE, &tab);
        assert_eq!(role_word(&n), Some("tab"));
        assert_eq!(state_words(&n, false, false).as_slice(), ["selected"]);
        let ti = at(&[
            (web_attrs::XML_ROLES, "treeitem"),
            (web_attrs::LEVEL, "2"),
            (web_attrs::TAG, "li"),
        ]);
        let n = node(
            Role::TreeItem,
            StateSet::SELECTED | StateSet::SELECTABLE | StateSet::EXPANDED,
            &ti,
        );
        assert_eq!(role_word(&n), Some("tree view item"));
        assert_eq!(state_words(&n, false, false).as_slice(), ["expanded"]);
        assert_eq!(level_phrase(&n).as_deref(), Some("level 2"));
        let mi = at(&[(web_attrs::XML_ROLES, "menuitemcheckbox")]);
        let n = node(Role::CheckBox, StateSet::CHECKED, &mi);
        assert_eq!(role_word(&n), Some("check menu item"));
        assert_eq!(state_words(&n, false, false).as_slice(), ["checked"]);
        let mr = at(&[(web_attrs::XML_ROLES, "menuitemradio")]);
        let n = node(Role::RadioButton, StateSet::empty(), &mr);
        assert_eq!(role_word(&n), Some("radio menu item"));
        assert_eq!(state_words(&n, false, false).as_slice(), ["not checked"]);
    }

    #[test]
    fn popups_current_and_invalid() {
        let a = at(&[(web_attrs::HASPOPUP, "menu"), (web_attrs::TAG, "button")]);
        let n = node(Role::Button, StateSet::COLLAPSED | StateSet::HAS_POPUP, &a);
        assert_eq!(
            state_words(&n, false, false).as_slice(),
            ["collapsed", "submenu"]
        );
        let a = at(&[(web_attrs::HASPOPUP, "dialog")]);
        let n = node(Role::Button, StateSet::HAS_POPUP, &a);
        assert_eq!(state_words(&n, false, false).as_slice(), ["opens dialog"]);
        // A combo box opens a list by definition; a menu item with a popup is not "collapsed".
        let a = at(&[(web_attrs::HASPOPUP, "listbox")]);
        let n = node(
            Role::ComboBox,
            StateSet::COLLAPSED | StateSet::HAS_POPUP,
            &a,
        );
        assert_eq!(state_words(&n, false, false).as_slice(), ["collapsed"]);
        let a = at(&[(web_attrs::HASPOPUP, "menu")]);
        let n = node(
            Role::MenuItem,
            StateSet::COLLAPSED | StateSet::HAS_POPUP,
            &a,
        );
        assert_eq!(state_words(&n, false, false).as_slice(), ["submenu"]);

        let a = at(&[(web_attrs::CURRENT, "page")]);
        assert_eq!(
            current_word(&node(Role::Link, StateSet::empty(), &a)),
            Some("current page")
        );
        for (v, w) in [
            ("step", Some("current step")),
            ("location", Some("current location")),
            ("date", Some("current date")),
            ("time", Some("current time")),
            ("true", Some("current")),
            ("false", None),
        ] {
            let a = at(&[(web_attrs::CURRENT, v)]);
            assert_eq!(current_word(&node(Role::Link, StateSet::empty(), &a)), w);
        }

        let a = at(&[(web_attrs::ERROR_TEXT, "Enter a valid address")]);
        let n = node(Role::TextEntry, StateSet::INVALID | StateSet::REQUIRED, &a);
        assert_eq!(
            state_words(&n, false, false).as_slice(),
            ["required", "invalid entry"]
        );
        assert_eq!(error_text(&n), Some("Enter a valid address"));
        let n = node(Role::TextEntry, StateSet::empty(), &a);
        assert_eq!(error_text(&n), None);
    }

    #[test]
    fn read_only_and_unavailable() {
        let a = at(&[(web_attrs::TAG, "input")]);
        let n = node(Role::TextEntry, StateSet::READ_ONLY, &a);
        assert_eq!(state_words(&n, false, false).as_slice(), ["read only"]);
        assert_eq!(
            state_words(&n, true, false).as_slice(),
            ["unavailable", "read only"]
        );
        let n = node(Role::Button, StateSet::READ_ONLY, &a);
        assert!(state_words(&n, false, false).is_empty());
    }

    #[test]
    fn descriptions_that_repeat_the_name_are_dropped() {
        assert_eq!(
            speakable_description(Some(" Help "), Some("Name")),
            Some("Help")
        );
        assert_eq!(speakable_description(Some("name"), Some("Name")), None);
        assert_eq!(speakable_description(Some("  "), None), None);
        assert_eq!(speakable_description(None, None), None);
    }

    #[test]
    fn containers_and_items() {
        assert!(Kind::TabList.is_container() && Kind::Tab.is_item());
        assert_eq!(Kind::TabList.word(), Some("tab control"));
        assert_eq!(Kind::TabPanel.word(), Some("property page"));
        assert_eq!(Kind::Tree.word(), Some("tree view"));
        assert_eq!(Kind::Toolbar.word(), Some("tool bar"));
        assert_eq!(Kind::Plain.word(), None);
    }
}
