// SPDX-License-Identifier: GPL-3.0-or-later
//! The one AT-SPI state mapping (ADR-0022 SM-2, M22a-B4): state bits to the tri-state groups and
//! flags. Exclusive states (`EXPANDED|COLLAPSED`, `CHECKED|INDETERMINATE`) are canonicalised
//! here, so a record never carries an impossible combination.

use gorca_model::sem::{Avail, Check, Expand, Orientation, Press, Select, SemFlags, SemRole};

/// `AtspiStateType` bit numbers (at-spi2-core 2.56, stable ABI; the state set is a 64-bit mask
/// indexed by these).
pub mod bit {
    pub const ACTIVE: u8 = 1;
    pub const ARMED: u8 = 2;
    pub const BUSY: u8 = 3;
    pub const CHECKED: u8 = 4;
    pub const COLLAPSED: u8 = 5;
    pub const DEFUNCT: u8 = 6;
    pub const EDITABLE: u8 = 7;
    pub const ENABLED: u8 = 8;
    pub const EXPANDABLE: u8 = 9;
    pub const EXPANDED: u8 = 10;
    pub const FOCUSABLE: u8 = 11;
    pub const FOCUSED: u8 = 12;
    pub const HAS_TOOLTIP: u8 = 13;
    pub const HORIZONTAL: u8 = 14;
    pub const ICONIFIED: u8 = 15;
    pub const MODAL: u8 = 16;
    pub const MULTI_LINE: u8 = 17;
    pub const MULTISELECTABLE: u8 = 18;
    pub const OPAQUE: u8 = 19;
    pub const PRESSED: u8 = 20;
    pub const RESIZABLE: u8 = 21;
    pub const SELECTABLE: u8 = 22;
    pub const SELECTED: u8 = 23;
    pub const SENSITIVE: u8 = 24;
    pub const SHOWING: u8 = 25;
    pub const SINGLE_LINE: u8 = 26;
    pub const STALE: u8 = 27;
    pub const TRANSIENT: u8 = 28;
    pub const VERTICAL: u8 = 29;
    pub const VISIBLE: u8 = 30;
    pub const MANAGES_DESCENDANTS: u8 = 31;
    pub const INDETERMINATE: u8 = 32;
    pub const REQUIRED: u8 = 33;
    pub const TRUNCATED: u8 = 34;
    pub const ANIMATED: u8 = 35;
    pub const INVALID_ENTRY: u8 = 36;
    pub const SUPPORTS_AUTOCOMPLETION: u8 = 37;
    pub const SELECTABLE_TEXT: u8 = 38;
    pub const IS_DEFAULT: u8 = 39;
    pub const VISITED: u8 = 40;
    pub const CHECKABLE: u8 = 41;
    pub const HAS_POPUP: u8 = 42;
    pub const READ_ONLY: u8 = 43;
    /// Number of states defined by at-spi2-core 2.56 (`ATSPI_STATE_LAST_DEFINED`).
    pub const COUNT: u8 = 44;
}

/// A raw AT-SPI state set.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct StateBits(pub u64);

impl StateBits {
    /// Build from the wire form: two `u32` words, low word first.
    pub const fn from_words(lo: u32, hi: u32) -> StateBits {
        StateBits((lo as u64) | ((hi as u64) << 32))
    }

    /// True when the state is set.
    #[inline]
    pub const fn has(self, state: u8) -> bool {
        state < 64 && self.0 & (1u64 << state) != 0
    }
}

