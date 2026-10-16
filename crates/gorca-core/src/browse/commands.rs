//! The Reader's browse arm: focus handling, document notices, key commands (browse-mode design
//! sections 7 and 8). Everything on a key's path to speech reads one published `WebDoc` and
//! sends nothing but fire-and-forget effects afterwards (BI-1, BI-2).

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
    BrowseState, CursorFrom, LIVE_TTL_ASSERTIVE, LIVE_TTL_POLITE, ModeWord, SavedWeb, WebEv, WebFx,
    WebMode, step,
};
use crate::SpeechIntent;
use crate::commands::CoreCmd;
use crate::doc_actor::live::LiveMsg;
use crate::doc_actor::{DocDirectory, DocNotice, WebJoin};
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
}

/// How long after the cursor's node vanished a re-render may still move the cursor back.
const RESTORE_WINDOW: Duration = Duration::from_millis(1500);
/// A focus event this soon after a click on a check box is the click's own.
const CLICK_FOCUS_WINDOW: Duration = Duration::from_millis(800);
/// Characters of a selected span that are read back.
const SELECT_SPEAK_CAP: u32 = 300;
/// A mode word waits this long for the utterance it belongs to.
/// A document focus this soon after our own click or Enter is the click's own.
const REFOCUS_SILENT_WINDOW: Duration = Duration::from_millis(2000);
const WORD_TTL: Duration = Duration::from_millis(600);
/// The line that follows a document's focus utterance waits for a cold fetch this long (M4).
const TAIL_TTL: Duration = Duration::from_secs(2);
/// A late first line (the mirror was empty at focus time) is still spoken within this window.
const ENTRY_WAIT: Duration = Duration::from_secs(2);
/// Held cursor keys scroll the page once per this interval (LP-6).
pub(super) const SCROLL_DEBOUNCE: Duration = Duration::from_millis(150);
/// Focus in a document the mirror does not know yet waits this long (browse design 7.3).
const LOADING_GRACE: Duration = Duration::from_secs(1);

