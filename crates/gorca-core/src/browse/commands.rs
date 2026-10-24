// SPDX-License-Identifier: GPL-3.0-or-later
//! The Reader's browse arm: focus handling, document notices, key commands (browse-mode design
//! sections 7 and 8). Everything on a key's path to speech reads one published `WebDoc` and
//! sends nothing but fire-and-forget effects afterwards (BI-1, BI-2).

use gorca_i18n::{MsgId as M, t};
use smallvec::SmallVec;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc::web::{Cat, WebDoc, WebSpace};
use gorca_input_api::{GrabSet, InputContext, ModeKind};
use gorca_model::{
    AppId, DocAction, DocId, DocNodeId, Lookup, NodeKey, Prio, Role, Segment, SegmentKind, StateSet,
};
use gorca_text::{Dir, TextSpace, Unit};

use super::cursor::{self, Edge, Mv};
use super::forms::{self, Reason};
use super::present::{self, Item};
use super::{
    BrowseState, CursorFrom, LIVE_TTL_ASSERTIVE, LIVE_TTL_POLITE, ModeWord, NativeSync, SavedWeb,
    SelectionModel, WebEv, WebFx, WebMode, step,
};
use crate::SpeechIntent;
use crate::commands::CoreCmd;
use crate::doc_actor::live::LiveMsg;
use crate::doc_actor::{DocDirectory, DocNotice, SelOutcome, WebJoin};
use crate::focus::FocusState;
use crate::mode::{Mode, ModeMsg, WebCursor};
use crate::present::message;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// What the Reader asks of the web side, executed by the runtime (fire and forget).
#[derive(Clone, Debug)]
pub enum WebEffect {
    /// Find or build the document around a focused node.
    Ensure { focus: NodeKey },
    /// Wake or put to sleep a document (DR-3).
    Activity { doc: DocId, awake: bool },
    /// A user-driven pull on a node.
    Act {
        doc: DocId,
        node: DocNodeId,
        act: DocAction,
    },
    /// Infer the label of a nameless control from the text around it (C3b); the answer, if any,
    /// returns as `DocNotice::Label { req }`.
    Label {
        doc: DocId,
        node: DocNodeId,
        role: Role,
        req: u32,
    },
    /// A page-selection action; its result returns as `DocNotice::SelectionDone { req }`.
    Select {
        doc: DocId,
        node: DocNodeId,
        act: DocAction,
        req: u32,
    },
}

/// How long after the cursor's node vanished a re-render may still move the cursor back.
const RESTORE_WINDOW: Duration = Duration::from_millis(1500);
/// A focus event this soon after a click on a check box is the click's own.
const CLICK_FOCUS_WINDOW: Duration = Duration::from_millis(800);
/// Characters of a selected span that are read back.
const SELECT_SPEAK_CAP: u32 = 300;
/// A mode word waits this long for the utterance it belongs to.
/// A document focus this soon after our own click or Enter is the click's own.
/// How long the speech of a frame document that took the focus waits for the focus to move on
/// into the frame before the stop is announced.
const FRAME_FOCUS_DEFER: Duration = Duration::from_millis(250);
const REFOCUS_SILENT_WINDOW: Duration = Duration::from_millis(2000);
const WORD_TTL: Duration = Duration::from_millis(600);
/// The line that follows a document's focus utterance waits for a cold fetch this long (M4).
const TAIL_TTL: Duration = Duration::from_secs(2);
/// A late first line (the mirror was empty at focus time) is still spoken within this window.
const ENTRY_WAIT: Duration = Duration::from_secs(2);
/// The page selection follows the virtual one this long after the last change, so a held
/// Shift+arrow sets it once; short, so a Ctrl+C right after the keys finds it in place.
const SEL_DEBOUNCE: Duration = Duration::from_millis(40);
/// Held cursor keys scroll the page once per this interval (LP-6).
pub(super) const SCROLL_DEBOUNCE: Duration = Duration::from_millis(150);
/// An edge hit this soon after a scroll request is spoken as "loading" (older items are coming).
const LOADING_CUE_WINDOW: Duration = Duration::from_millis(1200);
/// Focus in a document the mirror does not know yet waits this long (browse design 7.3).
const LOADING_GRACE: Duration = Duration::from_secs(1);
/// The shortest time between two focus resolutions restarted by key presses.
const KEY_RESYNC_INTERVAL: Duration = Duration::from_millis(500);

/// What a cursor command computed from one snapshot.
enum Outcome {
    Moved {
        cur: WebCursor,
        off: u32,
        /// The active end of a selection: `off`, or one past it when the move ended on the last
        /// character of a line (End), so that character is inside the range.
        active: u32,
        /// Where the cursor was (the active end of a selection being extended).
        from: u32,
        items: Vec<Item>,
        /// The text between `from` and `off`, for selection speech.
        span: String,
    },
    Edge(Edge),
    Nothing,
}

#[derive(Copy, Clone)]
enum Kind {
    Char,
    Word,
    Line,
}

/// A cursor move.
#[derive(Copy, Clone)]
enum Move {
    Line(Dir),
    Char(Dir),
    Word(Dir),
    LineStart,
    LineEnd,
    Top,
    Bottom,
    Page(Dir),
    Paragraph(Dir),
    Sentence(Dir),
}

const CURSOR: &[(CoreCmd, Move)] = &[
    (CoreCmd::BrowseCursorNextLine, Move::Line(Dir::Next)),
    (CoreCmd::BrowseCursorPrevLine, Move::Line(Dir::Prev)),
    (CoreCmd::BrowseCursorNextChar, Move::Char(Dir::Next)),
    (CoreCmd::BrowseCursorPrevChar, Move::Char(Dir::Prev)),
    (CoreCmd::BrowseCursorNextWord, Move::Word(Dir::Next)),
    (CoreCmd::BrowseCursorPrevWord, Move::Word(Dir::Prev)),
    (CoreCmd::BrowseCursorLineStart, Move::LineStart),
    (CoreCmd::BrowseCursorLineEnd, Move::LineEnd),
    (CoreCmd::BrowseCursorTop, Move::Top),
    (CoreCmd::BrowseCursorBottom, Move::Bottom),
    (CoreCmd::BrowseCursorPageUp, Move::Page(Dir::Prev)),
    (CoreCmd::BrowseCursorPageDown, Move::Page(Dir::Next)),
    (
        CoreCmd::BrowseCursorNextParagraph,
        Move::Paragraph(Dir::Next),
    ),
    (
        CoreCmd::BrowseCursorPrevParagraph,
        Move::Paragraph(Dir::Prev),
    ),
    (CoreCmd::BrowseCursorNextSentence, Move::Sentence(Dir::Next)),
    (CoreCmd::BrowseCursorPrevSentence, Move::Sentence(Dir::Prev)),
];

/// Where `,` or Shift+`,` lands.
enum Out {
    NoContainer,
    Bottom,
    To(WebCursor, u32, Vec<Item>),
}

/// Selection commands: the same moves, extending the virtual selection (D-3).
const SELECT: &[(CoreCmd, Move)] = &[
    (CoreCmd::BrowseSelectNextLine, Move::Line(Dir::Next)),
    (CoreCmd::BrowseSelectPrevLine, Move::Line(Dir::Prev)),
    (CoreCmd::BrowseSelectNextChar, Move::Char(Dir::Next)),
    (CoreCmd::BrowseSelectPrevChar, Move::Char(Dir::Prev)),
    (CoreCmd::BrowseSelectNextWord, Move::Word(Dir::Next)),
    (CoreCmd::BrowseSelectPrevWord, Move::Word(Dir::Prev)),
    (CoreCmd::BrowseSelectLineStart, Move::LineStart),
    (CoreCmd::BrowseSelectLineEnd, Move::LineEnd),
    (CoreCmd::BrowseSelectTop, Move::Top),
    (CoreCmd::BrowseSelectBottom, Move::Bottom),
    (CoreCmd::BrowseSelectPageUp, Move::Page(Dir::Prev)),
    (CoreCmd::BrowseSelectPageDown, Move::Page(Dir::Next)),
];

/// The quick-navigation commands that are implemented: direction, category, spoken name.
const QUICKNAV: &[(CoreCmd, Dir, Cat, M)] = &[
    (
        CoreCmd::QuicknavNextHeading,
        Dir::Next,
        Cat::HEADING,
        M::WebNoNextHeading,
    ),
    (
        CoreCmd::QuicknavPrevHeading,
        Dir::Prev,
        Cat::HEADING,
        M::WebNoPrevHeading,
    ),
    (
        CoreCmd::QuicknavNextHeading1,
        Dir::Next,
        Cat::HEADING_1,
        M::WebNoNextHeading1,
    ),
    (
        CoreCmd::QuicknavPrevHeading1,
        Dir::Prev,
        Cat::HEADING_1,
        M::WebNoPrevHeading1,
    ),
    (
        CoreCmd::QuicknavNextHeading2,
        Dir::Next,
        Cat::HEADING_2,
        M::WebNoNextHeading2,
    ),
    (
        CoreCmd::QuicknavPrevHeading2,
        Dir::Prev,
        Cat::HEADING_2,
        M::WebNoPrevHeading2,
    ),
    (
        CoreCmd::QuicknavNextHeading3,
        Dir::Next,
        Cat::HEADING_3,
        M::WebNoNextHeading3,
    ),
    (
        CoreCmd::QuicknavPrevHeading3,
        Dir::Prev,
        Cat::HEADING_3,
        M::WebNoPrevHeading3,
    ),
    (
        CoreCmd::QuicknavNextHeading4,
        Dir::Next,
        Cat::HEADING_4,
        M::WebNoNextHeading4,
    ),
    (
        CoreCmd::QuicknavPrevHeading4,
        Dir::Prev,
        Cat::HEADING_4,
        M::WebNoPrevHeading4,
    ),
    (
        CoreCmd::QuicknavNextHeading5,
        Dir::Next,
        Cat::HEADING_5,
        M::WebNoNextHeading5,
    ),
    (
        CoreCmd::QuicknavPrevHeading5,
        Dir::Prev,
        Cat::HEADING_5,
        M::WebNoPrevHeading5,
    ),
    (
        CoreCmd::QuicknavNextHeading6,
        Dir::Next,
        Cat::HEADING_6,
        M::WebNoNextHeading6,
    ),
    (
        CoreCmd::QuicknavPrevHeading6,
        Dir::Prev,
        Cat::HEADING_6,
        M::WebNoPrevHeading6,
    ),
    (
        CoreCmd::QuicknavNextLink,
        Dir::Next,
        Cat::LINK,
        M::WebNoNextLink,
    ),
    (
        CoreCmd::QuicknavPrevLink,
        Dir::Prev,
        Cat::LINK,
        M::WebNoPrevLink,
    ),
    (
        CoreCmd::QuicknavNextVisited,
        Dir::Next,
        Cat::LINK_VISITED,
        M::WebNoNextVisitedLink,
    ),
    (
        CoreCmd::QuicknavPrevVisited,
        Dir::Prev,
        Cat::LINK_VISITED,
        M::WebNoPrevVisitedLink,
    ),
    (
        CoreCmd::QuicknavNextUnvisited,
        Dir::Next,
        Cat::LINK_UNVISITED,
        M::WebNoNextUnvisitedLink,
    ),
    (
        CoreCmd::QuicknavPrevUnvisited,
        Dir::Prev,
        Cat::LINK_UNVISITED,
        M::WebNoPrevUnvisitedLink,
    ),
    (
        CoreCmd::QuicknavNextFormField,
        Dir::Next,
        Cat::FORM_FIELD,
        M::WebNoNextFormField,
    ),
    (
        CoreCmd::QuicknavPrevFormField,
        Dir::Prev,
        Cat::FORM_FIELD,
        M::WebNoPrevFormField,
    ),
    (
        CoreCmd::QuicknavNextList,
        Dir::Next,
        Cat::LIST,
        M::WebNoNextList,
    ),
    (
        CoreCmd::QuicknavPrevList,
        Dir::Prev,
        Cat::LIST,
        M::WebNoPrevList,
    ),
    (
        CoreCmd::QuicknavNextListItem,
        Dir::Next,
        Cat::LIST_ITEM,
        M::WebNoNextListItem,
    ),
    (
        CoreCmd::QuicknavPrevListItem,
        Dir::Prev,
        Cat::LIST_ITEM,
        M::WebNoPrevListItem,
    ),
    (
        CoreCmd::QuicknavNextButton,
        Dir::Next,
        Cat::BUTTON,
        M::WebNoNextButton,
    ),
    (
        CoreCmd::QuicknavPrevButton,
        Dir::Prev,
        Cat::BUTTON,
        M::WebNoPrevButton,
    ),
    (
        CoreCmd::QuicknavNextEdit,
        Dir::Next,
        Cat::EDIT,
        M::WebNoNextEditField,
    ),
    (
        CoreCmd::QuicknavPrevEdit,
        Dir::Prev,
        Cat::EDIT,
        M::WebNoPrevEditField,
    ),
    (
        CoreCmd::QuicknavNextSeparator,
        Dir::Next,
        Cat::SEPARATOR,
        M::WebNoNextSeparator,
    ),
    (
        CoreCmd::QuicknavPrevSeparator,
        Dir::Prev,
        Cat::SEPARATOR,
        M::WebNoPrevSeparator,
    ),
    (
        CoreCmd::QuicknavNextRadio,
        Dir::Next,
        Cat::RADIO,
        M::WebNoNextRadioButton,
    ),
    (
        CoreCmd::QuicknavPrevRadio,
        Dir::Prev,
        Cat::RADIO,
        M::WebNoPrevRadioButton,
    ),
    (
        CoreCmd::QuicknavNextCombobox,
        Dir::Next,
        Cat::COMBOBOX,
        M::WebNoNextComboBox,
    ),
    (
        CoreCmd::QuicknavPrevCombobox,
        Dir::Prev,
        Cat::COMBOBOX,
        M::WebNoPrevComboBox,
    ),
    (
        CoreCmd::QuicknavNextCheckbox,
        Dir::Next,
        Cat::CHECKBOX,
        M::WebNoNextCheckBox,
    ),
    (
        CoreCmd::QuicknavPrevCheckbox,
        Dir::Prev,
        Cat::CHECKBOX,
        M::WebNoPrevCheckBox,
    ),
    (
        CoreCmd::QuicknavNextGraphic,
        Dir::Next,
        Cat::GRAPHIC,
        M::WebNoNextGraphic,
    ),
    (
        CoreCmd::QuicknavPrevGraphic,
        Dir::Prev,
        Cat::GRAPHIC,
        M::WebNoPrevGraphic,
    ),
    (
        CoreCmd::QuicknavNextBlockquote,
        Dir::Next,
        Cat::BLOCKQUOTE,
        M::WebNoNextBlockQuote,
    ),
    (
        CoreCmd::QuicknavPrevBlockquote,
        Dir::Prev,
        Cat::BLOCKQUOTE,
        M::WebNoPrevBlockQuote,
    ),
    (
        CoreCmd::QuicknavNextLandmark,
        Dir::Next,
        Cat::ANY_LANDMARK,
        M::WebNoNextLandmark,
    ),
    (
        CoreCmd::QuicknavPrevLandmark,
        Dir::Prev,
        Cat::ANY_LANDMARK,
        M::WebNoPrevLandmark,
    ),
    (
        CoreCmd::QuicknavNextEmbedded,
        Dir::Next,
        Cat::EMBEDDED_OR_DIALOG,
        M::WebNoNextEmbeddedObject,
    ),
    (
        CoreCmd::QuicknavPrevEmbedded,
        Dir::Prev,
        Cat::EMBEDDED_OR_DIALOG,
        M::WebNoPrevEmbeddedObject,
    ),
    (
        CoreCmd::QuicknavNextTable,
        Dir::Next,
        Cat::TABLE,
        M::WebNoNextTable,
    ),
    (
        CoreCmd::QuicknavPrevTable,
        Dir::Prev,
        Cat::TABLE,
        M::WebNoPrevTable,
    ),
    (
        CoreCmd::QuicknavNextAnnotation,
        Dir::Next,
        Cat::ANNOTATION,
        M::WebNoNextAnnotation,
    ),
    (
        CoreCmd::QuicknavPrevAnnotation,
        Dir::Prev,
        Cat::ANNOTATION,
        M::WebNoPrevAnnotation,
    ),
    (
        CoreCmd::QuicknavNextError,
        Dir::Next,
        Cat::ERROR,
        M::WebNoNextError,
    ),
    (
        CoreCmd::QuicknavPrevError,
        Dir::Prev,
        Cat::ERROR,
        M::WebNoPrevError,
    ),
    (
        CoreCmd::QuicknavNextParagraph,
        Dir::Next,
        Cat::TEXT_BLOCK,
        M::WebNoNextParagraph,
    ),
    (
        CoreCmd::QuicknavPrevParagraph,
        Dir::Prev,
        Cat::TEXT_BLOCK,
        M::WebNoPrevParagraph,
    ),
    (
        CoreCmd::QuicknavNextFrame,
        Dir::Next,
        Cat::FRAME,
        M::WebNoNextFrame,
    ),
    (
        CoreCmd::QuicknavPrevFrame,
        Dir::Prev,
        Cat::FRAME,
        M::WebNoPrevFrame,
    ),
    (
        CoreCmd::QuicknavNextArticle,
        Dir::Next,
        Cat::ARTICLE,
        M::WebNoNextArticle,
    ),
    (
        CoreCmd::QuicknavPrevArticle,
        Dir::Prev,
        Cat::ARTICLE,
        M::WebNoPrevArticle,
    ),
];

