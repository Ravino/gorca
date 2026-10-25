#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! Test support: the fused entry a hand-built source node would have had. The tests of the browse
//! mode build documents from `SrcNode`s with a legacy role, a state set and a few attributes;
//! production nodes carry the source's record (`SrcNode::rec`). This turns the former into the
//! latter with the real normaliser (`gorca_doc_source_atspi::norm::record`), so a test document
//! has the entries the presenter reads, made by the code that makes them live.

use std::collections::HashMap;
use std::sync::Arc;

use gorca_doc_source_api::web_attrs;
use gorca_doc_source_atspi::norm;
use gorca_doc_source_atspi::roles::raw;
use gorca_model::{DocDelta, Role, SemEntry, SrcNode, SrcNodeId, StateSet};

use super::fuse::SemLookup;

/// An AT-SPI role that maps back to `role` (and means what the tests mean by it).
fn raw_role(role: Role) -> u32 {
    match role {
        Role::Unknown => 0,
        Role::Application => raw::APPLICATION,
        Role::Window => raw::WINDOW,
        Role::Dialog => raw::DIALOG,
        Role::Panel => raw::SECTION,
        Role::Document => raw::DOCUMENT_FRAME,
        Role::DocumentWeb => raw::DOCUMENT_WEB,
        Role::Heading => raw::HEADING,
        Role::Paragraph => raw::PARAGRAPH,
        Role::Link => raw::LINK,
        Role::Image => raw::IMAGE,
        Role::List => raw::LIST,
        Role::ListItem => raw::LIST_ITEM,
        Role::Table => raw::TABLE,
        Role::Row => raw::TABLE_ROW,
        Role::Cell => raw::TABLE_CELL,
        Role::ColumnHeader => raw::TABLE_COLUMN_HEADER,
        Role::RowHeader => raw::TABLE_ROW_HEADER,
        Role::Button => raw::PUSH_BUTTON,
        Role::ToggleButton => raw::TOGGLE_BUTTON,
        Role::CheckBox => raw::CHECK_BOX,
        Role::RadioButton => raw::RADIO_BUTTON,
        Role::ComboBox => raw::COMBO_BOX,
        Role::TextEntry => raw::ENTRY,
        Role::PasswordEntry => raw::PASSWORD_TEXT,
        Role::SpinButton => raw::SPIN_BUTTON,
        Role::Slider => raw::SLIDER,
        Role::ProgressBar => raw::PROGRESS_BAR,
        Role::Menu => raw::MENU,
        Role::MenuBar => raw::MENU_BAR,
        Role::MenuItem => raw::MENU_ITEM,
        Role::Popover => raw::POPUP_MENU,
        Role::Tab => raw::PAGE_TAB,
        Role::TabList => raw::PAGE_TAB_LIST,
        Role::Tree => raw::TREE,
        Role::TreeItem => raw::TREE_ITEM,
        Role::Toolbar => raw::TOOL_BAR,
        Role::StatusBar => raw::STATUS_BAR,
        Role::Separator => raw::SEPARATOR,
        Role::Label => raw::LABEL,
        Role::StaticText => raw::STATIC,
        Role::Terminal => raw::TERMINAL,
        Role::Landmark => raw::LANDMARK,
        Role::Alert => raw::ALERT,
        Role::Switch => raw::SWITCH,
    }
}

/// The AT-SPI state bits of a legacy state set.
fn bits(s: StateSet) -> u64 {
    use gorca_doc_source_atspi::roles::state_bit as bit;
    let mut b = 0u64;
    let mut set = |n: u8| b |= 1u64 << n;
    for (flag, bits) in [
        (StateSet::FOCUSED, &[bit::FOCUSED][..]),
        (StateSet::FOCUSABLE, &[bit::FOCUSABLE]),
        (StateSet::SELECTED, &[bit::SELECTED, bit::SELECTABLE]),
        (StateSet::CHECKED, &[bit::CHECKED, bit::CHECKABLE]),
        (StateSet::PRESSED, &[bit::PRESSED]),
        (StateSet::EXPANDED, &[bit::EXPANDABLE, bit::EXPANDED]),
        (StateSet::COLLAPSED, &[bit::EXPANDABLE]),
        (StateSet::EDITABLE, &[bit::EDITABLE]),
        (StateSet::READ_ONLY, &[bit::READ_ONLY]),
        (StateSet::SHOWING, &[bit::SHOWING]),
        (StateSet::VISIBLE, &[bit::VISIBLE]),
        (StateSet::BUSY, &[bit::BUSY]),
        (StateSet::MODAL, &[bit::MODAL]),
        (StateSet::MULTILINE, &[bit::MULTI_LINE]),
        (StateSet::REQUIRED, &[bit::REQUIRED]),
        (StateSet::INVALID, &[bit::INVALID_ENTRY]),
        (StateSet::HAS_POPUP, &[bit::HAS_POPUP]),
        (StateSet::ACTIVE, &[bit::ACTIVE]),
        (StateSet::ENABLED, &[bit::ENABLED, bit::SENSITIVE]),
        (StateSet::SELECTABLE, &[bit::SELECTABLE]),
        (StateSet::VISITED, &[bit::VISITED]),
        (StateSet::MIXED, &[bit::INDETERMINATE, bit::CHECKABLE]),
    ] {
        if s.contains(flag) {
            for n in bits {
                set(*n);
            }
        }
    }
    b
}