/// What a cursor command computed from one snapshot.
enum Outcome {
    Moved {
        cur: WebCursor,
        off: u32,
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
const QUICKNAV: &[(CoreCmd, Dir, Cat, &str)] = &[
    (
        CoreCmd::QuicknavNextHeading,
        Dir::Next,
        Cat::HEADING,
        "heading",
    ),
    (
        CoreCmd::QuicknavPrevHeading,
        Dir::Prev,
        Cat::HEADING,
        "heading",
    ),
    (
        CoreCmd::QuicknavNextHeading1,
        Dir::Next,
        Cat::HEADING_1,
        "heading level 1",
    ),
    (
        CoreCmd::QuicknavPrevHeading1,
        Dir::Prev,
        Cat::HEADING_1,
        "heading level 1",
    ),
    (
        CoreCmd::QuicknavNextHeading2,
        Dir::Next,
        Cat::HEADING_2,
        "heading level 2",
    ),
    (
        CoreCmd::QuicknavPrevHeading2,
        Dir::Prev,
        Cat::HEADING_2,
        "heading level 2",
    ),
    (
        CoreCmd::QuicknavNextHeading3,
        Dir::Next,
        Cat::HEADING_3,
        "heading level 3",
    ),
    (
        CoreCmd::QuicknavPrevHeading3,
        Dir::Prev,
        Cat::HEADING_3,
        "heading level 3",
    ),
    (
        CoreCmd::QuicknavNextHeading4,
        Dir::Next,
        Cat::HEADING_4,
        "heading level 4",
    ),
    (
        CoreCmd::QuicknavPrevHeading4,
        Dir::Prev,
        Cat::HEADING_4,
        "heading level 4",
    ),
    (
        CoreCmd::QuicknavNextHeading5,
        Dir::Next,
        Cat::HEADING_5,
        "heading level 5",
    ),
    (
        CoreCmd::QuicknavPrevHeading5,
        Dir::Prev,
        Cat::HEADING_5,
        "heading level 5",
    ),
    (
        CoreCmd::QuicknavNextHeading6,
        Dir::Next,
        Cat::HEADING_6,
        "heading level 6",
    ),
    (
        CoreCmd::QuicknavPrevHeading6,
        Dir::Prev,
        Cat::HEADING_6,
        "heading level 6",
    ),
    (CoreCmd::QuicknavNextLink, Dir::Next, Cat::LINK, "link"),
    (CoreCmd::QuicknavPrevLink, Dir::Prev, Cat::LINK, "link"),
    (
        CoreCmd::QuicknavNextVisited,
        Dir::Next,
        Cat::LINK_VISITED,
        "visited link",
    ),
    (
        CoreCmd::QuicknavPrevVisited,
        Dir::Prev,
        Cat::LINK_VISITED,
        "visited link",
    ),
    (
        CoreCmd::QuicknavNextUnvisited,
        Dir::Next,
        Cat::LINK_UNVISITED,
        "unvisited link",
    ),
    (
        CoreCmd::QuicknavPrevUnvisited,
        Dir::Prev,
        Cat::LINK_UNVISITED,
        "unvisited link",
    ),
    (
        CoreCmd::QuicknavNextFormField,
        Dir::Next,
        Cat::FORM_FIELD,
        "form field",
    ),
    (
        CoreCmd::QuicknavPrevFormField,
        Dir::Prev,
        Cat::FORM_FIELD,
        "form field",
    ),
    (CoreCmd::QuicknavNextList, Dir::Next, Cat::LIST, "list"),
    (CoreCmd::QuicknavPrevList, Dir::Prev, Cat::LIST, "list"),
    (
        CoreCmd::QuicknavNextListItem,
        Dir::Next,
        Cat::LIST_ITEM,
        "list item",
    ),
    (
        CoreCmd::QuicknavPrevListItem,
        Dir::Prev,
        Cat::LIST_ITEM,
        "list item",
    ),
    (
        CoreCmd::QuicknavNextButton,
        Dir::Next,
        Cat::BUTTON,
        "button",
    ),
    (
        CoreCmd::QuicknavPrevButton,
        Dir::Prev,
        Cat::BUTTON,
        "button",
    ),
    (
        CoreCmd::QuicknavNextEdit,
        Dir::Next,
        Cat::EDIT,
        "edit field",
    ),
    (
        CoreCmd::QuicknavPrevEdit,
        Dir::Prev,
        Cat::EDIT,
        "edit field",
    ),
    (
        CoreCmd::QuicknavNextSeparator,
        Dir::Next,
        Cat::SEPARATOR,
        "separator",
    ),
    (
        CoreCmd::QuicknavPrevSeparator,
        Dir::Prev,
        Cat::SEPARATOR,
        "separator",
    ),
    (
        CoreCmd::QuicknavNextRadio,
        Dir::Next,
        Cat::RADIO,
        "radio button",
    ),
    (
        CoreCmd::QuicknavPrevRadio,
        Dir::Prev,
        Cat::RADIO,
        "radio button",
    ),
    (
        CoreCmd::QuicknavNextCombobox,
        Dir::Next,
        Cat::COMBOBOX,
        "combo box",
    ),
    (
        CoreCmd::QuicknavPrevCombobox,
        Dir::Prev,
        Cat::COMBOBOX,
        "combo box",
    ),
    (
        CoreCmd::QuicknavNextCheckbox,
        Dir::Next,
        Cat::CHECKBOX,
        "check box",
    ),
    (
        CoreCmd::QuicknavPrevCheckbox,
        Dir::Prev,
        Cat::CHECKBOX,
        "check box",
    ),
    (
        CoreCmd::QuicknavNextGraphic,
        Dir::Next,
        Cat::GRAPHIC,
        "graphic",
    ),
    (
        CoreCmd::QuicknavPrevGraphic,
        Dir::Prev,
        Cat::GRAPHIC,
        "graphic",
    ),
    (
        CoreCmd::QuicknavNextBlockquote,
        Dir::Next,
        Cat::BLOCKQUOTE,
        "block quote",
    ),
    (
        CoreCmd::QuicknavPrevBlockquote,
        Dir::Prev,
        Cat::BLOCKQUOTE,
        "block quote",
    ),
    (
        CoreCmd::QuicknavNextLandmark,
        Dir::Next,
        Cat::ANY_LANDMARK,
        "landmark",
    ),
    (
        CoreCmd::QuicknavPrevLandmark,
        Dir::Prev,
        Cat::ANY_LANDMARK,
        "landmark",
    ),
    (
        CoreCmd::QuicknavNextEmbedded,
        Dir::Next,
        Cat::EMBEDDED_OR_DIALOG,
        "embedded object",
    ),
    (
        CoreCmd::QuicknavPrevEmbedded,
        Dir::Prev,
        Cat::EMBEDDED_OR_DIALOG,
        "embedded object",
    ),
    (CoreCmd::QuicknavNextTable, Dir::Next, Cat::TABLE, "table"),
    (CoreCmd::QuicknavPrevTable, Dir::Prev, Cat::TABLE, "table"),
    (
        CoreCmd::QuicknavNextAnnotation,
        Dir::Next,
        Cat::ANNOTATION,
        "annotation",
    ),
    (
        CoreCmd::QuicknavPrevAnnotation,
        Dir::Prev,
        Cat::ANNOTATION,
        "annotation",
    ),
    (CoreCmd::QuicknavNextError, Dir::Next, Cat::ERROR, "error"),
    (CoreCmd::QuicknavPrevError, Dir::Prev, Cat::ERROR, "error"),
    (
        CoreCmd::QuicknavNextParagraph,
        Dir::Next,
        Cat::TEXT_BLOCK,
        "paragraph",
    ),
    (
        CoreCmd::QuicknavPrevParagraph,
        Dir::Prev,
        Cat::TEXT_BLOCK,
        "paragraph",
    ),
    (CoreCmd::QuicknavNextFrame, Dir::Next, Cat::FRAME, "frame"),
    (CoreCmd::QuicknavPrevFrame, Dir::Prev, Cat::FRAME, "frame"),
    (
        CoreCmd::QuicknavNextArticle,
        Dir::Next,
        Cat::ARTICLE,
        "article",
    ),
    (
        CoreCmd::QuicknavPrevArticle,
        Dir::Prev,
        Cat::ARTICLE,
        "article",
    ),
];

fn quicknav(c: CoreCmd) -> Option<(Dir, Cat, &'static str)> {
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

/// The nearest focusable node from `node` up to, but not including, the document (which every
/// engine reports as focusable). An embedded document (`Cat::FRAME`) is not one: its content is
/// reached through its own focusable elements, and a frame is never a Tab stop (R-4, R-5).
fn focusable_around(d: &WebDoc, node: DocNodeId) -> Option<DocNodeId> {
    let root = d.root();
    let mut n = node;
    for _ in 0..64 {
        if Some(n) != root
            && d.node(n).is_some_and(|v| {
                v.cats.intersects(Cat::FOCUSABLE) && !v.cats.intersects(Cat::FRAME)
            })
        {
            return Some(n);
        }
        n = d.parent(n)?;
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
            return vec![Item::Text(present::char_name(c))];
        }
        return vec![Item::Text(t.clone())];
    }
    if items.is_empty() {
        let t = d.text_in(r.start, r.end);
        return t
            .chars()
            .next()
            .map(|c| vec![Item::Text(present::char_name(c))])
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
        let hit = b.join.get(node);
        tracing::info!(target: "gorca_phase", phase = "web_focus", hit = hit.is_some());
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
            if let Some((t, at)) = b.click_target.take()
                && t == id
                && self.now.saturating_duration_since(at) < CLICK_FOCUS_WINDOW
            {
                b.suppress_focus = Some(node);
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
            self.web_refresh_popup(fx);
            // The cursor jumped with the focus: Tab availability changed with it (R-5), and no
            // scroll timer is armed to notice.
            self.web_sync_tab_grabs(fx);
            return;
        }
        // IR-3: the join missed. Browse keys must never be grabbed while focus may be in a
        // field we have not mirrored, so this is `Loading` (grabs off) until it is resolved.
        if b.web_apps.contains(&node.app) {
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

    /// A cold focus on a mirrored web node: its snapshot from the document mirror (role, states,
    /// name, value), so the first focus into a field is spoken at once and the fetch only
    /// confirms it (the mirror knows the node: this is what the page last told us).
    pub(crate) fn web_focus_snapshot(&self, node: NodeKey) -> Option<gorca_model::NodeSnapshot> {
        use gorca_model::{AttrMap, ChildList, Freshness, Ifaces, PropMask, Relations};
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
                description: None,
                value,
                parent: None,
                children: ChildList::Unknown,
                interfaces: Ifaces::empty(),
                attrs: AttrMap::default(),
                relations: Relations::default(),
                bounds: None,
                have,
                confirmed_seq: 0,
                fresh: Freshness::Stale {
                    since: Instant::now(),
                    mask: PropMask::VALUE,
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
                    out.push(Item::Lead("dialog".to_owned()));
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
                    out.push(Item::Lead("list box".to_owned()));
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
            DocNotice::FocusFailed { doc } => {
                // The grabbed Tab asked for a focus the page refused: say so (R-6).
                if self.web.as_ref().is_some_and(|b| b.mode.doc() == Some(doc)) {
                    self.say("can't move focus there", fx);
                }
            }
            DocNotice::Gone { doc } => {
                if let Some(b) = self.web.as_mut() {
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
                let spoke = self.web_speak_entry(doc, at_top || at_root, at_top, fx);
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

    /// Is a mode word waiting for the next utterance?
    fn web_has_word(&self) -> bool {
        self.web.as_ref().is_some_and(|b| {
            b.pending_word
                .is_some_and(|(_, at)| self.now.saturating_duration_since(at) <= WORD_TTL)
        })
    }

    fn web_changed(&mut self, doc: DocId, fx: &mut ReaderEffects) {
        self.web_refresh_popup(fx);
        self.web_late_entry(doc, fx);
        let Some(b) = self.web.as_mut() else { return };
        match b.mode {
            WebMode::Loading { .. } => self.web_retry_loading(false, fx),
            WebMode::Browse { doc: d, cur, .. } if d == doc => {
                // The mirror changed under the cursor: follow the node, or the flat offset. A
                // re-render replaces nodes in several publishes: for a moment after the node
                // was lost the offset is re-applied, so the cursor settles on the same place in
                // the new content (position restore, ADR-0003 10.3).
                let Some(h) = b.docs.get(doc) else { return };
                let last = b.last_off;
                let now = self.now;
                let window = b
                    .restore_until
                    .is_some_and(|(t, off)| now < t && off == last);
                let fixed = h.read.with(|d| {
                    if d.is_attached(cur.node) && !window {
                        None
                    } else {
                        cursor::cursor_at(d, last).filter(|c| c.node != cur.node)
                    }
                });
                let lost = h.read.with(|d| !d.is_attached(cur.node));
                if lost {
                    b.restore_until = Some((now + RESTORE_WINDOW, last));
                }
                if let Some(c) = fixed
                    && let WebMode::Browse { cur, .. } = &mut b.mode
                {
                    *cur = c;
                }
            }
            WebMode::Focus { .. } | WebMode::Browse { .. } | WebMode::Inactive => {}
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
        if !inline && (dialog || listbox) {
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
    pub(crate) fn web_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        self.web_auto_read_timer(fx);
        let Some(b) = self.web.as_mut() else { return };
        let mut sync_tabs = false;
        if let Some((due, doc, node, _)) = b.scroll_due {
            if due <= now {
                b.scroll_due = None;
                fx.push(ReaderEffect::Web(WebEffect::Act {
                    doc,
                    node,
                    act: DocAction::ScrollIntoView,
                }));
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
        if let WebMode::Loading {
            doc: Some(doc),
            since,
            editable: false,
            known: true,
            ..
        } = b.mode
            && now.saturating_duration_since(since) >= LOADING_GRACE
        {
            // The document is there, the node is not mirrored yet, and the focus snapshot says it
            // is not a field: browse the page from the top (7.3). A focus whose snapshot is
            // still unknown stays in `Loading` with the grabs off (H2).
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
            }
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
        u.segments.push(Segment {
            text: Arc::clone(&msg.text),
            kind: SegmentKind::Text,
        });
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
            u.segments.push(Segment {
                text: Arc::from(w),
                kind: SegmentKind::Text,
            });
        }
        u.segments.extend(present::segments(items));
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
                self.say("loading", fx);
                None
            }
            WebMode::Focus { .. } | WebMode::Inactive => None,
        }
    }

    pub(crate) fn browse_command(
        &mut self,
        c: CoreCmd,
        _rep: u8,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        if self.web.is_none() {
            self.say("browse mode is not available", fx);
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
        } else if let Some((dir, cat, name)) = quicknav(c) {
            self.web_quicknav(dir, cat, name, now, fx);
        } else if let Some((_, mv)) = CURSOR.iter().find(|(cmd, _)| *cmd == c) {
            self.web_cursor_cmd(*mv, false, now, fx);
        } else if let Some((_, mv)) = SELECT.iter().find(|(cmd, _)| *cmd == c) {
            self.web_cursor_cmd(*mv, true, now, fx);
        } else if c == CoreCmd::BrowseSelectAll {
            self.web_select_all(now, fx);
        }
    }

    fn web_toggle(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        let (doc, cur_node) = match b.mode {
            WebMode::Browse { doc, cur, .. } => (doc, Some(cur.node)),
            WebMode::Focus { doc, .. } => (doc, None),
            WebMode::Inactive | WebMode::Loading { .. } => {
                self.say("not in a web page", fx);
                return;
            }
        };
        let Some(h) = b.docs.get(doc) else { return };
        let (focusable, root) = h.read.with(|d| {
            let f = cur_node.and_then(|n| up_to(d, n, Cat::FOCUSABLE, 32));
            (f, d.root())
        });
        let Some(root) = root else { return };
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
                forms::passes_through(role, states, Reason::Activate),
                matches!(
                    role,
                    Role::CheckBox | Role::RadioButton | Role::ToggleButton
                ),
            ))
        });
        let Some((node, passes, checkable)) = target else {
            return;
        };
        if passes {
            // Browse -> Focus{Auto}; the machine emits `Action::Focus`.
            self.web_apply(WebEv::Activate { node, passes }, fx, false);
            self.web_speak(&[], fx);
        } else {
            if let Some(b) = self.web.as_mut() {
                b.activated = Some(self.now);
                // The click focuses a check box: its focus speech would announce the state it
                // had before the click. The new state is what the user wants to hear.
                b.click_target = checkable.then_some((node, self.now));
            }
            fx.push(ReaderEffect::Web(WebEffect::Act {
                doc,
                node,
                act: DocAction::Click,
            }));
        }
    }

    pub(super) fn web_set_cursor(&mut self, doc: DocId, cur: WebCursor, off: u32, now: Instant) {
        let Some(b) = self.web.as_mut() else { return };
        if let WebMode::Browse { cur: c, .. } = &mut b.mode {
            *c = cur;
        }
        b.last_off = off;
        b.sel = None;
        b.cursor_from = CursorFrom::Own;
        b.counters.moves += 1;
        b.scroll_due = Some((now + SCROLL_DEBOUNCE, doc, cur.node, cur.local));
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
        let outcome = h.read.with(|d| {
            let off = cursor::offset_of(d, cur).unwrap_or_else(|| last.min(d.text_len()));
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
                        Kind::Char if blank => vec![Item::Text("blank".to_owned())],
                        Kind::Char => single_char(d, &speak, Some(off0)),
                        Kind::Word => trim_items(present::describe_from(
                            d,
                            speak.start,
                            speak.end,
                            Some(off0),
                        )),
                        Kind::Line => present::describe_from(d, speak.start, speak.end, Some(off0)),
                    };
                    let (lo, hi) = (off0.min(off), off0.max(off));
                    let span = d.text_in(lo, hi.min(lo.saturating_add(SELECT_SPEAK_CAP)));
                    Outcome::Moved {
                        cur,
                        off,
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
                    let growing = off.abs_diff(anchor) >= from.abs_diff(anchor);
                    if let Some(b) = self.web.as_mut() {
                        b.sel = (off != anchor).then_some((anchor, off));
                    }
                    let text = span.split_whitespace().collect::<Vec<_>>().join(" ");
                    let text = if text.is_empty() {
                        "blank".to_owned()
                    } else {
                        text
                    };
                    self.web_speak(
                        &[
                            Item::Text(text),
                            Item::State(if growing { "selected" } else { "unselected" }),
                        ],
                        fx,
                    );
                    self.web_native_selection(doc, anchor, off, fx);
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
                    Edge::Top => "top",
                    Edge::Bottom => "bottom",
                };
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
            b.sel = Some((0, last));
        }
        self.web_speak(&[Item::Text("all".to_owned()), Item::State("selected")], fx);
        self.web_native_selection(doc, 0, last, fx);
        fx.push(ReaderEffect::ArmTimer(now + SCROLL_DEBOUNCE));
    }

    /// Native selection mode (D-3): mirror the virtual range into the page's own selection so
    /// its Ctrl+C copies it. Only when the mode is on and both ends lie in one text node that is
    /// not inside a focusable element (an engine may focus the element that gets a selection,
    /// which would move system focus, BM-2). Anything else clears what we set before, so the
    /// page never keeps a stale selection.
    fn web_native_selection(&mut self, doc: DocId, a: u32, b_off: u32, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_ref() else { return };
        if !b.native_selection {
            return;
        }
        let Some(h) = b.docs.get(doc) else { return };
        let target = h.read.with(|d| {
            let (n1, l1) = d.at_offset(a)?;
            let (n2, l2) = d.at_offset(b_off)?;
            let inside_focusable = focusable_around(d, n1).is_some();
            (n1 == n2 && !inside_focusable).then(|| (n1, l1.min(l2)..l1.max(l2)))
        });
        let prev = self.web.as_ref().and_then(|b| b.native_sel_node);
        match target {
            Some((node, range)) => {
                if let Some((pd, pn)) = prev
                    && (pd, pn) != (doc, node)
                {
                    self.web_native_clear(fx);
                }
                if let Some(b) = self.web.as_mut() {
                    b.native_sel_node = Some((doc, node));
                }
                fx.push(ReaderEffect::Web(WebEffect::Act {
                    doc,
                    node,
                    act: DocAction::SetSelection(range),
                }));
            }
            None => self.web_native_clear(fx),
        }
    }

    /// Collapse the page selection we set earlier, if any.
    fn web_native_clear(&mut self, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        if let Some((doc, node)) = b.native_sel_node.take() {
            fx.push(ReaderEffect::Web(WebEffect::Act {
                doc,
                node,
                act: DocAction::SetSelection(0..0),
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
        h.read.with(|d| {
            (
                Self::tab_target(d, cur, Dir::Next).is_some(),
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
        let target = h.read.with(|d| Self::tab_target(d, cur, dir));
        if let Some(node) = target {
            fx.push(ReaderEffect::Web(WebEffect::Act {
                doc,
                node,
                act: DocAction::Focus,
            }));
        } else {
            let w = match dir {
                Dir::Next => "no next focusable element",
                Dir::Prev => "no previous focusable element",
            };
            self.say(w, fx);
        }
    }

    /// gorca+Shift+F10: the page's own selection follows the virtual one, or stops following.
    fn web_native_selection_toggle(&mut self, fx: &mut ReaderEffects) {
        if self.web.as_ref().is_none_or(|b| b.mode.doc().is_none()) {
            self.say("native selection is only available on web pages", fx);
            return;
        }
        let Some(b) = self.web.as_mut() else { return };
        b.native_selection = !b.native_selection;
        let on = b.native_selection;
        let sel = b.sel;
        let doc = match b.mode {
            WebMode::Browse { doc, .. } => Some(doc),
            WebMode::Inactive | WebMode::Loading { .. } | WebMode::Focus { .. } => None,
        };
        if on {
            if let (Some(doc), Some((a, e))) = (doc, sel) {
                self.web_native_selection(doc, a, e, fx);
            }
        } else {
            self.web_native_clear(fx);
        }
        self.say(
            if on {
                "native selection on"
            } else {
                "native selection off"
            },
            fx,
        );
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
                "single letter navigation on"
            } else {
                "single letter navigation off"
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
            Out::NoContainer => self.say("Not in a container", fx),
            Out::Bottom => self.say("bottom", fx),
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
            self.say("no more text after a block of links", fx);
        }
    }

    fn web_quicknav(
        &mut self,
        dir: Dir,
        cat: Cat,
        name: &'static str,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
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
            let w = match dir {
                Dir::Next => "next",
                Dir::Prev => "previous",
            };
            self.say(&format!("no {w} {name}"), fx);
        }
    }
}