/// How each AT-SPI state is used (the M-10 list for states).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StateUse {
    /// Feeds a semantic group or flag; the text names it.
    Mapped(&'static str),
    /// Deliberately not part of the semantic model; the text says why.
    Unmapped(&'static str),
}

/// Coverage of every defined AT-SPI state, by bit number. The coverage test checks the table is
/// complete (no state missing) and ordered by bit.
pub const STATE_COVERAGE: &[(u8, &str, StateUse)] = &[
    (
        0,
        "invalid",
        StateUse::Unmapped("error marker, never set on a live node"),
    ),
    (
        1,
        "active",
        StateUse::Unmapped("window activation, a window event concern"),
    ),
    (
        2,
        "armed",
        StateUse::Unmapped("pressed-but-not-released menu item, no ARIA state"),
    ),
    (3, "busy", StateUse::Mapped("flags BUSY")),
    (4, "checked", StateUse::Mapped("check / press")),
    (5, "collapsed", StateUse::Mapped("expand")),
    (
        6,
        "defunct",
        StateUse::Unmapped("liveness, handled by the connector"),
    ),
    (
        7,
        "editable",
        StateUse::Mapped("flags EDITABLE, READ_ONLY inference"),
    ),
    (8, "enabled", StateUse::Mapped("avail")),
    (9, "expandable", StateUse::Mapped("expand")),
    (10, "expanded", StateUse::Mapped("expand")),
    (
        11,
        "focusable",
        StateUse::Mapped("flags FOCUSABLE, presentational conflict"),
    ),
    (
        12,
        "focused",
        StateUse::Unmapped("focus is tracked by the focus machine, not a register"),
    ),
    (
        13,
        "has-tooltip",
        StateUse::Unmapped("tooltip text is read on demand"),
    ),
    (14, "horizontal", StateUse::Mapped("orientation")),
    (
        15,
        "iconified",
        StateUse::Unmapped("window state, a window event concern"),
    ),
    (16, "modal", StateUse::Mapped("flags MODAL")),
    (17, "multi-line", StateUse::Mapped("flags MULTILINE")),
    (
        18,
        "multiselectable",
        StateUse::Mapped("flags MULTISELECTABLE"),
    ),
    (19, "opaque", StateUse::Unmapped("rendering hint")),
    (20, "pressed", StateUse::Mapped("press")),
    (
        21,
        "resizable",
        StateUse::Unmapped("window property, no ARIA state"),
    ),
    (22, "selectable", StateUse::Mapped("select")),
    (23, "selected", StateUse::Mapped("select")),
    (24, "sensitive", StateUse::Mapped("avail")),
    (
        25,
        "showing",
        StateUse::Unmapped("visibility is a mirror concern (vis_rev), not a semantic group"),
    ),
    (
        26,
        "single-line",
        StateUse::Unmapped("the absence of multi-line carries it"),
    ),
    (
        27,
        "stale",
        StateUse::Unmapped("cache invalidation hint, handled by the connector"),
    ),
    (28, "transient", StateUse::Unmapped("liveness hint")),
    (29, "vertical", StateUse::Mapped("orientation")),
    (
        30,
        "visible",
        StateUse::Unmapped("visibility is a mirror concern (vis_rev), not a semantic group"),
    ),
    (
        31,
        "manages-descendants",
        StateUse::Unmapped("connector concern (children are fetched lazily)"),
    ),
    (
        32,
        "indeterminate",
        StateUse::Mapped("check Mixed on checkables, press Mixed on toggles"),
    ),
    (33, "required", StateUse::Mapped("flags REQUIRED")),
    (34, "truncated", StateUse::Unmapped("no ARIA state")),
    (35, "animated", StateUse::Unmapped("no ARIA state")),
    (36, "invalid-entry", StateUse::Mapped("invalid")),
    (
        37,
        "supports-autocompletion",
        StateUse::Unmapped("autocomplete kind is not a semantic group"),
    ),
    (
        38,
        "selectable-text",
        StateUse::Unmapped("text interface hint, not aria-selected"),
    ),
    (
        39,
        "is-default",
        StateUse::Unmapped("default button, no ARIA state"),
    ),
    (40, "visited", StateUse::Mapped("flags VISITED")),
    (
        41,
        "checkable",
        StateUse::Mapped("check on option, treeitem, menuitem"),
    ),
    (42, "has-popup", StateUse::Mapped("popup")),
    (43, "read-only", StateUse::Mapped("flags READ_ONLY")),
];

/// Roles that always carry a checked state (`aria-checked` is required or implied).
const fn check_intrinsic(r: SemRole) -> bool {
    matches!(
        r,
        SemRole::Checkbox
            | SemRole::Radio
            | SemRole::Switch
            | SemRole::MenuItemCheckbox
            | SemRole::MenuItemRadio
    )
}

/// Roles that carry a checked state only when the engine says they are checkable.
const fn check_optional(r: SemRole) -> bool {
    matches!(r, SemRole::Option | SemRole::TreeItem | SemRole::MenuItem)
}

/// Roles on which `aria-selected` is supported.
const fn select_role(r: SemRole) -> bool {
    matches!(
        r,
        SemRole::Option
            | SemRole::Tab
            | SemRole::TreeItem
            | SemRole::GridCell
            | SemRole::Cell
            | SemRole::Row
            | SemRole::ColumnHeader
            | SemRole::RowHeader
            | SemRole::ListItem
    )
}

/// Roles on which a disabled state is meaningful (SM-5). Static and structural roles are
/// `NotApplicable`, never "unavailable".
pub const fn is_disableable(r: SemRole) -> bool {
    matches!(
        r,
        SemRole::Button
            | SemRole::Checkbox
            | SemRole::Radio
            | SemRole::Switch
            | SemRole::Combobox
            | SemRole::TextBox
            | SemRole::SearchBox
            | SemRole::PasswordBox
            | SemRole::SpinButton
            | SemRole::Slider
            | SemRole::Link
            | SemRole::MenuItem
            | SemRole::MenuItemCheckbox
            | SemRole::MenuItemRadio
            | SemRole::Option
            | SemRole::Tab
            | SemRole::TreeItem
            | SemRole::GridCell
            | SemRole::Listbox
            | SemRole::Scrollbar
            | SemRole::Separator
            | SemRole::Group
            | SemRole::Application
            | SemRole::Tree
            | SemRole::Grid
            | SemRole::TreeGrid
            | SemRole::Menu
            | SemRole::Menubar
            | SemRole::Toolbar
            | SemRole::TabList
    )
}

/// Roles that are text fields (a missing EDITABLE state on them means read-only).
const fn is_text_field(r: SemRole) -> bool {
    matches!(
        r,
        SemRole::TextBox | SemRole::SearchBox | SemRole::PasswordBox
    )
}

/// The state-derived groups of one node. Each field is only meaningful when the caller saw the
/// STATES mask; the caller decides which groups are known.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StateGroups {
    pub check: Check,
    pub press: Press,
    pub expand: Expand,
    pub select: Select,
    pub avail: Avail,
    pub orientation: Orientation,
    pub flags: SemFlags,
    /// INVALID_ENTRY is set (the kind comes from the `invalid` attribute, if any).
    pub invalid_entry: bool,
    /// HAS_POPUP is set (the kind comes from the `haspopup` attribute, if any).
    pub has_popup: bool,
    /// REQUIRED is set (used to discount an INVALID_ENTRY the engine adds to empty required
    /// fields).
    pub required: bool,
}

