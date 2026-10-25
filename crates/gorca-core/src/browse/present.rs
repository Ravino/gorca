// SPDX-License-Identifier: GPL-3.0-or-later
//! Line and node presentation (browse-mode design 8.2, LP-1 to LP-4): what the user hears for a
//! range of the flat text. Everything is built from one published `WebDoc` snapshot, so it costs
//! no IPC and finishes in microseconds (LP-3).
//!
//! A range is described by walking the nodes it covers in document order: text runs give text;
//! a node *entered* on the range whose role is announced inline adds a prefix ("link", "heading
//! level 2", "button", "edit"); a control that has no text of its own (FFFC marker, FT-6) is
//! spoken as its role, state and value; containers (lists, tables, landmarks) are the field diff
//! of M7b. The walk is geometry; every word comes from the node's fused entry through
//! `gorca_present::browse` (M22b2), is collected as `PresIr` fields, deduplicated on the fields
//! and rendered once.

use std::sync::Arc;

use gorca_doc::web::{NodeFlags, NodeView, WebDoc};
use gorca_i18n::MsgId as M;
use gorca_model::sem::{Select, SemEntry, SemRole};
use gorca_model::{DocNodeId, NodeKey, Segment, SegmentKind};
use gorca_present::browse as pb;
use gorca_present::{
    Content, Field, FieldKind, PresIr, Reason, RenderOpts, TokenText, render_kinds,
};
use gorca_speech::{How, VerbalizeOpts, Verbalizer};
use smallvec::SmallVec;

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

/// Fields of one description, in speaking order.
type Said = Vec<Field>;

fn sem_of<'a>(v: &NodeView<'a>) -> Option<&'a SemEntry> {
    v.sem_ref()
}

/// The options of a combo box are its value, not items of their own: nothing inside one is
/// announced.
fn within_combobox(doc: &WebDoc, id: DocNodeId) -> bool {
    let mut cur = doc.parent(id);
    for _ in 0..8 {
        let Some(p) = cur else { return false };
        if doc
            .node(p)
            .and_then(|v| sem_of(&v).map(|e| e.sem.role == SemRole::Combobox))
            .unwrap_or(false)
        {
            return true;
        }
        cur = doc.parent(p);
    }
    false
}

/// A text box whose content is spin buttons (a date or time input) is a wrapper: its parts are
/// announced, the box is not.
fn composite_input(doc: &WebDoc, id: DocNodeId, e: &SemEntry) -> bool {
    e.sem.role == SemRole::TextBox
        && doc.subtree(id).skip(1).take(8).any(|c| {
            doc.node(c)
                .and_then(|v| sem_of(&v).map(|x| x.sem.role == SemRole::SpinButton))
                .unwrap_or(false)
        })
}

fn announced_node(doc: &WebDoc, id: DocNodeId, v: &NodeView<'_>) -> bool {
    sem_of(v).is_some_and(|e| pb::announced(e) && !composite_input(doc, id, e))
        && !within_combobox(doc, id)
}

fn origin() -> gorca_present::Origin {
    gorca_present::Origin {
        row: pb::ROW_BROWSE,
        group: gorca_present::Origin::NO_GROUP,
        via: 0,
    }
}

fn text_run(t: &str) -> Field {
    Field::new(FieldKind::TEXT, Content::Text(Arc::from(t)), origin())
}

fn name_field(t: &str) -> Field {
    Field::new(FieldKind::NAME, Content::Text(Arc::from(t)), origin())
}

/// Offset where the node's content starts: after its own line break and the blank breaks of
/// the blocks that open inside it.
fn eff_start(doc: &WebDoc, id: DocNodeId) -> Option<u32> {
    super::cursor::content_start(doc, id)
}

/// Is the flat offset `at` inside the node's text?
fn node_holds(doc: &WebDoc, id: DocNodeId, at: u32) -> bool {
    doc.subtree_text_range(id)
        .is_some_and(|(s, e)| s <= at && at < e)
}

