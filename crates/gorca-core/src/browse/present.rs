//! Line presentation (browse-mode design 8.2, LP-1 to LP-4): what the user hears for a range of
//! the flat text. Everything is built from one published `WebDoc` snapshot, so it costs no IPC
//! and finishes in microseconds (LP-3).
//!
//! A range is described by walking the nodes it covers in document order: text runs give text
//! segments; a node *entered* on the range whose role is announced inline adds a prefix ("link",
//! "heading level 2", "button", "edit"); a control that has no text of its own (FFFC marker,
//! FT-6) is spoken as its role, state and value. Containers (lists, tables, landmarks) are the
//! field diff of M7b and are not announced here.

use gorca_i18n::{MsgId as M, msg, t};
use std::sync::Arc;

use gorca_doc::web::{Cat, NodeFlags, NodeView, WebDoc};
use gorca_doc_source_api::web_attrs;
use gorca_model::{DocNodeId, Role, Segment, SegmentKind, StateSet};
use gorca_speech::{How, VerbalizeOpts, Verbalizer};
use smallvec::SmallVec;

use crate::aria;

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
    /// One character read on its own: spoken by name whatever the punctuation level.
    Char(String),
    /// Position and error words that belong to the control spoken just before ("level 2", the
    /// text of an `aria-errormessage`): not content, so they never count as the name.
    Info(String),
    /// The description of the node (`aria-describedby`): said after a short pause
    /// (`SegmentKind::Description`).
    Desc(String),
}

impl Item {
    pub fn kind(&self) -> SegmentKind {
        match self {
            Item::Text(_) | Item::Name(_) | Item::Lead(_) | Item::Char(_) | Item::Info(_) => {
                SegmentKind::Text
            }
            Item::Desc(_) => SegmentKind::Description,
            Item::Role(_) => SegmentKind::Role,
            Item::State(_) => SegmentKind::State,
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Item::Text(t)
            | Item::Role(t)
            | Item::Name(t)
            | Item::Lead(t)
            | Item::Char(t)
            | Item::Info(t)
            | Item::Desc(t) => t,
            Item::State(s) => s,
        }
    }
}

