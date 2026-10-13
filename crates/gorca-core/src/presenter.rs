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

/// Spoken form of a key press (key echo). Modifier keys alone are silent.
pub fn key_echo_text(unichar: u32, keysym: u32) -> Option<String> {
    let named = match keysym {
        0xff0d | 0xff8d => Some("enter"),
        0xff08 => Some("backspace"),
        0xff09 | 0xfe20 => Some("tab"),
        0xff1b => Some("escape"),
        0xffff => Some("delete"),
        0xff50 => Some("home"),
        0xff57 => Some("end"),
        0xff55 => Some("page up"),
        0xff56 => Some("page down"),
        0xff51 => Some("left"),
        0xff52 => Some("up"),
        0xff53 => Some("right"),
        0xff54 => Some("down"),
        0x20 => Some("space"),
        _ => None,
    };
    if let Some(n) = named {
        return Some(n.to_owned());
    }
    // Shift, Control, Alt, Super, Caps Lock, Num Lock: pressed on their own they say nothing.
    if (0xffe1..=0xffee).contains(&keysym) || keysym == 0xff7f {
        return None;
    }
    char::from_u32(unichar)
        .filter(|c| !c.is_control())
        .map(String::from)
}

/// Key echo: interrupts, carries the epoch of the key press.
pub fn key_echo(text: &str, id: UttId, epoch: Epoch) -> Utterance {
    let mut u = message(text, id, epoch);
    u.prio = Prio::Interrupt;
    u
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
    fn key_echo_names_special_keys_and_skips_modifiers() {
        assert_eq!(key_echo_text('a' as u32, 0x61).as_deref(), Some("a"));
        assert_eq!(key_echo_text(0x20, 0x20).as_deref(), Some("space"));
        assert_eq!(key_echo_text(0, 0xff0d).as_deref(), Some("enter"));
        assert_eq!(key_echo_text(0, 0xff52).as_deref(), Some("up"));
        assert_eq!(key_echo_text(0, 0xffe1), None, "shift alone is silent");
        assert_eq!(
            key_echo_text(0x07, 0x07),
            None,
            "control characters are not echoed"
        );
        assert_eq!(key_echo_text('é' as u32, 0xe9).as_deref(), Some("é"));
    }

    #[test]
    fn message_has_message_priority() {
        let u = message("application not responding", UttId(2), Epoch(3));
        assert_eq!(u.prio, Prio::Message);
        assert_eq!(texts(&u), ["application not responding"]);
    }
}