/// Rows and columns of a table node, from the tree (rows are `Row` descendants through at most
/// two group levels; columns are the widest row).
pub fn table_dims(doc: &WebDoc, id: DocNodeId) -> (usize, usize) {
    let role_of = |n: DocNodeId| doc.node(n).and_then(|v| sem_of(&v).map(|e| e.sem.role));
    let mut rows = 0usize;
    let mut cols = 0usize;
    let mut stack: Vec<(DocNodeId, u8)> = doc.children(id).map(|c| (c, 0)).collect();
    while let Some((n, depth)) = stack.pop() {
        let Some(v) = doc.node(n) else { continue };
        if role_of(n) == Some(SemRole::Row) {
            rows += 1;
            let w = doc
                .children(n)
                .filter(|c| {
                    doc.node(*c).is_some_and(|cv| {
                        matches!(
                            role_of(*c),
                            Some(
                                SemRole::Cell
                                    | SemRole::GridCell
                                    | SemRole::ColumnHeader
                                    | SemRole::RowHeader
                            )
                        ) && !cv.flags.contains(NodeFlags::HIDDEN)
                    })
                })
                .count();
            cols = cols.max(w);
        } else if depth < 2 && !v.flags.contains(NodeFlags::HIDDEN) {
            stack.extend(doc.children(n).map(|c| (c, depth + 1)));
        }
    }
    (rows, cols)
}

fn facts_of(doc: &WebDoc, id: DocNodeId, e: &SemEntry) -> pb::ContainerFacts {
    let role = e.sem.role;
    if matches!(role, SemRole::Table | SemRole::Grid | SemRole::TreeGrid) {
        let (rows, cols) = table_dims(doc, id);
        return pb::ContainerFacts {
            rows,
            cols,
            ..pb::ContainerFacts::default()
        };
    }
    if matches!(role, SemRole::List | SemRole::Listbox) {
        return pb::ContainerFacts {
            list_items: doc
                .children(id)
                .filter(|c| {
                    doc.node(*c)
                        .and_then(|x| {
                            sem_of(&x)
                                .map(|e| matches!(e.sem.role, SemRole::ListItem | SemRole::Option))
                        })
                        .unwrap_or(false)
                })
                .count(),
            ..pb::ContainerFacts::default()
        };
    }
    pb::ContainerFacts::default()
}

fn container_fields(doc: &WebDoc, id: DocNodeId, v: &NodeView<'_>) -> Option<Said> {
    if within_combobox(doc, id) {
        return None;
    }
    let e = sem_of(v)?;
    pb::container_fields(e, facts_of(doc, id, e)).map(smallvec::SmallVec::into_vec)
}

/// Is `name` already part of what is said?
fn name_spoken(fields: &[Field], name: &str) -> bool {
    let name = name.trim().to_lowercase();
    fields.iter().any(|f| {
        matches!(f.kind, FieldKind::TEXT | FieldKind::NAME)
            && matches!(&f.content, Content::Text(t) if t.to_lowercase().contains(&name))
    })
}

/// "out of list", "out of table", "out of block quote" for the containers that hold `from` but
/// not `to` (innermost first).
fn leaving_fields(doc: &WebDoc, from: u32, to: u32) -> Said {
    let mut out = Vec::new();
    let Some((mut cur, _)) = doc.at_offset(from.min(doc.text_len().saturating_sub(1))) else {
        return out;
    };
    for _ in 0..64 {
        let Some(v) = doc.node(cur) else { break };
        if let Some(w) = sem_of(&v).and_then(pb::leaving_word)
            && !v.flags.contains(NodeFlags::HIDDEN)
            && node_holds(doc, cur, from)
            && !node_holds(doc, cur, to)
        {
            out.push(pb::role_word_field(w));
        }
        match doc.parent(cur) {
            Some(p) => cur = p,
            None => break,
        }
    }
    out
}

/// A node that was just announced and whose name was written by the author: say the name and
/// return the offset where its content ends (the content is not spoken). 0 for no override.
fn authored_name_fields(doc: &WebDoc, id: DocNodeId, v: &NodeView<'_>, out: &mut Said) -> u32 {
    let Some(name) = v.name.map(str::trim).filter(|n| !n.is_empty()) else {
        return 0;
    };
    if !sem_of(v).is_some_and(pb::takes_authored_name) {
        return 0;
    }
    out.push(name_field(name));
    doc.subtree_text_range(id).map_or(0, |(_, e)| e)
}

/// The literal text of a field (a name or a text run), `None` for a catalog word.
fn text_of(f: &Field) -> Option<&str> {
    if let Content::Text(t) = &f.content {
        Some(t)
    } else {
        None
    }
}

