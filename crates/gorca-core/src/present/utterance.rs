//! Node presentation: name, role, states and value as one utterance.

use std::sync::Arc;

use gorca_model::{
    Epoch, Freshness, NodeSnapshot, Prio, Segment, SegmentKind, StateSet, UttId, Utterance, Value,
};
use smallvec::SmallVec;

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

pub(super) fn seg(text: &str, kind: SegmentKind) -> Segment {
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
    if let Some(v) = value_text(n.value.as_ref()) {
        segments.push(seg(&v, SegmentKind::Value));
    }
    let may_refine = match n.fresh {
        Freshness::Fresh { .. } => false,
        Freshness::Stale { .. } | Freshness::Defunct => true,
    };
    Utterance {
        id,
        segments,
        lang: None,
        epoch,
        prio: Prio::Interrupt,
        may_refine,
        survives_focus_change: false,
        refines: None,
    }
}

/// The spoken form of a value, if it has one.
pub(crate) fn value_text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Text(t)) if !t.is_empty() => Some(t.to_string()),
        Some(Value::Range { current, .. }) => Some(format!("{current}")),
        Some(Value::Text(_)) | None => None,
    }
}

/// The states a presentation speaks (and the presentation log records), as a set.
fn spoken_states(n: &NodeSnapshot) -> StateSet {
    STATE_WORDS
        .iter()
        .fold(StateSet::empty(), |acc, (flag, _)| acc | (n.states & *flag))
}

/// Every state name of a snapshot, for the presentation log: the AT-SPI names of the states the
/// connector decoded, plus `sensitive` (the core records its absence as `DISABLED`).
pub(crate) fn log_states(n: &NodeSnapshot) -> SmallVec<[&'static str; 8]> {
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

/// What differs between two snapshots in the fields a presentation speaks.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Changed {
    pub name: bool,
    pub role: bool,
    /// Spoken states whose presence differs.
    pub states: StateSet,
    pub value: bool,
}

impl Changed {
    pub fn is_empty(self) -> bool {
        !self.name && !self.role && self.states.is_empty() && !self.value
    }
}

/// What differs in the spoken fields. `skip` are states that are not spoken in this context (a
/// switcher item is spoken *because* it is selected, so `SELECTED` is never news).
pub(crate) fn changed(old: &NodeSnapshot, new: &NodeSnapshot, skip: StateSet) -> Changed {
    let name = |n: &NodeSnapshot| {
        n.name
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    Changed {
        name: name(old) != name(new),
        role: old.role != new.role,
        states: (spoken_states(old) ^ spoken_states(new)) & !skip,
        value: value_text(old.value.as_ref()) != value_text(new.value.as_ref()),
    }
}

/// A short utterance that only says what changed (a state flip, a new value, a new name).
pub(crate) fn present_change(
    new: &NodeSnapshot,
    ch: Changed,
    id: UttId,
    epoch: Epoch,
    refines: Option<UttId>,
) -> Utterance {
    let mut segments: SmallVec<[Segment; 4]> = SmallVec::new();
    if ch.name
        && let Some(name) = new.name.as_deref().filter(|s| !s.is_empty())
    {
        segments.push(seg(name, SegmentKind::Text));
    }
    if ch.role {
        let role = new.role.spoken();
        if !role.is_empty() {
            segments.push(seg(role, SegmentKind::Role));
        }
    }
    for (flag, word) in STATE_WORDS {
        if ch.states.contains(*flag) {
            if new.states.contains(*flag) {
                segments.push(seg(word, SegmentKind::State));
            } else {
                segments.push(seg(&format!("not {word}"), SegmentKind::State));
            }
        }
    }
    if ch.value
        && let Some(v) = value_text(new.value.as_ref())
    {
        segments.push(seg(&v, SegmentKind::Value));
    }
    Utterance {
        id,
        segments,
        lang: None,
        epoch,
        prio: Prio::Message,
        may_refine: false,
        survives_focus_change: false,
        refines,
    }
}

#[cfg(test)]
mod change_tests {
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
        }
    }

    fn texts(u: &Utterance) -> Vec<String> {
        u.segments.iter().map(|s| s.text.to_string()).collect()
    }

    #[test]
    fn equal_snapshots_have_no_change_and_focus_state_is_ignored() {
        let a = snap("Wi-Fi", StateSet::CHECKED, None);
        let mut b = a.clone();
        b.states |= StateSet::FOCUSED | StateSet::SHOWING;
        assert!(changed(&a, &b, StateSet::empty()).is_empty());
    }

    #[test]
    fn a_flipped_state_is_spoken_both_ways() {
        let off = snap("Wi-Fi", StateSet::empty(), None);
        let on = snap("Wi-Fi", StateSet::CHECKED, None);
        let ch = changed(&off, &on, StateSet::empty());
        assert_eq!(ch.states, StateSet::CHECKED);
        let u = present_change(&on, ch, UttId(2), Epoch(1), Some(UttId(1)));
        assert_eq!(texts(&u), ["checked"]);
        assert_eq!(u.refines, Some(UttId(1)));
        let u = present_change(
            &off,
            changed(&on, &off, StateSet::empty()),
            UttId(3),
            Epoch(1),
            None,
        );
        assert_eq!(texts(&u), ["not checked"]);
    }

    #[test]
    fn name_and_value_changes_are_spoken() {
        let a = snap(
            "Volume",
            StateSet::empty(),
            Some(Value::Range {
                current: 3.0,
                min: 0.0,
                max: 10.0,
            }),
        );
        let mut b = a.clone();
        b.name = Some(Arc::from("Master volume"));
        b.value = Some(Value::Range {
            current: 4.0,
            min: 0.0,
            max: 10.0,
        });
        let ch = changed(&a, &b, StateSet::empty());
        assert!(ch.name && ch.value && !ch.role);
        assert!(changed(&off_selected(), &selected(), StateSet::SELECTED).is_empty());
        let u = present_change(&b, ch, UttId(1), Epoch(1), None);
        assert_eq!(texts(&u), ["Master volume", "4"]);
    }

    fn off_selected() -> NodeSnapshot {
        snap("Files", StateSet::empty(), None)
    }

    fn selected() -> NodeSnapshot {
        snap("Files", StateSet::SELECTED, None)
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
