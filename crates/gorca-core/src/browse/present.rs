//! Line presentation (browse-mode design 8.2, LP-1 to LP-4): what the user hears for a range of
//! the flat text. Everything is built from one published `WebDoc` snapshot, so it costs no IPC
//! and finishes in microseconds (LP-3).
//!
//! A range is described by walking the nodes it covers in document order: text runs give text
//! segments; a node *entered* on the range whose role is announced inline adds a prefix ("link",
//! "heading level 2", "button", "edit"); a control that has no text of its own (FFFC marker,
//! FT-6) is spoken as its role, state and value. Containers (lists, tables, landmarks) are the
//! field diff of M7b and are not announced here.

use std::sync::Arc;

use gorca_doc::web::{NodeFlags, NodeView, WebDoc};
use gorca_model::{DocNodeId, Role, Segment, SegmentKind, StateSet};
use smallvec::SmallVec;

/// A scalar of text that stands for a control with no text (FT-6).
pub const MARKER: char = '\u{FFFC}';
/// Quick navigation speaks at most this many scalars of the target (LP-4).
pub const QUICK_NAV_CAP: u32 = 400;
/// A line is cut here when it is spoken (LP-3 guard bound).
pub const LINE_CAP: u32 = 2048;

/// One thing to say.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Item {
    Text(String),
    /// "link", "heading level 2", "button"...
    Role(String),
    State(&'static str),
    Name(String),
}

impl Item {
    pub fn kind(&self) -> SegmentKind {
        match self {
            Item::Text(_) | Item::Name(_) => SegmentKind::Text,
            Item::Role(_) => SegmentKind::Role,
            Item::State(_) => SegmentKind::State,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Item::Text(t) | Item::Role(t) | Item::Name(t) => t,
            Item::State(s) => s,
        }
    }
}

/// Roles announced when entered inside a line.
fn announced(role: Role) -> bool {
    matches!(
        role,
        Role::Link
            | Role::Heading
            | Role::Button
            | Role::ToggleButton
            | Role::CheckBox
            | Role::RadioButton
            | Role::ComboBox
            | Role::TextEntry
            | Role::PasswordEntry
            | Role::Slider
            | Role::SpinButton
            | Role::Image
            | Role::ProgressBar
    )
}

fn role_phrase(v: &NodeView<'_>) -> String {
    let st = v.meta.map_or(StateSet::empty(), |m| m.states);
    if v.role == Role::Link {
        if st.contains(StateSet::VISITED) {
            "visited link".to_owned()
        } else {
            "link".to_owned()
        }
    } else if v.role == Role::Heading {
        if (1..=6).contains(&v.level) {
            format!("heading level {}", v.level)
        } else {
            "heading".to_owned()
        }
    } else if v.role == Role::Image {
        "graphic".to_owned()
    } else {
        v.role.spoken().to_owned()
    }
}

/// States spoken after a control's role.
fn push_states(role: Role, st: StateSet, out: &mut Vec<Item>) {
    let checkable = matches!(role, Role::CheckBox | Role::RadioButton);
    if checkable {
        out.push(Item::State(if st.contains(StateSet::CHECKED) {
            "checked"
        } else {
            "not checked"
        }));
    }
    if st.contains(StateSet::PRESSED) {
        out.push(Item::State("pressed"));
    }
    if st.contains(StateSet::EXPANDED) {
        out.push(Item::State("expanded"));
    } else if st.contains(StateSet::COLLAPSED) {
        out.push(Item::State("collapsed"));
    }
    // Firefox never sets ENABLED on static content (the document, headings), and clears it on
    // disabled controls: the absence only means "unavailable" for things that can be disabled.
    let control = matches!(
        role,
        Role::Button
            | Role::ToggleButton
            | Role::CheckBox
            | Role::RadioButton
            | Role::ComboBox
            | Role::TextEntry
            | Role::PasswordEntry
            | Role::SpinButton
    );
    if st.contains(StateSet::DISABLED)
        || (control && !st.is_empty() && !st.contains(StateSet::ENABLED))
    {
        out.push(Item::State("unavailable"));
    }
    if st.contains(StateSet::READ_ONLY) && matches!(role, Role::TextEntry | Role::PasswordEntry) {
        out.push(Item::State("read only"));
    }
    if st.contains(StateSet::REQUIRED) {
        out.push(Item::State("required"));
    }
    if st.contains(StateSet::INVALID) {
        out.push(Item::State("invalid entry"));
    }
}

fn enter_items(v: &NodeView<'_>, out: &mut Vec<Item>) {
    out.push(Item::Role(role_phrase(v)));
    let st = v.meta.map_or(StateSet::empty(), |m| m.states);
    // A control that carries its own text is followed by that text; its states come first.
    if !matches!(v.role, Role::Link | Role::Heading) {
        push_states(v.role, st, out);
    } else if st.contains(StateSet::EXPANDED) {
        out.push(Item::State("expanded"));
    } else if st.contains(StateSet::COLLAPSED) {
        out.push(Item::State("collapsed"));
    }
}

