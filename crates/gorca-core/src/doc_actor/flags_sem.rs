// SPDX-License-Identifier: GPL-3.0-or-later
//! The structural flags of a document node from its fused semantics (ADR-0022 section 8, M22b4).
//!
//! The mirror's node flags (`NodeFlags`: region, article, live, listbox, annotation, block, error,
//! ...) used to be derived from the raw attribute map of the source node (tag, xml roles, live,
//! layout guess, display). They follow from the node's fused entry instead: the semantic role (an
//! author's role token is already folded into it), the `flags` group (`LAYOUT_TABLE`,
//! `DISPLAY_BLOCK`, `DISPLAY_INLINE`), the `live` and `invalid` groups, the heading `level`, and
//! the DOM tag of the source record (a typed field, not an attribute). Nothing here reads
//! `SrcNode::attrs`.

use gorca_doc::web::NodeFlags;
use gorca_model::sem::{Invalid, SemFlags, SemRole, Semantics};
use gorca_model::{Role, SrcNode, StateSet};

/// Roles whose source node is a table cell: one line, cells separated by a space.
const fn is_cell(r: Role) -> bool {
    matches!(r, Role::Cell | Role::ColumnHeader | Role::RowHeader)
}

fn role_is_block(r: Role) -> bool {
    matches!(
        r,
        Role::Paragraph
            | Role::Heading
            | Role::List
            | Role::ListItem
            | Role::Table
            | Role::Row
            | Role::Landmark
            | Role::Separator
            | Role::Document
            | Role::DocumentWeb
            | Role::Dialog
            | Role::Alert
    )
}

/// The node flags that follow from the source node and its fused semantics `sem` (everything but
/// the break flags), and the heading level. `tag` is the DOM tag of the source record.
#[must_use]
pub fn flags_from_sem(
    n: &SrcNode,
    sem: &Semantics,
    tag: &str,
    nested_doc: bool,
) -> (NodeFlags, u8) {
    let mut f = NodeFlags::NONE;
    // `showing` is viewport state and flips without events (TBD-WEB-1): only `visible` counts.
    if !n.states.contains(StateSet::VISIBLE) {
        f = f | NodeFlags::HIDDEN;
    }
    if n.states.contains(StateSet::VISITED) {
        f = f | NodeFlags::VISITED;
    }
    if n.states.contains(StateSet::FOCUSABLE) {
        f = f | NodeFlags::FOCUSABLE;
    }
    let role = sem.role;
    if n.role == Role::Landmark
        && ((role == SemRole::Region && sem.flags.contains(SemFlags::ROLE_AUTHORED))
            || tag == "section")
    {
        f = f | NodeFlags::REGION;
    }
    if tag == "article" || role == SemRole::Article {
        f = f | NodeFlags::ARTICLE;
    }
    if tag == "blockquote" {
        f = f | NodeFlags::BLOCKQUOTE;
    }
    if n.role == Role::Table
        && (sem.flags.contains(SemFlags::LAYOUT_TABLE) || role == SemRole::Presentation)
    {
        f = f | NodeFlags::LAYOUT;
    }
    // A live region root of its own (an `aria-live` or a status or log), or a role that is a live
    // region only to be quiet. A node the engine reports as an alert is a live region by its
    // category already.
    let own_live = sem.live.is_some_and(|l| l.root) && n.role != Role::Alert;
    if own_live
        || matches!(
            role,
            SemRole::Status | SemRole::Log | SemRole::Marquee | SemRole::Timer
        )
    {
        f = f | NodeFlags::LIVE;
    }
    if n.role == Role::List && (role == SemRole::Listbox || tag == "select") {
        f = f | NodeFlags::LISTBOX;
    }
    if matches!(tag, "ins" | "del")
        || matches!(
            role,
            SemRole::Comment | SemRole::Suggestion | SemRole::Insertion | SemRole::Deletion
        )
    {
        f = f | NodeFlags::ANNOTATION;
    }
    // A form field that is invalid is announced as the field; W finds the marked-up text.
    if (sem.invalid != Invalid::No || n.states.contains(StateSet::INVALID))
        && !matches!(
            n.role,
            Role::TextEntry
                | Role::PasswordEntry
                | Role::ComboBox
                | Role::SpinButton
                | Role::Slider
        )
    {
        f = f | NodeFlags::ERROR;
    }
    if n.role == Role::DocumentWeb && nested_doc {
        f = f | NodeFlags::FRAME;
    }
    // Table cells stay inline: a row is one line, cells separated by a space (NVDA screen layout).
    let cell = is_cell(n.role);
    if cell {
        f = f | NodeFlags::CELL;
    }
    let block = !cell
        && if sem.flags.contains(SemFlags::DISPLAY_BLOCK) {
            true
        } else if sem.flags.contains(SemFlags::DISPLAY_INLINE) {
            false
        } else {
            role_is_block(n.role)
        };
    if block {
        f = f | NodeFlags::BLOCK;
    }
    let level = if n.role == Role::Heading && (1..=6).contains(&sem.level) {
        sem.level
    } else {
        0
    };
    (f, level)
}