/// The entry of one source node: its own record when it has one, else the record the normaliser
/// makes of its legacy role, states, name and attributes.
pub(crate) fn entry_of(n: &SrcNode) -> Arc<SemEntry> {
    if let Some(r) = &n.rec {
        return Arc::new(r.provisional_entry());
    }
    let pairs: Vec<(&str, &str)> = n
        .attrs
        .iter()
        .filter_map(|(k, v)| web_attrs::name_of(*k).map(|name| (name, &**v)))
        .collect();
    let rec = norm::record(
        raw_role(n.role),
        Some(bits(n.states)),
        n.name.as_deref(),
        &pairs,
    );
    Arc::new(rec.provisional_entry())
}

/// The entries of the nodes of a batch, by source id.
#[derive(Debug, Default)]
pub(crate) struct LegacySem(HashMap<SrcNodeId, Arc<SemEntry>>);

fn text_below_node(
    id: SrcNodeId,
    nodes: &HashMap<SrcNodeId, &SrcNode>,
    kids: &HashMap<SrcNodeId, Vec<SrcNodeId>>,
    out: &mut Vec<String>,
) {
    if let Some(n) = nodes.get(&id)
        && let Some(t) = n.text.as_deref()
        && n.role == Role::StaticText
    {
        out.push(t.trim().to_owned());
    }
    for c in kids.get(&id).into_iter().flatten() {
        text_below_node(*c, nodes, kids, out);
    }
}

impl LegacySem {
    /// The entries of the upserted nodes. A node whose role takes its name from its content and
    /// that has none gets the text below it (what the fusion's content stratum derives live).
    pub(crate) fn of(batch: &[DocDelta]) -> LegacySem {
        let mut nodes: HashMap<SrcNodeId, &SrcNode> = HashMap::new();
        let mut kids: HashMap<SrcNodeId, Vec<SrcNodeId>> = HashMap::new();
        for d in batch {
            match d {
                DocDelta::Upsert(ns) => nodes.extend(ns.iter().map(|n| (n.id, n))),
                DocDelta::Children { parent, children } => {
                    kids.insert(*parent, children.iter().copied().collect());
                }
                DocDelta::Reset { .. }
                | DocDelta::Text { .. }
                | DocDelta::Removed(_)
                | DocDelta::Settled { .. }
                | DocDelta::Hint(_) => {}
            }
        }
        let takes_content = |r: Role| {
            matches!(
                r,
                Role::Link
                    | Role::Heading
                    | Role::Button
                    | Role::ToggleButton
                    | Role::Tab
                    | Role::MenuItem
                    | Role::TreeItem
                    | Role::ListItem
                    | Role::Cell
                    | Role::ColumnHeader
                    | Role::RowHeader
            )
        };
        let mut m = HashMap::new();
        for (id, n) in &nodes {
            let mut e = entry_of(n);
            if e.name.is_none() && takes_content(n.role) {
                let mut parts = Vec::new();
                text_below_node(*id, &nodes, &kids, &mut parts);
                let name = parts
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !name.is_empty() {
                    let mut x = (*e).clone();
                    x.name = Some(Arc::from(name));
                    e = Arc::new(x);
                }
            }
            m.insert(*id, e);
        }
        LegacySem(m)
    }
}

impl SemLookup for LegacySem {
    fn sem(&self, src: SrcNodeId) -> Option<Arc<SemEntry>> {
        self.0.get(&src).cloned()
    }
}

