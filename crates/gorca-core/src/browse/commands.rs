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
use super::{BrowseState, ModeWord, SavedWeb, WebEv, WebFx, WebMode, step};
use crate::SpeechIntent;
use crate::commands::CoreCmd;
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

/// A mode word waits this long for the utterance it belongs to.
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
        items: Vec<Item>,
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
        Cat::EMBEDDED,
        "embedded object",
    ),
    (
        CoreCmd::QuicknavPrevEmbedded,
        Dir::Prev,
        Cat::EMBEDDED,
        "embedded object",
    ),
    (CoreCmd::QuicknavNextTable, Dir::Next, Cat::TABLE, "table"),
    (CoreCmd::QuicknavPrevTable, Dir::Prev, Cat::TABLE, "table"),
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

/// The ancestor-or-self of `n` that is the outermost node of category `c` containing it.
fn outermost_match(d: &WebDoc, n: DocNodeId, c: Cat) -> DocNodeId {
    let mut best = n;
    let mut cur = Some(n);
    let mut hops = 0;
    while let Some(x) = cur {
        if d.node(x).is_some_and(|v| v.cats.intersects(c)) {
            best = x;
        }
        cur = d.parent(x);
        hops += 1;
        if hops > 64 {
            break;
        }
    }
    best
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
        if let Some((doc, id)) = hit
            && let Some(h) = b.docs.get(doc)
            && let Some((role, states)) = h.read.with(|d| {
                d.node(id)?;
                Some(role_states(d, id))
            })
        {
            b.counters.join_hits += 1;
            let passes = forms::passes_through(role, states, Reason::Focus);
            let application = forms::is_application(role);
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
                self.web_root_entry(doc, id);
            }
            self.web_refresh_popup(fx);
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
            self.web_apply(WebEv::FocusAway, fx, false);
        }
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
            self.web_retry_loading(fx);
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
                self.web_retry_loading(fx);
            }
            DocNotice::NoDocument { focus } => {
                self.web_apply(WebEv::NoDocument { focus }, fx, true);
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
                let spoke = self.web_speak_entry(doc, at_top || at_root, at_top, fx);
                if at_top || at_root {
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
                    let spoke = self.web_speak_entry(doc, true, true, fx);
                    self.web_auto_read(doc, spoke, fx);
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
            WebMode::Loading { .. } => self.web_retry_loading(fx),
            WebMode::Browse { doc: d, cur, .. } if d == doc => {
                // The mirror changed under the cursor: follow the node, or the flat offset.
                let Some(h) = b.docs.get(doc) else { return };
                let last = b.last_off;
                let fixed = h.read.with(|d| {
                    if d.is_attached(cur.node) {
                        None
                    } else {
                        cursor::cursor_at(d, last)
                    }
                });
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
    fn web_retry_loading(&mut self, fx: &mut ReaderEffects) {
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
    }

    /// Timers: a document that is slow to mirror, and the debounced scroll (LP-6).
    pub(crate) fn web_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        self.web_auto_read_timer(fx);
        let Some(b) = self.web.as_mut() else { return };
        if let Some((due, doc, node)) = b.scroll_due {
            if due <= now {
                b.scroll_due = None;
                fx.push(ReaderEffect::Web(WebEffect::Act {
                    doc,
                    node,
                    act: DocAction::ScrollIntoView,
                }));
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
        } else if let Some((dir, cat, name)) = quicknav(c) {
            self.web_quicknav(dir, cat, name, now, fx);
        } else if let Some((_, mv)) = CURSOR.iter().find(|(cmd, _)| *cmd == c) {
            self.web_cursor_cmd(*mv, now, fx);
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
            Some((n, forms::passes_through(role, states, Reason::Activate)))
        });
        let Some((node, passes)) = target else { return };
        if passes {
            // Browse -> Focus{Auto}; the machine emits `Action::Focus`.
            self.web_apply(WebEv::Activate { node, passes }, fx, false);
            self.web_speak(&[], fx);
        } else {
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
        b.counters.moves += 1;
        b.scroll_due = Some((now + SCROLL_DEBOUNCE, doc, cur.node));
    }

    fn web_cursor_cmd(&mut self, mv_kind: Move, now: Instant, fx: &mut ReaderEffects) {
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
            };
            match mv {
                Mv::Edge(e) => Outcome::Edge(e),
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
                        Kind::Line => {
                            present::describe_from(d, speak.start, speak.end, Some(off0))
                        }
                    };
                    Outcome::Moved { cur, off, items }
                }
            }
        });
        match outcome {
            Outcome::Moved { cur, off, items } => {
                self.web_set_cursor(doc, cur, off, now);
                self.web_speak(&items, fx);
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
            let target = match dir {
                Dir::Next => d.next(cur.node, cat),
                // Inside a match: step out of it, or "previous" would find the one we are in.
                Dir::Prev => d.prev(outermost_match(d, cur.node, cat), cat),
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