/// The aria facts of a document node.
fn node_of<'a>(v: &NodeView<'a>) -> aria::Node<'a> {
    aria::Node {
        role: v.role,
        states: v.meta.map_or(StateSet::empty(), |m| m.states),
        attrs: v.meta.map_or(&[][..], |m| m.attrs.as_slice()),
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

/// Is the node announced where it starts, by role: the plain roles above, plus the ARIA items
/// (tab, tree item, menu item, switch) that have no role of their own in the mirror.
fn announced_node(v: &NodeView<'_>) -> bool {
    announced(v.role)
        || matches!(v.role, Role::Tab | Role::TreeItem)
        || aria::Kind::is_item(node_of(v).kind())
}

fn role_phrase(v: &NodeView<'_>) -> String {
    let st = v.meta.map_or(StateSet::empty(), |m| m.states);
    if let Some(w) = aria::role_word(&node_of(v)).filter(|_| v.role != Role::Heading) {
        w.to_owned()
    } else if v.role == Role::Link {
        if st.contains(StateSet::VISITED) {
            t(M::WebVisitedLink).to_owned()
        } else {
            v.role.spoken().to_owned()
        }
    } else if v.role == Role::Heading {
        if (1..=6).contains(&v.level) {
            msg!(WebHeadingLevel, n = v.level)
        } else {
            v.role.spoken().to_owned()
        }
    } else if v.role == Role::Image {
        t(M::WebGraphic).to_owned()
    } else {
        v.role.spoken().to_owned()
    }
}

/// States spoken after a control's role (NVDA order and words: `aria::state_words`).
fn push_states(v: &NodeView<'_>, out: &mut Vec<Item>) {
    let st = v.meta.map_or(StateSet::empty(), |m| m.states);
    let node = node_of(v);
    // Firefox never sets ENABLED on static content (the document, headings), and clears it on
    // disabled controls: the absence only means "unavailable" for things that can be disabled.
    let control = matches!(
        v.role,
        Role::Button
            | Role::ToggleButton
            | Role::CheckBox
            | Role::RadioButton
            | Role::ComboBox
            | Role::TextEntry
            | Role::PasswordEntry
            | Role::SpinButton
    );
    let unavailable = st.contains(StateSet::DISABLED)
        || (control && !st.is_empty() && !st.contains(StateSet::ENABLED));
    for w in aria::state_words(&node, unavailable, false) {
        out.push(Item::State(w));
    }
    if let Some(c) = aria::current_word(&node) {
        out.push(Item::State(c));
    }
    if let Some(l) = aria::level_phrase(&node) {
        out.push(Item::Info(l));
    }
}

/// The ARIA landmark kind of a landmark node, spoken before the word "landmark".
fn landmark_kind(v: &NodeView<'_>) -> M {
    let attr = |k| {
        v.meta
            .and_then(|m| m.attrs.iter().find(|(a, _)| *a == k).map(|(_, x)| &**x))
    };
    let from_role = attr(web_attrs::XML_ROLES).and_then(|r| {
        r.split_ascii_whitespace().find_map(|t| match t {
            "banner" => Some(M::WebLandmarkBanner),
            "main" => Some(M::WebLandmarkMain),
            "navigation" => Some(M::WebLandmarkNavigation),
            "search" => Some(M::WebLandmarkSearch),
            "complementary" => Some(M::WebLandmarkComplementary),
            "contentinfo" => Some(M::WebLandmarkContentInfo),
            "form" => Some(M::WebLandmarkForm),
            "region" => Some(M::WebLandmarkRegion),
            _ => None,
        })
    });
    from_role.unwrap_or_else(|| match attr(web_attrs::TAG) {
        Some("main") => M::WebLandmarkMain,
        Some("nav") => M::WebLandmarkNavigation,
        Some("aside") => M::WebLandmarkComplementary,
        Some("header") => M::WebLandmarkBanner,
        Some("footer") => M::WebLandmarkContentInfo,
        Some("form") => M::WebLandmarkForm,
        _ => M::WebLandmarkRegion,
    })
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
    let kind = node_of(v).kind();
    if v.cats.intersects(Cat::LIST) && v.role == Role::List {
        let n = doc
            .children(id)
            .filter(|c| doc.node(*c).is_some_and(|x| x.role == Role::ListItem))
            .count();
        out.push(Item::Role(msg!(WebListWith, n = n)));
    } else if v.cats.intersects(Cat::TABLE) {
        let (r, c) = table_dims(doc, id);
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        out.push(Item::Role(msg!(WebTableWith, c = c, r = r)));
    } else if v.cats.intersects(Cat::BLOCKQUOTE) {
        out.push(Item::Role(t(M::WebBlockQuote).to_owned()));
    } else if v.cats.intersects(Cat::ANY_LANDMARK) {
        let kind = landmark_kind(v);
        let region = v.flags.contains(NodeFlags::REGION) || kind == M::WebLandmarkRegion;
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        out.push(Item::Role(if region || kind == M::WebLandmarkForm {
            t(kind).to_owned()
        } else {
            msg!(WebLandmark, kind = t(kind))
        }));
    } else if v.cats.intersects(Cat::DIALOG) {
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        out.push(Item::Role(t(M::RoleDialog).to_owned()));
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
                t(M::RoleApplication)
            } else {
                t(M::WebEmbeddedObject)
            }
            .to_owned(),
        ));
    } else if kind.is_container()
        && (name.is_some() || !matches!(kind, aria::Kind::Group | aria::Kind::TabPanel))
    {
        // An ARIA container (tab list, tree, menu, tool bar, a named group or tab panel) is
        // announced with its name and NVDA's word for it.
        if let Some(n) = name {
            out.push(Item::Name(n.to_owned()));
        }
        if let Some(w) = kind.word() {
            out.push(Item::Role(w.to_owned()));
        }
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
        Item::Role(_)
        | Item::State(_)
        | Item::Lead(_)
        | Item::Char(_)
        | Item::Info(_)
        | Item::Desc(_) => false,
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
    // A control that carries its own text is followed by that text; its states come first. A
    // link or a heading says only whether it is expanded and which one is current.
    if matches!(v.role, Role::Link | Role::Heading) {
        if st.contains(StateSet::EXPANDED) {
            out.push(Item::State(t(M::StateExpanded)));
        } else if st.contains(StateSet::COLLAPSED) {
            out.push(Item::State(t(M::StateCollapsed)));
        }
        if let Some(c) = aria::current_word(&node_of(v)) {
            out.push(Item::State(c));
        }
    } else {
        push_states(v, out);
        if let Some(e) = aria::error_text(&node_of(v)) {
            out.push(Item::Info(e.to_owned()));
        }
    }
}