/// Map the state set of a node whose semantic role is `role`.
///
/// `engine_toggle` is true when the raw role was a toggle button: such a button always has a
/// pressed state, even when it is off.
pub fn map_states(
    role: SemRole,
    engine_toggle: bool,
    engine_text_field: bool,
    s: StateBits,
) -> StateGroups {
    let check = map_check(role, s);
    let press = map_press(role, engine_toggle, s);
    let expand = map_expand(s);
    let select = map_select(role, s);
    let avail = if is_disableable(role) {
        if s.has(bit::ENABLED) || s.has(bit::SENSITIVE) {
            Avail::Enabled
        } else {
            Avail::Disabled
        }
    } else {
        Avail::NotApplicable
    };
    let orientation = if s.has(bit::VERTICAL) {
        Orientation::Vertical
    } else if s.has(bit::HORIZONTAL) {
        Orientation::Horizontal
    } else {
        Orientation::NotApplicable
    };

    let mut flags = SemFlags::empty();
    flags.set(SemFlags::REQUIRED, s.has(bit::REQUIRED));
    // Gecko marks a read-only field by omitting EDITABLE, Blink and GTK set READ_ONLY.
    let read_only = s.has(bit::READ_ONLY)
        || (!s.has(bit::EDITABLE) && engine_text_field && is_text_field(role));
    flags.set(SemFlags::READ_ONLY, read_only);
    flags.set(SemFlags::BUSY, s.has(bit::BUSY));
    flags.set(SemFlags::MODAL, s.has(bit::MODAL));
    flags.set(SemFlags::MULTISELECTABLE, s.has(bit::MULTISELECTABLE));
    flags.set(SemFlags::MULTILINE, s.has(bit::MULTI_LINE));
    flags.set(SemFlags::VISITED, s.has(bit::VISITED));
    flags.set(SemFlags::FOCUSABLE, s.has(bit::FOCUSABLE));
    flags.set(SemFlags::EDITABLE, s.has(bit::EDITABLE));

    StateGroups {
        check,
        press,
        expand,
        select,
        avail,
        orientation,
        flags,
        invalid_entry: s.has(bit::INVALID_ENTRY),
        has_popup: s.has(bit::HAS_POPUP),
        required: s.has(bit::REQUIRED),
    }
}

