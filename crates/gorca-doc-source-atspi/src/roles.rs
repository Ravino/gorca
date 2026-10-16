//! AT-SPI role and state numbers (values of `AtspiRole` / `AtspiStateType`, stable ABI) and
//! their normalisation to `gorca_model`. The numbers were read from `Atspi.Role` /
//! `Atspi.StateType` of at-spi2-core 2.56; this crate deliberately does not link the `atspi`
//! crate (ADR-0001 rule 2).

use gorca_model::{Role, StateSet};

pub mod raw {
    pub const ALERT: u32 = 2;
    pub const CHECK_BOX: u32 = 7;
    pub const CHECK_MENU_ITEM: u32 = 8;
    pub const COLUMN_HEADER: u32 = 10;
    pub const COMBO_BOX: u32 = 11;
    pub const DIALOG: u32 = 16;
    pub const EMBEDDED: u32 = 78;
    pub const FILLER: u32 = 20;
    pub const FRAME: u32 = 23;
    pub const ICON: u32 = 26;
    pub const IMAGE: u32 = 27;
    pub const LABEL: u32 = 29;
    pub const LIST: u32 = 31;
    pub const LIST_ITEM: u32 = 32;
    pub const MENU: u32 = 33;
    pub const MENU_BAR: u32 = 34;
    pub const MENU_ITEM: u32 = 35;
    pub const PAGE_TAB: u32 = 37;
    pub const PAGE_TAB_LIST: u32 = 38;
    pub const PANEL: u32 = 39;
    pub const PASSWORD_TEXT: u32 = 40;
    pub const POPUP_MENU: u32 = 41;
    pub const PROGRESS_BAR: u32 = 42;
    pub const PUSH_BUTTON: u32 = 43;
    pub const RADIO_BUTTON: u32 = 44;
    pub const RADIO_MENU_ITEM: u32 = 45;
    pub const ROW_HEADER: u32 = 47;
    pub const SEPARATOR: u32 = 50;
    pub const SLIDER: u32 = 51;
    pub const SPIN_BUTTON: u32 = 52;
    pub const STATUS_BAR: u32 = 54;
    pub const TABLE: u32 = 55;
    pub const TABLE_CELL: u32 = 56;
    pub const TABLE_COLUMN_HEADER: u32 = 57;
    pub const TABLE_ROW_HEADER: u32 = 58;
    pub const TERMINAL: u32 = 60;
    pub const TEXT: u32 = 61;
    pub const TOGGLE_BUTTON: u32 = 62;
    pub const TOOL_BAR: u32 = 63;
    pub const TREE: u32 = 65;
    pub const TREE_TABLE: u32 = 66;
    pub const WINDOW: u32 = 69;
    pub const FOOTER: u32 = 72;
    pub const HEADER: u32 = 71;
    pub const PARAGRAPH: u32 = 73;
    pub const APPLICATION: u32 = 75;
    pub const ENTRY: u32 = 79;
    pub const CAPTION: u32 = 81;
    pub const DOCUMENT_FRAME: u32 = 82;
    pub const HEADING: u32 = 83;
    pub const SECTION: u32 = 85;
    pub const FORM: u32 = 87;
    pub const LINK: u32 = 88;
    pub const TABLE_ROW: u32 = 90;
    pub const TREE_ITEM: u32 = 91;
    pub const DOCUMENT_SPREADSHEET: u32 = 92;
    pub const DOCUMENT_PRESENTATION: u32 = 93;
    pub const DOCUMENT_TEXT: u32 = 94;
    pub const DOCUMENT_WEB: u32 = 95;
    pub const DOCUMENT_EMAIL: u32 = 96;
    pub const LIST_BOX: u32 = 98;
    pub const GROUPING: u32 = 99;
    pub const BLOCK_QUOTE: u32 = 105;
    pub const ARTICLE: u32 = 109;
    pub const LANDMARK: u32 = 110;
    pub const LOG: u32 = 111;
    pub const STATIC: u32 = 116;
    pub const SWITCH: u32 = 130;
}

