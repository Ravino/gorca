// SPDX-License-Identifier: GPL-3.0-or-later
//! Test support (feature `testing`): the source record a connector would have made for a bare
//! snapshot. After the M22b1 cut-over the Reader presents from the fused model only; its tests
//! build snapshots from a role, states and a name, and this gives such a snapshot the record
//! (`NodeSem::rec`) the production path starts from. Not used by any non-test code.

use std::sync::Arc;

use crate::sem::{
    Avail, Check, Expand, Group, GroupMask, Invalid, Popup, Press, Select, SemFlags, SemRole,
    SemText, SrcRecord,
};
use crate::{NodeSem, NodeSnapshot, Role, StateSet, Value};

/// The closest model role of a legacy role. Lossy (`Panel` is `Generic`, `ToggleButton` is
/// `Button`, `Label` is `Caption`): test fixtures describe a node by the connector's role and
/// this is what its normaliser would make of it. Never a production path.
pub fn sem_role(role: Role) -> SemRole {
    use SemRole as S;
    match role {
        Role::Unknown => S::Unknown,
        Role::Application => S::Application,
        Role::Window => S::Window,
        Role::Dialog => S::Dialog,
        Role::Panel => S::Generic,
        Role::Document => S::Document,
        Role::DocumentWeb => S::DocumentWeb,
        Role::Heading => S::Heading,
        Role::Paragraph => S::Paragraph,
        Role::Link => S::Link,
        Role::Image => S::Img,
        Role::List => S::List,
        Role::ListItem => S::ListItem,
        Role::Table => S::Table,
        Role::Row => S::Row,
        Role::Cell => S::Cell,
        Role::ColumnHeader => S::ColumnHeader,
        Role::RowHeader => S::RowHeader,
        Role::Button | Role::ToggleButton => S::Button,
        Role::Switch => S::Switch,
        Role::CheckBox => S::Checkbox,
        Role::RadioButton => S::Radio,
        Role::ComboBox => S::Combobox,
        Role::TextEntry => S::TextBox,
        Role::PasswordEntry => S::PasswordBox,
        Role::SpinButton => S::SpinButton,
        Role::Slider => S::Slider,
        Role::ProgressBar => S::ProgressBar,
        Role::Menu => S::Menu,
        Role::MenuBar => S::Menubar,
        Role::MenuItem => S::MenuItem,
        Role::Popover => S::Popover,
        Role::Tab => S::Tab,
        Role::TabList => S::TabList,
        Role::Tree => S::Tree,
        Role::TreeItem => S::TreeItem,
        Role::Toolbar => S::Toolbar,
        Role::StatusBar => S::Status,
        Role::Separator => S::Separator,
        Role::Label => S::Caption,
        Role::StaticText => S::StaticText,
        Role::Terminal => S::Terminal,
        Role::Landmark => S::Region,
        Role::Alert => S::Alert,
    }
}

/// The legacy role of a model role. Lossy: every landmark is `Landmark`, every filler `Panel`, a
/// role with no legacy word `Unknown`. For tests that rebuild a connector-side node from a model
/// one; never a production path.
pub fn legacy_role(r: SemRole) -> Role {
    use SemRole as S;
    match r {
        S::Application => Role::Application,
        S::Window => Role::Window,
        S::Dialog | S::AlertDialog => Role::Dialog,
        S::Generic | S::Group | S::Presentation | S::RadioGroup | S::TabPanel => Role::Panel,
        S::Document => Role::Document,
        S::DocumentWeb => Role::DocumentWeb,
        S::Heading => Role::Heading,
        S::Paragraph => Role::Paragraph,
        S::Link => Role::Link,
        S::Img => Role::Image,
        S::List | S::Listbox | S::Feed => Role::List,
        S::ListItem | S::Option => Role::ListItem,
        S::Table | S::Grid | S::TreeGrid => Role::Table,
        S::Row => Role::Row,
        S::Cell | S::GridCell => Role::Cell,
        S::ColumnHeader => Role::ColumnHeader,
        S::RowHeader => Role::RowHeader,
        S::Button => Role::Button,
        S::Switch => Role::Switch,
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
        S::Popover => Role::Popover,
        S::Tab => Role::Tab,
        S::TabList => Role::TabList,
        S::Tree => Role::Tree,
        S::TreeItem => Role::TreeItem,
        S::Toolbar => Role::Toolbar,
        S::Status => Role::StatusBar,
        S::Separator => Role::Separator,
        S::Caption => Role::Label,
        S::StaticText
        | S::Emphasis
        | S::Strong
        | S::Mark
        | S::Code
        | S::Time
        | S::Insertion
        | S::Deletion
        | S::Subscript
        | S::Superscript => Role::StaticText,
        S::Terminal => Role::Terminal,
        S::Region
        | S::Banner
        | S::Complementary
        | S::ContentInfo
        | S::Main
        | S::Navigation
        | S::Search
        | S::Form => Role::Landmark,
        S::Alert => Role::Alert,
        S::Unknown
        | S::Article
        | S::Blockquote
        | S::Comment
        | S::Definition
        | S::DescriptionList
        | S::Details
        | S::Figure
        | S::Iframe
        | S::Log
        | S::Marquee
        | S::Math
        | S::Note
        | S::RowGroup
        | S::Suggestion
        | S::Summary
        | S::Term
        | S::Timer
        | S::Tooltip => Role::Unknown,
    }
}