fn quicknav(c: CoreCmd) -> Option<(Dir, Cat, M)> {
    QUICKNAV
        .iter()
        .find(|(cmd, ..)| *cmd == c)
        .map(|(_, d, cat, name)| (*d, *cat, *name))
}

/// States of a node, from its mirror metadata.
fn states_of(d: &WebDoc, id: DocNodeId) -> StateSet {
    d.node(id)
        .and_then(|v| v.meta)
        .map_or(StateSet::empty(), |m| m.states)
}

/// A group of controls: a `fieldset`, or an element the author gave `role=group` or
/// `role=radiogroup`. Landmarks, forms and regions are not groups here.
fn is_group(v: &gorca_doc::web::NodeView<'_>) -> bool {
    use gorca_doc_source_api::web_attrs::{TAG, XML_ROLES};
    if v.role != Role::Panel {
        return false;
    }
    let Some(m) = v.meta else { return false };
    let attr = |k| m.attrs.iter().find(|(a, _)| *a == k).map(|(_, t)| &**t);
    attr(TAG) == Some("fieldset")
        || attr(XML_ROLES).is_some_and(|r| {
            r.split_whitespace()
                .any(|t| matches!(t, "group" | "radiogroup"))
        })
}

fn role_states(d: &WebDoc, id: DocNodeId) -> (Role, StateSet) {
    (
        d.node(id).map_or(Role::Unknown, |v| v.role),
        states_of(d, id),
    )
}

/// The nearest ancestor-or-self (within `hops`) whose categories intersect `c`.
fn up_to(d: &WebDoc, mut id: DocNodeId, c: Cat, hops: usize) -> Option<DocNodeId> {
    for _ in 0..=hops {
        if d.node(id).is_some_and(|v| v.cats.intersects(c)) {
            return Some(id);
        }
        id = d.parent(id)?;
    }
    None
}

/// The previous match before the cursor: the previous one in document order from the innermost
/// match the cursor is in, or that match's enclosing match when it starts later (Shift+D from a
/// landmark inside a landmark goes to the outer one: it starts before the inner one).
fn prev_match(d: &WebDoc, node: DocNodeId, c: Cat, scope: Option<DocNodeId>) -> Option<DocNodeId> {
    let inner = up_to(d, node, c, 64).unwrap_or(node);
    let before = match scope {
        Some(s) => d.prev_in(inner, c, s),
        None => d.prev(inner, c),
    };
    let outer = d
        .parent(inner)
        .and_then(|p| up_to(d, p, c, 64))
        .filter(|o| {
            scope.is_none_or(|s| d.cmp_order(s, *o).is_some_and(std::cmp::Ordering::is_le))
        });
    match (before, outer) {
        (Some(b), Some(o)) => match d.cmp_order(b, o) {
            Some(std::cmp::Ordering::Less) => Some(o),
            _ => Some(b),
        },
        (b, o) => b.or(o),
    }
}

/// The modal dialog around `node` (`aria-modal`, the MODAL state), if any: the cursor keys
/// stay inside it while the cursor is.
fn modal_scope(d: &WebDoc, mut node: DocNodeId) -> Option<DocNodeId> {
    for _ in 0..64 {
        if states_of(d, node).contains(StateSet::MODAL) {
            return Some(node);
        }
        node = d.parent(node)?;
    }
    None
}

/// The document itself (or an embedded one): focus on it is not a move onto a control.
fn is_document(d: &WebDoc, node: DocNodeId) -> bool {
    d.root() == Some(node) || d.node(node).is_some_and(|v| v.role == Role::DocumentWeb)
}

/// Whether `anc` is `node` or one of its ancestors.
fn is_ancestor_or_self(d: &WebDoc, anc: DocNodeId, node: DocNodeId) -> bool {
    let mut n = node;
    for _ in 0..64 {
        if n == anc {
            return true;
        }
        match d.parent(n) {
            Some(p) => n = p,
            None => return false,
        }
    }
    false
}

/// The nearest ancestors of a node, nearest first (at most the inline capacity), each with the
/// sibling that follows the child on the path to `node`: where the browse cursor re-anchors when
/// the page removes the node together with its whole subtree.
fn ancestor_chain(d: &WebDoc, node: DocNodeId) -> SmallVec<[(DocNodeId, Option<DocNodeId>); 8]> {
    let mut chain = SmallVec::new();
    let mut below = node;
    while let Some(p) = d.parent(below) {
        if chain.len() == chain.inline_size() {
            break;
        }
        chain.push((p, d.next_sibling(below)));
        below = p;
    }
    chain
}

/// Where the cursor lands once its node is gone: the first line of what follows the removed
/// node in document order under the nearest surviving ancestor, else of what follows that
/// ancestor, else the last line of the ancestor.
fn reanchor(d: &WebDoc, chain: &[(DocNodeId, Option<DocNodeId>)]) -> Option<(WebCursor, u32)> {
    let k = chain.iter().position(|(a, _)| d.is_attached(*a))?;
    let following = chain[k..]
        .iter()
        .filter_map(|(_, after)| *after)
        .find(|n| d.is_attached(*n));
    let off = match following {
        Some(n) => cursor::content_start(d, n)?,
        None => d.subtree_text_range(chain[k].0)?.1.saturating_sub(1),
    };
    cursor::cursor_at(d, off).map(|c| (c, off))
}

/// How a scroll request for the cursor is addressed. A text run is not an object of its own:
/// its owner scrolls to the cursor's character (an owner taller than the viewport must not
/// jump to an edge). An addressable node scrolls as a whole.
fn scroll_target(d: &WebDoc, node: DocNodeId, local: u32) -> Option<(DocNodeId, DocAction)> {
    let addressable = |n: DocNodeId| d.local_of(n).is_some();
    if addressable(node) {
        return None;
    }
    let owner = d.parent(node)?;
    let mut off = 0u32;
    for c in d.children(owner) {
        if c == node {
            break;
        }
        // A run adds its text; an embedded object is one placeholder in the owner's text.
        off = off.saturating_add(if addressable(c) {
            1
        } else {
            d.own_len(c).unwrap_or(0)
        });
    }
    Some((owner, DocAction::ScrollToOffset(off.saturating_add(local))))
}

/// The page item (message, row) the cursor is in: the nearest ancestor-or-self with a DOM id,
/// and the cursor's distance from the item's start. The item is what the user reads; the row
/// object a virtual list reuses for it is not.
fn item_of(d: &WebDoc, node: DocNodeId, off: u32) -> Option<(Arc<str>, u32)> {
    let mut at = Some(node);
    for _ in 0..12 {
        let n = at?;
        let id = d.sem_of(n).and_then(|e| e.sem.dom_id()).map(Arc::clone);
        if let Some(id) = id.filter(|i| !i.is_empty()) {
            let start = d.offset_of(n)?;
            return Some((id, off.saturating_sub(start)));
        }
        at = d.parent(n);
    }
    None
}

/// The cursor on the item with this DOM id, wherever the page re-rendered it.
fn find_item(d: &WebDoc, id: &str, delta: u32) -> Option<(WebCursor, u32)> {
    let n = d.preorder().find(|n| {
        d.sem_of(*n)
            .and_then(|e| e.sem.dom_id())
            .is_some_and(|v| &**v == id)
    })?;
    if !d.is_attached(n) {
        return None;
    }
    let (s, e) = d.subtree_text_range(n)?;
    let off = cursor::content_start(d, n)?
        .max(s)
        .saturating_add(delta)
        .min(e.saturating_sub(1));
    cursor::cursor_at(d, off).map(|c| (c, off))
}

fn title_of(d: &WebDoc) -> Option<String> {
    d.root()
        .and_then(|r| d.node(r))
        .and_then(|v| v.name.map(str::to_owned))
        .filter(|t| !t.is_empty())
}

/// The speakable items of the soft line containing `off`.
fn line_items(d: &WebDoc, max_line: u16, off: u32) -> Vec<Item> {
    let sp = WebSpace::new(d, max_line);
    let r = sp.unit_at(off, Unit::Line).ready().unwrap_or(off..off);
    let end = if r.end > r.start && d.text_in(r.end - 1, r.end) == "\n" {
        r.end - 1
    } else {
        r.end
    };
    present::describe(d, r.start, end)
}

/// A single character, spoken by name.
fn single_char(d: &WebDoc, r: &Range<u32>, from: Option<u32>) -> Vec<Item> {
    let items = present::describe_from(d, r.start, r.end, from);
    if let [Item::Text(t)] = items.as_slice() {
        let mut it = t.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            return vec![Item::Char(c.to_string())];
        }
        return vec![Item::Text(t.clone())];
    }
    if items.is_empty() {
        let t = d.text_in(r.start, r.end);
        return t
            .chars()
            .next()
            .map(|c| vec![Item::Char(c.to_string())])
            .unwrap_or_default();
    }
    items
}

/// A word without its trailing white space.
fn trim_items(mut items: Vec<Item>) -> Vec<Item> {
    for i in &mut items {
        if let Item::Text(t) = i {
            *t = t.trim_end().to_owned();
        }
    }
    items.retain(|i| !i.text().is_empty());
    items
}

impl Reader {
    /// Attach browse mode: the directory of documents and the focus join.
    #[must_use]
    pub fn with_web(mut self, docs: Arc<DocDirectory>, join: Arc<WebJoin>) -> Self {
        self.web = Some(BrowseState::new(docs, join));
        self
    }

    /// The browse state machine, for tests and the debug probe.
    /// The transport key of the node a pointer should aim at for the browse cursor: the nearest
    /// interactive node at or above it (a run inside a link gives the link), else a keyed leaf
    /// text or heading node. The climb stops at the first container (document, section,
    /// paragraph, landmark, list, table, group): its centre is empty page space, so the answer is
    /// `None` and the caller refuses. A read of the published mirror snapshot: no wait, no IPC.
    pub(crate) fn web_cursor_key(&self) -> Option<NodeKey> {
        let b = self.web.as_ref()?;
        let WebMode::Browse { doc, cur, .. } = b.mode else {
            return None;
        };
        let h = b.docs.get(doc)?;
        let local = h.read.with(|d| {
            let mut leaf = None;
            let mut n = Some(cur.node);
            // The depth bound keeps a corrupt parent chain from looping.
            for _ in 0..64 {
                let Some(id) = n else { break };
                let v = d.node(id)?;
                let local = d.local_of(id);
                match pointer_class(v.role) {
                    PointerClass::Interactive => {
                        if local.is_some() {
                            return local;
                        }
                    }
                    PointerClass::Leaf => {
                        if leaf.is_none() {
                            leaf = local;
                        }
                    }
                    PointerClass::Container => break,
                }
                n = d.parent(id);
            }
            leaf
        })?;
        Some(NodeKey { app: h.app, local })
    }

    pub fn web_mode(&self) -> WebMode {
        self.web.as_ref().map_or(WebMode::Inactive, |w| w.mode)
    }

    /// The grab set for the current mode, with Escape in focus-auto mode (modes.md section 2).
    pub(crate) fn current_grabs(&self) -> GrabSet {
        let mut set = self.mode.grab_set(&self.keys);
        if matches!(self.mode, Mode::Browse { .. })
            && self.web.as_ref().is_some_and(|w| !w.single_letter)
        {
            for c in &self.keys.letters {
                set.chords.remove(c);
            }
        }
        if matches!(self.mode, Mode::Browse { .. }) {
            // D-2: Tab is ours only while the mirror has a focusable target in its direction;
            // otherwise the key stays with the page (a grabbed key cannot be given back).
            let (next, prev) = self.web_tab_targets();
            if let Some(w) = self.web.as_ref() {
                w.tab_grab.set((next, prev));
            }
            if !next {
                for c in &self.keys.tab_next {
                    set.chords.remove(c);
                }
            }
            if !prev {
                for c in &self.keys.tab_prev {
                    set.chords.remove(c);
                }
            }
        }
        if matches!(self.mode, Mode::Focus)
            && self
                .web
                .as_ref()
                .is_some_and(|w| w.mode.escape_grabbed() && !w.popup_open)
            && !self.keys.web_escape.is_empty()
        {
            // Escape lives in the browse keymap table; the lane looks it up there.
            set.ctx = InputContext::of(ModeKind::Browse);
            for c in &self.keys.web_escape {
                set.chords.insert(*c);
            }
        }
        set
    }

    fn web_word(&mut self, w: ModeWord) {
        let now = self.now;
        if let Some(b) = self.web.as_mut() {
            b.pending_word = Some((w, now));
        }
    }

    /// The focus speech of `key` is swallowed once (the document re-focused under the cursor).
    pub(crate) fn web_suppress_focus(&mut self, key: NodeKey) -> bool {
        self.web
            .as_mut()
            .is_some_and(|b| b.suppress_focus.take() == Some(key))
    }

    /// A caret event while browsing: the page answering our own click (a same-page link to a
    /// paragraph or a named anchor moves the DOM caret and scrolls, no focus event) moves the
    /// cursor there and reads the line. Anything else is not ours (BM-2). True when it moved.
    pub(crate) fn web_caret(&mut self, node: NodeKey, offset: u32, fx: &mut ReaderEffects) -> bool {
        let now = self.now;
        let Some(b) = self.web.as_mut() else {
            return false;
        };
        let WebMode::Browse { doc, .. } = b.mode else {
            return false;
        };
        if b.activated
            .is_none_or(|t| now.saturating_duration_since(t) > super::CARET_FOLLOW)
        {
            return false;
        }
        let Some((d_id, id)) = b.join.get(node) else {
            return false;
        };
        if d_id != doc {
            return false;
        }
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let max_line = b.settings.max_line;
        let placed = h.read.with(|d| {
            let start = cursor::content_start(d, id)?;
            let end = d.subtree_text_range(id)?.1;
            let off = start
                .saturating_add(offset)
                .min(end.saturating_sub(1))
                .max(start);
            Some((
                cursor::cursor_at(d, off)?,
                off,
                line_items(d, max_line, off),
            ))
        });
        let Some((cur, off, items)) = placed else {
            return false;
        };
        b.activated = None;
        self.web_set_cursor(doc, cur, off, now);
        self.web_speak(&items, fx);
        true
    }

