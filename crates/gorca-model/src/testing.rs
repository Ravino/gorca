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

fn sem_role(role: Role) -> SemRole {
    match role {
        Role::Unknown => SemRole::Unknown,
        Role::Application => SemRole::Application,
        Role::Window => SemRole::Window,
        Role::Dialog => SemRole::Dialog,
        Role::Panel => SemRole::Generic,
        Role::Document => SemRole::Document,
        Role::DocumentWeb => SemRole::DocumentWeb,
        Role::Heading => SemRole::Heading,
        Role::Paragraph => SemRole::Paragraph,
        Role::Link => SemRole::Link,
        Role::Image => SemRole::Img,
        Role::List => SemRole::List,
        Role::ListItem => SemRole::ListItem,
        Role::Table => SemRole::Table,
        Role::Row => SemRole::Row,
        Role::Cell => SemRole::Cell,
        Role::ColumnHeader => SemRole::ColumnHeader,
        Role::RowHeader => SemRole::RowHeader,
        Role::Button | Role::ToggleButton => SemRole::Button,
        Role::Switch => SemRole::Switch,
        Role::CheckBox => SemRole::Checkbox,
        Role::RadioButton => SemRole::Radio,
        Role::ComboBox => SemRole::Combobox,
        Role::TextEntry => SemRole::TextBox,
        Role::PasswordEntry => SemRole::PasswordBox,
        Role::SpinButton => SemRole::SpinButton,
        Role::Slider => SemRole::Slider,
        Role::ProgressBar => SemRole::ProgressBar,
        Role::Menu => SemRole::Menu,
        Role::MenuBar => SemRole::Menubar,
        Role::MenuItem => SemRole::MenuItem,
        Role::Popover => SemRole::Popover,
        Role::Tab => SemRole::Tab,
        Role::TabList => SemRole::TabList,
        Role::Tree => SemRole::Tree,
        Role::TreeItem => SemRole::TreeItem,
        Role::Toolbar => SemRole::Toolbar,
        Role::StatusBar => SemRole::Status,
        Role::Separator => SemRole::Separator,
        Role::Label => SemRole::Caption,
        Role::StaticText => SemRole::StaticText,
        Role::Terminal => SemRole::Terminal,
        Role::Landmark => SemRole::Region,
        Role::Alert => SemRole::Alert,
    }
}

/// The record a connector would make for `n`.
pub fn record_of(n: &NodeSnapshot) -> SrcRecord {
    let st = n.states;
    let mut r = SrcRecord::default();
    let s = &mut r.sem;
    s.role = sem_role(n.role);
    let checkable = matches!(n.role, Role::CheckBox | Role::RadioButton | Role::Switch);
    if checkable {
        s.check = if st.contains(StateSet::MIXED) {
            Check::Mixed
        } else if st.contains(StateSet::CHECKED) {
            Check::On
        } else {
            Check::Off
        };
    }
    if n.role == Role::ToggleButton {
        s.press = if st.contains(StateSet::MIXED) {
            Press::Mixed
        } else if st.intersects(StateSet::PRESSED | StateSet::CHECKED) {
            Press::On
        } else {
            Press::Off
        };
    } else if st.contains(StateSet::PRESSED) && n.role == Role::Button {
        s.press = Press::On;
    }
    if n.role == Role::ProgressBar && st.contains(StateSet::MIXED) {
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
    let value_text = match &n.value {
        Some(Value::Range { current, .. }) => Some(Arc::<str>::from(format!("{current}"))),
        Some(Value::Text(_)) | None => None,
    };
    if n.description.is_some() || value_text.is_some() {
        s.text = Some(Arc::new(SemText {
            description: n.description.clone(),
            value_text,
            ..SemText::default()
        }));
    }
    r.name = n.name.clone().filter(|x| !x.is_empty());
    r.known = GroupMask::EMPTY;
    for &g in Group::ALL {
        r.known = r.known.with(g);
    }
    r
}

impl NodeSnapshot {
    /// This snapshot with the source record a connector would have made for it (test support).
    #[must_use]
    pub fn with_test_record(mut self) -> NodeSnapshot {
        self.sem = NodeSem {
            rec: Some(Arc::new(record_of(&self))),
            entry: None,
        };
        self
    }
}