/// What a test says about a hand-built node besides its role and name: the legacy state set, the
/// attributes of the page and the description. They are not part of the node's data any more
/// (the data holds the fused entry only), so they travel next to the spec, keyed by the spec's
/// signature, until [`fuse_spec`] turns them into the entry.
#[derive(Clone, Debug, Default)]
struct Hint {
    /// The legacy role the test meant, where the model role loses it (`ToggleButton`).
    role: Option<Role>,
    states: StateSet,
    attrs: Vec<(gorca_model::AttrKey, Arc<str>)>,
    description: Option<Arc<str>>,
}

thread_local! {
    static HINTS: std::cell::RefCell<HashMap<u64, Hint>> = std::cell::RefCell::new(HashMap::new());
}

/// `n` with the states, attributes and description a test gives it (see [`Hint`]).
pub(crate) fn hinted(
    n: gorca_doc::web::NodeSpec,
    states: StateSet,
    attrs: &[(gorca_model::AttrKey, &str)],
    description: Option<&str>,
) -> gorca_doc::web::NodeSpec {
    hinted_as(None, n, states, attrs, description)
}

/// [`hinted`] for a node whose legacy `role` the model role does not keep.
pub(crate) fn hinted_as(
    role: Option<Role>,
    mut n: gorca_doc::web::NodeSpec,
    states: StateSet,
    attrs: &[(gorca_model::AttrKey, &str)],
    description: Option<&str>,
) -> gorca_doc::web::NodeSpec {
    HINTS.with(|h| {
        let mut h = h.borrow_mut();
        let sig = 0xF000_0000_0000_0000 + h.len() as u64 + 1;
        n.sig = sig;
        h.insert(
            sig,
            Hint {
                role,
                states,
                attrs: attrs.iter().map(|(k, v)| (*k, Arc::from(*v))).collect(),
                description: description.map(Arc::from),
            },
        );
    });
    n
}

/// The tree of a document spec with every node carrying the entry its legacy role, states, name
/// and attributes give it (and a content name where the role takes one and the node has none).
pub(crate) fn fuse_spec(s: gorca_doc::web::SubtreeSpec) -> gorca_doc::web::SubtreeSpec {
    fn text_below(s: &gorca_doc::web::SubtreeSpec, out: &mut Vec<String>) {
        if s.node.role.is_inline_text()
            && let Some(t) = s.node.text.as_deref()
        {
            out.push(t.trim().to_owned());
        }
        for c in &s.children {
            text_below(c, out);
        }
    }
    fn go(mut s: gorca_doc::web::SubtreeSpec) -> gorca_doc::web::SubtreeSpec {
        let hint = HINTS
            .with(|h| h.borrow().get(&s.node.sig).cloned())
            .unwrap_or_default();
        if s.node.sig >= 0xF000_0000_0000_0000 {
            s.node.sig = 0;
        }
        let role = hint
            .role
            .unwrap_or_else(|| gorca_model::testing::legacy_role(s.node.role));
        let mut node = SrcNode {
            id: SrcNodeId(0),
            role,
            states: hint.states,
            name: s.node.name.clone(),
            text: None,
            attrs: hint.attrs.iter().cloned().collect(),
            local: None,
            rec: None,
        };
        if let Some(d) = hint.description.as_ref() {
            node.attrs.push((web_attrs::DESCRIPTION, Arc::clone(d)));
        }
        if role == Role::Heading && s.node.level > 0 {
            node.attrs
                .push((web_attrs::LEVEL, Arc::from(s.node.level.to_string())));
        }
        if node.name.is_none()
            && matches!(
                role,
                Role::Link
                    | Role::Heading
                    | Role::Button
                    | Role::ToggleButton
                    | Role::Tab
                    | Role::MenuItem
                    | Role::TreeItem
                    | Role::ListItem
                    | Role::Cell
                    | Role::ColumnHeader
                    | Role::RowHeader
            )
        {
            let mut parts = Vec::new();
            for c in &s.children {
                text_below(c, &mut parts);
            }
            let name = parts
                .into_iter()
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !name.is_empty() {
                node.name = Some(Arc::from(name));
            }
        }
        let mut data = s.node.data.as_deref().cloned().unwrap_or_default();
        data.sem = Some(entry_of(&node));
        s.node.data = Some(Arc::new(data));
        s.children = s.children.into_iter().map(go).collect();
        s
    }
    go(s)
}
