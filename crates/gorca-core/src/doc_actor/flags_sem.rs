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