/// A control with no text: its role, state and value (and name where the name is all there is).
fn marker_items(v: &NodeView<'_>, out: &mut Vec<Item>) {
    // A graphic or a button is its name; a check box or an edit is labelled by text beside it.
    // (NVDA also names a check box or radio button whose label is not visible: nameIsContent in
    // `gecko_ia2.cpp`; a label on the same line makes `dedupe_names` drop the repeat.)
    let named = matches!(
        v.role,
        Role::Image | Role::Button | Role::ToggleButton | Role::ProgressBar | Role::Slider
    ) || (matches!(v.role, Role::CheckBox | Role::RadioButton)
        && node_of(v).authored_name());
    if named && let Some(n) = v.name.filter(|n| !n.is_empty()) {
        out.push(Item::Name(n.to_owned()));
    }
    out.push(Item::Role(role_phrase(v)));
    push_states(v, out);
    if matches!(v.role, Role::TextEntry | Role::PasswordEntry) {
        out.push(Item::State(t(M::WebBlank)));
    }
    if v.role == Role::Image && v.name.is_none_or(str::is_empty) {
        out.insert(0, Item::State(t(M::WebUnlabeled)));
    }
    // What is wrong with an invalid field is said with it, in a line as well as on a hit.
    if let Some(e) = aria::error_text(&node_of(v)) {
        out.push(Item::Info(e.to_owned()));
    }
}

/// Offset where the node's content starts: after its own line break and the blank breaks of
/// the blocks that open inside it.
fn eff_start(doc: &WebDoc, id: DocNodeId, _v: &NodeView<'_>) -> Option<u32> {
    super::cursor::content_start(doc, id)
}

/// Roles whose accessible name the author may replace (`aria-label`, `aria-labelledby`, `alt`,
/// `title`): the name is spoken instead of the content text (the browser-computed accname,
/// M8b), like NVDA's buffer shows the label of such a node. The list is NVDA's `nameIsContent`
/// (`gecko_ia2.cpp`): link, button, toggle button, menu item, heading, page tab. A tree item is
/// not in it: its content stays and its label is only its focus name.
fn takes_authored_name(v: &NodeView<'_>) -> bool {
    let n = node_of(v);
    n.authored_name()
        && (matches!(
            v.role,
            Role::Link | Role::Button | Role::ToggleButton | Role::Tab | Role::Heading
        ) || matches!(
            n.kind(),
            aria::Kind::MenuItem | aria::Kind::MenuItemCheckBox | aria::Kind::MenuItemRadio
        ))
}