/// A control with no text: its role, state and value (and name where the name is all there is).
fn marker_items(v: &NodeView<'_>, out: &mut Vec<Item>) {
    let st = v.meta.map_or(StateSet::empty(), |m| m.states);
    // A graphic or a button is its name; a check box or an edit is labelled by text beside it.
    let named = matches!(
        v.role,
        Role::Image | Role::Button | Role::ToggleButton | Role::ProgressBar | Role::Slider
    );
    if named && let Some(n) = v.name.filter(|n| !n.is_empty()) {
        out.push(Item::Name(n.to_owned()));
    }
    out.push(Item::Role(role_phrase(v)));
    push_states(v.role, st, out);
    if matches!(v.role, Role::TextEntry | Role::PasswordEntry) {
        out.push(Item::State("blank"));
    }
    if v.role == Role::Image && v.name.is_none_or(str::is_empty) {
        out.insert(0, Item::State("unlabeled"));
    }
}

/// Offset where the node's content starts: after its own line break.
fn eff_start(doc: &WebDoc, id: DocNodeId, v: &NodeView<'_>) -> Option<u32> {
    let base = doc.offset_of(id)?;
    Some(
        base + u32::from(
            v.flags.contains(NodeFlags::BREAK_BEFORE) && !v.flags.contains(NodeFlags::HIDDEN),
        ),
    )
}

/// Describe the flat-text range `[a, b)`.
pub fn describe(doc: &WebDoc, a: u32, b: u32) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let total = doc.text_len();
    let b = b.min(total).min(a.saturating_add(LINE_CAP));
    if a >= total {
        return out;
    }
    let Some((n0, local0)) = doc.at_offset(a) else {
        return out;
    };
    // Ancestors whose content starts exactly here were entered on this range.
    let mut chain: Vec<DocNodeId> = Vec::new();
    let mut cur = doc.parent(n0);
    while let Some(p) = cur {
        chain.push(p);
        cur = doc.parent(p);
    }
    for id in chain.into_iter().rev() {
        let Some(v) = doc.node(id) else { continue };
        if announced(v.role)
            && !v.flags.contains(NodeFlags::HIDDEN)
            && eff_start(doc, id, &v) == Some(a)
        {
            enter_items(&v, &mut out);
        }
    }
    // Then everything from the first node on.
    let mut off = a - local0;
    for id in doc.preorder_from(n0) {
        let Some(v) = doc.node(id) else { continue };
        let own = doc.own_len(id).unwrap_or(0);
        let hidden = v.flags.contains(NodeFlags::HIDDEN);
        let eff = off + u32::from(v.flags.contains(NodeFlags::BREAK_BEFORE) && !hidden);
        let first = id == n0;
        if !first && eff >= b {
            break;
        }
        let entered = if first { eff == a } else { eff >= a };
        let is_marker = v.text == Some("\u{FFFC}");
        if entered && announced(v.role) && !hidden && !is_marker {
            enter_items(&v, &mut out);
        }
        let lo = a.max(off);
        let hi = b.min(off + own);
        if lo < hi {
            if is_marker {
                if eff >= lo && eff < hi {
                    marker_items(&v, &mut out);
                }
            } else {
                let t = doc.text_in(lo, hi);
                let t = t.trim_matches('\n');
                if !t.is_empty() {
                    out.push(Item::Text(t.to_owned()));
                }
            }
        }
        off += own;
    }
    out
}

/// Group items into utterance segments.
pub fn segments(items: &[Item]) -> SmallVec<[Segment; 4]> {
    items
        .iter()
        .map(|i| (i.text().trim(), i.kind()))
        .filter(|(t, _)| !t.is_empty())
        .map(|(t, kind)| Segment {
            text: Arc::from(t),
            kind,
        })
        .collect()
}

/// What a single character is called when it is spoken alone.
pub fn char_name(c: char) -> String {
    match c {
        ' ' | '\u{00A0}' => "space".to_owned(),
        '\t' => "tab".to_owned(),
        '\n' | '\r' => "blank".to_owned(),
        MARKER => "object".to_owned(),
        c => c.to_string(),
    }
}