/// The flags of a source node: from its fused entry when it has one, else from the semantics of
/// its source record, else from the role alone (a synthetic node).
#[must_use]
pub fn flags_of(
    n: &SrcNode,
    entry: Option<&gorca_model::sem::SemEntry>,
    nested_doc: bool,
) -> (NodeFlags, u8) {
    let tag = n.rec.as_ref().map_or("", |r| r.tag.as_str());
    match (entry, &n.rec) {
        (Some(e), _) => flags_from_sem(n, &e.sem, tag, nested_doc),
        (None, Some(r)) => flags_from_sem(n, &r.sem, tag, nested_doc),
        (None, None) => flags_from_sem(n, &Semantics::default(), tag, nested_doc),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_model::sem::{Live, Relevant, SemFlags, SemRole, SrcRecord, TagName};
    use gorca_model::{SrcNodeId, StateSet};

    use super::*;

    fn node(role: Role, tag: &str) -> SrcNode {
        let mut r = SrcRecord::default();
        r.tag = TagName::new(tag).0;
        SrcNode {
            id: SrcNodeId(1),
            role,
            states: StateSet::VISIBLE,
            name: None,
            text: None,
            attrs: smallvec::SmallVec::default(),
            local: None,
            rec: Some(Arc::new(r)),
        }
    }

    fn sem(role: SemRole) -> Semantics {
        Semantics {
            role,
            ..Semantics::default()
        }
    }

    #[test]
    fn hidden_and_visited_follow_the_source_states() {
        let mut n = node(Role::Link, "a");
        n.states = StateSet::VISITED | StateSet::FOCUSABLE;
        let (f, _) = flags_from_sem(&n, &sem(SemRole::Link), "a", false);
        assert!(f.contains(NodeFlags::HIDDEN | NodeFlags::VISITED | NodeFlags::FOCUSABLE));
    }

    #[test]
    fn block_follows_the_display_class_then_the_role_and_cells_stay_inline() {
        let n = node(Role::Paragraph, "p");
        let mut s = sem(SemRole::Paragraph);
        assert!(
            flags_from_sem(&n, &s, "p", false)
                .0
                .contains(NodeFlags::BLOCK)
        );
        s.flags |= SemFlags::DISPLAY_INLINE;
        assert!(
            !flags_from_sem(&n, &s, "p", false)
                .0
                .contains(NodeFlags::BLOCK)
        );
        let span = node(Role::StaticText, "span");
        let mut s = sem(SemRole::StaticText);
        s.flags |= SemFlags::DISPLAY_BLOCK;
        assert!(
            flags_from_sem(&span, &s, "span", false)
                .0
                .contains(NodeFlags::BLOCK)
        );
        let cell = node(Role::Cell, "td");
        let mut s = sem(SemRole::Cell);
        s.flags |= SemFlags::DISPLAY_BLOCK;
        let (f, _) = flags_from_sem(&cell, &s, "td", false);
        assert!(f.contains(NodeFlags::CELL) && !f.contains(NodeFlags::BLOCK));
    }

    #[test]
    fn article_region_listbox_and_annotation_follow_tag_and_role() {
        let n = node(Role::Paragraph, "article");
        assert!(
            flags_from_sem(&n, &sem(SemRole::Generic), "article", false)
                .0
                .contains(NodeFlags::ARTICLE)
        );
        let n = node(Role::Paragraph, "div");
        assert!(
            flags_from_sem(&n, &sem(SemRole::Article), "div", false)
                .0
                .contains(NodeFlags::ARTICLE)
        );
        let n = node(Role::Landmark, "section");
        assert!(
            flags_from_sem(&n, &sem(SemRole::Region), "section", false)
                .0
                .contains(NodeFlags::REGION)
        );
        // A region the fused model derived (doc-chapter) is no authored region.
        let n = node(Role::Landmark, "div");
        assert!(
            !flags_from_sem(&n, &sem(SemRole::Region), "div", false)
                .0
                .contains(NodeFlags::REGION)
        );
        let n = node(Role::List, "select");
        assert!(
            flags_from_sem(&n, &sem(SemRole::List), "select", false)
                .0
                .contains(NodeFlags::LISTBOX)
        );
        let n = node(Role::StaticText, "ins");
        assert!(
            flags_from_sem(&n, &sem(SemRole::Insertion), "ins", false)
                .0
                .contains(NodeFlags::ANNOTATION)
        );
    }

    #[test]
    fn live_roots_are_flagged_except_a_node_the_engine_calls_an_alert() {
        let live = Live {
            politeness: gorca_model::event::Politeness::Polite,
            atomic: false,
            relevant: Relevant::default(),
            root: true,
        };
        let mut s = sem(SemRole::Generic);
        s.live = Some(live);
        let n = node(Role::Panel, "div");
        assert!(
            flags_from_sem(&n, &s, "div", false)
                .0
                .contains(NodeFlags::LIVE)
        );
        let n = node(Role::Alert, "div");
        assert!(
            !flags_from_sem(&n, &s, "div", false)
                .0
                .contains(NodeFlags::LIVE)
        );
        let n = node(Role::Panel, "div");
        assert!(
            flags_from_sem(&n, &sem(SemRole::Log), "div", false)
                .0
                .contains(NodeFlags::LIVE)
        );
    }

    #[test]
    fn invalid_fields_other_than_edits_are_errors_and_headings_carry_their_level() {
        let mut n = node(Role::Table, "table");
        n.states |= StateSet::INVALID;
        assert!(
            flags_from_sem(&n, &sem(SemRole::Table), "table", false)
                .0
                .contains(NodeFlags::ERROR)
        );
        let mut e = node(Role::TextEntry, "input");
        e.states |= StateSet::INVALID;
        assert!(
            !flags_from_sem(&e, &sem(SemRole::TextBox), "input", false)
                .0
                .contains(NodeFlags::ERROR)
        );
        let h = node(Role::Heading, "h3");
        let mut s = sem(SemRole::Heading);
        s.level = 3;
        assert_eq!(flags_from_sem(&h, &s, "h3", false).1, 3);
        s.level = 9;
        assert_eq!(flags_from_sem(&h, &s, "h3", false).1, 0);
    }

    #[test]
    fn a_layout_table_and_a_nested_document_are_marked() {
        let t = node(Role::Table, "table");
        let mut s = sem(SemRole::Table);
        s.flags |= SemFlags::LAYOUT_TABLE;
        assert!(
            flags_from_sem(&t, &s, "table", false)
                .0
                .contains(NodeFlags::LAYOUT)
        );
        assert!(
            flags_from_sem(&t, &sem(SemRole::Presentation), "table", false)
                .0
                .contains(NodeFlags::LAYOUT)
        );
        let d = node(Role::DocumentWeb, "iframe");
        assert!(
            flags_from_sem(&d, &sem(SemRole::DocumentWeb), "iframe", true)
                .0
                .contains(NodeFlags::FRAME)
        );
        assert!(
            !flags_from_sem(&d, &sem(SemRole::DocumentWeb), "iframe", false)
                .0
                .contains(NodeFlags::FRAME)
        );
    }
}
