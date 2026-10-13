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
    match &n.value {
        Some(Value::Text(t)) if !t.is_empty() => segments.push(seg(t, SegmentKind::Value)),
        Some(Value::Range { current, .. }) => {
            segments.push(seg(&format!("{current}"), SegmentKind::Value));
        }
        Some(Value::Text(_)) | None => {}
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
    }
}