/// A container's name is dropped when its first line says it already ("Fruit table table"). The
/// error message of an invalid control is dropped when the description already says it: the text
/// of the error element lies in the same range, or the name or description holds the same text.
fn dedupe_names(fields: &mut Said) {
    let texts: Vec<String> = fields
        .iter()
        .filter(|f| f.kind == FieldKind::TEXT)
        .filter_map(|f| text_of(f).map(str::to_lowercase))
        .collect();
    let said: Vec<String> = fields
        .iter()
        .filter(|f| {
            f.kind == FieldKind::TEXT
                || f.kind == FieldKind::NAME
                || f.kind == FieldKind::DESCRIPTION
        })
        .filter_map(|f| text_of(f).map(|t| t.trim().to_lowercase()))
        .collect();
    fields.retain(|f| {
        let Content::Text(n) = &f.content else {
            return true;
        };
        match f.kind {
            FieldKind::NAME => {
                let n = n.trim().to_lowercase();
                n.is_empty() || !texts.iter().any(|t| t.contains(&n))
            }
            FieldKind::ERROR_TEXT => {
                let n = n.trim().to_lowercase();
                n.is_empty() || !said.iter().any(|t| t.contains(&n))
            }
            _ => true,
        }
    });
}

/// Describe the flat-text range `[a, b)` as a move from the cursor at `from` (BM-3 field diff): a
/// container or control that already holds `from` was entered earlier and is not announced again.
fn describe_from_fields(doc: &WebDoc, a: u32, b: u32, from: Option<u32>) -> Said {
    let mut out: Said = Vec::new();
    let total = doc.text_len();
    let b = b.min(total).min(a.saturating_add(LINE_CAP));
    if a >= total {
        return out;
    }
    let Some((n0, local0)) = doc.at_offset(a) else {
        return out;
    };
    if let Some(f) = from {
        out.extend(leaving_fields(doc, f, a));
    }
    let mut skip_to: u32 = 0;
    let mut chain: Vec<DocNodeId> = Vec::new();
    let mut cur = doc.parent(n0);
    while let Some(p) = cur {
        chain.push(p);
        cur = doc.parent(p);
    }
    for id in chain.into_iter().rev() {
        let Some(v) = doc.node(id) else { continue };
        if v.flags.contains(NodeFlags::HIDDEN)
            || eff_start(doc, id) != Some(a)
            || from.is_some_and(|f| node_holds(doc, id, f))
        {
            continue;
        }
        if announced_node(doc, id, &v) {
            if let Some(e) = sem_of(&v) {
                out.extend(pb::enter_fields(e));
            }
            skip_to = skip_to.max(authored_name_fields(doc, id, &v, &mut out));
        } else if let Some(items) = container_fields(doc, id, &v) {
            out.extend(items);
        }
    }
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
        if entered && announced_node(doc, id, &v) && !hidden && !is_marker {
            if let Some(e) = sem_of(&v) {
                out.extend(pb::enter_fields(e));
            }
            skip_to = skip_to.max(authored_name_fields(doc, id, &v, &mut out));
        }
        let lo = a.max(off).max(skip_to);
        let hi = b.min(off + own);
        if lo < hi {
            if is_marker {
                if eff >= lo
                    && eff < hi
                    && let Some(e) = sem_of(&v)
                {
                    if let Some(u) = pb::unlabeled_graphic(e) {
                        // The word leads the whole description.
                        out.insert(0, u);
                    }
                    out.extend(pb::marker_fields(e));
                }
            } else {
                let t = doc.text_in(lo, hi);
                let t = t.trim_matches('\n');
                if !t.is_empty() {
                    out.push(text_run(t));
                }
            }
        }
        off += own;
    }
    dedupe_names(&mut out);
    out
}

/// The text of the selected option of a combo box or list box.
fn selected_value(doc: &WebDoc, id: DocNodeId) -> Option<String> {
    for n in doc.subtree(id).skip(1) {
        let v = doc.node(n)?;
        if sem_of(&v).is_some_and(|e| {
            e.sem.select == Select::On
                && matches!(
                    e.sem.role,
                    SemRole::MenuItem | SemRole::ListItem | SemRole::Option
                )
        }) {
            let t = v.name.filter(|t| !t.is_empty()).or(v.text)?;
            return Some(t.trim().to_owned());
        }
    }
    None
}