    /// The mode word waiting for an utterance, if still fresh (BM-5).
    pub(crate) fn web_take_word(&mut self) -> Option<&'static str> {
        let now = self.now;
        let b = self.web.as_mut()?;
        let (w, at) = b.pending_word.take()?;
        (now.saturating_duration_since(at) <= WORD_TTL).then(|| w.words())
    }

    /// The line that follows the focus utterance of a document root, if still fresh.
    pub(crate) fn web_take_tail(&mut self) -> Vec<Item> {
        let now = self.now;
        let Some(b) = self.web.as_mut() else {
            return Vec::new();
        };
        match b.pending_tail.take() {
            Some((items, at)) if now.saturating_duration_since(at) <= TAIL_TTL => items,
            Some(_) | None => Vec::new(),
        }
    }

    /// Keep `Mode` and the grab set in line with the web mode.
    fn web_sync(&mut self, old: WebMode, fx: &mut ReaderEffects, quiet: bool) {
        let Some(b) = self.web.as_ref() else { return };
        let new = b.mode;
        let want_browse = match new {
            WebMode::Browse { doc, .. } => Some(doc),
            WebMode::Inactive | WebMode::Loading { .. } | WebMode::Focus { .. } => None,
        };
        let before = self.mode;
        match (want_browse, self.mode) {
            (Some(doc), Mode::Focus) => {
                self.web_mode_msg(ModeMsg::EnterBrowse { doc }, fx, quiet);
            }
            (Some(doc), Mode::Browse { doc: d, .. }) if d != doc => {
                self.web_mode_msg(ModeMsg::EnterBrowse { doc }, fx, quiet);
            }
            (None, Mode::Browse { .. }) => self.web_mode_msg(ModeMsg::ExitBrowse, fx, quiet),
            (
                _,
                Mode::Focus
                | Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp { .. },
            ) => {}
        }
        // The grab set also follows Escape: Focus{Auto} is Mode::Focus with a different set.
        if self.mode == before && old.escape_grabbed() != new.escape_grabbed() {
            fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
        }
    }

    fn web_mode_msg(&mut self, m: ModeMsg, fx: &mut ReaderEffects, quiet: bool) {
        if quiet {
            // An asynchronous transition must not cancel the utterance that is being spoken.
            let (next, effects) = self.mode.step(m, &self.keys);
            self.mode = next;
            if !effects.is_empty() {
                fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
            }
        } else {
            self.on_mode(m, fx);
        }
    }

    /// Run the state machine and interpret its effects. True when the mode changed.
    fn web_apply(&mut self, ev: WebEv, fx: &mut ReaderEffects, quiet: bool) -> bool {
        let Some(b) = self.web.as_mut() else {
            return false;
        };
        let old = b.mode;
        let (next, effects) = step(old, ev);
        b.mode = next;
        let changed = next != old;
        // The cursor of a document the user leaves is remembered for when they come back (W3).
        if let WebMode::Browse { doc, cur, .. } = old
            && next.doc() != Some(doc)
        {
            b.saved.insert(
                doc,
                SavedWeb {
                    cur,
                    off: b.last_off,
                },
            );
        }
        for e in effects {
            match e {
                WebFx::Announce(w) => self.web_word(w),
                WebFx::Ensure(focus) => fx.push(ReaderEffect::Web(WebEffect::Ensure { focus })),
                WebFx::FocusNode(doc, node) => fx.push(ReaderEffect::Web(WebEffect::Act {
                    doc,
                    node,
                    act: DocAction::Focus,
                })),
            }
        }
        self.web_sync(old, fx, quiet);
        changed
    }

    /// Wake the document the user is in, put the previous one to sleep (DR-3).
    fn web_activate(&mut self, doc: DocId, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        if b.active == Some(doc) {
            return;
        }
        if let Some(prev) = b.active.replace(doc) {
            fx.push(ReaderEffect::Web(WebEffect::Activity {
                doc: prev,
                awake: false,
            }));
        }
        fx.push(ReaderEffect::Web(WebEffect::Activity { doc, awake: true }));
    }

    /// Put the cursor at the first content of `node` and remember its flat offset.
    fn web_place(&mut self, doc: DocId, node: DocNodeId) {
        let Some(b) = self.web.as_mut() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let placed = h.read.with(|d| {
            let off = cursor::content_start(d, node)?;
            Some((cursor::cursor_at(d, off)?, off))
        });
        if let Some((cur, off)) = placed {
            b.last_off = off;
            match &mut b.mode {
                WebMode::Browse { cur: c, .. } => *c = cur,
                WebMode::Focus { resume, .. } => *resume = cur,
                WebMode::Inactive | WebMode::Loading { .. } => {}
            }
        }
    }

    /// Put the cursor on the first line of the page (the line an entry speaks).
    fn web_place_first_line(&mut self, doc: DocId) {
        let Some(b) = self.web.as_mut() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let max_line = b.settings.max_line;
        let placed = h.read.with(|d| match cursor::first_line(d, max_line) {
            Mv::Landed { off, .. } => cursor::cursor_at(d, off).map(|c| (c, off)),
            Mv::Edge(_) => None,
        });
        if let Some((cur, off)) = placed {
            b.last_off = off;
            if let WebMode::Browse { cur: c, .. } = &mut b.mode {
                *c = cur;
            }
        }
    }

    // ---- focus ----------------------------------------------------------------------------

    /// Keep `popup_open` true to the field in focus mode, and the grab set with it: Escape is
    /// ours only while the field has no popup of its own to close.
    pub(super) fn web_refresh_popup(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        let open = match b.mode {
            WebMode::Focus {
                doc,
                node,
                cause: super::FocusCause::Auto,
                ..
            } => b.docs.get(doc).is_some_and(|h| {
                h.read
                    .with(|d| states_of(d, node).contains(StateSet::EXPANDED))
            }),
            WebMode::Focus { .. }
            | WebMode::Browse { .. }
            | WebMode::Loading { .. }
            | WebMode::Inactive => false,
        };
        if open != b.popup_open {
            b.popup_open = open;
            if matches!(self.mode, Mode::Focus) && !self.keys.web_escape.is_empty() {
                fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
            }
        }
    }

    /// The focus joined to a mirrored node, or left web content. Called before the focus is
    /// presented, so the mode word can ride on the focus utterance (BM-5).
    pub(crate) fn web_on_focus(&mut self, node: NodeKey, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        b.entry_wait = None;
        b.grace_fallback = None;
        b.focus_key = Some(node);
        let prev_focus = b.focus_node;
        let hit = b.join.get(node);
        tracing::info!(
            target: "gorca_phase",
            phase = "web_focus",
            hit = hit.is_some(),
            id = ?hit.map(|(_, i)| i),
        );
        if let Some((doc, id)) = hit
            && let Some(h) = b.docs.get(doc)
            && let Some((role, states)) = h.read.with(|d| {
                d.node(id)?;
                Some(role_states(d, id))
            })
        {
            b.counters.join_hits += 1;
            b.focus_node = Some((doc, id));
            // The browser re-focuses the document we are already browsing (after a same-page
            // link, a click): the cursor stays where it is and nothing is announced.
            // Only when we caused it (a click or Enter a moment ago): a document that is focused
            // again with no action of ours is a return to the page and is announced.
            let browsing_here = matches!(b.mode, WebMode::Browse { doc: d, .. } if d == doc)
                && b.activated.is_some_and(|t| {
                    self.now.saturating_duration_since(t) <= REFOCUS_SILENT_WINDOW
                });
            if browsing_here && h.read.with(|d| d.root() == Some(id)) {
                b.suppress_focus = Some(node);
                return;
            }
            // The focus fell onto the document itself while we browse it with the cursor on live
            // content of that document, and nothing of ours caused it: the page destroyed the
            // focused node (a framework re-rendering its list) and the browser parked the focus
            // on the document. That is not a return to the page. The cursor stays where the user
            // is and nothing is announced; moving it to the top would turn the next Enter into
            // a click on whatever comes first on the page (the account menu). Decided from the
            // cursor, not from the focus history, which a loaded host coalesces or reorders.
            // When the cursor's own node is gone (a route change replaced the view) the old
            // behaviour applies: announce the document, start at its top.
            let on_root = h.read.with(|d| d.root() == Some(id));
            let cursor_live = match b.mode {
                WebMode::Browse { doc: d, cur, .. } if d == doc && b.active == Some(doc) => {
                    b.displaced != Some(cur.node)
                        && h.read.with(|dd| {
                            cur.node != id
                                && dd.is_attached(cur.node)
                                && dd.root() != Some(cur.node)
                        })
                }
                WebMode::Inactive
                | WebMode::Loading { .. }
                | WebMode::Browse { .. }
                | WebMode::Focus { .. } => false,
            };
            let prev_in_doc = prev_focus.is_some_and(|(pd, pid)| pd == doc && pid != id);
            if on_root {
                tracing::info!(
                    target: "gorca_phase",
                    phase = "web_focus_root",
                    keep_cursor = cursor_live,
                    prev_focus_in_doc = prev_in_doc,
                );
            }
            if on_root && cursor_live {
                b.suppress_focus = Some(node);
                return;
            }
            if let Some((t, at)) = b.click_target.take()
                && t == id
                && self.now.saturating_duration_since(at) < CLICK_FOCUS_WINDOW
            {
                b.suppress_focus = Some(node);
            }
            // A nested document taking the focus on its way to its content (Chromium focuses the
            // frame's document before the link inside it): the page is browsed, not announced
            // frame by frame.
            if matches!(b.mode, WebMode::Browse { .. })
                && h.read.with(|d| {
                    d.node(id)
                        .is_some_and(|v| v.cats.intersects(gorca_doc::web::Cat::FRAME))
                })
            {
                b.suppress_focus = Some(node);
                let due = self.now + FRAME_FOCUS_DEFER;
                b.frame_focus = Some((node, due));
                fx.push(ReaderEffect::ArmTimer(due));
            }
            let passes = forms::passes_through(role, states, Reason::Focus);
            let application = forms::is_application(role);
            let last_before = match b.mode {
                WebMode::Browse { doc: d, cur, .. } if d == doc => Some(cur.node),
                WebMode::Focus { doc: d, node, .. } if d == doc => Some(node),
                WebMode::Inactive
                | WebMode::Loading { .. }
                | WebMode::Browse { .. }
                | WebMode::Focus { .. } => None,
            };
            // The focus moved into another document than the one we were in (a tab switch, a
            // window switch, or a return from the browser chrome to a different page) and lands
            // on something inside it: the page's title leads, as when the document itself is
            // focused. A document that is still loading announces itself on adoption.
            let switched = self.web.as_ref().is_some_and(|b| {
                b.active != Some(doc)
                    && b.suppress_focus != Some(node)
                    && !matches!(b.mode, WebMode::Loading { .. })
            });
            self.web_activate(doc, fx);
            self.web_apply(
                WebEv::FocusIn {
                    doc,
                    node: id,
                    passes,
                    application,
                },
                fx,
                false,
            );
            if matches!(self.web_mode(), WebMode::Browse { .. }) {
                self.web_place(doc, id);
                // The document itself gaining focus is not a "focus move": Tab from the top of a
                // page still walks the mirror.
                if let Some(b) = self.web.as_mut()
                    && let Some(h) = b.docs.get(doc)
                {
                    let on_control = h.read.with(|d| {
                        !is_document(d, id)
                            && d.node(id)
                                .is_some_and(|v| v.cats.intersects(Cat::FOCUSABLE))
                    });
                    b.cursor_from = if on_control {
                        CursorFrom::Focus
                    } else {
                        CursorFrom::Own
                    };
                }
                self.web_root_entry(doc, id);
                self.web_container_tail(doc, id);
            }
            self.web_dialog_lead(doc, id, last_before);
            self.web_listbox_lead(doc, id, last_before);
            self.web_group_lead(doc, id, last_before);
            if switched {
                self.web_document_lead(doc, id);
            }
            self.web_refresh_popup(fx);
            // The cursor jumped with the focus: Tab availability changed with it (R-5), and no
            // scroll timer is armed to notice.
            self.web_sync_tab_grabs(fx);
            // A focus move is a new place: nothing is displaced, and what is under the cursor now
            // is what the user heard.
            if let Some(b) = self.web.as_mut() {
                b.displaced = None;
            }
            self.note_cursor_sig();
            return;
        }
        // IR-3: the join missed. Browse keys must never be grabbed while focus may be in a
        // field we have not mirrored, so this is `Loading` (grabs off) until it is resolved.
        if b.web_apps.contains(&node.app) {
            // The browser's own interface (address bar, its suggestion list, tab strip, menus):
            // the whole ancestor chain is known and holds no document, so the focus is not in
            // web content and must neither wait for a document nor say a mode word.
            if self.outside_every_document(node) {
                if let Some(b) = self.web.as_mut() {
                    b.focus_node = None;
                }
                self.web_apply(WebEv::FocusAway, fx, false);
                return;
            }
            let Some(b) = self.web.as_mut() else { return };
            b.counters.join_misses += 1;
            let (editable, known) = match self.dir.lookup(node) {
                Lookup::Hit(snap) => (
                    forms::passes_through(snap.role, snap.states, Reason::Focus),
                    true,
                ),
                Lookup::Miss | Lookup::Gone => (false, false),
            };
            let now = self.now;
            if self.web_apply(
                WebEv::FocusPending {
                    focus: node,
                    editable,
                    known,
                    now,
                },
                fx,
                false,
            ) {
                fx.push(ReaderEffect::ArmTimer(now + LOADING_GRACE));
            }
        } else {
            if let Some(b) = self.web.as_mut() {
                b.focus_node = None;
            }
            self.web_apply(WebEv::FocusAway, fx, false);
        }
    }

    /// True when every ancestor of `node` up to the top is in the directory and none is a
    /// document: the node is in an application's own interface, not in web content. Any gap in
    /// the chain gives false (unknown), so a cold web node keeps the Loading path.
    fn outside_every_document(&self, node: NodeKey) -> bool {
        let mut cur = node;
        for _ in 0..32 {
            let Some(sn) = self.dir.lookup(cur).hit() else {
                return false;
            };
            if matches!(sn.role, Role::DocumentWeb | Role::Document) {
                return false;
            }
            match sn.parent {
                Some(p) => cur = p,
                None => return sn.role == Role::Application,
            }
        }
        false
    }

    /// A key press while the focus is in a web application but the web layer is not browsing
    /// (`Inactive` or still `Loading`): the document lookup that should have settled the focus
    /// failed or never answered (a busy page, a lookup that timed out) and nothing else retries
    /// it until the next focus event. Resolve the focus again, at most once per
    /// `KEY_RESYNC_INTERVAL`. True when it started.
    pub(crate) fn web_resync_on_key(&mut self, node: NodeKey, fx: &mut ReaderEffects) -> bool {
        let now = self.now;
        let Some(b) = self.web.as_mut() else {
            return false;
        };
        if !b.web_apps.contains(&node.app)
            || matches!(b.mode, WebMode::Browse { .. } | WebMode::Focus { .. })
            || b.resync_at
                .is_some_and(|at| now.saturating_duration_since(at) < KEY_RESYNC_INTERVAL)
        {
            return false;
        }
        b.resync_at = Some(now);
        b.counters.key_resyncs += 1;
        tracing::debug!(target: "gorca_phase", phase = "web_key_resync", mode = ?b.mode);
        if let WebMode::Loading { focus, .. } = b.mode {
            // Waiting for a document that never came: ask for it again, and look the join up.
            fx.push(ReaderEffect::Web(WebEffect::Ensure { focus }));
            self.web_retry_loading(false, fx);
        } else {
            self.web_on_focus(node, fx);
        }
        true
    }

    /// Whether the document mirror holds `node` (a freshly inserted subtree is not there yet).
    pub(crate) fn web_knows(&self, node: NodeKey) -> bool {
        self.web
            .as_ref()
            .is_some_and(|b| b.join.get(node).is_some())
    }

    /// The menu (role and name) a web menu item is entered through, when focus comes from
    /// outside that menu: `from` (the node presented before) is neither the menu nor one of its
    /// items. `None` for anything else, so a move between items speaks the item alone.
    pub(crate) fn web_menu_entered(
        &self,
        item: NodeKey,
        from: Option<NodeKey>,
    ) -> Option<(Role, Arc<str>)> {
        let b = self.web.as_ref()?;
        let (doc, id) = b.join.get(item)?;
        let h = b.docs.get(doc)?;
        let came_from = from.and_then(|f| b.join.get(f)).filter(|(d, _)| *d == doc);
        h.read.with(|d| {
            if d.node(id)?.role != Role::MenuItem {
                return None;
            }
            let menu = d.parent(id)?;
            let v = d.node(menu)?;
            if !matches!(v.role, Role::Menu | Role::MenuBar) {
                return None;
            }
            if let Some((_, f)) = came_from
                && (f == menu || d.parent(f) == Some(menu))
            {
                return None;
            }
            let name = v.name.filter(|n| !n.is_empty())?;
            Some((v.role, Arc::from(name)))
        })
    }

    /// A cold focus on a mirrored web node: its snapshot from the document mirror (role, states,
    /// name, value), so the first focus into a field is spoken at once and the fetch only
    /// confirms it (the mirror knows the node: this is what the page last told us).
    pub(crate) fn web_focus_snapshot(&self, node: NodeKey) -> Option<gorca_model::NodeSnapshot> {
        use gorca_model::{ChildList, Freshness, Ifaces, PropMask, Relations};
        let b = self.web.as_ref()?;
        let (doc, id) = b.join.get(node)?;
        let h = b.docs.get(doc)?;
        h.read.with(|d| {
            let v = d.node(id)?;
            // A document root keeps the full path: its title, the mode word and the first line
            // are arranged around the fetch (BM-5, W3).
            if v.flags.contains(gorca_doc::web::NodeFlags::HIDDEN)
                || matches!(v.role, Role::DocumentWeb | Role::Document)
            {
                return None;
            }
            let name = v.name.filter(|n| !n.is_empty())?;
            let meta = v.meta?;
            // An invalid control whose error text the mirror lacks (the source reads a bounded
            // number per fetch) takes the full focus fetch, which reads it (M8b).
            if meta.states.contains(StateSet::INVALID)
                && gorca_doc_source_api::takes_error_text(v.role)
                && !meta
                    .attrs
                    .iter()
                    .any(|(k, _)| *k == gorca_doc_source_api::web_attrs::ERROR_TEXT)
            {
                return None;
            }
            let mut have = PropMask::NAME | PropMask::ROLE | PropMask::STATES;
            // A password's value is never spoken, not even from the mirror.
            let value = meta.value.clone().filter(|_| v.role != Role::PasswordEntry);
            if value.is_some() {
                have |= PropMask::VALUE;
            }
            Some(gorca_model::NodeSnapshot {
                key: node,
                role: v.role,
                states: meta.states,
                name: Some(Arc::from(name)),
                description: meta.description.clone(),
                value,
                parent: None,
                children: ChildList::Unknown,
                interfaces: Ifaces::empty(),
                attrs: meta.attrs.clone(),
                relations: Relations::default(),
                bounds: None,
                have,
                confirmed_seq: 0,
                fresh: Freshness::Stale {
                    since: Instant::now(),
                    mask: PropMask::VALUE,
                },
                sem: gorca_model::NodeSem {
                    rec: None,
                    entry: meta.sem.clone(),
                },
            })
        })
    }

    /// N2: the focus fetch failed and nothing is cached: speak a web node from the document
    /// mirror (its name and role are there). False when the node is not in a mirrored document.
    pub(crate) fn web_focus_fallback(&mut self, node: NodeKey, fx: &mut ReaderEffects) -> bool {
        let Some(b) = self.web.as_ref() else {
            return false;
        };
        let Some((doc, id)) = b.join.get(node) else {
            return false;
        };
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let items = h.read.with(|d| present::describe_node(d, id));
        if items.is_empty() {
            return false;
        }
        self.web_speak(&items, fx);
        true
    }

    /// The focus snapshot arrived (a presentation reply): a `Loading` focus learns whether it is
    /// a field. A field keeps the grabs off for good; a plain node lets the grace timer run.
    pub(crate) fn web_learn_focus(
        &mut self,
        snap: &gorca_model::NodeSnapshot,
        fx: &mut ReaderEffects,
    ) {
        let Some(b) = self.web.as_mut() else { return };
        let WebMode::Loading {
            doc,
            since,
            focus,
            known: false,
            ..
        } = b.mode
        else {
            return;
        };
        if focus != snap.key {
            return;
        }
        let editable = forms::passes_through(snap.role, snap.states, Reason::Focus);
        b.mode = WebMode::Loading {
            doc,
            since,
            focus,
            editable,
            known: true,
        };
        if editable {
            self.web_retry_loading(true, fx);
        }
    }

    /// Focus moved into another document (a tab or window switch) onto a node inside it: the
    /// page title leads the focus speech. Focus on the document itself speaks its own name.
    fn web_document_lead(&mut self, doc: DocId, id: DocNodeId) -> bool {
        let Some(b) = self.web.as_mut() else {
            return false;
        };
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let title = h.read.with(|d| {
            if d.root() == Some(id) || is_document(d, id) {
                return None;
            }
            title_of(d)
        });
        let Some(title) = title else {
            return false;
        };
        let mut lead = vec![Item::Lead(title)];
        lead.extend(b.pending_tail.take().map(|(t, _)| t).unwrap_or_default());
        b.pending_tail = Some((lead, self.now));
        true
    }

    /// Focus entered a dialog (its name and role lead the focus speech, once): the dialog around
    /// `id` does not hold the cursor's previous place.
    fn web_dialog_lead(&mut self, doc: DocId, id: DocNodeId, before: Option<DocNodeId>) -> bool {
        let Some(b) = self.web.as_mut() else {
            return false;
        };
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let last_lead = b.lead_dialog;
        let lead = h.read.with(|d| {
            let mut cur = Some(id);
            for _ in 0..32 {
                let n = cur?;
                let v = d.node(n)?;
                if v.cats.intersects(Cat::DIALOG)
                    && !v.flags.contains(gorca_doc::web::NodeFlags::HIDDEN)
                {
                    // The previous place was inside this dialog (a node test: the flat offsets
                    // shift when the dialog appears).
                    let holds = before.is_some_and(|p| {
                        let mut c = Some(p);
                        for _ in 0..64 {
                            match c {
                                Some(x) if x == n => return true,
                                Some(x) => c = d.parent(x),
                                None => return false,
                            }
                        }
                        false
                    });
                    if holds || last_lead == Some(n) {
                        return Some((n, Vec::new()));
                    }
                    let mut out = Vec::new();
                    if let Some(name) = v.name.filter(|n| !n.is_empty()) {
                        out.push(Item::Lead(name.to_owned()));
                    }
                    out.push(Item::Lead(t(M::RoleDialog).to_owned()));
                    return Some((n, out));
                }
                cur = d.parent(n);
            }
            None
        });
        // Remember the dialog announced; leaving every dialog forgets it.
        b.lead_dialog = lead.as_ref().map(|(n, _)| *n);
        if let Some((_, mut lead)) = lead
            && !lead.is_empty()
        {
            let now = self.now;
            let rest = b.pending_tail.take().map(|(t, _)| t).unwrap_or_default();
            lead.extend(rest);
            b.pending_tail = Some((lead, now));
            return true;
        }
        false
    }

    /// Focus landed in a named group (a `fieldset` with its legend, a `role=group`) that the
    /// focus was not in: the group's name and "group" lead the focus speech, once, as Orca says
    /// the legend when the focus enters a fieldset. Browsers expose the legend as the group's
    /// name, never as the control's, so nothing else would say it.
    fn web_group_lead(&mut self, doc: DocId, id: DocNodeId, before: Option<DocNodeId>) -> bool {
        let Some(b) = self.web.as_mut() else {
            return false;
        };
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let last = b.lead_group;
        let found = h.read.with(|d| {
            // Only a control is announced into its group, not a node the cursor walks over.
            if !d.node(id)?.cats.intersects(Cat::FORM_FIELD) {
                return None;
            }
            let mut cur = d.parent(id);
            for _ in 0..8 {
                let n = cur?;
                let v = d.node(n)?;
                if is_group(&v) {
                    let name = v.name.filter(|x| !crate::present::name_is_blank(Some(x)))?;
                    let inside = before.is_some_and(|p| {
                        let mut c = Some(p);
                        for _ in 0..64 {
                            match c {
                                Some(x) if x == n => return true,
                                Some(x) => c = d.parent(x),
                                None => return false,
                            }
                        }
                        false
                    });
                    if inside || last == Some(n) {
                        return Some((n, Vec::new()));
                    }
                    return Some((
                        n,
                        vec![
                            Item::Lead(name.to_owned()),
                            Item::Lead(t(M::WebGroup).to_owned()),
                        ],
                    ));
                }
                cur = d.parent(n);
            }
            None
        });
        b.lead_group = found.as_ref().map(|(n, _)| *n);
        if let Some((_, mut lead)) = found
            && !lead.is_empty()
        {
            let now = self.now;
            let rest = b.pending_tail.take().map(|(t, _)| t).unwrap_or_default();
            lead.extend(rest);
            b.pending_tail = Some((lead, now));
            return true;
        }
        false
    }

    /// Focus landed on an option of a list box the focus was not in: the list box's name leads
    /// ("Toppings list box, Cheese selected").
    fn web_listbox_lead(&mut self, doc: DocId, id: DocNodeId, before: Option<DocNodeId>) -> bool {
        let Some(b) = self.web.as_mut() else {
            return false;
        };
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let lead = h.read.with(|d| {
            if d.node(id)?.role != Role::ListItem {
                return None;
            }
            let mut cur = d.parent(id);
            for _ in 0..4 {
                let n = cur?;
                let v = d.node(n)?;
                if v.cats.intersects(Cat::LISTBOX) {
                    let inside = before.is_some_and(|p| {
                        let mut c = Some(p);
                        for _ in 0..64 {
                            match c {
                                Some(x) if x == n => return true,
                                Some(x) => c = d.parent(x),
                                None => return false,
                            }
                        }
                        false
                    });
                    if inside {
                        return None;
                    }
                    let mut out = Vec::new();
                    if let Some(name) = v.name.filter(|n| !n.is_empty()) {
                        out.push(Item::Lead(name.to_owned()));
                    }
                    out.push(Item::Lead(t(M::WebListBox).to_owned()));
                    return Some(out);
                }
                cur = d.parent(n);
            }
            None
        });
        if let Some(mut lead) = lead {
            let now = self.now;
            let rest = b.pending_tail.take().map(|(t, _)| t).unwrap_or_default();
            lead.extend(rest);
            b.pending_tail = Some((lead, now));
            return true;
        }
        false
    }

    /// Focus moved to a container (a landmark, a `tabindex=-1` block: the target of a skip
    /// link): the cursor is at its first text, and that line follows the focus speech, as NVDA
    /// reads the line at the caret when focus lands on non-interactive content.
    fn web_container_tail(&mut self, doc: DocId, id: DocNodeId) {
        let Some(b) = self.web.as_mut() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let max_line = b.settings.max_line;
        let now = self.now;
        let items = h.read.with(|d| {
            let v = d.node(id)?;
            let plain = matches!(
                v.role,
                Role::Landmark
                    | Role::Panel
                    | Role::Unknown
                    | Role::Paragraph
                    | Role::List
                    | Role::ListItem
                    | Role::Table
                    | Role::Cell
                    | Role::Document
            );
            if !plain || d.root() == Some(id) {
                return None;
            }
            let off = cursor::content_start(d, id)?;
            let end = d.subtree_text_range(id)?.1;
            if off >= end {
                return None;
            }
            Some(line_items(d, max_line, off))
        });
        if let Some(items) = items.filter(|i| !i.is_empty()) {
            b.pending_tail = Some((items, now));
        }
    }

    /// Focus landed on the document itself (a tab switch, a page load): put the cursor where it
    /// was, or at the first line, and have that line follow the focus speech (W3).
    fn web_root_entry(&mut self, doc: DocId, id: DocNodeId) {
        let Some(b) = self.web.as_mut() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let saved = b.saved.remove(&doc);
        let max_line = b.settings.max_line;
        let now = self.now;
        let placed = h.read.with(|d| {
            if d.root() != Some(id) {
                return None;
            }
            let off = match saved {
                Some(sv) if d.is_attached(sv.cur.node) => cursor::offset_of(d, sv.cur),
                Some(sv) => Some(sv.off.min(d.text_len().saturating_sub(1))),
                None => match cursor::first_line(d, max_line) {
                    Mv::Landed { off, .. } => Some(off),
                    Mv::Edge(_) => None,
                },
            }?;
            Some((
                cursor::cursor_at(d, off)?,
                off,
                line_items(d, max_line, off),
            ))
        });
        if let Some((cur, off, items)) = placed {
            if let WebMode::Browse { cur: c, .. } = &mut b.mode {
                *c = cur;
            }
            b.last_off = off;
            b.pending_tail = Some((items, now));
        } else if b
            .docs
            .get(doc)
            .is_some_and(|h| h.read.with(|d| d.root() == Some(id)))
        {
            // Nothing readable yet (the first publish holds the root only): speak the first
            // line when the content arrives (M4).
            b.entry_wait = Some((doc, now));
        }
    }

    // ---- notices from the document side ---------------------------------------------------

    pub(crate) fn on_doc(&mut self, n: DocNotice, fx: &mut ReaderEffects) {
        match n {
            DocNotice::WebApp { app } => {
                if let Some(b) = self.web.as_mut() {
                    b.web_apps.insert(app);
                }
            }
            DocNotice::FirstUseful { doc, app } => self.web_first_useful(doc, app, fx),
            DocNotice::Loaded { node } => self.web_loaded(node, fx),
            DocNotice::Changed { doc, .. } => self.web_changed(doc, fx),
            DocNotice::InDocument { focus, doc } => {
                if let Some(b) = self.web.as_mut()
                    && let WebMode::Loading {
                        focus: f,
                        since,
                        editable,
                        known,
                        ..
                    } = b.mode
                    && f == focus
                {
                    b.mode = WebMode::Loading {
                        doc: Some(doc),
                        since,
                        focus: f,
                        editable,
                        known,
                    };
                }
                self.web_retry_loading(false, fx);
            }
            DocNotice::NoDocument { focus } => {
                self.web_apply(WebEv::NoDocument { focus }, fx, true);
            }
            DocNotice::Live { doc, msg } => self.web_live(doc, &msg, fx),
            DocNotice::SelectionDone { doc, req, outcome } => {
                self.web_selection_done(doc, req, outcome, fx);
            }
            DocNotice::Label { req, .. } => self.web_label_done(req, fx),
            DocNotice::FocusFailed { doc } => {
                // The grabbed Tab asked for a focus the page refused: say so (R-6).
                if self.web.as_ref().is_some_and(|b| b.mode.doc() == Some(doc)) {
                    self.say(t(M::WebCantMoveFocus), fx);
                }
            }
            DocNotice::ActionFailed { doc, gone } => {
                // The click did nothing. Say why, never leave Enter silent. (The source already
                // refreshed the parents of a node that was gone, so the mirror catches up.)
                if let Some(b) = self.web.as_mut()
                    && b.mode.doc() == Some(doc)
                {
                    b.activated = None;
                    b.click_target = None;
                    self.say(
                        t(if gone {
                            M::WebItemGone
                        } else {
                            M::WebCantActivate
                        }),
                        fx,
                    );
                }
            }
            DocNotice::ActionDisplaced { doc, now } => {
                // The source did not click: the object is not what was spoken. Say what it is
                // now; the mirror is being refreshed and the next Enter acts on that name.
                if let Some(b) = self.web.as_mut()
                    && b.mode.doc() == Some(doc)
                {
                    let target = b.activated_node.map(|a| a.target);
                    b.activated = None;
                    b.click_target = None;
                    b.activated_node = None;
                    b.displaced = None;
                    b.announced_change = None;
                    // Remember what is said only when a name is said: without one the next
                    // Enter must still announce the refreshed object.
                    let said = match now {
                        Some(name) if !name.is_empty() => {
                            let role = target.and_then(|n| {
                                b.docs.get(doc).and_then(|h| {
                                    h.read.with(|d| d.node(n).map(|_| role_states(d, n).0))
                                })
                            });
                            if let (Some(node), Some(role)) = (target, role) {
                                b.announced_change = Some(super::AnnouncedChange {
                                    node,
                                    name: Arc::clone(&name),
                                    role,
                                    at: self.now,
                                });
                            }
                            format!("{}: {name}", t(M::WebItemChanged))
                        }
                        Some(_) | None => t(M::WebItemChanged).to_owned(),
                    };
                    self.say(&said, fx);
                }
            }
            DocNotice::Gone { doc } => {
                if let Some(b) = self.web.as_mut() {
                    b.displaced = None;
                    b.grace_fallback = None;
                    b.saved.remove(&doc);
                    if b.active == Some(doc) {
                        b.active = None;
                    }
                }
                self.web_apply(WebEv::DocGone { doc }, fx, true);
            }
        }
    }

    /// Is the user's focus in application `app`?
    fn focus_in_app(&self, app: AppId) -> bool {
        match self.focus {
            FocusState::Focused { node, .. } => node.app == app,
            FocusState::Settling { candidate, .. } => candidate.app == app,
            FocusState::Orphaned { last, .. } => last.app == app,
            FocusState::NoFocus => false,
        }
    }

    /// The document was published for the first time.
    fn web_first_useful(&mut self, doc: DocId, app: AppId, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let Some(root) = h.read.with(WebDoc::root) else {
            return;
        };
        match b.mode {
            WebMode::Loading { focus, .. } => {
                // Focus was waiting for this document: resolve it through the join.
                let focus_node = b.join.get(focus).filter(|(d, _)| *d == doc).map(|(_, id)| {
                    let (role, states) = h.read.with(|d| role_states(d, id));
                    (id, forms::passes_through(role, states, Reason::Focus))
                });
                // A document the focus was not in before (the first visit of a tab): the page
                // title leads when the focus sits inside it rather than on its root.
                let switched = b.active != Some(doc);
                self.web_activate(doc, fx);
                self.web_apply(
                    WebEv::FirstUseful {
                        doc,
                        focus: focus_node,
                        top: root,
                    },
                    fx,
                    true,
                );
                // Focus on the document itself already spoke its title: the first line follows.
                let at_root = focus_node.is_some_and(|(n, _)| n == root);
                let at_top = focus_node.is_none();
                self.web_place(doc, focus_node.map_or(root, |(n, _)| n));
                if at_top || at_root {
                    // The first line is about to be spoken: the cursor sits on it, so that Down
                    // reads the line after it (Chromium announces the page by itself when its
                    // window activates, with no focus event of ours to place the cursor).
                    self.web_place_first_line(doc);
                }
                // The focus utterance went out before the page was known, without its title:
                // say the mode word, the title and the focus again in one piece, worded as the
                // focus presenter words it (it replaces the first, short utterance).
                let titled = match focus_node {
                    Some((n, _)) if switched && n != root => self.web.as_ref().and_then(|b| {
                        b.docs
                            .get(doc)?
                            .read
                            .with(|d| Some((title_of(d)?, present::describe_node(d, n))))
                    }),
                    Some(_) | None => None,
                };
                let spoke = match titled {
                    Some((title, items)) => {
                        self.web_speak_cold_focus(focus, title, &items, fx);
                        true
                    }
                    None => self.web_speak_entry(doc, at_top || at_root, at_top, fx),
                };
                if (at_top || at_root) && !self.dir.is_settling(app, self.now) {
                    self.web_auto_read(doc, spoke, fx);
                }
            }
            WebMode::Inactive => {
                // A page load in the application the user is in: adopt the new document.
                if self.focus_in_app(app) {
                    self.web_activate(doc, fx);
                    self.web_apply(
                        WebEv::FocusIn {
                            doc,
                            node: root,
                            passes: false,
                            application: false,
                        },
                        fx,
                        true,
                    );
                    self.web_place(doc, root);
                    self.web_place_first_line(doc);
                    let spoke = self.web_speak_entry(doc, true, true, fx);
                    if !self.dir.is_settling(app, self.now) {
                        self.web_auto_read(doc, spoke, fx);
                    }
                }
            }
            WebMode::Browse { .. } | WebMode::Focus { .. } => {}
        }
    }

    /// Say the mode word and, at the top of a page, its title and first line.
    /// With page auto-read on, the first line is left to the reading that follows. Returns
    /// whether anything was spoken (the reading then queues behind it).
    fn web_speak_entry(
        &mut self,
        doc: DocId,
        with_page: bool,
        title: bool,
        fx: &mut ReaderEffects,
    ) -> bool {
        let reads = self.cmd.effective.browse.say_all_on_load;
        let Some(b) = self.web.as_ref() else {
            return false;
        };
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let mode_browse = matches!(b.mode, WebMode::Browse { .. });
        let max_line = b.settings.max_line;
        let items = if with_page && mode_browse {
            h.read.with(|d| {
                let mut items = Vec::new();
                if title && let Some(t) = title_of(d) {
                    items.push(Item::Text(t));
                }
                if !reads && let Mv::Landed { speak, .. } = cursor::first_line(d, max_line) {
                    items.extend(present::describe(d, speak.start, speak.end));
                }
                items
            })
        } else {
            Vec::new()
        };
        let spoke = !items.is_empty() || self.web_has_word();
        self.web_speak(&items, fx);
        spoke
    }

    /// The first visit of a tab: the focus presented before the page was mirrored is spoken
    /// again with the mode word and the page title in front, in the focus presenter's wording
    /// (`items`, the mirror's wording, only when that presentation is gone).
    fn web_speak_cold_focus(
        &mut self,
        focus: NodeKey,
        title: String,
        items: &[Item],
        fx: &mut ReaderEffects,
    ) {
        let presented = self.presented.clone().filter(|p| p.node == focus);
        let Some(p) = presented else {
            let mut all = vec![Item::Lead(title)];
            all.extend(items.iter().cloned());
            self.web_speak(&all, fx);
            return;
        };
        let word = self.web_take_word();
        let id = self.utt_id();
        let mut u = self
            .build_focus(
                crate::PresentContext::Focus,
                &p.snap,
                id,
                self.epoch.current(),
                false,
            )
            .utterance;
        u.prio = Prio::Interrupt;
        u.may_refine = true;
        let front = word
            .into_iter()
            .map(Arc::<str>::from)
            .chain([Arc::from(title)]);
        for (at, text) in front.enumerate() {
            u.segments.insert(at, Segment::new(text, SegmentKind::Text));
        }
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// Is a mode word waiting for the next utterance?
    fn web_has_word(&self) -> bool {
        self.web.as_ref().is_some_and(|b| {
            b.pending_word
                .is_some_and(|(_, at)| self.now.saturating_duration_since(at) <= WORD_TTL)
        })
    }

    fn web_changed(&mut self, doc: DocId, fx: &mut ReaderEffects) {
        self.web_changed_inner(doc, fx);
        self.note_cursor_sig();
    }

    /// Remember what is under the browse cursor now, to notice a later silent replacement.
    pub(super) fn note_cursor_sig(&mut self) {
        let Some(b) = self.web.as_mut() else { return };
        let WebMode::Browse { doc, cur, .. } = b.mode else {
            b.cursor_sig = None;
            return;
        };
        let sig = b
            .docs
            .get(doc)
            .and_then(|h| h.read.with(|d| content_sig(d, cur.node)));
        b.cursor_sig = sig.map(|s| (cur.node, s));
    }

    /// CHR-0 B-2: the loading grace put the cursor at the top of the page while the focus was
    /// not mirrored. The mirror caught up (a repair, an update). If the focus is still the same
    /// and the user has not moved the cursor, join the focus now, as the first focus would have.
    fn web_rejoin_after_grace(&mut self, doc: DocId, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        let Some(g) = b.grace_fallback else { return };
        if g.doc != doc {
            return;
        }
        if self.now.saturating_duration_since(g.at) > super::GRACE_REJOIN_WINDOW {
            if let Some(b) = self.web.as_mut() {
                b.grace_fallback = None;
            }
            return;
        }
        let at_root =
            matches!(b.mode, WebMode::Browse { doc: d, cur, .. } if d == doc && cur.node == g.root);
        // A focus change clears the fallback (`web_on_focus`), so the focus is still the one that
        // waited; only the cursor can have moved.
        if !at_root {
            if let Some(b) = self.web.as_mut() {
                b.grace_fallback = None;
            }
            return;
        }
        let Some((jd, id)) = b.join.get(g.focus) else {
            return;
        };
        let Some(h) = b.docs.get(jd) else { return };
        let (role, states) = h.read.with(|d| role_states(d, id));
        let passes = forms::passes_through(role, states, Reason::Focus);
        let application = forms::is_application(role);
        if let Some(b) = self.web.as_mut() {
            b.grace_fallback = None;
            b.focus_node = Some((jd, id));
        }
        tracing::info!(target: "gorca_phase", phase = "web_rejoin_after_grace", passes);
        self.web_apply(
            WebEv::FocusIn {
                doc: jd,
                node: id,
                passes,
                application,
            },
            fx,
            false,
        );
        if matches!(self.web_mode(), WebMode::Browse { .. }) {
            self.web_place(jd, id);
        }
        self.web_speak_line_here(fx);
        self.web_sync_tab_grabs(fx);
    }

    fn web_changed_inner(&mut self, doc: DocId, fx: &mut ReaderEffects) {
        self.web_refresh_popup(fx);
        self.web_rejoin_after_grace(doc, fx);
        self.web_late_entry(doc, fx);
        let Some(b) = self.web.as_mut() else { return };
        let mut resync_tabs = false;
        match b.mode {
            WebMode::Loading { .. } => self.web_retry_loading(false, fx),
            WebMode::Browse { doc: d, cur, .. } if d == doc => {
                // The mirror changed under the cursor: follow the node, or the flat offset. A
                // re-render replaces nodes in several publishes: for a moment after the node
                // was lost the offset is re-applied, so the cursor settles on the same place in
                // the new content (position restore, ADR-0003 10.3).
                let Some(h) = b.docs.get(doc) else { return };
                // The node under the cursor survived but now holds other content: the page
                // re-used it for another item (the identity pass pairs a removed row with its
                // neighbour). The user has not seen this item.
                // The relabel of the control the user just clicked (Play to Pause, Show more to
                // Show less) is the answer to the click, not a swap: same cursor node, same
                // control, same role, one of its pressed, checked or expanded states actually flipped,
                // and only for a moment (a name change without a state flip, "Delete" to
                // "Confirm delete", is a different action and is announced). Anything
                // else under the cursor, a different node, a changed role, a link, a late
                // change, is displacement and costs the next Enter its click.
                let just_activated = b.activated_node.is_some_and(|a| {
                    self.now.saturating_duration_since(a.at) < super::CARET_FOLLOW
                        && a.cursor == cur.node
                        && h.read.with(|dd| {
                            let (role, st) = role_states(dd, a.target);
                            dd.is_attached(a.target)
                                && role == a.role
                                && (st & TOGGLE_STATES) != a.toggles
                        })
                });
                if !just_activated
                    && let Some((n, sig)) = b.cursor_sig
                    && n == cur.node
                    && h.read
                        .with(|dd| content_sig(dd, n).is_some_and(|now| now != sig))
                {
                    // The refresh of the object whose new name was just spoken is not a new
                    // displacement; any other change (another label, another role, too late)
                    // is, and the next Enter announces it again.
                    let spoken = b.announced_change.take().is_some_and(|a| {
                        self.now.saturating_duration_since(a.at) <= super::ANNOUNCE_WINDOW
                            && h.read.with(|dd| {
                                dd.node(a.node).is_some_and(|v| {
                                    v.name == Some(&*a.name) && role_states(dd, a.node).0 == a.role
                                })
                            })
                    });
                    if !spoken {
                        b.displaced = Some(n);
                    }
                }
                tracing::debug!(
                    target: "gorca_phase",
                    phase = "web_changed",
                    cur = ?cur.node,
                    sig_node = ?b.cursor_sig.map(|(n, _)| n),
                    attached = h.read.with(|dd| dd.is_attached(cur.node)),
                );
                let last = b.last_off;
                let now = self.now;
                let window = b
                    .restore_until
                    .is_some_and(|(t, off)| now < t && off == last);
                let lost = h.read.with(|d| !d.is_attached(cur.node));
                if !lost {
                    // Remember where the node sits, for the day the page removes all of it.
                    b.cursor_chain = h.read.with(|d| ancestor_chain(d, cur.node));
                    b.cursor_item = h.read.with(|d| item_of(d, cur.node, last));
                }
                // The page removed the cursor's whole subtree (a pane closed) and the flat offset
                // now lies past the end of what is left: re-anchor to the nearest surviving
                // ancestor instead of the end of the document, where no Tab target and nothing
                // to click remain.
                // The item the user was reading comes back under another row object (a virtual
                // list re-rendered): follow the item by its DOM id before the structural guess.
                let anchored = h.read.with(|d| {
                    if !lost {
                        return None;
                    }
                    b.cursor_item
                        .as_ref()
                        .and_then(|(id, delta)| find_item(d, id, *delta))
                        .or_else(|| reanchor(d, &b.cursor_chain))
                });
                let fixed = match anchored {
                    Some(a) => Some(a),
                    None => h.read.with(|d| {
                        if d.is_attached(cur.node) && !window {
                            None
                        } else {
                            cursor::cursor_at(d, last)
                                .filter(|c| c.node != cur.node)
                                .map(|c| (c, last))
                        }
                    }),
                };
                if lost {
                    b.restore_until = Some((now + RESTORE_WINDOW, last));
                }
                if let Some((c, off)) = fixed
                    && let WebMode::Browse { cur, .. } = &mut b.mode
                {
                    *cur = c;
                    b.last_off = off;
                    // The page took the node the user was on and the cursor settled on
                    // another: the next Enter says where it is before it clicks.
                    b.displaced = Some(c.node);
                }
                // The cursor moved without a key: Tab availability follows it (D-2); a grab that
                // was right for the old place would keep eating Tab for good.
                if lost {
                    b.tab_fallback = true;
                }
                resync_tabs = lost;
            }
            WebMode::Focus { .. } | WebMode::Browse { .. } | WebMode::Inactive => {}
        }
        if resync_tabs {
            self.web_sync_tab_grabs(fx);
        }
        self.web_loading_cue(doc, fx);
    }

    /// The text grew after an edge hit that followed our scroll request: the list is loading
    /// more. Said once per edge visit; a page that did not grow stays with "top"/"bottom".
    fn web_loading_cue(&mut self, doc: DocId, fx: &mut ReaderEffects) {
        let now = self.now;
        let Some(b) = self.web.as_mut() else { return };
        let Some((at, len)) = b.edge_wait else { return };
        if now.saturating_duration_since(at) >= LOADING_CUE_WINDOW {
            b.edge_wait = None;
            return;
        }
        let grown = b
            .docs
            .get(doc)
            .is_some_and(|h| h.read.with(|d| d.text_len() > len));
        if grown {
            b.edge_wait = None;
            self.say(t(M::WebLoading), fx);
        }
    }

    /// M4: the document's root got focus before it had a line; speak the first line now, if
    /// nothing else happened since and the wait is not stale.
    fn web_late_entry(&mut self, doc: DocId, fx: &mut ReaderEffects) {
        let now = self.now;
        let Some(b) = self.web.as_mut() else { return };
        let Some((d, at)) = b.entry_wait else { return };
        if d != doc {
            return;
        }
        if now.saturating_duration_since(at) > ENTRY_WAIT {
            b.entry_wait = None;
            return;
        }
        let WebMode::Browse { doc: cur_doc, .. } = b.mode else {
            return;
        };
        if cur_doc != doc {
            return;
        }
        let Some(h) = b.docs.get(doc) else { return };
        let max_line = b.settings.max_line;
        let placed = h.read.with(|dd| match cursor::first_line(dd, max_line) {
            Mv::Landed { off, speak, .. } => cursor::cursor_at(dd, off)
                .map(|c| (c, off, present::describe(dd, speak.start, speak.end))),
            Mv::Edge(_) => None,
        });
        let Some((cur, off, items)) = placed else {
            return;
        };
        b.entry_wait = None;
        if let WebMode::Browse { cur: c, .. } = &mut b.mode {
            *c = cur;
        }
        b.last_off = off;
        if self.cmd.effective.browse.say_all_on_load {
            // The page reads itself from this line (the title was spoken with the focus).
            let known = self.web.as_ref().is_some_and(|b| b.docs.get(doc).is_some());
            if known {
                self.web_auto_read(doc, true, fx);
                return;
            }
        }
        self.web_speak(&items, fx);
    }

    /// Focus waited for the mirror: look the join up again.
    /// `inline`: called while the focus utterance is being built, which takes the mode word and
    /// the lead itself; otherwise the focus was already spoken and they are spoken now.
    fn web_retry_loading(&mut self, inline: bool, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        let WebMode::Loading { focus, .. } = b.mode else {
            return;
        };
        let Some((doc, id)) = b.join.get(focus) else {
            return;
        };
        let Some(h) = b.docs.get(doc) else { return };
        let (role, states) = h.read.with(|d| role_states(d, id));
        let passes = forms::passes_through(role, states, Reason::Focus);
        let switched = b.active != Some(doc);
        self.web_activate(doc, fx);
        self.web_apply(
            WebEv::FocusIn {
                doc,
                node: id,
                passes,
                application: forms::is_application(role),
            },
            fx,
            true,
        );
        if matches!(self.web_mode(), WebMode::Browse { .. }) {
            self.web_place(doc, id);
        }
        let dialog = self.web_dialog_lead(doc, id, None);
        let listbox = self.web_listbox_lead(doc, id, None);
        // A tab whose document exists but whose focused node joined late: its title leads too.
        let titled = switched && self.web_document_lead(doc, id);
        if !inline && (dialog || listbox || titled) {
            // The focus was already spoken without its lead: say the lead now; whatever else
            // waits for the next utterance (the first line of a document) stays.
            let tail = self.web_take_tail();
            let (lead, rest): (Vec<Item>, Vec<Item>) =
                tail.into_iter().partition(|i| matches!(i, Item::Lead(_)));
            self.web_speak(&lead, fx);
            if !rest.is_empty() {
                let now = self.now;
                if let Some(b) = self.web.as_mut() {
                    b.pending_tail = Some((rest, now));
                }
            }
        }
    }

    /// Timers: a document that is slow to mirror, and the debounced scroll (LP-6).
    /// The deadline of a swallowed frame-document focus (`Browse::frame_focus`): when the frame
    /// document is still what was presented last and nothing newer happened (no focus moved
    /// into the frame, no key), the Tab stop is announced like any other focus.
    fn web_frame_focus_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        let Some((node, due)) = b.frame_focus else {
            return;
        };
        if now < due {
            fx.push(ReaderEffect::ArmTimer(due));
            return;
        }
        b.frame_focus = None;
        if b.suppress_focus == Some(node) {
            b.suppress_focus = None;
        }
        let Some(p) = self.presented.clone() else {
            return;
        };
        if p.node != node || p.epoch != self.epoch.current() {
            return;
        }
        let id = self.utt_id();
        let u = self
            .build_focus(crate::PresentContext::Focus, &p.snap, id, p.epoch, false)
            .utterance;
        self.log_presentation(&u, &p.snap, crate::PresentSource::MissReply, None);
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// The document of a `Loading` focus is there, the node is not mirrored, and the focus
    /// snapshot says it is not a field: browse the page from the top (7.3). A focus whose
    /// snapshot is still unknown stays in `Loading` with the grabs off (H2).
    fn web_loading_to_browse(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        let WebMode::Loading {
            doc: Some(doc),
            editable: false,
            known: true,
            focus: waiting_focus,
            ..
        } = b.mode
        else {
            return;
        };
        let root = b.docs.get(doc).and_then(|h| h.read.with(WebDoc::root));
        if let Some(root) = root {
            self.web_activate(doc, fx);
            self.web_apply(
                WebEv::FirstUseful {
                    doc,
                    focus: None,
                    top: root,
                },
                fx,
                true,
            );
            self.web_place(doc, root);
            // The focus is still unmirrored: remember it, a repair may find it (B-2).
            if let Some(b) = self.web.as_mut()
                && let WebMode::Browse { cur, .. } = b.mode
            {
                b.grace_fallback = Some(super::GraceFallback {
                    focus: waiting_focus,
                    doc,
                    root: cur.node,
                    at: now,
                });
            }
        }
    }

    pub(crate) fn web_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        self.web_frame_focus_timer(now, fx);
        self.web_auto_read_timer(fx);
        let Some(b) = self.web.as_mut() else { return };
        let mut sync_tabs = false;
        let mut sync_sel = false;
        if let Some(due) = b.sel_due {
            if due <= now {
                b.sel_due = None;
                sync_sel = true;
            } else {
                fx.push(ReaderEffect::ArmTimer(due));
            }
        }
        if let Some((due, doc, node, local)) = b.scroll_due {
            if due <= now {
                b.scroll_due = None;
                b.last_scroll = Some(now);
                let (node, act) = b
                    .docs
                    .get(doc)
                    .and_then(|h| h.read.with(|d| scroll_target(d, node, local)))
                    .unwrap_or((node, DocAction::ScrollIntoView));
                fx.push(ReaderEffect::Web(WebEffect::Act { doc, node, act }));
                // D-2: Tab availability follows the cursor (a grabbed Tab cannot be passed on).
                // Reading never moves focus or the page caret (BM-2).
                sync_tabs = true;
            } else {
                fx.push(ReaderEffect::ArmTimer(due));
            }
        }
        if let WebMode::Loading {
            doc: Some(_),
            since,
            editable: false,
            known: true,
            ..
        } = b.mode
            && now.saturating_duration_since(since) < LOADING_GRACE
        {
            // Not due yet (the timer was armed for something else): keep the grace armed.
            fx.push(ReaderEffect::ArmTimer(since + LOADING_GRACE));
        }
        if let WebMode::Loading { since, .. } = b.mode
            && now.saturating_duration_since(since) >= LOADING_GRACE
        {
            self.web_loading_to_browse(now, fx);
        }
        if sync_sel {
            self.web_mirror_selection(fx);
        }
        if sync_tabs {
            self.web_sync_tab_grabs(fx);
            // A plain move dropped the virtual range: the page selection we mirrored goes too.
            if self.web.as_ref().is_some_and(|b| b.sel.is_none()) {
                self.web_native_clear(fx);
            }
        }
    }

    // ---- live regions ---------------------------------------------------------------------

    /// A live region of `doc` changed (ADR-0003 13). Spoken only for the document in front, as a
    /// notification that waits for the current speech (polite) or as a message that goes ahead
    /// of other queued speech (assertive); never as an interrupt, so it cannot cut off the focus
    /// or key speech, and it survives the epoch moves of ordinary navigation.
    fn web_live(&mut self, doc: DocId, msg: &LiveMsg, fx: &mut ReaderEffects) {
        let now = self.now;
        let Some(b) = self.web.as_mut() else { return };
        let app = b.docs.get(doc).map(|h| h.app);
        let in_front = b.active == Some(doc)
            && matches!(b.mode, WebMode::Browse { .. } | WebMode::Focus { .. });
        if !in_front {
            b.counters.live_muted += 1;
            tracing::info!(target: "gorca_phase", phase = "live_muted", text = %msg.text, active = ?b.active, doc = doc.0);
            return;
        }
        let (tokens, last) = b.live_tokens;
        let dt = last.map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f32());
        let tokens = (tokens + dt * super::LIVE_RATE).min(super::LIVE_BURST);
        if tokens < 1.0 {
            b.live_tokens = (tokens, Some(now));
            b.counters.live_dropped += 1;
            return;
        }
        b.live_tokens = (tokens - 1.0, Some(now));
        b.counters.live_spoken += 1;
        let id = self.utt_id();
        let mut u = message("", id, self.epoch.current());
        u.prio = if msg.assertive {
            Prio::Message
        } else {
            Prio::Notification
        };
        // Polite news waits for the speech lane and survives navigation; assertive news is
        // stale after a focus change, so a flood can never queue in front of focus speech.
        u.survives_focus_change = !msg.assertive;
        // News expires in the speech queue (ADR-0003 13.2): 5 s polite, 10 s assertive.
        u.ttl_secs = if msg.assertive {
            LIVE_TTL_ASSERTIVE
        } else {
            LIVE_TTL_POLITE
        };
        u.segments.clear();
        u.segments
            .push(Segment::new(Arc::clone(&msg.text), SegmentKind::Text));
        // Through the owner's speech budget like any other notice (2026-10-16 fairness): a page
        // that floods its regions spends its own process's tokens, never another app's.
        match app {
            Some(app) => self.notice_speak(app, u, fx),
            None => fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u })),
        }
    }

    // ---- speech ---------------------------------------------------------------------------

    /// Speak items now, interrupting, with the pending mode word in front.
    pub(super) fn web_speak(&mut self, items: &[Item], fx: &mut ReaderEffects) {
        let word = self.web_take_word();
        if items.is_empty() && word.is_none() {
            return;
        }
        let id = self.utt_id();
        let mut u = message("", id, self.epoch.current());
        u.prio = Prio::Interrupt;
        u.may_refine = true;
        u.segments.clear();
        if let Some(w) = word {
            u.segments
                .push(Segment::new(Arc::from(w), SegmentKind::Text));
        }
        u.segments
            .extend(present::segments(items, self.verbalize_opts()));
        if !self.cmd.effective.browse.description_pause {
            crate::aria::strip_pause(&mut u);
        }
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    // ---- commands -------------------------------------------------------------------------

    /// The browse context of a key: the document and the cursor, or a spoken reason why not.
    pub(super) fn browse_ctx(&mut self, fx: &mut ReaderEffects) -> Option<(DocId, WebCursor, u16)> {
        let b = self.web.as_mut()?;
        match b.mode {
            WebMode::Browse { doc, .. } if b.docs.get(doc).is_none() => {
                // The document is gone and its notice never came (M3): give the keys back.
                self.on_doc(DocNotice::Gone { doc }, fx);
                None
            }
            WebMode::Browse { doc, cur, .. } => Some((doc, cur, b.settings.max_line)),
            WebMode::Loading { .. } => {
                b.counters.still_loading += 1;
                self.say(t(M::WebLoading), fx);
                None
            }
            WebMode::Focus { .. } | WebMode::Inactive => None,
        }
    }

    pub(crate) fn browse_command(
        &mut self,
        c: CoreCmd,
        rep: u8,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        if let Some(b) = self.web.as_mut() {
            b.grace_fallback = None;
            if c != CoreCmd::BrowseActivate {
                b.announced_change = None;
            }
        }
        self.browse_command_inner(c, rep, now, fx);
        // Any command but Enter is the user moving or reading: a displacement mark is spent.
        if c != CoreCmd::BrowseActivate
            && let Some(b) = self.web.as_mut()
        {
            b.displaced = None;
            b.armed_click = None;
        }
        self.note_cursor_sig();
    }

    fn browse_command_inner(&mut self, c: CoreCmd, _rep: u8, now: Instant, fx: &mut ReaderEffects) {
        if self.web.is_none() {
            self.say(t(M::WebBrowseUnavailable), fx);
            return;
        }
        if let Some(b) = self.web.as_mut() {
            b.entry_wait = None;
        }
        if c == CoreCmd::BrowseToggle {
            self.web_toggle(fx);
        } else if c == CoreCmd::BrowseEscape {
            self.web_escape(fx);
        } else if c == CoreCmd::BrowseActivate {
            self.web_activate_cmd(fx);
        } else if let Some(mv) = super::tablenav::table_move(c) {
            self.web_table_move(mv, now, fx);
        } else if let Some(column) = super::tablenav::table_say(c) {
            self.web_table_say(column, fx);
        } else if c == CoreCmd::BrowseTabNext {
            self.web_tab(Dir::Next, fx);
        } else if c == CoreCmd::BrowseTabPrev {
            self.web_tab(Dir::Prev, fx);
        } else if c == CoreCmd::BrowseNativeSelectionToggle {
            self.web_native_selection_toggle(fx);
        } else if c == CoreCmd::BrowseSingleLetterToggle {
            self.web_single_letter_toggle(fx);
        } else if matches!(
            c,
            CoreCmd::BrowseContainerStart | CoreCmd::BrowseContainerEnd
        ) {
            self.web_container(c == CoreCmd::BrowseContainerEnd, now, fx);
        } else if matches!(
            c,
            CoreCmd::QuicknavNextNotLinkBlock | CoreCmd::QuicknavPrevNotLinkBlock
        ) {
            let dir = if c == CoreCmd::QuicknavNextNotLinkBlock {
                Dir::Next
            } else {
                Dir::Prev
            };
            self.web_not_link_block(dir, now, fx);
        } else if let Some((dir, cat, none)) = quicknav(c) {
            self.web_quicknav(dir, cat, none, now, fx);
        } else if let Some((_, mv)) = CURSOR.iter().find(|(cmd, _)| *cmd == c) {
            self.web_cursor_cmd(*mv, false, now, fx);
        } else if let Some((_, mv)) = SELECT.iter().find(|(cmd, _)| *cmd == c) {
            self.web_cursor_cmd(*mv, true, now, fx);
        } else if c == CoreCmd::BrowseSelectAll {
            self.web_select_all(now, fx);
        }
    }

    fn web_toggle(&mut self, fx: &mut ReaderEffects) {
        // The focus is on the container that hosts the page (a window activation) and the
        // document is known: the user is in the page, do not make them wait out the grace.
        if self.web.as_ref().is_some_and(|b| {
            matches!(
                b.mode,
                WebMode::Loading {
                    doc: Some(_),
                    editable: false,
                    known: true,
                    ..
                }
            )
        }) {
            let now = self.now;
            self.web_loading_to_browse(now, fx);
        }
        let Some(b) = self.web.as_ref() else { return };
        let (doc, cur_node) = match b.mode {
            WebMode::Browse { doc, cur, .. } => (doc, Some(cur.node)),
            WebMode::Focus { doc, .. } => (doc, None),
            WebMode::Inactive | WebMode::Loading { .. } => {
                self.say(t(M::WebNotInPage), fx);
                return;
            }
        };
        let Some(h) = b.docs.get(doc) else { return };
        let (focusable, root) = h.read.with(|d| {
            let f = cur_node.and_then(|n| up_to(d, n, Cat::FOCUSABLE, 32));
            (f, d.root())
        });
        let Some(root) = root else { return };
        // The sticky focus mode is for operating the page: the keys go to the widget that has
        // the focus. When the mirror knows a focusable node at the cursor it gets the focus (as
        // always). When it knows none but the document itself (a roving `tabindex` row is not
        // marked focusable until it is read) and the page already has focus on something inside
        // the document, the focus stays where it is: moving it to the document would take the
        // arrows away from the list the user is in.
        let page_focus_inside = b
            .focus_node
            .is_some_and(|(d, f)| d == doc && h.read.with(|dd| dd.root() != Some(f)));
        let focusable = match focusable {
            Some(f) if f != root => Some(f),
            Some(_) | None if page_focus_inside => None,
            other => other,
        };
        tracing::info!(
            target: "gorca_phase",
            phase = "web_toggle",
            cur = ?cur_node,
            focusable = ?focusable,
            root = ?root,
        );
        let was_browse = cur_node.is_some();
        self.web_apply(WebEv::Toggle { focusable, root }, fx, false);
        if was_browse {
            self.web_speak(&[], fx);
        } else {
            self.web_speak_line_here(fx);
        }
    }

    fn web_escape(&mut self, fx: &mut ReaderEffects) {
        if self.web_apply(WebEv::Escape, fx, false) {
            if let WebMode::Browse { doc, cur, .. } = self.web_mode() {
                self.web_place(doc, cur.node);
            }
            self.web_speak_line_here(fx);
        }
    }

    /// Speak the mode word and the line at the cursor.
    fn web_speak_line_here(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        let WebMode::Browse { doc, cur, .. } = b.mode else {
            self.web_speak(&[], fx);
            return;
        };
        let Some(h) = b.docs.get(doc) else { return };
        let max_line = b.settings.max_line;
        let last = b.last_off;
        let items = h.read.with(|d| {
            let off = cursor::offset_of(d, cur).unwrap_or(last);
            line_items(d, max_line, off)
        });
        self.web_speak(&items, fx);
    }

    fn web_activate_cmd(&mut self, fx: &mut ReaderEffects) {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return;
        };
        if let Some(b) = self.web.as_ref() {
            tracing::debug!(
                target: "gorca_phase",
                phase = "web_activate",
                cur = ?cur.node,
                displaced = ?b.displaced,
                sig_node = ?b.cursor_sig.map(|(n, _)| n),
            );
        }
        let mut armed_name = None;
        let sig = self.click_sig(doc, cur.node);
        if let Some(b) = self.web.as_mut() {
            let armed = b.armed_click.take();
            if b.displaced.take() == Some(cur.node) {
                // The item the user was on changed or is gone. The Enter that announced it armed
                // this one: it clicks only when the target is exactly what was said (role, name,
                // toggle states). Otherwise say what is there now and arm with that; a role
                // change is announced but never armed.
                if let (Some(a), Some(s)) = (&armed, &sig)
                    && a == s
                {
                    b.announced_change = None;
                    armed_name = Some(s.name.clone());
                } else {
                    b.armed_click = sig.filter(|s| armed.as_ref().is_none_or(|a| a.role == s.role));
                    self.web_speak_line_here(fx);
                    return;
                }
            }
        }
        // After a refused click the user heard a name. Act on it only if the mirror shows exactly
        // that name and role on that object and the announcement is fresh; otherwise say what is
        // there now first (a second change, a late refresh).
        if let Some(b) = self.web.as_mut()
            && let Some(a) = b.announced_change.take()
        {
            let matches = self.now.saturating_duration_since(a.at) <= super::ANNOUNCE_WINDOW
                && b.docs.get(doc).is_some_and(|h| {
                    h.read.with(|dd| {
                        dd.node(a.node).is_some_and(|v| {
                            v.name == Some(&*a.name) && role_states(dd, a.node).0 == a.role
                        })
                    })
                });
            if !matches {
                self.web_speak_line_here(fx);
                return;
            }
        }
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let target = h.read.with(|d| {
            let n = up_to(
                d,
                cur.node,
                Cat::LINK | Cat::FORM_FIELD | Cat::CLICKABLE | Cat::FOCUSABLE,
                8,
            )?;
            let (role, states) = role_states(d, n);
            Some((
                n,
                role,
                states & TOGGLE_STATES,
                forms::passes_through(role, states, Reason::Activate),
                matches!(
                    role,
                    Role::CheckBox | Role::RadioButton | Role::ToggleButton
                ),
            ))
        });
        let Some((node, target_role, toggles, passes, checkable)) = target else {
            return;
        };
        if passes {
            // Browse -> Focus{Auto}; the machine emits `Action::Focus`.
            self.web_apply(WebEv::Activate { node, passes }, fx, false);
            self.web_speak(&[], fx);
        } else {
            if let Some(b) = self.web.as_mut() {
                b.activated = Some(self.now);
                b.activated_node = Some(super::ActivatedNode {
                    cursor: cur.node,
                    target: node,
                    role: target_role,
                    toggles,
                    at: self.now,
                });
                // The click focuses a check box: its focus speech would announce the state it
                // had before the click. The new state is what the user wants to hear.
                b.click_target = checkable.then_some((node, self.now));
            }
            fx.push(ReaderEffect::Web(WebEffect::Act {
                doc,
                node,
                act: match armed_name {
                    Some(name) => DocAction::ClickAnnounced { name },
                    None => DocAction::Click,
                },
            }));
        }
    }

    /// The click target of the cursor as the user would hear it now.
    fn click_sig(&self, doc: DocId, cursor: DocNodeId) -> Option<super::ArmedClick> {
        let h = self.web.as_ref()?.docs.get(doc)?;
        h.read.with(|d| {
            let target = up_to(
                d,
                cursor,
                Cat::LINK | Cat::FORM_FIELD | Cat::CLICKABLE | Cat::FOCUSABLE,
                8,
            )?;
            let (role, states) = role_states(d, target);
            Some(super::ArmedClick {
                cursor,
                target,
                role,
                name: d.node(target).and_then(|v| v.name).map(Arc::from),
                toggles: states & TOGGLE_STATES,
            })
        })
    }

    pub(super) fn web_set_cursor(&mut self, doc: DocId, cur: WebCursor, off: u32, now: Instant) {
        let Some(b) = self.web.as_mut() else { return };
        if let WebMode::Browse { cur: c, .. } = &mut b.mode {
            *c = cur;
        }
        b.last_off = off;
        b.sel = None;
        b.tab_fallback = false;
        b.edge_wait = None;
        if let Some(h) = b.docs.get(doc) {
            b.cursor_chain = h.read.with(|d| ancestor_chain(d, cur.node));
            b.cursor_item = h.read.with(|d| item_of(d, cur.node, off));
        }
        if b.native_sel_node.is_some() && b.model == SelectionModel::Native {
            b.sel_due = Some(now + SEL_DEBOUNCE);
        }
        b.cursor_from = CursorFrom::Own;
        b.counters.moves += 1;
        // A throttle, not a debounce: the deadline of a pending scroll is kept and only its
        // target moves to the newest position. Under key auto-repeat (30 ms) a debounce never
        // fires until the keys stop, so a virtual list never saw the cursor near its edge.
        let due = match b.scroll_due {
            Some((due, d, ..)) if d == doc && due > now => due,
            Some(_) | None => now + SCROLL_DEBOUNCE,
        };
        b.scroll_due = Some((due, doc, cur.node, cur.local));
    }

    fn web_cursor_cmd(
        &mut self,
        mv_kind: Move,
        select: bool,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let Some((doc, cur, max_line)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let last = b.last_off;
        let page = b.settings.lines_per_page;
        let sel_now = b.sel;
        let outcome = h.read.with(|d| {
            let at = cursor::offset_of(d, cur).unwrap_or_else(|| last.min(d.text_len()));
            // A selection that ended on the last character of a line (End, Ctrl+A) has its
            // active end one past the cursor: the next selecting move starts from that end.
            let off = match sel_now {
                Some((_, act)) if select && act == at.saturating_add(1) && act <= d.text_len() => {
                    act
                }
                Some(_) | None => at,
            };
            let off0 = off;
            let (mv, kind) = match mv_kind {
                Move::Line(dir) => (cursor::line(d, max_line, off, dir), Kind::Line),
                Move::Char(dir) => (cursor::character(d, max_line, off, dir), Kind::Char),
                Move::Word(dir) => (cursor::word(d, max_line, off, dir), Kind::Word),
                Move::LineStart => (cursor::line_start(d, max_line, off), Kind::Char),
                Move::LineEnd => (cursor::line_end(d, max_line, off), Kind::Char),
                Move::Top => (cursor::first_line(d, max_line), Kind::Line),
                Move::Bottom => (cursor::last_line(d, max_line), Kind::Line),
                Move::Page(dir) => (cursor::page(d, max_line, off, dir, page), Kind::Line),
                Move::Paragraph(dir) => (cursor::paragraph(d, off, dir), Kind::Line),
                Move::Sentence(dir) => (cursor::sentence(d, off, dir), Kind::Line),
            };
            // A modal dialog is a wall for the cursor while it is inside (ADR-0003 11).
            let wall = modal_scope(d, cur.node).and_then(|s| d.subtree_text_range(s));
            match mv {
                Mv::Edge(e) => Outcome::Edge(e),
                Mv::Landed { off, .. } if wall.is_some_and(|(a, b)| off < a || off >= b) => {
                    let below = wall.is_some_and(|(_, b)| off >= b);
                    Outcome::Edge(if below { Edge::Bottom } else { Edge::Top })
                }
                Mv::Landed { off, speak, blank } => {
                    let Some(cur) = cursor::cursor_at(d, off) else {
                        return Outcome::Nothing;
                    };
                    let items = match kind {
                        Kind::Char if blank => vec![Item::Text(t(M::WebBlank).to_owned())],
                        Kind::Char => single_char(d, &speak, Some(off0)),
                        Kind::Word => trim_items(present::describe_from(
                            d,
                            speak.start,
                            speak.end,
                            Some(off0),
                        )),
                        Kind::Line => present::describe_from(d, speak.start, speak.end, Some(off0)),
                    };
                    let active = if select && matches!(mv_kind, Move::LineEnd) {
                        off.saturating_add(1)
                    } else {
                        off
                    };
                    let (lo, hi) = (off0.min(active), off0.max(active));
                    let span = d.text_in(lo, hi.min(lo.saturating_add(SELECT_SPEAK_CAP)));
                    Outcome::Moved {
                        cur,
                        off,
                        active,
                        from: off0,
                        items,
                        span,
                    }
                }
            }
        });
        match outcome {
            Outcome::Moved {
                cur,
                off,
                active,
                from,
                items,
                span,
            } => {
                let anchor = self
                    .web
                    .as_ref()
                    .and_then(|b| b.sel)
                    .map_or(from, |(a, _)| a);
                self.web_set_cursor(doc, cur, off, now);
                if select {
                    let growing = active.abs_diff(anchor) >= from.abs_diff(anchor);
                    if let Some(b) = self.web.as_mut() {
                        b.sel = (active != anchor).then_some((anchor, active));
                    }
                    let text = span.split_whitespace().collect::<Vec<_>>().join(" ");
                    let text = if text.is_empty() {
                        t(M::WebBlank).to_owned()
                    } else {
                        text
                    };
                    self.web_speak(
                        &[
                            Item::Text(text),
                            Item::State(if growing {
                                t(M::StateSelected)
                            } else {
                                t(M::WebUnselected)
                            }),
                        ],
                        fx,
                    );
                    self.web_sel_changed(now, fx);
                } else {
                    self.web_speak(&items, fx);
                }
                fx.push(ReaderEffect::ArmTimer(now + SCROLL_DEBOUNCE));
            }
            Outcome::Edge(e) => {
                if let Some(b) = self.web.as_mut() {
                    b.counters.edges += 1;
                }
                let w = match e {
                    Edge::Top => t(M::WebTop),
                    Edge::Bottom => t(M::WebBottom),
                };
                // After our own scroll request a virtual list may be fetching more: remember the
                // length, a change that grows it within the window is announced once.
                if let Some(b) = self.web.as_mut()
                    && b.edge_wait.is_none()
                    && b.last_scroll
                        .is_some_and(|t| now.saturating_duration_since(t) < LOADING_CUE_WINDOW)
                    && let WebMode::Browse { doc, .. } = b.mode
                    && let Some(h) = b.docs.get(doc)
                {
                    let len = h.read.with(gorca_doc::web::WebDoc::text_len);
                    b.edge_wait = Some((now, len));
                }
                self.say(w, fx);
            }
            Outcome::Nothing => {}
        }
    }

    /// Ctrl+A: select the whole document (virtual, and native when it fits one text node).
    fn web_select_all(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let Some((doc, _, max_line)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let end = h.read.with(|d| {
            let len = d.text_len();
            (len > 0).then(|| {
                let last = len - 1;
                (last, cursor::cursor_at(d, last), max_line)
            })
        });
        let Some((last, Some(cu), _)) = end else {
            return;
        };
        self.web_set_cursor(doc, cu, last, now);
        if let Some(b) = self.web.as_mut() {
            // The cursor sits on the last character; the range includes it (see `web_cursor_cmd`:
            // an active end one past the cursor is the end of the line or of the text).
            b.sel = Some((0, last.saturating_add(1)));
        }
        self.web_speak(
            &[
                Item::Text(t(M::WebAll).to_owned()),
                Item::State(t(M::StateSelected)),
            ],
            fx,
        );
        self.web_sel_changed(now, fx);
        fx.push(ReaderEffect::ArmTimer(now + SCROLL_DEBOUNCE));
    }

    /// The virtual selection changed: bring the page's own selection up to date a moment later
    /// (one update for a burst of keys), so the browser's Ctrl+C copies it.
    fn web_sel_changed(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        if b.model != SelectionModel::Native {
            return;
        }
        let due = now + SEL_DEBOUNCE;
        b.sel_due = Some(due);
        fx.push(ReaderEffect::ArmTimer(due));
    }

    /// Mirror `sel` into the page: one deadline-bounded action to the document's source, never
    /// awaited. A range the page cannot take (a password field, a focusable container whose
    /// selection would move system focus, BM-2) clears what we set before instead, so the page
    /// never keeps a stale selection for its Ctrl+C to copy.
    fn web_mirror_selection(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        if b.model != SelectionModel::Native {
            return;
        }
        let WebMode::Browse { doc, .. } = b.mode else {
            return;
        };
        let Some(h) = b.docs.get(doc) else { return };
        let range = b.sel.map(|(a, e)| (a.min(e), a.max(e)));
        let root = h.read.with(WebDoc::root);
        let focused = b.focus_node.filter(|(fd, _)| *fd == doc).map(|(_, n)| n);
        let plan = range.and_then(|(lo, hi)| {
            h.read.with(|d| {
                // Select-all may skip leading text whose container is focusable (a skip
                // link); any other selection that would start there is not mirrored.
                let all = lo == 0 && hi >= d.text_len();
                // Firefox focuses the container that holds the start of a selection range when
                // it is focusable (a link, and the document itself for text directly in the
                // body) and not focused yet: that would move system focus (BM-2).
                let risky = |d: &WebDoc, n: DocNodeId| {
                    Some(n) != focused
                        && d.node(n).is_some_and(|v| {
                            v.cats.intersects(Cat::FOCUSABLE) && !v.cats.intersects(Cat::FRAME)
                        })
                };
                let mut from = lo;
                for _ in 0..8 {
                    let p = super::selmirror::plan_with(d, from, hi, &risky)?;
                    if !risky(d, p.start_container) {
                        return Some(p);
                    }
                    if !all || p.first_end >= hi {
                        return None;
                    }
                    from = p.first_end;
                }
                None
            })
        });
        let had_sel = range.is_some();
        let Some(plan) = plan else {
            self.web_native_clear(fx);
            if had_sel {
                // Nothing the page can take (a link start, a password field): Ctrl+C would
                // copy a stale clipboard, so say so (ADR-0021 S-1).
                self.web_selection_notice(M::WebSelectionNotSet, fx);
            }
            return;
        };
        let Some(root) = root else { return };
        let spec = gorca_model::SelectionSpec {
            root,
            spans: plan.spans.clone(),
            snap: plan.snap.clone(),
            doc_range: plan.doc_range.clone(),
            gecko_lossy: plan.gecko_lossy,
        };
        let Some(first) = plan.spans.first().map(|s| s.node) else {
            return;
        };
        let Some(b) = self.web.as_mut() else { return };
        b.native_sel_node = Some((doc, root));
        b.sel_req = b.sel_req.wrapping_add(1);
        let req = b.sel_req;
        b.sync = NativeSync::Pending { req };
        fx.push(ReaderEffect::Web(WebEffect::Select {
            doc,
            node: first,
            act: DocAction::SetSelection(spec),
            req,
        }));
    }

    /// Control went down (the first half of a possible Ctrl+C) while a page selection we mirrored
    /// exists and the page focus sits in a text field outside it: ask the source to release the
    /// focus from that field, because a plain text control would answer Ctrl+C from its own
    /// selection. Only here, never at selection time, so plain selection moves never blur the
    /// field (blur/change handlers, autosave, validation). The source acts on `input` and
    /// `textarea` only. The focus is not put back: the browse cursor owns the keys (ADR-0021).
    pub(crate) fn web_ctrl_down(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        let WebMode::Browse { doc, .. } = b.mode else {
            return;
        };
        if b.model != SelectionModel::Native || b.native_sel_node.is_none() {
            return;
        }
        let Some((a, e)) = b.sel.filter(|(a, e)| a != e) else {
            return;
        };
        let (lo, hi) = (a.min(e), a.max(e));
        let Some(h) = b.docs.get(doc) else { return };
        let field = b
            .focus_key
            .and_then(|k| b.join.get(k))
            .filter(|(fd, _)| *fd == doc)
            .map(|(_, n)| n);
        let Some(field) = field.filter(|f| {
            h.read.with(|d| {
                d.node(*f).is_some_and(|_| {
                    let (role, states) = role_states(d, *f);
                    forms::passes_through(role, states, Reason::Focus)
                }) && d
                    .subtree_text_range(*f)
                    .is_none_or(|(flo, fhi)| fhi <= lo || flo >= hi)
            })
        }) else {
            return;
        };
        fx.push(ReaderEffect::Web(WebEffect::Act {
            doc,
            node: field,
            act: DocAction::ReleaseField,
        }));
    }

    /// Collapse the page selection we set earlier, if any.
    fn web_native_clear(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        b.sync = NativeSync::Idle;
        b.sel_notified = false;
        if let Some((doc, node)) = b.native_sel_node.take() {
            fx.push(ReaderEffect::Web(WebEffect::Act {
                doc,
                node,
                act: DocAction::ClearSelection,
            }));
        }
    }

    /// D-2 target of Tab (`Dir::Next`) or Shift+Tab (`Dir::Prev`): the first focusable node after
    /// (before) the browse cursor, never the document itself, inside the modal dialog when the
    /// cursor is in one. Pure mirror lookup, O(log n).
    fn tab_target(d: &WebDoc, cur: WebCursor, dir: Dir) -> Option<DocNodeId> {
        let root = d.root();
        let scope = modal_scope(d, cur.node);
        let mut from = cur.node;
        // Walk past the document itself, embedded documents and (backwards) the focusable
        // containers the cursor is inside: they precede the cursor in document order but are not
        // "before" it (R-4). The walk is bounded by the number of such stops.
        for _ in 0..256 {
            let n = match (dir, scope) {
                (Dir::Next, Some(s)) => d.next_in(from, Cat::FOCUSABLE, s),
                (Dir::Next, None) => d.next(from, Cat::FOCUSABLE),
                (Dir::Prev, Some(s)) => d.prev_in(from, Cat::FOCUSABLE, s),
                (Dir::Prev, None) => d.prev(from, Cat::FOCUSABLE),
            }?;
            let skip = Some(n) == root
                || d.node(n).is_some_and(|v| v.cats.intersects(Cat::FRAME))
                || (dir == Dir::Prev && is_ancestor_or_self(d, n, cur.node));
            if !skip {
                return Some(n);
            }
            from = n;
        }
        None
    }

    /// Whether Tab / Shift+Tab have a target from the current browse cursor.
    pub(crate) fn web_tab_targets(&self) -> (bool, bool) {
        let Some(b) = self.web.as_ref() else {
            return (false, false);
        };
        let WebMode::Browse { doc, cur, .. } = b.mode else {
            return (false, false);
        };
        let Some(h) = b.docs.get(doc) else {
            return (false, false);
        };
        // R-5 (NVDA `_tabOverride`): the page's own tab order is not ours to override while the
        // cursor came from a focus change or sits on or inside the focused node.
        if b.cursor_from == CursorFrom::Focus
            || b.focus_node.is_some_and(|(fd, fid)| {
                fd == doc
                    && h.read.with(|d| {
                        !is_document(d, fid)
                            && d.node(fid)
                                .is_some_and(|v| v.cats.intersects(Cat::FOCUSABLE))
                            && is_ancestor_or_self(d, fid, cur.node)
                    })
            })
        {
            return (false, false);
        }
        let fallback = b.tab_fallback;
        h.read.with(|d| {
            (
                fallback || Self::tab_target(d, cur, Dir::Next).is_some(),
                Self::tab_target(d, cur, Dir::Prev).is_some(),
            )
        })
    }

    /// Offer Tab to the key grab only in the directions that have a target; one grab-set update
    /// when that changes (the cursor settled, 150 ms after the last move).
    fn web_sync_tab_grabs(&mut self, fx: &mut ReaderEffects) {
        if !matches!(self.mode, Mode::Browse { .. }) {
            return;
        }
        let now_avail = self.web_tab_targets();
        let Some(b) = self.web.as_ref() else { return };
        if b.tab_grab.get() != now_avail {
            fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
        }
    }

    /// Tab / Shift+Tab in browse mode (D-2, NVDA `_tabOverride`): focus the next (previous)
    /// focusable element after the browse cursor. The focus event then moves the cursor and
    /// speaks the element like any focus. With no target the chord is not grabbed (the page
    /// gets the key); a grab that went stale for a moment says so instead of staying silent.
    fn web_tab(&mut self, dir: Dir, fx: &mut ReaderEffects) {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        // The first focusable after the cursor; after a removed pane with none left, the
        // document itself (the page keeps the key, the browser chrome does not get it).
        let fallback = dir == Dir::Next && b.tab_fallback;
        let target = h.read.with(|d| {
            Self::tab_target(d, cur, dir).or_else(|| if fallback { d.root() } else { None })
        });
        if let Some(b) = self.web.as_mut() {
            b.tab_fallback = false;
        }
        if let Some(node) = target {
            fx.push(ReaderEffect::Web(WebEffect::Act {
                doc,
                node,
                act: DocAction::Focus,
            }));
            if fallback {
                // The fallback was spent: release the grab so the next Tab walks the page natively.
                self.web_sync_tab_grabs(fx);
            }
        } else {
            let w = match dir {
                Dir::Next => t(M::WebNoNextFocusable),
                Dir::Prev => t(M::WebNoPrevFocusable),
            };
            self.say(w, fx);
        }
    }

    /// gorca+Shift+F10: switch between "page selection" (the page's own selection follows the
    /// virtual one, the browser's Ctrl+C copies it) and "virtual selection" (the page is left
    /// alone). Switching to the page selection first asks the document whether it can select at
    /// all, and says "page selection" only when it can (ADR-0021 S-4).
    fn web_native_selection_toggle(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        let WebMode::Browse { doc, .. } = b.mode else {
            self.say(t(M::WebNativeSelectionWebOnly), fx);
            return;
        };
        if b.model == SelectionModel::Native {
            if let Some(b) = self.web.as_mut() {
                b.model = SelectionModel::Virtual;
                b.sel_due = None;
            }
            self.web_native_clear(fx);
            self.say(t(M::WebNativeSelectionOff), fx);
            return;
        }
        let Some(root) = b.docs.get(doc).and_then(|h| h.read.with(WebDoc::root)) else {
            self.say(t(M::WebSelectionUnavailable), fx);
            return;
        };
        let Some(b) = self.web.as_mut() else { return };
        b.sel_req = b.sel_req.wrapping_add(1);
        let req = b.sel_req;
        b.sync = NativeSync::Probing { req };
        // Clearing the document's ranges is a harmless probe of the selection path.
        fx.push(ReaderEffect::Web(WebEffect::Select {
            doc,
            node: root,
            act: DocAction::ClearSelection,
            req,
        }));
    }

    /// A notice about the page selection, spoken once per selection burst.
    fn web_selection_notice(&mut self, msg: M, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        if std::mem::replace(&mut b.sel_notified, true) {
            return;
        }
        self.say_queued(t(msg), fx);
    }

    /// The result of a selection request: the probe of the switch, or a mirror of the range.
    fn web_selection_done(
        &mut self,
        doc: DocId,
        req: u32,
        outcome: SelOutcome,
        fx: &mut ReaderEffects,
    ) {
        let Some(b) = self.web.as_mut() else { return };
        if b.mode.doc() != Some(doc) {
            return;
        }
        match b.sync {
            NativeSync::Probing { req: r } if r == req => {
                b.sync = NativeSync::Idle;
                if matches!(outcome, SelOutcome::Done | SelOutcome::Approximate) {
                    b.model = SelectionModel::Native;
                    let has_sel = b.sel.is_some();
                    if has_sel {
                        self.web_mirror_selection(fx);
                    }
                    self.say(t(M::WebNativeSelectionOn), fx);
                } else {
                    self.say(t(M::WebSelectionUnavailable), fx);
                }
            }
            NativeSync::Pending { req: r } if r == req => {
                b.sync = NativeSync::Idle;
                match outcome {
                    SelOutcome::Done => {}
                    // Applied, but the browser's copy may differ from what was heard.
                    SelOutcome::Approximate => {
                        self.web_selection_notice(M::WebSelectionIncomplete, fx);
                    }
                    SelOutcome::NotSupported | SelOutcome::Failed => {
                        self.web_selection_notice(M::WebSelectionNotSet, fx);
                    }
                }
            }
            NativeSync::Idle | NativeSync::Pending { .. } | NativeSync::Probing { .. } => {}
        }
    }

    /// gorca+Shift+Space: the bare quick-navigation keys go to the page, or come back.
    fn web_single_letter_toggle(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        b.single_letter = !b.single_letter;
        let on = b.single_letter;
        if matches!(self.mode, Mode::Browse { .. }) {
            fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
        }
        self.say(
            if on {
                t(M::WebSingleLetterOn)
            } else {
                t(M::WebSingleLetterOff)
            },
            fx,
        );
    }

    /// `,` / Shift+`,`: past the end of the container around the cursor, or to its start
    /// (NVDA `script_movePastEndOfContainer`, `script_moveToStartOfContainer`).
    fn web_container(&mut self, end: bool, now: Instant, fx: &mut ReaderEffects) {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let max_line = b.settings.max_line;
        let last = b.last_off;
        let out = h.read.with(|d| {
            let from = cursor::offset_of(d, cur).unwrap_or_else(|| last.min(d.text_len()));
            let Some(c) = up_to(d, cur.node, Cat::CONTAINER, 64) else {
                return Out::NoContainer;
            };
            if end {
                let Some((_, e)) = d.subtree_text_range(c) else {
                    return Out::NoContainer;
                };
                match cursor::line(d, max_line, e.saturating_sub(1), Dir::Next) {
                    Mv::Landed { off, speak, .. } => match cursor::cursor_at(d, off) {
                        Some(cu) => Out::To(
                            cu,
                            off,
                            present::describe_from(d, speak.start, speak.end, Some(from)),
                        ),
                        None => Out::NoContainer,
                    },
                    Mv::Edge(_) => Out::Bottom,
                }
            } else {
                let Some(off) = cursor::content_start(d, c) else {
                    return Out::NoContainer;
                };
                match cursor::cursor_at(d, off) {
                    Some(cu) => Out::To(cu, off, present::describe_node_from(d, c, Some(from))),
                    None => Out::NoContainer,
                }
            }
        });
        match out {
            Out::To(cu, off, items) => {
                self.web_set_cursor(doc, cu, off, now);
                self.web_speak(&items, fx);
                fx.push(ReaderEffect::ArmTimer(now + SCROLL_DEBOUNCE));
            }
            Out::NoContainer => self.say(t(M::WebNoContainer), fx),
            Out::Bottom => self.say(t(M::WebBottom), fx),
        }
    }

    /// N: skip past a block of links to the next text (NVDA "not link block").
    fn web_not_link_block(&mut self, dir: Dir, now: Instant, fx: &mut ReaderEffects) {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let max_line = b.settings.max_line;
        let last = b.last_off;
        let found = h.read.with(|d| {
            let from = cursor::offset_of(d, cur).unwrap_or_else(|| last.min(d.text_len()));
            let link_line = |r: &std::ops::Range<u32>| -> bool {
                let at = |off: u32| {
                    cursor::cursor_at(d, off)
                        .is_some_and(|c| up_to(d, c.node, Cat::LINK, 16).is_some())
                };
                r.end > r.start && at(r.start) && at(r.end.saturating_sub(1))
            };
            let mut at = from;
            let mut seen_block = false;
            for _ in 0..4096 {
                match cursor::line(d, max_line, at, dir) {
                    Mv::Landed { off, speak, .. } => {
                        if link_line(&speak) {
                            seen_block = true;
                        } else if seen_block {
                            let cu = cursor::cursor_at(d, off)?;
                            return Some((
                                cu,
                                off,
                                present::describe_from(d, speak.start, speak.end, Some(from)),
                            ));
                        }
                        at = off;
                    }
                    Mv::Edge(_) => return None,
                }
            }
            None
        });
        if let Some((cu, off, items)) = found {
            self.web_set_cursor(doc, cu, off, now);
            self.web_speak(&items, fx);
            fx.push(ReaderEffect::ArmTimer(now + SCROLL_DEBOUNCE));
        } else {
            self.say(t(M::WebNoMoreTextAfterLinks), fx);
        }
    }

    fn web_quicknav(&mut self, dir: Dir, cat: Cat, none: M, now: Instant, fx: &mut ReaderEffects) {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let last = b.last_off;
        let found = h.read.with(|d| {
            let from = Some(cursor::offset_of(d, cur).unwrap_or_else(|| last.min(d.text_len())));
            // A modal dialog confines the keys while the cursor is inside it (ADR-0003 11).
            let scope = modal_scope(d, cur.node);
            let target = match (dir, scope) {
                (Dir::Next, None) => d.next(cur.node, cat),
                (Dir::Next, Some(s)) => d.next_in(cur.node, cat, s),
                // Inside a match: step out of it, or "previous" would find the one we are in.
                (Dir::Prev, None) => prev_match(d, cur.node, cat, None),
                (Dir::Prev, Some(s)) => prev_match(d, cur.node, cat, Some(s)),
            }?;
            let off = cursor::content_start(d, target)?;
            let cur = cursor::cursor_at(d, off)?;
            Some((cur, off, present::describe_node_from(d, target, from)))
        });
        if let Some((cur, off, items)) = found {
            self.web_set_cursor(doc, cur, off, now);
            self.web_speak(&items, fx);
            fx.push(ReaderEffect::ArmTimer(now + SCROLL_DEBOUNCE));
        } else {
            self.say(t(none), fx);
        }
    }
}

/// What the node holds, as a number: its structural signature plus its name and text, so a node
/// the page re-used for other content changes it.
fn content_sig(d: &WebDoc, n: DocNodeId) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let v = d.node(n)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.sig.hash(&mut h);
    v.name.hash(&mut h);
    v.text.hash(&mut h);
    Some(h.finish())
}