/// Normalise a raw AT-SPI role (C-4: unknown roles map to `Role::Unknown`, the caller keeps the
/// raw number in the `RAW_ROLE` attribute).
pub fn map_role(r: u32) -> Role {
    use raw::{
        ALERT, APPLICATION, ARTICLE, BLOCK_QUOTE, CAPTION, CHECK_BOX, CHECK_MENU_ITEM,
        COLUMN_HEADER, COMBO_BOX, DIALOG, DOCUMENT_EMAIL, DOCUMENT_FRAME, DOCUMENT_PRESENTATION,
        DOCUMENT_SPREADSHEET, DOCUMENT_TEXT, DOCUMENT_WEB, EMBEDDED, ENTRY, FILLER, FOOTER, FORM, FRAME,
        GROUPING, HEADER, HEADING, ICON, IMAGE, LABEL, LANDMARK, LINK, LIST, LIST_BOX, LIST_ITEM,
        LOG, MENU, MENU_BAR, MENU_ITEM, PAGE_TAB, PAGE_TAB_LIST, PANEL, PARAGRAPH, PASSWORD_TEXT,
        POPUP_MENU, PROGRESS_BAR, PUSH_BUTTON, RADIO_BUTTON, RADIO_MENU_ITEM, ROW_HEADER, SECTION,
        SEPARATOR, SLIDER, SPIN_BUTTON, STATIC, STATUS_BAR, SWITCH, TABLE, TABLE_CELL,
        TABLE_COLUMN_HEADER, TABLE_ROW, TABLE_ROW_HEADER, TERMINAL, TEXT, TOGGLE_BUTTON, TOOL_BAR,
        TREE, TREE_ITEM, TREE_TABLE, WINDOW,
    };
    match r {
        // Gecko exposes role=application and <object>/<embed> as ATK_ROLE_EMBEDDED.
        APPLICATION | EMBEDDED => Role::Application,
        FRAME | WINDOW => Role::Window,
        DIALOG => Role::Dialog,
        ALERT => Role::Alert,
        PANEL | FILLER | GROUPING | SECTION | FORM | HEADER | FOOTER | ARTICLE | BLOCK_QUOTE
        | LOG => Role::Panel,
        DOCUMENT_FRAME
        | DOCUMENT_TEXT
        | DOCUMENT_EMAIL
        | DOCUMENT_SPREADSHEET
        | DOCUMENT_PRESENTATION => Role::Document,
        DOCUMENT_WEB => Role::DocumentWeb,
        HEADING => Role::Heading,
        PARAGRAPH => Role::Paragraph,
        LINK => Role::Link,
        IMAGE | ICON => Role::Image,
        LIST | LIST_BOX => Role::List,
        LIST_ITEM => Role::ListItem,
        TABLE | TREE_TABLE => Role::Table,
        TABLE_ROW => Role::Row,
        TABLE_CELL => Role::Cell,
        COLUMN_HEADER | TABLE_COLUMN_HEADER => Role::ColumnHeader,
        ROW_HEADER | TABLE_ROW_HEADER => Role::RowHeader,
        PUSH_BUTTON => Role::Button,
        TOGGLE_BUTTON | SWITCH => Role::ToggleButton,
        CHECK_BOX | CHECK_MENU_ITEM => Role::CheckBox,
        RADIO_BUTTON | RADIO_MENU_ITEM => Role::RadioButton,
        COMBO_BOX => Role::ComboBox,
        ENTRY | TEXT => Role::TextEntry,
        PASSWORD_TEXT => Role::PasswordEntry,
        SPIN_BUTTON => Role::SpinButton,
        SLIDER => Role::Slider,
        PROGRESS_BAR => Role::ProgressBar,
        MENU | POPUP_MENU => Role::Menu,
        MENU_BAR => Role::MenuBar,
        MENU_ITEM => Role::MenuItem,
        PAGE_TAB => Role::Tab,
        PAGE_TAB_LIST => Role::TabList,
        TREE => Role::Tree,
        TREE_ITEM => Role::TreeItem,
        TOOL_BAR => Role::Toolbar,
        STATUS_BAR => Role::StatusBar,
        SEPARATOR => Role::Separator,
        LABEL | CAPTION => Role::Label,
        STATIC => Role::StaticText,
        TERMINAL => Role::Terminal,
        LANDMARK => Role::Landmark,
        _ => Role::Unknown,
    }
}