fn describe_node_from_fields(doc: &WebDoc, id: DocNodeId, from: Option<u32>) -> Said {
    let Some(v) = doc.node(id) else {
        return Vec::new();
    };
    let Some(a) = eff_start(doc, id) else {
        return Vec::new();
    };
    let Some((_, end)) = doc.subtree_text_range(id) else {
        return Vec::new();
    };
    let entry = sem_of(&v);
    let container = container_fields(doc, id, &v).is_some();
    let mut b = end.min(a + QUICK_NAV_CAP);
    if container {
        let (_, line_end) = doc.line_range(a);
        b = b.min(line_end.max(a));
    }
    let mut items = if a < b {
        describe_from_fields(doc, a, b, from)
    } else {
        Vec::new()
    };
    if container
        && !from.is_some_and(|f| node_holds(doc, id, f))
        && !items.iter().any(|i| i.kind == FieldKind::ROLE)
        && let Some(mut head) = container_fields(doc, id, &v)
    {
        head.append(&mut items);
        items = head;
    }
    // A secret field is named and announced, never read: whatever text or value the mirror holds
    // under it (the source's bullets included) stays out of the description.
    if super::selmirror::node_is_secret(&v) {
        items.retain(|i| i.kind != FieldKind::TEXT && i.kind != FieldKind::VALUE);
    }
    let announced_here = items.iter().any(|i| i.kind == FieldKind::ROLE);
    if !announced_here
        && announced_node(doc, id, &v)
        && let Some(e) = entry
    {
        let mut pre: Said = pb::enter_fields(e).into_vec();
        pre.append(&mut items);
        items = pre;
    }
    if !announced_here && entry.is_some_and(|e| e.sem.role == SemRole::Separator) {
        items.insert(0, pb::role_word_field(M::RoleSeparator));
    }
    if let Some(e) = entry
        && pb::is_named_control(e)
        && let Some(n) = v.name.filter(|n| !n.is_empty())
        && !name_spoken(&items, n)
    {
        items.insert(0, name_field(n));
    }
    if entry.is_some_and(|e| e.sem.role == SemRole::Combobox)
        && let Some(val) = selected_value(doc, id)
        && !val.is_empty()
    {
        items.push(text_run(&val));
    }
    let mut pre: Said = Vec::new();
    if v.flags.contains(NodeFlags::FRAME) {
        pre.push(pb::role_word_field(M::WebFrame));
        let holder = doc
            .parent(id)
            .and_then(|p| doc.node(p))
            .filter(|p| {
                sem_of(p).is_none_or(|e| {
                    matches!(
                        e.sem.role,
                        SemRole::Unknown | SemRole::Generic | SemRole::Iframe
                    )
                })
            })
            .and_then(|p| p.name)
            .filter(|n| !n.is_empty());
        if let Some(n) = holder.or(v.name.filter(|n| !n.is_empty())) {
            pre.push(name_field(n));
        }
    } else if v.flags.contains(NodeFlags::ARTICLE) {
        pre.push(pb::role_word_field(M::WebArticle));
    }
    if !pre.is_empty() {
        pre.append(&mut items);
        items = pre;
    }
    if let Some(e) = entry
        && let Some(d) = pb::speakable_description(e)
        && let Content::Text(t) = &d.content
        && !name_spoken(&items, t)
    {
        items.push(d);
    }
    dedupe_names(&mut items);
    items
}

fn dummy_key() -> NodeKey {
    NodeKey {
        app: gorca_model::AppId {
            connector: gorca_model::ConnectorId(0),
            serial: 0,
        },
        local: gorca_model::LocalId(0),
    }
}

/// Render the fields of one description into the items the commands speak.
pub fn items_of(fields: Said, reason: Reason) -> Vec<Item> {
    if fields.is_empty() {
        return Vec::new();
    }
    let ir = PresIr::new(reason, dummy_key(), 0, fields.into());
    let opts = RenderOpts {
        description_pause: true,
        lang: gorca_i18n::language(),
        ..RenderOpts::default()
    };
    render_kinds(&ir, opts)
        .into_iter()
        .map(|(kind, tok)| {
            let text = tok.text.as_str().to_owned();
            match kind {
                FieldKind::NAME => Item::Name(text),
                FieldKind::ROLE => Item::Role(text),
                // A state is always a catalog word.
                FieldKind::STATE => Item::State(match tok.text {
                    TokenText::Static(w) => w,
                    TokenText::Shared(_) | TokenText::Owned(_) => "",
                }),
                FieldKind::LEVEL | FieldKind::ERROR_TEXT => Item::Info(text),
                FieldKind::DESCRIPTION => Item::Desc(text),
                _ => Item::Text(text),
            }
        })
        .collect()
}

/// `browse.description_pause = false`: the description follows the other words like any segment.
pub fn strip_pause(u: &mut gorca_model::Utterance) {
    for seg in &mut u.segments {
        if seg.kind == gorca_model::SegmentKind::Description {
            seg.kind = gorca_model::SegmentKind::Text;
        }
    }
}

