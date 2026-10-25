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
use gorca_model::SrcNode;
use gorca_model::sem::{Invalid, SemFlags, SemRole, Semantics};

/// Roles whose source node is a table cell: one line, cells separated by a space.
const fn is_cell(r: SemRole) -> bool {
    matches!(
        r,
        SemRole::Cell | SemRole::GridCell | SemRole::ColumnHeader | SemRole::RowHeader
    )
}

fn role_is_block(r: SemRole) -> bool {
    matches!(
        r,
        SemRole::Paragraph
            | SemRole::Heading
            | SemRole::List
            | SemRole::Listbox
            | SemRole::ListItem
            | SemRole::Option
            | SemRole::Table
            | SemRole::Grid
            | SemRole::TreeGrid
            | SemRole::Row
            | SemRole::Region
            | SemRole::Banner
            | SemRole::Complementary
            | SemRole::ContentInfo
            | SemRole::Main
            | SemRole::Navigation
            | SemRole::Search
            | SemRole::Form
            | SemRole::Separator
            | SemRole::Document
            | SemRole::DocumentWeb
            | SemRole::Dialog
            | SemRole::AlertDialog
            | SemRole::Alert
    )
}

/// The normaliser's `invalid` group of the node's source record: set from the `invalid` state on
/// any role, a form container included, where the fused group is only given for fields.
fn source_invalid(n: &SrcNode) -> bool {
    n.rec.as_ref().is_some_and(|r| r.sem.invalid != Invalid::No)
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
    if !n.is_visible() {
        f = f | NodeFlags::HIDDEN;
    }
    if sem.flags.contains(SemFlags::VISITED) {
        f = f | NodeFlags::VISITED;
    }
    if n.is_focusable() || sem.flags.contains(SemFlags::FOCUSABLE) {
        f = f | NodeFlags::FOCUSABLE;
    }
    let role = sem.role;
    if role == SemRole::Region && (sem.flags.contains(SemFlags::ROLE_AUTHORED) || tag == "section")
    {
        f = f | NodeFlags::REGION;
    }
    if tag == "article" || role == SemRole::Article {
        f = f | NodeFlags::ARTICLE;
    }
    if tag == "blockquote" {
        f = f | NodeFlags::BLOCKQUOTE;
    }
    if (matches!(role, SemRole::Table | SemRole::Grid | SemRole::TreeGrid)
        && sem.flags.contains(SemFlags::LAYOUT_TABLE))
        || (role == SemRole::Presentation && tag == "table")
    {
        f = f | NodeFlags::LAYOUT;
    }
    // A live region root of its own (an `aria-live` or a status or log), or a role that is a live
    // region only to be quiet. A node the engine reports as an alert is a live region by its
    // category already.
    let own_live = sem.live.is_some_and(|l| l.root);
    if own_live
        || matches!(
            role,
            SemRole::Status | SemRole::Log | SemRole::Marquee | SemRole::Timer
        )
    {
        f = f | NodeFlags::LIVE;
    }
    if role == SemRole::Listbox || (role == SemRole::List && tag == "select") {
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
    if (sem.invalid != Invalid::No || source_invalid(n))
        && !matches!(
            role,
            SemRole::TextBox
                | SemRole::SearchBox
                | SemRole::PasswordBox
                | SemRole::Combobox
                | SemRole::SpinButton
                | SemRole::Slider
        )
    {
        f = f | NodeFlags::ERROR;
    }
    if role == SemRole::DocumentWeb && nested_doc {
        f = f | NodeFlags::FRAME;
    }
    // Table cells stay inline: a row is one line, cells separated by a space (NVDA screen layout).
    let cell = is_cell(role);
    if cell {
        f = f | NodeFlags::CELL;
    }
    let block = !cell
        && if sem.flags.contains(SemFlags::DISPLAY_BLOCK) {
            true
        } else if sem.flags.contains(SemFlags::DISPLAY_INLINE) {
            false
        } else {
            role_is_block(role)
        };
    if block {
        f = f | NodeFlags::BLOCK;
    }
    let level = if role == SemRole::Heading && (1..=6).contains(&sem.level) {
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
        (None, None) => {
            let sem = Semantics {
                role: n.model_role(None),
                ..Semantics::default()
            };
            flags_from_sem(n, &sem, tag, nested_doc)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_model::sem::{Live, Relevant, SemFlags, SemRole, SrcRecord, TagName};
    use gorca_model::{Role, SrcNodeId, StateSet};

    use super::*;

    fn node(role: Role, tag: &str) -> SrcNode {
        let r = SrcRecord {
            tag: TagName::new(tag).0,
            ..SrcRecord::default()
        };
        SrcNode {
            id: SrcNodeId(1),
            role,
            states: StateSet::VISIBLE,
            name: None,
            text: None,
            attrs: smallvec::SmallVec::default(),
            local: None,
            rec: Some(Arc::new(r)),
            synth_role: gorca_model::testing::sem_role(role),
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
        n.states = StateSet::FOCUSABLE;
        let mut visited = sem(SemRole::Link);
        visited.flags |= SemFlags::VISITED;
        let (f, _) = flags_from_sem(&n, &visited, "a", false);
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
    fn live_roots_and_live_roles_are_flagged_and_an_alert_is_live_by_its_category_too() {
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
        let mut alert = s.clone();
        alert.role = SemRole::Alert;
        assert!(
            flags_from_sem(&n, &alert, "div", false)
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

    fn invalid_record(role: SemRole) -> gorca_model::sem::SrcRecord {
        let mut r = gorca_model::sem::SrcRecord::default();
        r.sem.role = role;
        r.sem.invalid = Invalid::True;
        r
    }

    #[test]
    fn invalid_fields_other_than_edits_are_errors_and_headings_carry_their_level() {
        let mut n = node(Role::Table, "table");
        n.rec = Some(Arc::new(invalid_record(SemRole::Table)));
        assert!(
            flags_from_sem(&n, &sem(SemRole::Table), "table", false)
                .0
                .contains(NodeFlags::ERROR)
        );
        let mut e = node(Role::TextEntry, "input");
        e.rec = Some(Arc::new(invalid_record(SemRole::TextBox)));
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