/// Roles whose state matters for browse mode and forms. The fast tier fetches `GetState` only
/// for these; every other node gets [`DEFAULT_PLAIN_STATES`] until a revalidation says otherwise.
pub fn is_stateful(r: u32) -> bool {
    use raw::{
        ALERT, CHECK_BOX, CHECK_MENU_ITEM, COMBO_BOX, DIALOG, DOCUMENT_WEB, ENTRY, LINK, LIST_BOX,
        LIST_ITEM, MENU_ITEM, PAGE_TAB, PASSWORD_TEXT, PROGRESS_BAR, PUSH_BUTTON, RADIO_BUTTON,
        RADIO_MENU_ITEM, SLIDER, SPIN_BUTTON, SWITCH, TABLE_CELL, TABLE_ROW, TEXT, TOGGLE_BUTTON,
        TREE, TREE_ITEM, TREE_TABLE,
    };
    matches!(
        r,
        CHECK_BOX
            | CHECK_MENU_ITEM
            | COMBO_BOX
            | ENTRY
            | TEXT
            | PASSWORD_TEXT
            | LINK
            | LIST_ITEM
            | MENU_ITEM
            | PAGE_TAB
            | PUSH_BUTTON
            | RADIO_BUTTON
            | RADIO_MENU_ITEM
            | SLIDER
            | SPIN_BUTTON
            | SWITCH
            | TABLE_CELL
            | TOGGLE_BUTTON
            | TREE_ITEM
            | TABLE_ROW
            | LIST_BOX
            | TREE
            | TREE_TABLE
            | DIALOG
            | ALERT
            | PROGRESS_BAR
            | DOCUMENT_WEB
    )
}

/// States assumed for a node whose `GetState` was skipped by the fast tier.
pub const DEFAULT_PLAIN_STATES: StateSet = StateSet::SHOWING
    .union(StateSet::VISIBLE)
    .union(StateSet::ENABLED);

/// Roles that can carry text through the `Text` interface in Gecko/Blink trees. Image-like and
/// purely structural roles are skipped by the text tier.
pub fn may_have_text(r: u32) -> bool {
    use raw::{
        FILLER, ICON, IMAGE, LIST, LIST_BOX, MENU_BAR, PAGE_TAB_LIST, PROGRESS_BAR, SEPARATOR,
        SLIDER, TABLE, TABLE_ROW, TOOL_BAR, TREE_TABLE,
    };
    !matches!(
        r,
        IMAGE
            | ICON
            | SEPARATOR
            | TABLE
            | TREE_TABLE
            | TABLE_ROW
            | LIST
            | LIST_BOX
            | MENU_BAR
            | PAGE_TAB_LIST
            | TOOL_BAR
            | PROGRESS_BAR
            | SLIDER
            | FILLER
    )
}

