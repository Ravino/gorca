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

use gorca_doc::web::{Cat, NodeFlags, NodeView, WebDoc};
use gorca_doc_source_api::web_attrs;
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
    /// Spoken before the focused control (a dialog entered with the focus): placed in front of
    /// the focus utterance by the presenter, not after it.
    Lead(String),
}

impl Item {
    pub fn kind(&self) -> SegmentKind {
        match self {
            Item::Text(_) | Item::Name(_) | Item::Lead(_) => SegmentKind::Text,
            Item::Role(_) => SegmentKind::Role,
            Item::State(_) => SegmentKind::State,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Item::Text(t) | Item::Role(t) | Item::Name(t) | Item::Lead(t) => t,
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
    let mixed = st
        .contains(StateSet::MIXED)
        .then(|| crate::present::mixed_word(role))
        .flatten();
    if !checkable && let Some(w) = mixed {
        out.push(Item::State(w));
    }
    if checkable {
        out.push(Item::State(if let Some(w) = mixed {
            w
        } else if st.contains(StateSet::CHECKED) {
            "checked"
        } else {
            "not checked"
        }));
    }
    if st.contains(StateSet::PRESSED) && mixed.is_none() {
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

/// The ARIA landmark kind of a landmark node, spoken before the word "landmark".
fn landmark_kind(v: &NodeView<'_>) -> &'static str {
    let attr = |k| {
        v.meta
            .and_then(|m| m.attrs.iter().find(|(a, _)| *a == k).map(|(_, x)| &**x))
    };
    let from_role = attr(web_attrs::XML_ROLES).and_then(|r| {
        r.split_ascii_whitespace().find_map(|t| match t {
            "banner" => Some("banner"),
            "main" => Some("main"),
            "navigation" => Some("navigation"),
            "search" => Some("search"),
            "complementary" => Some("complementary"),
            "contentinfo" => Some("content info"),
            "form" => Some("form"),
            "region" => Some("region"),
            _ => None,
        })
    });
    from_role.unwrap_or_else(|| match attr(web_attrs::TAG) {
        Some("main") => "main",
        Some("nav") => "navigation",
        Some("aside") => "complementary",
        Some("header") => "banner",
        Some("footer") => "content info",
        Some("form") => "form",
        _ => "region",
    })
}

/// "1 item" / "3 items".
fn counted(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// Rows and columns of a table node, from the tree (rows are `Row` descendants through at most
/// two group levels; columns are the widest row). `aria-rowcount`/`aria-colcount` win.
pub fn table_dims(doc: &WebDoc, id: DocNodeId) -> (usize, usize) {
    let num = |v: &NodeView<'_>, k| -> Option<usize> {
        v.meta
            .and_then(|m| m.attrs.iter().find(|(a, _)| *a == k).map(|(_, x)| &**x))
            .and_then(|x| x.parse::<usize>().ok())
    };
    let mut rows = 0usize;
    let mut cols = 0usize;
    let mut stack: Vec<(DocNodeId, u8)> = doc.children(id).map(|c| (c, 0)).collect();
    while let Some((n, depth)) = stack.pop() {
        let Some(v) = doc.node(n) else { continue };
        if v.role == Role::Row {
            rows += 1;
            let w = doc
                .children(n)
                .filter(|c| {
                    doc.node(*c).is_some_and(|cv| {
                        matches!(cv.role, Role::Cell | Role::ColumnHeader | Role::RowHeader)
                            && !cv.flags.contains(NodeFlags::HIDDEN)
                    })
                })
                .count();
            cols = cols.max(w);
        } else if depth < 2 && !v.flags.contains(NodeFlags::HIDDEN) {
            stack.extend(doc.children(n).map(|c| (c, depth + 1)));
        }
    }
    if let Some(v) = doc.node(id) {
        if let Some(r) = num(&v, web_attrs::ROWCOUNT).filter(|r| *r > 0) {
            rows = r;
        }
        if let Some(c) = num(&v, web_attrs::COLCOUNT).filter(|c| *c > 0) {
            cols = c;
        }
    }
    (rows, cols)
}

/// What entering a container says (BM-3 field diff, NVDA `getControlFieldSpeech`): lists and
/// tables with their sizes, block quotes, landmarks and regions with their names, dialogs and
/// forms. `None` for nodes that are not announced as containers.
fn container_items(doc: &WebDoc, v: &NodeView<'_>, id: DocNodeId) -> Option<Vec<Item>> {
    let mut out = Vec::new();
    let name = v.name.filter(|n| !n.is_empty());
    if v.cats.intersects(Cat::LIST) && v.role == Role::List {
        let n = doc
            .children(id)
            .filter(|c| doc.node(*c).is_some_and(|x| x.role == Role::ListItem))
            .count();
        out.push(Item::Role(format!(
            "list with {}",
            counted(n, "item", "items")
        )));
    } else if v.cats.intersects(Cat::TABLE) {
        let (r, c) = table_dims(doc, id);
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        out.push(Item::Role(format!(
            "table with {} and {}",
            counted(c, "column", "columns"),
            counted(r, "row", "rows")
        )));
    } else if v.cats.intersects(Cat::BLOCKQUOTE) {
        out.push(Item::Role("block quote".to_owned()));
    } else if v.cats.intersects(Cat::ANY_LANDMARK) {
        let kind = landmark_kind(v);
        let region = v.flags.contains(NodeFlags::REGION) || kind == "region";
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        out.push(Item::Role(if region || kind == "form" {
            kind.to_owned()
        } else {
            format!("{kind} landmark")
        }));
    } else if v.cats.intersects(Cat::DIALOG) {
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        out.push(Item::Role("dialog".to_owned()));
    } else if v.cats.intersects(Cat::EMBEDDED) {
        // role=application, or an <object>/<embed> (both are ATK_ROLE_EMBEDDED).
        let application = v.meta.is_some_and(|m| {
            m.attrs.iter().any(|(k, x)| {
                *k == web_attrs::XML_ROLES && x.split_ascii_whitespace().any(|t| t == "application")
            })
        });
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        out.push(Item::Role(
            if application {
                "application"
            } else {
                "embedded object"
            }
            .to_owned(),
        ));
    } else {
        return None;
    }
    Some(out)
}

/// Is `name` already part of what is said?
fn name_spoken(items: &[Item], name: &str) -> bool {
    let name = name.trim().to_lowercase();
    items.iter().any(|i| match i {
        Item::Text(t) | Item::Name(t) => t.to_lowercase().contains(&name),
        Item::Role(_) | Item::State(_) | Item::Lead(_) => false,
    })
}

/// A form control spoken on its own (quick navigation, focus follow): its accessible name first.
fn is_named_control(role: Role) -> bool {
    matches!(
        role,
        Role::TextEntry
            | Role::PasswordEntry
            | Role::ComboBox
            | Role::CheckBox
            | Role::RadioButton
            | Role::Slider
            | Role::SpinButton
            | Role::ToggleButton
    )
}

/// The text of the selected option of a combo box or list box.
fn selected_value(doc: &WebDoc, id: DocNodeId) -> Option<String> {
    for n in doc.subtree(id).skip(1) {
        let v = doc.node(n)?;
        if v.meta
            .is_some_and(|m| m.states.contains(StateSet::SELECTED))
            && matches!(v.role, Role::MenuItem | Role::ListItem)
        {
            let t = v.name.filter(|t| !t.is_empty()).or(v.text)?;
            return Some(t.trim().to_owned());
        }
    }
    None
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

/// Offset where the node's content starts: after its own line break and the blank breaks of
/// the blocks that open inside it.
fn eff_start(doc: &WebDoc, id: DocNodeId, _v: &NodeView<'_>) -> Option<u32> {
    super::cursor::content_start(doc, id)
}

/// Describe the flat-text range `[a, b)`.
pub fn describe(doc: &WebDoc, a: u32, b: u32) -> Vec<Item> {
    describe_from(doc, a, b, None)
}

/// Is the flat offset `at` inside the node's text?
fn node_holds(doc: &WebDoc, id: DocNodeId, at: u32) -> bool {
    doc.subtree_text_range(id)
        .is_some_and(|(s, e)| s <= at && at < e)
}

/// "out of list", "out of table", "out of block quote" for the containers that hold `from` but
/// not `to` (innermost first).
fn leaving_items(doc: &WebDoc, from: u32, to: u32) -> Vec<Item> {
    let mut out = Vec::new();
    let Some((mut cur, _)) = doc.at_offset(from.min(doc.text_len().saturating_sub(1))) else {
        return out;
    };
    for _ in 0..64 {
        let Some(v) = doc.node(cur) else { break };
        let word = if v.cats.intersects(Cat::TABLE) {
            Some("table")
        } else if v.role == Role::List && v.cats.intersects(Cat::LIST) {
            Some("list")
        } else if v.cats.intersects(Cat::BLOCKQUOTE) {
            Some("block quote")
        } else {
            None
        };
        if let Some(w) = word
            && !v.flags.contains(NodeFlags::HIDDEN)
            && node_holds(doc, cur, from)
            && !node_holds(doc, cur, to)
        {
            out.push(Item::Role(format!("out of {w}")));
        }
        match doc.parent(cur) {
            Some(p) => cur = p,
            None => break,
        }
    }
    out
}

/// Describe `[a, b)` as a move from the cursor at `from` (BM-3 field diff): a container or
/// control that already holds `from` was entered earlier and is not announced again.
pub fn describe_from(doc: &WebDoc, a: u32, b: u32, from: Option<u32>) -> Vec<Item> {
    let mut out: Vec<Item> = Vec::new();
    let total = doc.text_len();
    let b = b.min(total).min(a.saturating_add(LINE_CAP));
    if a >= total {
        return out;
    }
    let Some((n0, local0)) = doc.at_offset(a) else {
        return out;
    };
    // Containers the cursor leaves with this move (NVDA "out of list").
    if let Some(f) = from {
        out.extend(leaving_items(doc, f, a));
    }
    // Ancestors whose content starts exactly here were entered on this range.
    let mut chain: Vec<DocNodeId> = Vec::new();
    let mut cur = doc.parent(n0);
    while let Some(p) = cur {
        chain.push(p);
        cur = doc.parent(p);
    }
    for id in chain.into_iter().rev() {
        let Some(v) = doc.node(id) else { continue };
        if v.flags.contains(NodeFlags::HIDDEN)
            || eff_start(doc, id, &v) != Some(a)
            || from.is_some_and(|f| node_holds(doc, id, f))
        {
            continue;
        }
        if announced(v.role) {
            enter_items(&v, &mut out);
        } else if let Some(items) = container_items(doc, &v, id) {
            out.extend(items);
        }
    }
    // Then everything from the first node on.
    let mut off = a - local0;
    for id in doc.preorder_from(n0) {
        let Some(v) = doc.node(id) else { continue };
        let own = doc.own_len(id).unwrap_or(0);
        let hidden = v.flags.contains(NodeFlags::HIDDEN);
        let eff = off + v.flags.lead_len();
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
    dedupe_names(&mut out);
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
    describe_node_from(doc, id, None)
}

/// [`describe_node`] for a move from the cursor at `from`: containers the cursor is already in
/// are not announced again.
pub fn describe_node_from(doc: &WebDoc, id: DocNodeId, from: Option<u32>) -> Vec<Item> {
    let Some(v) = doc.node(id) else {
        return Vec::new();
    };
    let Some(a) = eff_start(doc, id, &v) else {
        return Vec::new();
    };
    let Some((_, end)) = doc.subtree_text_range(id) else {
        return Vec::new();
    };
    // A container is announced by what it is, then by its first line; anything else by its
    // whole text, capped (LP-4).
    let container = container_items(doc, &v, id).is_some();
    let mut b = end.min(a + QUICK_NAV_CAP);
    if container {
        let (_, line_end) = doc.line_range(a);
        b = b.min(line_end.max(a));
    }
    let mut items = if a < b {
        describe_from(doc, a, b, from)
    } else {
        Vec::new()
    };
    // A container whose first line starts later than its own content (a table that opens with
    // a caption line, an empty list) still gets its head.
    if container
        && !from.is_some_and(|f| node_holds(doc, id, f))
        && !items.iter().any(|i| matches!(i, Item::Role(_)))
        && let Some(mut head) = container_items(doc, &v, id)
    {
        head.append(&mut items);
        items = head;
    }
    // A node with no content on the page (an empty heading, a link that is all image) is still
    // announced, once.
    let announced_here = items.iter().any(|i| matches!(i, Item::Role(_)));
    if !announced_here && announced(v.role) {
        let mut pre = Vec::new();
        enter_items(&v, &mut pre);
        pre.append(&mut items);
        items = pre;
    }
    if !announced_here && v.role == Role::Separator {
        items.insert(0, Item::Role("separator".to_owned()));
    }
    // A form control alone on its range is a marker: it has no label of its own in the flat
    // text, so its accessible name leads (NVDA: "Search term edit blank").
    if is_named_control(v.role)
        && let Some(n) = v.name.filter(|n| !n.is_empty())
        && !name_spoken(&items, n)
    {
        items.insert(0, Item::Name(n.to_owned()));
    }
    if v.role == Role::ComboBox
        && let Some(val) = selected_value(doc, id)
        && !val.is_empty()
    {
        items.push(Item::Text(val));
    }
    // A frame (a nested document) and an article are announced by what they are, then by name.
    let mut pre: Vec<Item> = Vec::new();
    if v.flags.contains(NodeFlags::FRAME) {
        pre.push(Item::Role("frame".to_owned()));
        if let Some(n) = v.name.filter(|n| !n.is_empty()) {
            pre.push(Item::Name(n.to_owned()));
        }
    } else if v.flags.contains(NodeFlags::ARTICLE) {
        pre.push(Item::Role("article".to_owned()));
    }
    if !pre.is_empty() {
        pre.append(&mut items);
        items = pre;
    }
    dedupe_names(&mut items);
    items
}

/// A container's name is dropped when its first line says it already ("Fruit table table").
fn dedupe_names(items: &mut Vec<Item>) {
    let texts: Vec<String> = items
        .iter()
        .filter_map(|i| match i {
            Item::Text(t) => Some(t.to_lowercase()),
            Item::Role(_) | Item::State(_) | Item::Name(_) | Item::Lead(_) => None,
        })
        .collect();
    items.retain(|i| match i {
        Item::Name(n) => {
            let n = n.trim().to_lowercase();
            n.is_empty() || !texts.iter().any(|t| t.contains(&n))
        }
        Item::Text(_) | Item::Role(_) | Item::State(_) | Item::Lead(_) => true,
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_doc::web::{DocOp, NodeMeta, NodeSpec, SubtreeSpec};
    use gorca_doc_source_api::web_attrs;

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

    fn named(role: Role, name: &str) -> NodeSpec {
        leaf(role).name(name)
    }

    // main "Main content" > [ul > 2 li], then a form field "Search term", a combo box "Country"
    // with options, a 2x2 table "Fruit".
    fn containers() -> WebDoc {
        let mut d = WebDoc::new(7);
        let blk = NodeFlags::BLOCK | NodeFlags::BREAK_BEFORE;
        let txt = |t: &str| SubtreeSpec::leaf(leaf(Role::StaticText).text(t));
        let li =
            |t: &str| SubtreeSpec::with_children(leaf(Role::ListItem).flags(blk), vec![txt(t)]);
        let ul =
            SubtreeSpec::with_children(leaf(Role::List).flags(blk), vec![li("One"), li("Two")]);
        let main = SubtreeSpec::with_children(
            named(Role::Landmark, "Main content")
                .flags(blk)
                .meta(Arc::new(NodeMeta {
                    attrs: [(web_attrs::TAG, Arc::<str>::from("main"))]
                        .into_iter()
                        .collect(),
                    ..NodeMeta::default()
                })),
            vec![ul],
        );
        let edit = SubtreeSpec::leaf(with_states(
            named(Role::TextEntry, "Search term")
                .flags(blk)
                .text("\u{FFFC}"),
            StateSet::ENABLED | StateSet::EDITABLE,
        ));
        let opt = |n: &str, sel: bool| {
            SubtreeSpec::leaf(with_states(
                named(Role::MenuItem, n),
                if sel {
                    StateSet::SELECTED
                } else {
                    StateSet::empty()
                },
            ))
        };
        let combo = SubtreeSpec::with_children(
            with_states(
                named(Role::ComboBox, "Country").flags(blk).text("\u{FFFC}"),
                StateSet::ENABLED,
            ),
            vec![SubtreeSpec::with_children(
                leaf(Role::Menu),
                vec![opt("Albania", true), opt("Belgium", false)],
            )],
        );
        let cell = |t: &str| SubtreeSpec::with_children(leaf(Role::Cell).flags(blk), vec![txt(t)]);
        let row = |a: &str, b: &str| {
            SubtreeSpec::with_children(leaf(Role::Row).flags(blk), vec![cell(a), cell(b)])
        };
        let table = SubtreeSpec::with_children(
            named(Role::Table, "Fruit").flags(blk),
            vec![row("a", "b"), row("c", "d")],
        );
        let root =
            SubtreeSpec::with_children(leaf(Role::DocumentWeb), vec![main, edit, combo, table]);
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

    #[test]
    fn entering_a_landmark_and_a_list_is_announced_on_the_first_line() {
        let d = containers();
        let m = d.first_of(gorca_doc::web::Cat::LANDMARK).unwrap();
        let s = super::super::cursor::content_start(&d, m).unwrap();
        let (_, e) = d.line_range(s);
        assert_eq!(
            say(&describe(&d, s, e)),
            "Main content main landmark list with 2 items One"
        );
        // The second item enters nothing.
        let (_, e1) = d.line_range(s);
        let (s2, e2) = d.line_range(e1 + 1);
        assert_eq!(say(&describe(&d, s2, e2)), "Two");
    }

    #[test]
    fn quick_nav_to_a_list_or_landmark_says_what_it_is_then_one_line() {
        let d = containers();
        let l = d.first_of(gorca_doc::web::Cat::LIST).unwrap();
        assert_eq!(
            say(&describe_node(&d, l)),
            "Main content main landmark list with 2 items One"
        );
        let m = d.first_of(gorca_doc::web::Cat::LANDMARK).unwrap();
        assert_eq!(
            say(&describe_node(&d, m)),
            "Main content main landmark list with 2 items One"
        );
        let t = d.first_of(gorca_doc::web::Cat::TABLE).unwrap();
        assert_eq!(
            say(&describe_node(&d, t)),
            "Fruit table with 2 columns and 2 rows a"
        );
    }

    #[test]
    fn a_form_control_on_quick_nav_leads_with_its_name_and_a_combo_with_its_value() {
        let d = containers();
        let e = d.first_of(gorca_doc::web::Cat::EDIT).unwrap();
        assert_eq!(say(&describe_node(&d, e)), "Search term edit blank");
        let c = d.first_of(gorca_doc::web::Cat::COMBOBOX).unwrap();
        assert_eq!(say(&describe_node(&d, c)), "Country combo box Albania");
    }

    #[test]
    fn single_characters_have_names() {
        assert_eq!(char_name(' '), "space");
        assert_eq!(char_name('\n'), "blank");
        assert_eq!(char_name('x'), "x");
    }
}