/// The items for a quick-nav target: its own text range with its role announced first (LP-4).
pub fn describe_node(doc: &WebDoc, id: DocNodeId) -> Vec<Item> {
    let Some(v) = doc.node(id) else {
        return Vec::new();
    };
    let Some(a) = eff_start(doc, id, &v) else {
        return Vec::new();
    };
    let Some((_, end)) = doc.subtree_text_range(id) else {
        return Vec::new();
    };
    let b = end.min(a + QUICK_NAV_CAP);
    let mut items = if a < b {
        describe(doc, a, b)
    } else {
        Vec::new()
    };
    // A node with no content on the page (an empty heading, a link that is all image) is still
    // announced, once.
    let announced_here = items.iter().any(|i| matches!(i, Item::Role(_)));
    if !announced_here && announced(v.role) {
        let mut pre = Vec::new();
        enter_items(&v, &mut pre);
        pre.append(&mut items);
        items = pre;
    }
    // A frame (a nested document) and an article are announced by what they are, then by name.
    let mut pre: Vec<Item> = Vec::new();
    if v.flags.contains(NodeFlags::FRAME) {
        pre.push(Item::Role("frame".to_owned()));
        // Blink exposes the iframe element (an object of no mapped role, named by its title
        // attribute) above the frame's document, whose own name is the page title inside: the
        // frame is announced by the title the author gave it.
        let holder = doc
            .parent(id)
            .and_then(|p| doc.node(p))
            .filter(|p| p.role == Role::Unknown)
            .and_then(|p| p.name)
            .filter(|n| !n.is_empty());
        if let Some(n) = holder.or(v.name.filter(|n| !n.is_empty())) {
            pre.push(Item::Name(n.to_owned()));
        }
    } else if v.flags.contains(NodeFlags::ARTICLE) {
        pre.push(Item::Role("article".to_owned()));
    }
    if !pre.is_empty() {
        pre.append(&mut items);
        items = pre;
    }
    items
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_doc::web::{DocOp, NodeMeta, NodeSpec, SubtreeSpec};

    use super::*;

    fn leaf(role: Role) -> NodeSpec {
        NodeSpec::new(role)
    }

    fn with_states(n: NodeSpec, st: StateSet) -> NodeSpec {
        n.meta(Arc::new(NodeMeta {
            states: st,
            ..NodeMeta::default()
        }))
    }

    // body: h2 "Products", p ["Go to " link "docs" " now"], a check box and an image "logo".
    fn page() -> WebDoc {
        let mut d = WebDoc::new(5);
        let blk = NodeFlags::BLOCK | NodeFlags::BREAK_BEFORE;
        let h2 = SubtreeSpec::with_children(
            leaf(Role::Heading).level(2).flags(blk),
            vec![SubtreeSpec::leaf(leaf(Role::StaticText).text("Products"))],
        );
        let p = SubtreeSpec::with_children(
            leaf(Role::Paragraph).flags(blk),
            vec![
                SubtreeSpec::leaf(leaf(Role::StaticText).text("Go to ")),
                SubtreeSpec::with_children(
                    with_states(leaf(Role::Link), StateSet::VISITED | StateSet::ENABLED),
                    vec![SubtreeSpec::leaf(leaf(Role::StaticText).text("docs"))],
                ),
                SubtreeSpec::leaf(leaf(Role::StaticText).text(" now")),
            ],
        );
        let cb = SubtreeSpec::leaf(with_states(
            leaf(Role::CheckBox).flags(blk).text("\u{FFFC}"),
            StateSet::ENABLED | StateSet::CHECKED,
        ));
        let img = SubtreeSpec::leaf(leaf(Role::Image).flags(blk).text("\u{FFFC}").name("logo"));
        let root = SubtreeSpec::with_children(leaf(Role::DocumentWeb), vec![h2, p, cb, img]);
        assert!(
            d.apply(&DocOp::InsertSubtree {
                parent: None,
                before: None,
                subtree: Arc::new(root),
            })
            .is_ok()
        );
        d
    }

    fn say(items: &[Item]) -> String {
        items
            .iter()
            .flat_map(|i| i.text().split_whitespace())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn line_of(d: &WebDoc, n: u32) -> String {
        let s = d.line_start(n).unwrap();
        let (_, e) = d.line_range(s);
        say(&describe(d, s, e))
    }

    #[test]
    fn a_heading_line_announces_its_level_before_the_text() {
        let d = page();
        assert_eq!(
            d.text_in(0, d.text_len()),
            "\nProducts\nGo to docs now\n\u{FFFC}\n\u{FFFC}"
        );
        assert_eq!(line_of(&d, 1), "heading level 2 Products");
    }

    #[test]
    fn a_link_inside_a_line_is_announced_where_it_starts() {
        let d = page();
        assert_eq!(line_of(&d, 2), "Go to visited link docs now");
        // Starting the range inside the link does not announce it again.
        let s = d.line_start(2).unwrap() + 7;
        let (_, e) = d.line_range(s);
        assert_eq!(say(&describe(&d, s, e)), "ocs now");
    }

    #[test]
    fn markers_speak_role_state_and_name() {
        let d = page();
        assert_eq!(line_of(&d, 3), "check box checked");
        assert_eq!(line_of(&d, 4), "logo graphic");
    }

    #[test]
    fn quick_nav_describes_the_node_with_its_role_first() {
        let d = page();
        let h = d.first_of(gorca_doc::web::Cat::HEADING).unwrap();
        assert_eq!(say(&describe_node(&d, h)), "heading level 2 Products");
        let l = d.first_of(gorca_doc::web::Cat::LINK).unwrap();
        assert_eq!(say(&describe_node(&d, l)), "visited link docs");
    }

    #[test]
    fn single_characters_have_names() {
        assert_eq!(char_name(' '), "space");
        assert_eq!(char_name('\n'), "blank");
        assert_eq!(char_name('x'), "x");
    }
}