fn map_check(role: SemRole, s: StateBits) -> Check {
    let optional = check_optional(role);
    if !check_intrinsic(role) && !optional {
        return Check::NotCheckable;
    }
    if s.has(bit::INDETERMINATE) {
        // INDETERMINATE on a checkable is `aria-checked=mixed`.
        return Check::Mixed;
    }
    if s.has(bit::CHECKED) {
        return Check::On;
    }
    if optional && !s.has(bit::CHECKABLE) {
        return Check::NotCheckable;
    }
    Check::Off
}

fn map_press(role: SemRole, engine_toggle: bool, s: StateBits) -> Press {
    if role != SemRole::Button || !(engine_toggle || s.has(bit::PRESSED)) {
        return Press::NotToggle;
    }
    if s.has(bit::INDETERMINATE) {
        Press::Mixed
    } else if s.has(bit::PRESSED) {
        Press::On
    } else {
        Press::Off
    }
}

fn map_expand(s: StateBits) -> Expand {
    // EXPANDED wins over COLLAPSED when an engine sets both (SM-2 canonicalisation).
    if s.has(bit::EXPANDED) {
        Expand::Expanded
    } else if s.has(bit::EXPANDABLE) || s.has(bit::COLLAPSED) {
        Expand::Collapsed
    } else {
        Expand::NotExpandable
    }
}

