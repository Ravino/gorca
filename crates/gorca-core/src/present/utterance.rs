// SPDX-License-Identifier: GPL-3.0-or-later
//! What is left of the node presentation helpers after the M22b1 cut-over: the focus and change
//! utterances are made by `gorca-present` (`present::node`). This module keeps the helpers the
//! other surfaces still use: `seg` and the log's state names.

use std::sync::Arc;

use gorca_model::{NodeSnapshot, Segment, SegmentKind, StateSet, Value};
use smallvec::SmallVec;

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
}
