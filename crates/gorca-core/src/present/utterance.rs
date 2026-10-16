//! Node presentation: name, role, states and value as one utterance.

use std::sync::Arc;

use gorca_model::{
    Epoch, Freshness, NodeSnapshot, Prio, Role, Segment, SegmentKind, StateSet, UttId, Utterance,
    Value,
};
use smallvec::SmallVec;

/// State words in speaking order. TBD: localisation and verbosity settings.
const STATE_WORDS: &[(StateSet, &str)] = &[
    (StateSet::DISABLED, "unavailable"),
    (StateSet::MIXED, "half checked"),
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

/// Roles of static web content that are never meaningfully disabled.
pub(crate) fn never_disabled(role: Role) -> bool {
    matches!(
        role,
        Role::DocumentWeb
            | Role::Document
            | Role::Heading
            | Role::Paragraph
            | Role::Image
            | Role::List
            | Role::ListItem
            | Role::Table
            | Role::Row
            | Role::Cell
            | Role::ColumnHeader
            | Role::RowHeader
            | Role::Landmark
            | Role::StaticText
            | Role::Separator
    )
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
    // Gecko reports no Sensitive on its document; that is not "unavailable" (the connector maps
    // a missing Sensitive to DISABLED). Other static web roles are handled by the Reader, which
    // knows which applications are web applications (N7).
    let skip = if n.role == Role::DocumentWeb {
        skip | StateSet::DISABLED
    } else {
        skip
    };
    // A tri-state check box in its middle state says "half checked" only.
    let mixed = n.states.contains(StateSet::MIXED);
    for (flag, word) in STATE_WORDS {
        if *flag == StateSet::CHECKED && mixed {
            continue;
        }
        if n.states.contains(*flag) && !skip.contains(*flag) {
            segments.push(seg(word, SegmentKind::State));
        }
    }
    // A checkable control says its off state too ("check box not checked", "not pressed").
    let off = if matches!(n.role, Role::CheckBox | Role::RadioButton)
        && !mixed
        && !n.states.contains(StateSet::CHECKED)
        && !skip.contains(StateSet::CHECKED)
    {
        Some("not checked")
    } else if n.role == Role::ToggleButton
        && !n.states.intersects(StateSet::PRESSED | StateSet::CHECKED)
        && !skip.contains(StateSet::PRESSED)
    {
        Some("not pressed")
    } else {
        None
    };
    if let Some(w) = off {
        segments.push(seg(w, SegmentKind::State));
    }
    if let Some(v) = value_text(n.value.as_ref()) {
        segments.push(seg(&v, SegmentKind::Value));
    }
    // A field's description is its help or error text (aria-describedby, aria-errormessage).
    if speaks_description(n.role)
        && let Some(d) = desc(n)
    {
        segments.push(seg(d, SegmentKind::Text));
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
pub fn value_text(v: Option<&Value>) -> Option<String> {
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

/// What differs between two snapshots in the fields a presentation speaks.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // one flag per spoken field
pub(crate) struct Changed {
    pub name: bool,
    pub role: bool,
    /// Spoken states whose presence differs.
    pub states: StateSet,
    pub value: bool,
    /// A field's help or error text (spoken after the value).
    pub description: bool,
}

impl Changed {
    pub fn is_empty(self) -> bool {
        !self.name && !self.role && self.states.is_empty() && !self.value && !self.description
    }
}

/// What differs in the spoken fields. `skip` are states that are not spoken in this context (a
/// switcher item is spoken *because* it is selected, so `SELECTED` is never news).
fn speaks_description(role: Role) -> bool {
    matches!(
        role,
        Role::TextEntry | Role::PasswordEntry | Role::ComboBox | Role::SpinButton
    )
}

fn desc(n: &NodeSnapshot) -> Option<&str> {
    n.description.as_deref().filter(|d| !d.is_empty())
}

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
        description: speaks_description(new.role) && desc(old) != desc(new) && desc(new).is_some(),
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
    // Checked/half checked and expanded/collapsed are each one tri-state: say only the state
    // the control is in now ("expanded", "collapsed", "half checked", "not checked"), never the
    // flipped partner ("not collapsed").
    let check = StateSet::CHECKED | StateSet::MIXED;
    let expand = StateSet::EXPANDED | StateSet::COLLAPSED;
    if ch.states.intersects(check) {
        let w = if new.states.contains(StateSet::MIXED) {
            "half checked"
        } else if new.states.contains(StateSet::CHECKED) {
            "checked"
        } else {
            "not checked"
        };
        segments.push(seg(w, SegmentKind::State));
    }
    if ch.states.intersects(expand) {
        let w = if new.states.contains(StateSet::EXPANDED) {
            "expanded"
        } else if new.states.contains(StateSet::COLLAPSED) {
            "collapsed"
        } else {
            "not expanded"
        };
        segments.push(seg(w, SegmentKind::State));
    }
    for (flag, word) in STATE_WORDS {
        if (check | expand).contains(*flag) {
            continue;
        }
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
    if ch.description
        && let Some(d) = desc(new)
    {
        segments.push(seg(d, SegmentKind::Text));
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
            push(word);
        }
    }
    if let Some(v) = value_text(n.value.as_ref()) {
        push(&v);
    }
    out
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

    // Review M-1 / M-2 regressions: expand/collapse and the tri-state check box.
    fn change_words(old: &NodeSnapshot, new: &NodeSnapshot) -> Vec<String> {
        let ch = changed(old, new, StateSet::empty());
        texts(&present_change(new, ch, UttId(1), Epoch(1), None))
    }

    #[test]
    fn expand_and_collapse_say_only_the_state_that_holds() {
        let mut col = snap("Fruit", StateSet::COLLAPSED, None);
        col.role = Role::ComboBox;
        let mut exp = col.clone();
        exp.states = StateSet::EXPANDED;
        assert_eq!(change_words(&col, &exp), ["expanded"]);
        assert_eq!(change_words(&exp, &col), ["collapsed"]);
    }

    #[test]
    fn a_mixed_check_box_says_half_checked_and_never_not_checked() {
        let mut mixed = snap("Select all", StateSet::MIXED, None);
        mixed.role = Role::CheckBox;
        let words = texts(&present_focus(&mixed, UttId(1), Epoch(1)));
        assert!(words.contains(&"half checked".to_owned()), "{words:?}");
        assert!(
            !words.iter().any(|w| w.contains("not checked")),
            "{words:?}"
        );
        let mut off = mixed.clone();
        off.states = StateSet::empty();
        assert_eq!(change_words(&off, &mixed), ["half checked"]);
        let mut on = mixed.clone();
        on.states = StateSet::CHECKED;
        assert_eq!(change_words(&mixed, &on), ["checked"]);
        assert_eq!(change_words(&on, &off), ["not checked"]);
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

    #[test]
    fn a_web_document_without_sensitive_is_not_called_unavailable() {
        let mut d = snap("Page", StateSet::DISABLED | StateSet::SHOWING, None);
        d.role = Role::DocumentWeb;
        let text = |n: &NodeSnapshot| {
            present_focus(n, UttId(1), Epoch::default())
                .segments
                .iter()
                .map(|s| s.text.to_string())
                .collect::<Vec<_>>()
        };
        assert!(
            !text(&d).iter().any(|t| t == "unavailable"),
            "{:?}",
            text(&d)
        );
        d.role = Role::CheckBox;
        assert!(text(&d).iter().any(|t| t == "unavailable"));
    }

    #[test]
    fn a_web_document_is_not_called_unavailable_but_a_native_row_still_is() {
        let mut d = snap("x", StateSet::DISABLED | StateSet::SHOWING, None);
        d.role = Role::DocumentWeb;
        let u = present_focus(&d, UttId(1), Epoch::default());
        assert!(!u.segments.iter().any(|s| &*s.text == "unavailable"));
        d.role = Role::ListItem;
        let u = present_focus(&d, UttId(1), Epoch::default());
        assert!(
            u.segments.iter().any(|s| &*s.text == "unavailable"),
            "native rows keep it"
        );
    }
}
