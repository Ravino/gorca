//! Hot-path presentation: a pure function over a snapshot (ADR-0001 section 3.3).
//!
//! > "Hot-path presentation (focus, one nav step) runs inline as a pure function over snapshots.
//! > Bulk presentation (say-all chunks, tables, live-region diff text) runs as `PresentJob`s on
//! > `cpu`, and the results go straight to the speech lane with their epoch, without passing
//! > back through the Reader."

use std::sync::Arc;

use gorca_model::{
    Epoch, Freshness, NodeKey, NodeSnapshot, Prio, Segment, SegmentKind, StateSet, UttId,
    Utterance, Value,
};
use smallvec::SmallVec;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PresentKind {
    Focus,
    Table,
    SayAllChunk,
    LiveRegionDiff,
}

/// Bulk work for the `cpu` pool. The result carries `epoch` (E-1, E-3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PresentJob {
    pub node: NodeKey,
    pub epoch: Epoch,
    pub kind: PresentKind,
}

/// Reader or `cpu` to the speech lane. The utterance carries its epoch.
#[derive(Clone, Debug)]
pub struct SpeechIntent {
    pub utterance: Utterance,
}

/// State words in speaking order. TBD: localisation and verbosity settings.
const STATE_WORDS: &[(StateSet, &str)] = &[
    (StateSet::DISABLED, "unavailable"),
    (StateSet::CHECKED, "checked"),
    (StateSet::PRESSED, "pressed"),
    (StateSet::SELECTED, "selected"),
    (StateSet::EXPANDED, "expanded"),
    (StateSet::COLLAPSED, "collapsed"),
    (StateSet::REQUIRED, "required"),
    (StateSet::INVALID, "invalid entry"),
];

fn seg(text: &str, kind: SegmentKind) -> Segment {
    Segment {
        text: Arc::from(text),
        kind,
    }
}

/// Presentation of a focused node. Stale nodes are presented too, tagged `may_refine`.
pub fn present_focus(n: &NodeSnapshot, id: UttId, epoch: Epoch) -> Utterance {
    let mut segments: SmallVec<[Segment; 4]> = SmallVec::new();
    if let Some(name) = n.name.as_deref().filter(|s| !s.is_empty()) {
        segments.push(seg(name, SegmentKind::Text));
    }
    let role = n.role.spoken();
    if !role.is_empty() {
        segments.push(seg(role, SegmentKind::Role));
    }
    for (flag, word) in STATE_WORDS {
        if n.states.contains(*flag) {
            segments.push(seg(word, SegmentKind::State));
        }
    }
    match &n.value {
        Some(Value::Text(t)) if !t.is_empty() => segments.push(seg(t, SegmentKind::Value)),
        Some(Value::Range { current, .. }) => {
            segments.push(seg(&format!("{current}"), SegmentKind::Value));
        }
        Some(Value::Text(_)) | None => {}
    }
    let may_refine = match n.fresh {
        Freshness::Fresh { .. } => false,
        Freshness::Stale { .. } | Freshness::Fetching { .. } | Freshness::Defunct => true,
    };
    Utterance {
        id,
        segments,
        lang: None,
        epoch,
        prio: Prio::Interrupt,
        may_refine,
        survives_focus_change: false,
    }
}

/// A plain spoken message (for example "application not responding").
pub fn message(text: &str, id: UttId, epoch: Epoch) -> Utterance {
    let mut segments = SmallVec::new();
    segments.push(seg(text, SegmentKind::Text));
    Utterance {
        id,
        segments,
        lang: None,
        epoch,
        prio: Prio::Message,
        may_refine: false,
        survives_focus_change: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ChildList, ConnectorId, Ifaces, LocalId, Role};
    use std::time::Instant;

    fn snap(role: Role, name: Option<&str>, states: StateSet, fresh: Freshness) -> NodeSnapshot {
        NodeSnapshot {
            key: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(1),
            },
            role,
            states,
            name: name.map(Arc::from),
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            fresh,
        }
    }

    fn texts(u: &Utterance) -> Vec<&str> {
        u.segments.iter().map(|s| &*s.text).collect()
    }

    #[test]
    fn name_role_and_states_in_order() {
        let n = snap(
            Role::CheckBox,
            Some("Remember me"),
            StateSet::CHECKED | StateSet::DISABLED,
            Freshness::Fresh { at: Instant::now() },
        );
        let u = present_focus(&n, UttId(1), Epoch(4));
        assert_eq!(
            texts(&u),
            ["Remember me", "check box", "unavailable", "checked"]
        );
        assert_eq!(u.prio, Prio::Interrupt);
        assert_eq!(u.epoch, Epoch(4));
        assert!(!u.may_refine);
    }

    #[test]
    fn stale_nodes_are_tagged_may_refine_and_empty_names_are_skipped() {
        let n = snap(
            Role::Button,
            Some(""),
            StateSet::empty(),
            Freshness::Stale {
                since: Instant::now(),
            },
        );
        let u = present_focus(&n, UttId(1), Epoch(1));
        assert_eq!(texts(&u), ["button"]);
        assert!(u.may_refine);
    }

    #[test]
    fn values_are_spoken_last() {
        let mut n = snap(
            Role::Slider,
            Some("Volume"),
            StateSet::empty(),
            Freshness::Fresh { at: Instant::now() },
        );
        n.value = Some(Value::Range {
            current: 40.0,
            min: 0.0,
            max: 100.0,
        });
        assert_eq!(
            texts(&present_focus(&n, UttId(1), Epoch(1))),
            ["Volume", "slider", "40"]
        );
        n.value = Some(Value::Text(Arc::from("hello")));
        n.role = Role::TextEntry;
        assert_eq!(
            texts(&present_focus(&n, UttId(1), Epoch(1))),
            ["Volume", "edit", "hello"]
        );
    }

    #[test]
    fn message_has_message_priority() {
        let u = message("application not responding", UttId(2), Epoch(3));
        assert_eq!(u.prio, Prio::Message);
        assert_eq!(texts(&u), ["application not responding"]);
    }
}
