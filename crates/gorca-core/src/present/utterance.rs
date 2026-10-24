// SPDX-License-Identifier: GPL-3.0-or-later
//! What is left of the node presentation helpers after the M22b1 cut-over: the focus and change
//! utterances are made by `gorca-present` (`present::node`). This module keeps the helpers the
//! other surfaces still use: `seg`, the log's state names, the flat-review zone text (b3).

use std::sync::Arc;

use gorca_i18n::{MsgId as M, t};
use gorca_model::{NodeSnapshot, Segment, SegmentKind, StateSet, Value};
use smallvec::SmallVec;

/// State words in speaking order (catalog ids). TBD: verbosity settings.
const STATE_WORDS: &[(StateSet, M)] = &[
    (StateSet::DISABLED, M::StateUnavailable),
    (StateSet::MIXED, M::StateHalfChecked),
    (StateSet::CHECKED, M::StateChecked),
    (StateSet::PRESSED, M::StatePressed),
    (StateSet::SELECTED, M::StateSelected),
    (StateSet::EXPANDED, M::StateExpanded),
    (StateSet::COLLAPSED, M::StateCollapsed),
    (StateSet::REQUIRED, M::StateRequired),
    (StateSet::INVALID, M::StateInvalidEntry),
];

pub(super) fn seg(text: &str, kind: SegmentKind) -> Segment {
    Segment::new(Arc::from(text), kind)
}

/// The spoken form of a value, if it has one.
pub fn value_text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Text(t)) if !t.is_empty() => Some(t.to_string()),
        Some(Value::Range { current, .. }) => Some(format!("{current}")),
        Some(Value::Text(_)) | None => None,
    }
}

/// Every state name of a snapshot, for the presentation log: the AT-SPI names of the states the
/// connector decoded, plus `sensitive` (the core records its absence as `DISABLED`).
pub fn log_states(n: &NodeSnapshot) -> SmallVec<[&'static str; 8]> {
    const NAMES: &[(StateSet, &str)] = &[
        (StateSet::ACTIVE, "active"),
        (StateSet::CHECKED, "checked"),
        (StateSet::COLLAPSED, "collapsed"),
        (StateSet::DEFUNCT, "defunct"),
        (StateSet::ENABLED, "enabled"),
        (StateSet::EXPANDED, "expanded"),
        (StateSet::FOCUSABLE, "focusable"),
        (StateSet::FOCUSED, "focused"),
        (StateSet::PRESSED, "pressed"),
        (StateSet::SELECTABLE, "selectable"),
        (StateSet::SELECTED, "selected"),
        (StateSet::SHOWING, "showing"),
        (StateSet::TRANSIENT, "transient"),
        (StateSet::VISIBLE, "visible"),
        (StateSet::MANAGES_DESCENDANTS, "manages-descendants"),
    ];
    let mut out: SmallVec<[&'static str; 8]> = NAMES
        .iter()
        .filter(|(f, _)| n.states.contains(*f))
        .map(|(_, name)| *name)
        .collect();
    if !n.states.is_empty() && !n.states.contains(StateSet::DISABLED) {
        out.push("sensitive");
    }
    out.sort_unstable();
    out
}

/// Roles whose own name is the content: the role word adds nothing in a line of text.
fn is_plain_text_role(role: gorca_model::Role) -> bool {
    use gorca_model::Role;
    match role {
        Role::Unknown
        | Role::Label
        | Role::StaticText
        | Role::Paragraph
        | Role::ListItem
        | Role::Cell
        | Role::Row
        | Role::ColumnHeader
        | Role::RowHeader => true,
        Role::Application
        | Role::Window
        | Role::Dialog
        | Role::Panel
        | Role::Document
        | Role::DocumentWeb
        | Role::Heading
        | Role::Link
        | Role::Image
        | Role::List
        | Role::Table
        | Role::Button
        | Role::ToggleButton
        | Role::Switch
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::TextEntry
        | Role::PasswordEntry
        | Role::SpinButton
        | Role::Slider
        | Role::ProgressBar
        | Role::Menu
        | Role::MenuBar
        | Role::MenuItem
        | Role::Popover
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Terminal
        | Role::Landmark
        | Role::Alert => false,
    }
}

/// What a flat-review zone says for a whole object (ADR-0006 6.3 rule 2): the name (or the
/// value), then a role word for controls and the same state words as focus speech. "Wi-Fi toggle
/// button checked", "Apply button", "Volume slider 40". Empty when the node has nothing to say.
pub fn flat_zone_text(n: &NodeSnapshot) -> String {
    let mut out = String::new();
    let mut push = |s: &str| {
        if !s.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(s);
        }
    };
    if let Some(name) = n.name.as_deref().filter(|s| !s.is_empty()) {
        push(name);
    }
    if !is_plain_text_role(n.role) {
        push(n.role.spoken());
    }
    for (flag, word) in STATE_WORDS {
        if n.states.contains(*flag) {
            push(t(*word));
        }
    }
    if let Some(v) = value_text(n.value.as_ref()) {
        push(&v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{
        AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, NodeKey, PropMask,
        Relations, Role,
    };
    use std::time::Instant;

    fn snap(name: &str, states: StateSet, value: Option<Value>) -> NodeSnapshot {
        NodeSnapshot {
            key: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(1),
            },
            role: Role::CheckBox,
            states,
            name: Some(Arc::from(name)),
            description: None,
            value,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: AttrMap::default(),
            relations: Relations::default(),
            bounds: None,
            have: PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
            sem: gorca_model::NodeSem::NONE,
        }
    }

    #[test]
    fn the_log_lists_every_decoded_state_by_its_atspi_name() {
        let s = snap(
            "x",
            StateSet::CHECKED | StateSet::ENABLED | StateSet::SHOWING | StateSet::FOCUSED,
            None,
        );
        assert_eq!(
            log_states(&s).as_slice(),
            ["checked", "enabled", "focused", "sensitive", "showing"]
        );
        let off = snap("x", StateSet::DISABLED | StateSet::SHOWING, None);
        assert_eq!(log_states(&off).as_slice(), ["showing"], "no sensitive");
        assert!(log_states(&snap("x", StateSet::empty(), None)).is_empty());
    }

    #[test]
    fn a_zone_says_name_role_states_and_value() {
        let mut s = snap(
            "Volume",
            StateSet::empty(),
            Some(Value::Range {
                current: 40.0,
                min: 0.0,
                max: 100.0,
            }),
        );
        s.role = Role::Slider;
        assert_eq!(flat_zone_text(&s), "Volume slider 40");
        let c = snap("Wi-Fi", StateSet::CHECKED, None);
        assert_eq!(flat_zone_text(&c), "Wi-Fi check box checked");
    }
}