/// A node that was just announced and whose name was written by the author: say the name and
/// return the offset where its content ends (the content is not spoken). 0 for no override.
fn authored_name_items(doc: &WebDoc, id: DocNodeId, v: &NodeView<'_>, out: &mut Vec<Item>) -> u32 {
    let Some(name) = v.name.map(str::trim).filter(|n| !n.is_empty()) else {
        return 0;
    };
    if !takes_authored_name(v) {
        return 0;
    }
    out.push(Item::Name(name.to_owned()));
    doc.subtree_text_range(id).map_or(0, |(_, e)| e)
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
            Some(M::WebOutOfTable)
        } else if v.role == Role::List && v.cats.intersects(Cat::LIST) {
            Some(M::WebOutOfList)
        } else if v.cats.intersects(Cat::BLOCKQUOTE) {
            Some(M::WebOutOfBlockQuote)
        } else {
            None
        };
        if let Some(w) = word
            && !v.flags.contains(NodeFlags::HIDDEN)
            && node_holds(doc, cur, from)
            && !node_holds(doc, cur, to)
        {
            out.push(Item::Role(t(w).to_owned()));
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
    // Text before this offset is replaced by an authored accessible name (`aria-label`, ...).
    let mut skip_to: u32 = 0;
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
        if announced_node(&v) {
            enter_items(&v, &mut out);
            skip_to = skip_to.max(authored_name_items(doc, id, &v, &mut out));
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
        if entered && announced_node(&v) && !hidden && !is_marker {
            enter_items(&v, &mut out);
            skip_to = skip_to.max(authored_name_items(doc, id, &v, &mut out));
        }
        let lo = a.max(off).max(skip_to);
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

/// Group items into utterance segments. Text is verbalised with the punctuation level and the
/// symbol table of `o` (a character read alone is always named), so web content speaks its
/// symbols like any other text (VB-1/VB-2).
pub fn segments(items: &[Item], o: VerbalizeOpts) -> SmallVec<[Segment; 4]> {
    let v = Verbalizer;
    items
        .iter()
        .filter_map(|i| {
            let t = i.text().trim();
            if t.is_empty() {
                return None;
            }
            let text = match i {
                Item::Char(_) => v.unit(t, How::Char, &o),
                Item::Text(_) | Item::Name(_) | Item::Lead(_) | Item::Info(_) => {
                    v.unit(t, How::Text, &o)
                }
                // A description keeps its own kind and is symbol-processed once, by
                // `Reader::symbolize` at the speech sink (which handles that kind too).
                Item::Role(_) | Item::State(_) | Item::Desc(_) => t.to_owned(),
            };
            // A capital read alone gets the capital pitch (`speech.caps = "pitch"`).
            let capital = matches!(i, Item::Char(_))
                && o.caps == gorca_speech::Caps::Pitch
                && text.chars().count() == 1
                && text.chars().all(char::is_uppercase);
            (!text.is_empty()).then(|| Segment {
                text: Arc::from(text),
                kind: match i.kind() {
                    _ if capital => SegmentKind::Capital,
                    SegmentKind::Text => SegmentKind::Said,
                    other @ (SegmentKind::Role
                    | SegmentKind::State
                    | SegmentKind::Value
                    | SegmentKind::Description
                    | SegmentKind::Said
                    | SegmentKind::Capital) => other,
                },
            })
        })
        .collect()
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
    if !announced_here && announced_node(&v) {
        let mut pre = Vec::new();
        enter_items(&v, &mut pre);
        pre.append(&mut items);
        items = pre;
    }
    if !announced_here && v.role == Role::Separator {
        items.insert(0, Item::Role(t(M::RoleSeparator).to_owned()));
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
        pre.push(Item::Role(t(M::WebFrame).to_owned()));
        if let Some(n) = v.name.filter(|n| !n.is_empty()) {
            pre.push(Item::Name(n.to_owned()));
        }
    } else if v.flags.contains(NodeFlags::ARTICLE) {
        pre.push(Item::Role(t(M::WebArticle).to_owned()));
    }
    if !pre.is_empty() {
        pre.append(&mut items);
        items = pre;
    }
    // The description (`aria-describedby`, a tooltip title) is said after the rest, unless the
    // line already says it.
    if let Some(d) =
        aria::speakable_description(v.meta.and_then(|m| m.description.as_deref()), v.name)
        && !name_spoken(&items, d)
    {
        items.push(Item::Desc(d.to_owned()));
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
            Item::Role(_)
            | Item::State(_)
            | Item::Name(_)
            | Item::Lead(_)
            | Item::Char(_)
            | Item::Info(_)
            | Item::Desc(_) => None,
        })
        .collect();
    items.retain(|i| match i {
        Item::Name(n) => {
            let n = n.trim().to_lowercase();
            n.is_empty() || !texts.iter().any(|t| t.contains(&n))
        }
        Item::Text(_)
        | Item::Role(_)
        | Item::State(_)
        | Item::Lead(_)
        | Item::Char(_)
        | Item::Info(_)
        | Item::Desc(_) => true,
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

    // ---- M8b: ARIA names, roles, states, descriptions ---------------------------------------

    fn aria_node(
        role: Role,
        st: StateSet,
        attrs: &[(gorca_model::AttrKey, &str)],
        desc: Option<&str>,
    ) -> NodeSpec {
        leaf(role).meta(Arc::new(NodeMeta {
            states: st,
            description: desc.map(Arc::from),
            attrs: attrs.iter().map(|(k, v)| (*k, Arc::from(*v))).collect(),
            ..NodeMeta::default()
        }))
    }

    fn blk() -> NodeFlags {
        NodeFlags::BLOCK | NodeFlags::BREAK_BEFORE
    }

    fn doc_of(children: Vec<SubtreeSpec>) -> WebDoc {
        let mut d = WebDoc::new(9);
        let root = SubtreeSpec::with_children(leaf(Role::DocumentWeb), children);
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

    fn txt(t: &str) -> SubtreeSpec {
        SubtreeSpec::leaf(leaf(Role::StaticText).text(t))
    }

    #[test]
    fn an_authored_name_replaces_the_visible_text_of_a_button_or_link() {
        let author = [
            (web_attrs::EXPLICIT_NAME, "true"),
            (web_attrs::TAG, "button"),
        ];
        let btn = SubtreeSpec::with_children(
            aria_node(Role::Button, StateSet::ENABLED, &author, None)
                .name("Search the site")
                .flags(blk()),
            vec![txt("magnifier")],
        );
        let plain = SubtreeSpec::with_children(
            aria_node(
                Role::Button,
                StateSet::ENABLED,
                &[(web_attrs::TAG, "button")],
                None,
            )
            .name("Save draft")
            .flags(blk()),
            vec![txt("Save draft")],
        );
        let link = SubtreeSpec::with_children(
            aria_node(
                Role::Link,
                StateSet::ENABLED,
                &[(web_attrs::EXPLICIT_NAME, "true")],
                None,
            )
            .name("Read more about pandas")
            .flags(blk()),
            vec![txt("Read more")],
        );
        let d = doc_of(vec![btn, plain, link]);
        assert_eq!(line_of(&d, 1), "button Search the site");
        assert_eq!(line_of(&d, 2), "button Save draft");
        assert_eq!(line_of(&d, 3), "link Read more about pandas");
        let b = d.first_of(gorca_doc::web::Cat::BUTTON).unwrap();
        assert_eq!(say(&describe_node(&d, b)), "button Search the site");
    }

    #[test]
    fn switches_tabs_and_toggles_say_their_nvda_words() {
        let sw = |on: bool| {
            SubtreeSpec::with_children(
                aria_node(
                    Role::ToggleButton,
                    if on {
                        StateSet::ENABLED | StateSet::CHECKED
                    } else {
                        StateSet::ENABLED
                    },
                    &[(web_attrs::XML_ROLES, "switch"), (web_attrs::TAG, "button")],
                    None,
                )
                .flags(blk()),
                vec![txt("Notifications")],
            )
        };
        let tab = SubtreeSpec::with_children(
            aria_node(
                Role::Tab,
                StateSet::ENABLED | StateSet::SELECTED | StateSet::SELECTABLE,
                &[(web_attrs::XML_ROLES, "tab"), (web_attrs::TAG, "button")],
                None,
            )
            .flags(blk()),
            vec![txt("Nils Frahm")],
        );
        let toggle = SubtreeSpec::with_children(
            aria_node(
                Role::ToggleButton,
                StateSet::ENABLED,
                &[(web_attrs::TAG, "button")],
                None,
            )
            .flags(blk()),
            vec![txt("Italic")],
        );
        let d = doc_of(vec![sw(true), sw(false), tab, toggle]);
        assert_eq!(line_of(&d, 1), "switch on Notifications");
        assert_eq!(line_of(&d, 2), "switch off Notifications");
        assert_eq!(line_of(&d, 3), "tab selected Nils Frahm");
        assert_eq!(line_of(&d, 4), "toggle button not pressed Italic");
    }

    #[test]
    fn tree_items_say_level_and_expansion_and_links_say_current() {
        let ti = |t: &str, st: StateSet, lvl: &str| {
            SubtreeSpec::with_children(
                aria_node(
                    Role::TreeItem,
                    st,
                    &[
                        (web_attrs::XML_ROLES, "treeitem"),
                        (web_attrs::LEVEL, lvl),
                        (web_attrs::TAG, "li"),
                    ],
                    None,
                )
                .flags(blk()),
                vec![txt(t)],
            )
        };
        let cur = SubtreeSpec::with_children(
            aria_node(
                Role::Link,
                StateSet::ENABLED,
                &[(web_attrs::CURRENT, "page"), (web_attrs::TAG, "a")],
                None,
            )
            .flags(blk()),
            vec![txt("Gadgets")],
        );
        let d = doc_of(vec![
            ti("Documents", StateSet::ENABLED | StateSet::EXPANDED, "1"),
            ti("Pictures", StateSet::ENABLED | StateSet::COLLAPSED, "1"),
            cur,
        ]);
        assert_eq!(line_of(&d, 1), "tree view item expanded level 1 Documents");
        assert_eq!(line_of(&d, 2), "tree view item collapsed level 1 Pictures");
        assert_eq!(line_of(&d, 3), "link current page Gadgets");
    }

    #[test]
    fn a_hit_on_a_field_says_its_description_after_a_pause_and_its_error_text() {
        let field = SubtreeSpec::leaf(
            aria_node(
                Role::TextEntry,
                StateSet::ENABLED | StateSet::EDITABLE | StateSet::INVALID | StateSet::REQUIRED,
                &[
                    (web_attrs::TAG, "input"),
                    (
                        web_attrs::ERROR_TEXT,
                        "Enter an address like name@example.com",
                    ),
                ],
                Some("At least six characters"),
            )
            .name("Email address")
            .flags(blk())
            .text("\u{FFFC}"),
        );
        let d = doc_of(vec![field]);
        let e = d.first_of(gorca_doc::web::Cat::EDIT).unwrap();
        let items = describe_node(&d, e);
        assert_eq!(
            say(&items),
            "Email address edit required invalid entry blank Enter an address like name@example.com At least six characters"
        );
        // The description carries the pause; the rest is plain.
        let segs = segments(&items, test_opts());
        let last = segs.last().unwrap();
        assert_eq!(last.kind, SegmentKind::Description, "{:?}", last.text);
        // A line (arrow keys) says the error but not the description.
        assert_eq!(
            line_of(&d, 1),
            "edit required invalid entry blank Enter an address like name@example.com"
        );
    }

    #[test]
    fn aria_containers_are_announced_with_name_and_nvda_word() {
        let tl = SubtreeSpec::with_children(
            aria_node(
                Role::TabList,
                StateSet::ENABLED,
                &[(web_attrs::XML_ROLES, "tablist"), (web_attrs::TAG, "div")],
                None,
            )
            .name("Entertainment")
            .flags(blk()),
            vec![txt("First tab text")],
        );
        let toolbar = SubtreeSpec::with_children(
            aria_node(
                Role::Toolbar,
                StateSet::ENABLED,
                &[(web_attrs::XML_ROLES, "toolbar"), (web_attrs::TAG, "div")],
                None,
            )
            .name("Text formatting")
            .flags(blk()),
            vec![txt("Bold")],
        );
        let unnamed = SubtreeSpec::with_children(
            aria_node(
                Role::Panel,
                StateSet::ENABLED,
                &[(web_attrs::XML_ROLES, "group"), (web_attrs::TAG, "div")],
                None,
            )
            .flags(blk()),
            vec![txt("Plain group text")],
        );
        let d = doc_of(vec![tl, toolbar, unnamed]);
        assert_eq!(line_of(&d, 1), "Entertainment tab control First tab text");
        assert_eq!(line_of(&d, 2), "Text formatting tool bar Bold");
        assert_eq!(line_of(&d, 3), "Plain group text");
    }

    fn test_opts() -> VerbalizeOpts {
        use gorca_speech::{Caps, Lang, Punct};
        VerbalizeOpts {
            punct: Punct::Some,
            caps: Caps::Pitch,
            lang: Lang::En,
        }
    }

    #[test]
    fn web_text_is_spoken_with_the_symbol_names_of_the_language_and_level() {
        use gorca_speech::{Caps, Lang, Punct};
        let o = |punct, lang| VerbalizeOpts {
            punct,
            caps: Caps::Pitch,
            lang,
        };
        let items = [
            Item::Role("link".to_owned()),
            Item::Text("Цена: 5 ₽ (скидка)!".to_owned()),
        ];
        let said = |o| {
            segments(&items, o)
                .iter()
                .map(|s| s.text.to_string())
                .collect::<Vec<_>>()
                .join(" | ")
        };
        assert_eq!(
            said(o(Punct::Some, Lang::Ru)),
            "link | Цена: 5 рубль (скидка)!"
        );
        assert!(said(o(Punct::All, Lang::Ru)).contains("восклицательный знак!"));
        // A character read alone is named at any level, in the language.
        let c = [Item::Char("(".to_owned())];
        assert_eq!(
            segments(&c, o(Punct::None, Lang::Ru))[0].text.as_ref(),
            "левая круглая"
        );
    }

    mod probe_m8b {
        // Reviewer probes for M8b browse presentation. Insert as `mod probe_m8b { ... }` at the end of
        // `mod tests` in crates/gorca-core/src/browse/present.rs (needs `use super::*;`).
        use super::*;

        fn blk2() -> NodeFlags {
            NodeFlags::BLOCK | NodeFlags::BREAK_BEFORE
        }

        // P1: a check box named only by aria-label (no visible label). NVDA renders the name
        // (gecko_ia2.cpp nameIsContent for check/radio when the label is not visible).
        #[test]
        fn probe_checkbox_aria_label_in_line() {
            let cb = SubtreeSpec::leaf(
                aria_node(
                    Role::CheckBox,
                    StateSet::ENABLED | StateSet::FOCUSABLE,
                    &[
                        (web_attrs::EXPLICIT_NAME, "true"),
                        (web_attrs::TAG, "input"),
                    ],
                    None,
                )
                .name("Subscribe box")
                .flags(blk2())
                .text("\u{FFFC}"),
            );
            let d = doc_of(vec![cb]);
            eprintln!("P1 line: {}", line_of(&d, 1));
            assert!(
                line_of(&d, 1).contains("Subscribe box"),
                "{}",
                line_of(&d, 1)
            );
        }

        // P2: a heading with aria-label. NVDA: heading is nameIsContent, the label replaces content.
        #[test]
        fn probe_heading_aria_label() {
            let h = SubtreeSpec::with_children(
                aria_node(
                    Role::Heading,
                    StateSet::ENABLED,
                    &[
                        (web_attrs::EXPLICIT_NAME, "true"),
                        (web_attrs::TAG, "h2"),
                        (web_attrs::LEVEL, "2"),
                    ],
                    None,
                )
                .name("Short heading label")
                .flags(blk2()),
                vec![txt("Long visible heading content")],
            );
            let d = doc_of(vec![h]);
            eprintln!("P2 line: {}", line_of(&d, 1));
            assert!(
                line_of(&d, 1).contains("Short heading label"),
                "{}",
                line_of(&d, 1)
            );
        }

        // P4: a tree item with aria-label. NVDA's nameIsContent list has no outline item: content stays.
        #[test]
        fn probe_tree_item_aria_label() {
            let ti = SubtreeSpec::with_children(
                aria_node(
                    Role::TreeItem,
                    StateSet::ENABLED,
                    &[
                        (web_attrs::EXPLICIT_NAME, "true"),
                        (web_attrs::TAG, "li"),
                        (web_attrs::XML_ROLES, "treeitem"),
                    ],
                    None,
                )
                .name("Item label")
                .flags(blk2()),
                vec![txt("Item content")],
            );
            let d = doc_of(vec![ti]);
            eprintln!("P4 line: {}", line_of(&d, 1));
        }

        // P5: page text that starts with ". ". With description_pause=false the strip
        // removes it from content too.
        #[test]
        fn probe_pause_prefix_collides_with_content() {
            let p = SubtreeSpec::with_children(
                leaf(Role::Paragraph).flags(blk2()),
                vec![txt(". Dot leading paragraph")],
            );
            let d = doc_of(vec![p]);
            let s = d.line_start(1).unwrap();
            let (_, e) = d.line_range(s);
            let segs = segments(&describe(&d, s, e), test_opts());
            let mut u = gorca_model::Utterance {
                id: gorca_model::UttId(1),
                segments: segs,
                lang: None,
                epoch: gorca_model::Epoch(1),
                prio: gorca_model::Prio::Interrupt,
                may_refine: false,
                survives_focus_change: false,
                ttl_secs: 0,
                refines: None,
            };
            eprintln!(
                "P5 before: {:?}",
                u.segments
                    .iter()
                    .map(|s| s.text.to_string())
                    .collect::<Vec<_>>()
            );
            let before = u.segments.clone();
            crate::aria::strip_pause(&mut u);
            eprintln!(
                "P5 after strip: {:?}",
                u.segments
                    .iter()
                    .map(|s| s.text.to_string())
                    .collect::<Vec<_>>()
            );
            assert_eq!(u.segments, before, "content altered");
        }
    }
}
