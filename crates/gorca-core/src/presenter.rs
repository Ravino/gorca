//! Hot-path presentation: a pure function over a snapshot (ADR-0001 section 3.3).
//!
//! > "Hot-path presentation (focus, one nav step) runs inline as a pure function over snapshots.
//! > Bulk presentation (say-all chunks, tables, live-region diff text) runs as `PresentJob`s on
//! > `cpu`, and the results go straight to the speech lane with their epoch, without passing
//! > back through the Reader."

use std::sync::Arc;

use gorca_input_api::Modifiers;
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
    present_with(n, id, epoch, StateSet::empty())
}

/// Presentation of an item that just became selected (a switcher entry). The `selected` word is
/// left out: being selected is the reason it is spoken at all.
pub fn present_selected_item(n: &NodeSnapshot, id: UttId, epoch: Epoch) -> Utterance {
    present_with(n, id, epoch, StateSet::SELECTED)
}

fn present_with(n: &NodeSnapshot, id: UttId, epoch: Epoch, skip: StateSet) -> Utterance {
    let mut segments: SmallVec<[Segment; 4]> = SmallVec::new();
    if let Some(name) = n.name.as_deref().filter(|s| !s.is_empty()) {
        segments.push(seg(name, SegmentKind::Text));
    }
    let role = n.role.spoken();
    if !role.is_empty() {
        segments.push(seg(role, SegmentKind::Role));
    }
    for (flag, word) in STATE_WORDS {
        if n.states.contains(*flag) && !skip.contains(*flag) {
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

/// Modifiers that turn a key press into a command or shortcut. Orca never echoes a key pressed
/// together with Alt or Control (`typing_echo_presenter.should_echo_keyboard_event`); Super is
/// treated the same here. Shift, the locks and AltGr (a character modifier) do not count.
const COMMAND_MODIFIERS: Modifiers = Modifiers::CONTROL
    .union(Modifiers::MOD1)
    .union(Modifiers::MOD4);

/// Spoken form of a key press (key echo), following Orca's defaults: alphabetic, numeric,
/// punctuation, space, function and action keys (enter, tab, backspace, escape, delete) are
/// echoed; navigation keys and lone modifier keys are not; nothing is echoed while Alt,
/// Control or Super is held (`Alt+Tab` must not say "tab").
pub fn key_echo_text(unichar: u32, keysym: u32, modifiers: Modifiers) -> Option<String> {
    if modifiers.intersects(COMMAND_MODIFIERS) {
        return None;
    }
    let named = match keysym {
        0xff0d | 0xff8d => Some("enter"),
        0xff08 => Some("backspace"),
        0xff09 | 0xfe20 => Some("tab"),
        0xff1b => Some("escape"),
        0xffff => Some("delete"),
        0x20 => Some("space"),
        _ => None,
    };
    if let Some(n) = named {
        return Some(n.to_owned());
    }
    // Navigation keys: Home, Left, Up, Right, Down, Page Up, Page Down, End (and the keypad
    // copies). Their result is what gets spoken, not the key.
    if (0xff50..=0xff58).contains(&keysym) || (0xff95..=0xff9c).contains(&keysym) {
        return None;
    }
    // Function keys F1..F35 are named.
    if (0xffbe..=0xffe0).contains(&keysym) {
        return Some(format!("f {}", keysym - 0xffbe + 1));
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
    fn key_echo_names_action_keys_and_skips_lone_modifiers() {
        let none = Modifiers::empty();
        assert_eq!(key_echo_text('a' as u32, 0x61, none).as_deref(), Some("a"));
        assert_eq!(key_echo_text(0x20, 0x20, none).as_deref(), Some("space"));
        assert_eq!(key_echo_text(0, 0xff0d, none).as_deref(), Some("enter"));
        assert_eq!(key_echo_text(0, 0xff09, none).as_deref(), Some("tab"));
        assert_eq!(
            key_echo_text(0, 0xffe1, none),
            None,
            "shift alone is silent"
        );
        assert_eq!(key_echo_text(0, 0xffe9, Modifiers::MOD1), None, "so is alt");
        assert_eq!(
            key_echo_text(0x07, 0x07, none),
            None,
            "control characters are not echoed"
        );
        assert_eq!(key_echo_text('é' as u32, 0xe9, none).as_deref(), Some("é"));
        assert_eq!(key_echo_text(0, 0xffbe, none).as_deref(), Some("f 1"));
    }

    #[test]
    fn navigation_keys_are_not_echoed() {
        for sym in [
            0xff50, 0xff51, 0xff52, 0xff53, 0xff54, 0xff55, 0xff56, 0xff57,
        ] {
            assert_eq!(key_echo_text(0, sym, Modifiers::empty()), None, "{sym:#x}");
        }
    }

    #[test]
    fn nothing_is_echoed_under_alt_control_or_super() {
        for m in [
            Modifiers::MOD1,
            Modifiers::CONTROL,
            Modifiers::MOD4,
            Modifiers::MOD1 | Modifiers::SHIFT,
            Modifiers::CONTROL | Modifiers::MOD2,
        ] {
            assert_eq!(key_echo_text(0, 0xff09, m), None, "tab under {m:?}");
            assert_eq!(key_echo_text('c' as u32, 0x63, m), None, "c under {m:?}");
        }
    }

    #[test]
    fn shift_locks_and_altgr_still_echo() {
        for m in [
            Modifiers::SHIFT,
            Modifiers::LOCK,
            Modifiers::MOD2,
            Modifiers::MOD5,
        ] {
            assert_eq!(key_echo_text('A' as u32, 0x41, m).as_deref(), Some("A"));
        }
        assert_eq!(
            key_echo_text(0, 0xfe20, Modifiers::SHIFT).as_deref(),
            Some("tab")
        );
    }

    #[test]
    fn selected_items_do_not_say_selected() {
        let n = snap(
            Role::Button,
            Some("Terminal"),
            StateSet::SELECTED,
            Freshness::Fresh { at: Instant::now() },
        );
        assert_eq!(
            texts(&present_focus(&n, UttId(1), Epoch(1))),
            ["Terminal", "button", "selected"]
        );
        assert_eq!(
            texts(&present_selected_item(&n, UttId(1), Epoch(1))),
            ["Terminal", "button"]
        );
    }

    #[test]
    fn message_has_message_priority() {
        let u = message("application not responding", UttId(2), Epoch(3));
        assert_eq!(u.prio, Prio::Message);
        assert_eq!(texts(&u), ["application not responding"]);
    }
}