/// The states a toggle flips when it is pressed.
const TOGGLE_STATES: StateSet = StateSet::PRESSED
    .union(StateSet::CHECKED)
    .union(StateSet::EXPANDED);

/// What a pointer click on a web node's centre can hit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum PointerClass {
    /// A control: its centre is the control.
    Interactive,
    /// Text or an image: small, its centre is the item.
    Leaf,
    /// Holds other things: its centre may be empty space.
    Container,
}

fn pointer_class(role: Role) -> PointerClass {
    match role {
        Role::Link
        | Role::Button
        | Role::ToggleButton
        | Role::Switch
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::TextEntry
        | Role::PasswordEntry
        | Role::SpinButton
        | Role::Slider
        | Role::MenuItem
        | Role::Tab
        | Role::TreeItem => PointerClass::Interactive,
        Role::Heading | Role::StaticText | Role::Label | Role::Image | Role::ProgressBar => {
            PointerClass::Leaf
        }
        Role::Unknown
        | Role::Application
        | Role::Window
        | Role::Dialog
        | Role::Panel
        | Role::Document
        | Role::DocumentWeb
        | Role::Paragraph
        | Role::List
        | Role::ListItem
        | Role::Table
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Menu
        | Role::MenuBar
        | Role::Popover
        | Role::TabList
        | Role::Tree
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Terminal
        | Role::Landmark
        | Role::Alert => PointerClass::Container,
    }
}