/// The record a connector would make for `n`, whose connector-side role and states are `role`
/// and `st`.
pub fn record_of(n: &NodeSnapshot, role: Role, st: StateSet) -> SrcRecord {
    let mut r = SrcRecord::default();
    let s = &mut r.sem;
    s.role = sem_role(role);
    let checkable = matches!(role, Role::CheckBox | Role::RadioButton | Role::Switch);
    if checkable {
        s.check = if st.contains(StateSet::MIXED) {
            Check::Mixed
        } else if st.contains(StateSet::CHECKED) {
            Check::On
        } else {
            Check::Off
        };
    }
    if role == Role::ToggleButton {
        s.press = if st.contains(StateSet::MIXED) {
            Press::Mixed
        } else if st.intersects(StateSet::PRESSED | StateSet::CHECKED) {
            Press::On
        } else {
            Press::Off
        };
    } else if st.contains(StateSet::PRESSED) && role == Role::Button {
        s.press = Press::On;
    }
    if role == Role::ProgressBar && st.contains(StateSet::MIXED) {
        s.check = Check::Mixed;
    }
    s.expand = if st.contains(StateSet::EXPANDED) {
        Expand::Expanded
    } else if st.contains(StateSet::COLLAPSED) {
        Expand::Collapsed
    } else {
        Expand::NotExpandable
    };
    s.select = if st.contains(StateSet::SELECTED) {
        Select::On
    } else if st.contains(StateSet::SELECTABLE) {
        Select::Off
    } else {
        Select::NotSelectable
    };
    s.avail = if st.contains(StateSet::DISABLED) {
        Avail::Disabled
    } else {
        Avail::Enabled
    };
    if st.contains(StateSet::INVALID) {
        s.invalid = Invalid::True;
    }
    if st.contains(StateSet::HAS_POPUP) {
        s.popup = Popup::Menu;
    }
    for (state, flag) in [
        (StateSet::REQUIRED, SemFlags::REQUIRED),
        (StateSet::READ_ONLY, SemFlags::READ_ONLY),
        (StateSet::BUSY, SemFlags::BUSY),
        (StateSet::EDITABLE, SemFlags::EDITABLE),
        (StateSet::FOCUSABLE, SemFlags::FOCUSABLE),
        (StateSet::MODAL, SemFlags::MODAL),
        (StateSet::MULTILINE, SemFlags::MULTILINE),
    ] {
        if st.contains(state) {
            s.flags |= flag;
        }
    }
    let value_text = match n.value() {
        Some(Value::Range { current, .. }) => Some(Arc::<str>::from(format!("{current}"))),
        Some(Value::Text(_)) | None => None,
    };
    if n.description().is_some() || value_text.is_some() {
        s.text = Some(Arc::new(SemText {
            description: n.description().clone(),
            value_text,
            ..SemText::default()
        }));
    }
    r.name = n.name().clone().filter(|x| !x.is_empty());
    r.known = GroupMask::EMPTY;
    for &g in Group::ALL {
        r.known = r.known.with(g);
    }
    r
}

impl NodeSnapshot {
    /// The role `with_source` was given (test support; `Unknown` for a snapshot of a fused entry).
    pub fn test_role(&self) -> Role {
        self.test_src.0
    }

    /// The states `with_source` was given (test support).
    pub fn test_states(&self) -> StateSet {
        self.test_src.1
    }

    /// Replace the connector-side role (and the structure, log names and password mark derived
    /// from it). The source record is not re-made: call [`NodeSnapshot::with_test_record`].
    pub fn set_test_role(&mut self, role: Role) {
        let (_, st) = self.test_src;
        *self = self.clone().with_source(role, st);
    }

    /// Replace the connector-side states, as [`NodeSnapshot::set_test_role`].
    pub fn set_test_states(&mut self, states: StateSet) {
        let (role, _) = self.test_src;
        *self = self.clone().with_source(role, states);
    }

    /// This snapshot with the source record a connector would have made for its role and states.
    #[must_use]
    pub fn with_test_record(self) -> NodeSnapshot {
        let (role, states) = self.test_src;
        let rec = Arc::new(record_of(&self, role, states));
        self.with_sem(NodeSem {
            rec: Some(rec),
            entry: None,
        })
    }

    /// This snapshot as a connector would have decoded a node of `role` with `states`: the
    /// structure bits, the log names and the source record (test support). Set the name,
    /// description and value first; the record is made from them.
    #[must_use]
    pub fn with_test_source(self, role: Role, states: StateSet) -> NodeSnapshot {
        let s = self.with_source(role, states);
        let rec = Arc::new(record_of(&s, role, states));
        s.with_sem(NodeSem {
            rec: Some(rec),
            entry: None,
        })
    }
}