fn map_select(role: SemRole, s: StateBits) -> Select {
    if !select_role(role) {
        return Select::NotSelectable;
    }
    if s.has(bit::SELECTED) {
        Select::On
    } else if s.has(bit::SELECTABLE) {
        Select::Off
    } else {
        Select::NotSelectable
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(list: &[u8]) -> StateBits {
        StateBits(list.iter().fold(0u64, |a, b| a | (1u64 << b)))
    }

    #[test]
    fn coverage_lists_every_defined_state_once_in_order() {
        assert_eq!(STATE_COVERAGE.len(), usize::from(bit::COUNT));
        for (i, (n, _, _)) in STATE_COVERAGE.iter().enumerate() {
            assert_eq!(usize::from(*n), i);
        }
    }

    #[test]
    fn words_compose_low_first() {
        let s = StateBits::from_words(1 << 4, 1 << 4);
        assert!(s.has(bit::CHECKED));
        assert!(s.has(36), "bit 36 = INVALID_ENTRY lives in the high word");
        assert!(!s.has(bit::BUSY));
    }

    #[test]
    fn indeterminate_on_a_checkable_is_mixed_and_wins_over_checked() {
        let s = bits(&[bit::INDETERMINATE, bit::CHECKABLE, bit::ENABLED]);
        assert_eq!(
            map_states(SemRole::Checkbox, false, false, s).check,
            Check::Mixed
        );
        assert_eq!(
            map_states(SemRole::MenuItemCheckbox, false, false, s).check,
            Check::Mixed
        );
        let both = bits(&[bit::INDETERMINATE, bit::CHECKED]);
        assert_eq!(
            map_states(SemRole::Checkbox, false, false, both).check,
            Check::Mixed
        );
        // Not a checkable role: the bit is not a checked state.
        assert_eq!(
            map_states(SemRole::Heading, false, false, s).check,
            Check::NotCheckable
        );
    }

    #[test]
    fn checked_states_per_role_class() {
        let on = bits(&[bit::CHECKED]);
        let off = StateBits(0);
        assert_eq!(
            map_states(SemRole::Switch, false, false, on).check,
            Check::On
        );
        assert_eq!(
            map_states(SemRole::Radio, false, false, off).check,
            Check::Off
        );
        // Option is checkable only when the engine says so.
        assert_eq!(
            map_states(SemRole::Option, false, false, off).check,
            Check::NotCheckable
        );
        assert_eq!(
            map_states(SemRole::Option, false, false, bits(&[bit::CHECKABLE])).check,
            Check::Off
        );
        assert_eq!(
            map_states(SemRole::Option, false, false, on).check,
            Check::On
        );
        // aria-checked is not supported on a button.
        assert_eq!(
            map_states(SemRole::Button, false, false, on).check,
            Check::NotCheckable
        );
    }

    #[test]
    fn toggle_button_press() {
        assert_eq!(
            map_states(SemRole::Button, true, false, StateBits(0)).press,
            Press::Off
        );
        assert_eq!(
            map_states(SemRole::Button, true, false, bits(&[bit::PRESSED])).press,
            Press::On
        );
        assert_eq!(
            map_states(SemRole::Button, true, false, bits(&[bit::INDETERMINATE])).press,
            Press::Mixed
        );
        assert_eq!(
            map_states(SemRole::Button, false, false, StateBits(0)).press,
            Press::NotToggle
        );
        assert_eq!(
            map_states(SemRole::Button, false, false, bits(&[bit::PRESSED])).press,
            Press::On,
            "a plain push button with PRESSED is a toggle"
        );
        assert_eq!(
            map_states(SemRole::Link, true, false, StateBits(0)).press,
            Press::NotToggle
        );
    }

    #[test]
    fn expand_is_canonical() {
        let g = |l: &[u8]| map_states(SemRole::Button, false, false, bits(l)).expand;
        assert_eq!(g(&[]), Expand::NotExpandable);
        assert_eq!(g(&[bit::EXPANDABLE]), Expand::Collapsed);
        assert_eq!(g(&[bit::EXPANDABLE, bit::EXPANDED]), Expand::Expanded);
        assert_eq!(
            g(&[bit::EXPANDED, bit::COLLAPSED]),
            Expand::Expanded,
            "both set: EXPANDED wins"
        );
        assert_eq!(g(&[bit::COLLAPSED]), Expand::Collapsed);
    }

    #[test]
    fn selection_needs_a_selectable_role() {
        let sel = bits(&[bit::SELECTABLE, bit::SELECTED]);
        assert_eq!(
            map_states(SemRole::Tab, false, false, sel).select,
            Select::On
        );
        assert_eq!(
            map_states(SemRole::Option, false, false, bits(&[bit::SELECTABLE])).select,
            Select::Off
        );
        assert_eq!(
            map_states(SemRole::Button, false, false, sel).select,
            Select::NotSelectable
        );
        assert_eq!(
            map_states(SemRole::Option, false, false, StateBits(0)).select,
            Select::NotSelectable
        );
    }

    #[test]
    fn availability_only_on_disableable_roles() {
        let en = bits(&[bit::ENABLED, bit::SENSITIVE]);
        assert_eq!(
            map_states(SemRole::Button, false, false, en).avail,
            Avail::Enabled
        );
        assert_eq!(
            map_states(SemRole::Button, false, false, StateBits(0)).avail,
            Avail::Disabled
        );
        assert_eq!(
            map_states(SemRole::Paragraph, false, false, StateBits(0)).avail,
            Avail::NotApplicable
        );
        assert_eq!(
            map_states(SemRole::StaticText, false, false, en).avail,
            Avail::NotApplicable
        );
    }

    #[test]
    fn read_only_by_either_engine_convention() {
        let ed = bits(&[bit::EDITABLE]);
        let ro = |role, text_field, s| {
            map_states(role, false, text_field, s)
                .flags
                .contains(SemFlags::READ_ONLY)
        };
        assert!(ro(SemRole::TextBox, true, bits(&[bit::READ_ONLY])), "Blink");
        assert!(
            ro(SemRole::TextBox, true, StateBits(0)),
            "Gecko: EDITABLE omitted"
        );
        assert!(!ro(SemRole::TextBox, true, ed));
        // A role=textbox div (not an engine text field) without EDITABLE is not claimed read-only.
        assert!(!ro(SemRole::TextBox, false, StateBits(0)));
        assert!(!ro(SemRole::Grid, true, StateBits(0)));
    }

    #[test]
    fn orientation_and_flags() {
        let g = map_states(
            SemRole::Slider,
            false,
            false,
            bits(&[
                bit::VERTICAL,
                bit::HORIZONTAL,
                bit::REQUIRED,
                bit::MODAL,
                bit::MULTI_LINE,
            ]),
        );
        assert_eq!(
            g.orientation,
            Orientation::Vertical,
            "vertical wins when both are set"
        );
        assert!(
            g.flags
                .contains(SemFlags::REQUIRED | SemFlags::MODAL | SemFlags::MULTILINE)
        );
        assert!(g.required);
    }
}