pub mod state_bit {
    pub const ACTIVE: u8 = 1;
    pub const BUSY: u8 = 3;
    pub const CHECKED: u8 = 4;
    pub const COLLAPSED: u8 = 5;
    pub const DEFUNCT: u8 = 6;
    pub const EDITABLE: u8 = 7;
    pub const ENABLED: u8 = 8;
    pub const EXPANDED: u8 = 10;
    pub const FOCUSABLE: u8 = 11;
    pub const FOCUSED: u8 = 12;
    pub const MODAL: u8 = 16;
    pub const MULTI_LINE: u8 = 17;
    pub const PRESSED: u8 = 20;
    pub const SELECTABLE: u8 = 22;
    pub const SELECTED: u8 = 23;
    pub const SHOWING: u8 = 25;
    pub const TRANSIENT: u8 = 28;
    pub const VISIBLE: u8 = 30;
    pub const MANAGES_DESCENDANTS: u8 = 31;
    pub const REQUIRED: u8 = 33;
    pub const INVALID_ENTRY: u8 = 36;
    pub const VISITED: u8 = 40;
    pub const HAS_POPUP: u8 = 42;
    pub const READ_ONLY: u8 = 43;
}

/// Fold the AT-SPI state bitset (state number = bit index) into the normalised set.
pub fn map_states(bits: u64) -> StateSet {
    use state_bit::{
        ACTIVE, BUSY, CHECKED, COLLAPSED, DEFUNCT, EDITABLE, ENABLED, EXPANDED, FOCUSABLE, FOCUSED,
        HAS_POPUP, INVALID_ENTRY, MANAGES_DESCENDANTS, MODAL, MULTI_LINE, PRESSED, READ_ONLY,
        REQUIRED, SELECTABLE, SELECTED, SHOWING, TRANSIENT, VISIBLE, VISITED,
    };
    let has = |n: u8| bits & (1u64 << n) != 0;
    let mut s = StateSet::empty();
    for (n, f) in [
        (ACTIVE, StateSet::ACTIVE),
        (BUSY, StateSet::BUSY),
        (CHECKED, StateSet::CHECKED),
        (COLLAPSED, StateSet::COLLAPSED),
        (DEFUNCT, StateSet::DEFUNCT),
        (EDITABLE, StateSet::EDITABLE),
        (ENABLED, StateSet::ENABLED),
        (EXPANDED, StateSet::EXPANDED),
        (FOCUSABLE, StateSet::FOCUSABLE),
        (FOCUSED, StateSet::FOCUSED),
        (MODAL, StateSet::MODAL),
        (MULTI_LINE, StateSet::MULTILINE),
        (PRESSED, StateSet::PRESSED),
        (SELECTABLE, StateSet::SELECTABLE),
        (SELECTED, StateSet::SELECTED),
        (SHOWING, StateSet::SHOWING),
        (TRANSIENT, StateSet::TRANSIENT),
        (VISIBLE, StateSet::VISIBLE),
        (MANAGES_DESCENDANTS, StateSet::MANAGES_DESCENDANTS),
        (REQUIRED, StateSet::REQUIRED),
        (INVALID_ENTRY, StateSet::INVALID),
        (HAS_POPUP, StateSet::HAS_POPUP),
        (READ_ONLY, StateSet::READ_ONLY),
        (VISITED, StateSet::VISITED),
    ] {
        if has(n) {
            s |= f;
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_roles_map() {
        assert_eq!(map_role(raw::DOCUMENT_WEB), Role::DocumentWeb);
        assert_eq!(map_role(raw::HEADING), Role::Heading);
        assert_eq!(map_role(raw::LINK), Role::Link);
        assert_eq!(map_role(raw::SECTION), Role::Panel);
        assert_eq!(map_role(9999), Role::Unknown);
    }

    #[test]
    fn states_map() {
        let s = map_states((1 << state_bit::SHOWING) | (1 << state_bit::FOCUSABLE));
        assert!(s.contains(StateSet::SHOWING | StateSet::FOCUSABLE));
        assert!(!s.contains(StateSet::FOCUSED));
    }

    #[test]
    fn stateful_set_is_small() {
        assert!(is_stateful(raw::PUSH_BUTTON));
        assert!(!is_stateful(raw::PARAGRAPH));
        assert!(!is_stateful(raw::SECTION));
    }
}