/// The items of the range `[a, b)` as a move from the cursor at `from` (BM-3 field diff).
pub fn describe_from(doc: &WebDoc, a: u32, b: u32, from: Option<u32>) -> Vec<Item> {
    items_of(
        describe_from_fields(doc, a, b, from),
        Reason::BrowseLine {
            entered: from.is_none(),
        },
    )
}

/// Describe the flat-text range `[a, b)`.
pub fn describe(doc: &WebDoc, a: u32, b: u32) -> Vec<Item> {
    describe_from(doc, a, b, None)
}

/// [`describe_node`] for a move from the cursor at `from`: containers the cursor is already in
/// are not announced again.
pub fn describe_node_from(doc: &WebDoc, id: DocNodeId, from: Option<u32>) -> Vec<Item> {
    items_of(describe_node_from_fields(doc, id, from), Reason::QuickNav)
}

/// The items for a quick-nav target: its own text range with its role announced first (LP-4).
pub fn describe_node(doc: &WebDoc, id: DocNodeId) -> Vec<Item> {
    describe_node_from(doc, id, None)
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
            // An icon font's glyph (private use) draws a symbol and speaks as noise.
            if t.is_empty() || t.chars().all(gorca_model::text::is_icon_glyph) {
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
                && o.caps.marks()
                && t.chars().count() == 1
                && t.chars().all(char::is_uppercase);
            (!text.is_empty()).then(|| {
                Segment::new(
                    Arc::from(text),
                    match i.kind() {
                        _ if capital => SegmentKind::Capital,
                        SegmentKind::Text => SegmentKind::Said,
                        other @ (SegmentKind::Role
                        | SegmentKind::State
                        | SegmentKind::Value
                        | SegmentKind::Description
                        | SegmentKind::Said
                        | SegmentKind::Capital) => other,
                    },
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_doc::web::{DocOp, NodeSpec, SubtreeSpec};
    use gorca_doc_source_api::web_attrs;

    use super::*;
    use crate::doc_actor::legacy_sem_tests::{fuse_spec, hinted};
    use gorca_model::{Role, StateSet};

    fn leaf(role: Role) -> NodeSpec {
        NodeSpec::new(role)
    }

    fn with_states(n: NodeSpec, st: StateSet) -> NodeSpec {
        hinted(n, st, &[], None)
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
                subtree: Arc::new(fuse_spec(root)),
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
            hinted(
                named(Role::Landmark, "Main content"),
                StateSet::empty(),
                &[(web_attrs::TAG, "main"), (web_attrs::XML_ROLES, "main")],
                None,
            )
            .flags(blk),
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
                subtree: Arc::new(fuse_spec(root)),
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
        crate::doc_actor::legacy_sem_tests::hinted_as(Some(role), leaf(role), st, attrs, desc)
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
                subtree: Arc::new(fuse_spec(root)),
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
    fn an_error_message_in_the_range_is_spoken_once() {
        let msg = "Enter an address like name@example.com";
        let field = SubtreeSpec::leaf(
            aria_node(
                Role::TextEntry,
                StateSet::ENABLED | StateSet::EDITABLE | StateSet::INVALID,
                &[(web_attrs::TAG, "input"), (web_attrs::ERROR_TEXT, msg)],
                None,
            )
            .name("Email address")
            .flags(blk())
            .text("\u{FFFC}"),
        );
        // The error element sits in the same range as the field: its own text says the message.
        let d = doc_of(vec![field, txt(msg)]);
        let s = d.line_start(1).unwrap();
        let (_, e) = d.line_range(s);
        let (_, end) = d.line_range(d.line_start(d.line_count() - 1).unwrap());
        let said = say(&describe(&d, s, end.max(e)));
        assert_eq!(said.matches("Enter an address").count(), 1, "{said}");
        // Alone, the field still says the message (from the fused entry).
        assert!(
            line_of(&d, 1).contains("Enter an address"),
            "{}",
            line_of(&d, 1)
        );
        // A name or description that holds the same text says it once as well.
        let mut fields = vec![
            name_field("Enter an address"),
            Field::new(
                FieldKind::ERROR_TEXT,
                Content::Text(Arc::from("Enter an address")),
                origin(),
            ),
        ];
        dedupe_names(&mut fields);
        assert_eq!(fields.len(), 1);
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
            caps: Caps::PITCH,
            lang: Lang::En,
        }
    }

    #[test]
    fn web_text_is_spoken_with_the_symbol_names_of_the_language_and_level() {
        use gorca_speech::{Caps, Lang, Punct};
        let o = |punct, lang| VerbalizeOpts {
            punct,
            caps: Caps::PITCH,
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
            strip_pause(&mut u);
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
