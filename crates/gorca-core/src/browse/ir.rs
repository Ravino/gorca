// SPDX-License-Identifier: GPL-3.0-or-later
//! Line and node presentation through `gorca-present` (M22b2): the successor of the semantic half
//! of `present.rs`. The walk over the flat text is the same (which nodes a range covers, where
//! their text starts, what the cursor already holds); every word comes from the node's fused
//! entry through [`gorca_present::browse`], is collected as `PresIr` fields, deduplicated on the
//! fields and rendered once.

// The live commands still speak through `present.rs`; this module is wired, and `present.rs` and
// `aria.rs` deleted, by the cut-over commit after the gate (`cargo xtask cutover browse`). Until
// then the replay (`gate_tests.rs`) is its only caller.
#![cfg_attr(not(test), allow(dead_code))]

use std::sync::Arc;

use gorca_doc::web::{NodeFlags, NodeView, WebDoc};
use gorca_i18n::MsgId as M;
use gorca_model::sem::{Select, SemEntry, SemRole};
use gorca_model::{DocNodeId, NodeKey};
use gorca_present::browse as pb;
use gorca_present::{
    Content, Field, FieldKind, PresIr, Reason, RenderOpts, TokenText, render_kinds,
};

use super::present::{Item, LINE_CAP, QUICK_NAV_CAP};

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

/// A container's name is dropped when its first line says it already ("Fruit table table").
fn dedupe_names(fields: &mut Said) {
    let texts: Vec<String> = fields
        .iter()
        .filter(|f| f.kind == FieldKind::TEXT)
        .filter_map(|f| text_of(f).map(str::to_lowercase))
        .collect();
    fields.retain(|f| {
        if f.kind != FieldKind::NAME {
            return true;
        }
        let Content::Text(n) = &f.content else {
            return true;
        };
        let n = n.trim().to_lowercase();
        n.is_empty() || !texts.iter().any(|t| t.contains(&n))
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

/// [`super::present::describe_from`] through `gorca-present`.
pub fn describe_from(doc: &WebDoc, a: u32, b: u32, from: Option<u32>) -> Vec<Item> {
    items_of(
        describe_from_fields(doc, a, b, from),
        Reason::BrowseLine {
            entered: from.is_none(),
        },
    )
}

/// [`super::present::describe`] through `gorca-present`.
pub fn describe(doc: &WebDoc, a: u32, b: u32) -> Vec<Item> {
    describe_from(doc, a, b, None)
}

/// [`super::present::describe_node_from`] through `gorca-present`.
pub fn describe_node_from(doc: &WebDoc, id: DocNodeId, from: Option<u32>) -> Vec<Item> {
    items_of(describe_node_from_fields(doc, id, from), Reason::QuickNav)
}

/// [`super::present::describe_node`] through `gorca-present`.
pub fn describe_node(doc: &WebDoc, id: DocNodeId) -> Vec<Item> {
    describe_node_from(doc, id, None)
}
