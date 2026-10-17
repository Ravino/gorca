//! Text, caret and typing echo (milestone M3, ADR-0007).
//!
//! - [`caret`]: the per-object state machine (key-to-caret correlation, typing echo, edits,
//!   selection), pure and driven by the Reader;
//! - [`native`]: the text window and line cache the machine answers from, with no IPC;
//! - [`say_all`]: reading a text object from the caret to its end;
//! - this module: the messages, the fetch operations the runtime executes, and the Reader's
//!   side (tracking on focus, key and event routing, utterances, commands).
//!
//! Every fetch leaves the Reader as a [`ReaderEffect::TextFetch`], is run by a task the lane
//! owns, under a deadline, and returns as a [`TextMsg::Fetched`]; the Reader never awaits.

pub mod caret;
pub mod formatting;
pub mod native;
pub mod say_all;

use gorca_i18n::{MsgId as M, msg, t};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use gorca_connector_api::BackendError;
use gorca_input_api::{KeyIntent, classify_text_key};
use gorca_model::{
    Epoch, NodeKey, NodeSnapshot, Prio, Role, StateSet, TextDelta, TextGranularity, UttId,
    Utterance,
};
use gorca_rt::BudgetClass;
use gorca_speech::{
    Caps as VCaps, How, LaneProgress, Lang, Punct as VPunct, VerbalizeOpts, Verbalizer,
};
use gorca_text::caret::Say;

use crate::SpeechIntent;
use crate::mode::Mode;
use crate::present::message;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};
use caret::{CaretState, Cfg, Ctx, Edit, In, Kind, Out, Reply, Spell, Spoken};
use say_all::{Act, Run, SayAll, Target};

/// Most parent hops followed to place a node inside the focused rich editable.
const MAX_ANCESTRY: usize = 16;

/// Identifies one text fetch; unique per Reader.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct TextReqId(pub u64);

/// What the runtime does for the Reader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextOp {
    /// The text around `around` (the caret when `None`).
    Window {
        node: NodeKey,
        around: Option<u32>,
        radius: u32,
    },
    /// One unit by the toolkit's own segmentation.
    AtOffset {
        node: NodeKey,
        offset: u32,
        unit: TextGranularity,
    },
    Selections {
        node: NodeKey,
    },
    /// The formatting of the run at `offset` (`info.formatting`).
    Format {
        node: NodeKey,
        offset: u32,
    },
    /// Move the caret (say-all stopping).
    SetCaret {
        node: NodeKey,
        offset: u32,
    },
    /// The whole visible screen of a terminal and its caret (M6, `TextQuery::Screen`).
    Screen {
        node: NodeKey,
    },
}

impl TextOp {
    pub fn node(&self) -> NodeKey {
        match self {
            TextOp::Window { node, .. }
            | TextOp::AtOffset { node, .. }
            | TextOp::Selections { node }
            | TextOp::Format { node, .. }
            | TextOp::SetCaret { node, .. }
            | TextOp::Screen { node } => *node,
        }
    }
}

/// Messages for the text arm.
#[derive(Debug)]
pub enum TextMsg {
    /// `text-caret-moved` of a node of the focused app. The offset is the toolkit's.
    Caret { node: NodeKey, offset: u32 },
    /// `text-changed`.
    Edit { node: NodeKey, delta: TextDelta },
    /// `text-selection-changed`: it carries no ranges (T-F2).
    Selection { node: NodeKey },
    /// A fetch finished.
    Fetched {
        req: TextReqId,
        epoch: Epoch,
        result: Result<Reply, BackendError>,
    },
    /// The speech server's report on an utterance (say-all).
    Progress(LaneProgress),
}

/// Counters for tests, telemetry and the e2e gates (ADR-0007 14).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TextCounters {
    pub fetches: u64,
    pub fetch_failed: u64,
    pub fetch_timeout: u64,
    pub window_hits: u64,
    pub window_dirty: u64,
    pub caret_correlated: u64,
    pub caret_held_correlated: u64,
    pub caret_absorbed: u64,
    pub caret_late: u64,
    pub caret_edge: u64,
    pub caret_uncorrelated: u64,
    pub typed_echoed: u64,
    /// Text keys of any kind that reached the machine, and of those the typed characters.
    pub keys_seen: u64,
    pub keys_typed: u64,
    pub inserted_spoken: u64,
    pub insert_held_claimed: u64,
    pub inserts_coalesced: u64,
    pub delete_held_claimed: u64,
    pub erased_spoken: u64,
    pub text_flood_coalesced: u64,
    pub selection_spoken: u64,
    pub sel_fetch_failed: u64,
    pub say_all_chunks: u64,
    pub say_all_starved: u64,
    pub say_all_watchdog: u64,
    pub events_dropped: u64,
    /// The caret moved to another node of a focused rich editable (CE-1).
    pub retargets: u64,
}

/// How long a caret event that arrived in browse mode stays valid for the focus that follows.
const EARLY_CARET_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);

/// The Reader's text state.
#[derive(Debug)]
pub struct TextState {
    pub caret: CaretState,
    pub say_all: SayAll,
    pub stats: TextCounters,
    pub(crate) next_req: u64,
    pub(crate) origin: Instant,
    pub(crate) verbalizer: Verbalizer,
    /// Events the app actor could not queue for us: the window cannot be trusted.
    dropped: Option<Arc<AtomicU64>>,
    dropped_seen: u64,
    /// The utterance of the latest secret text, so the copy command can refuse it (PW-3).
    pub(crate) sensitive_utt: Option<UttId>,
    /// The `info.formatting` question in flight: its request and the epoch of the key.
    format_req: Option<(TextReqId, Epoch)>,
    /// Was the layer active (focus or pass-through mode) after the previous message?
    was_active: bool,
    /// A new tracking started in the current turn (its window prefetch is current).
    tracked_this_turn: bool,
    /// The latest caret event that arrived while the layer was inactive (browse mode).
    early_caret: Option<(NodeKey, u32, Instant)>,
    /// The terminal arm (M6).
    pub(crate) term: crate::terminal::TermState,
    out: Vec<Out>,
}

impl TextState {
    pub fn new() -> Self {
        Self {
            caret: CaretState::Off,
            say_all: SayAll::Idle,
            stats: TextCounters::default(),
            next_req: 0,
            origin: Instant::now(),
            verbalizer: Verbalizer,
            dropped: None,
            dropped_seen: 0,
            sensitive_utt: None,
            format_req: None,
            was_active: true,
            tracked_this_turn: false,
            early_caret: None,
            term: crate::terminal::TermState::default(),
            out: Vec::new(),
        }
    }
}

impl Default for TextState {
    fn default() -> Self {
        Self::new()
    }
}

fn punct(p: gorca_config::Punct) -> VPunct {
    match p {
        gorca_config::Punct::None => VPunct::None,
        gorca_config::Punct::Some => VPunct::Some,
        gorca_config::Punct::Most => VPunct::Most,
        gorca_config::Punct::All => VPunct::All,
    }
}

fn caps(c: gorca_config::Caps) -> VCaps {
    VCaps {
        pitch: c.pitch,
        sound: c.sound,
        word: c.word,
    }
}

/// How a spell command is spelled by its press count: letters, then the phonetic alphabet.
pub(crate) fn spell_for(rep: u8) -> Spell {
    match rep {
        0 | 1 => Spell::Letters,
        _ => Spell::Phonetic,
    }
}

/// `info.current_line` pressed once reads the line, twice spells it, three times spells it
/// phonetically.
pub(crate) fn line_spell_for(rep: u8) -> Spell {
    match rep {
        0 | 1 => Spell::No,
        2 => Spell::Letters,
        _ => Spell::Phonetic,
    }
}

/// Which nodes does the text layer track?
fn trackable(snap: &NodeSnapshot) -> Option<Kind> {
    if snap.states.contains(StateSet::DEFUNCT) {
        return None;
    }
    let editable = snap.states.contains(StateSet::EDITABLE);
    let text_role = match snap.role {
        Role::TextEntry | Role::PasswordEntry => true,
        Role::Document | Role::DocumentWeb | Role::Paragraph | Role::Unknown => editable,
        Role::Application
        | Role::Window
        | Role::Dialog
        | Role::Panel
        | Role::Heading
        | Role::Link
        | Role::Image
        | Role::List
        | Role::ListItem
        | Role::Table
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Button
        | Role::ToggleButton
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::SpinButton
        | Role::Slider
        | Role::ProgressBar
        | Role::Menu
        | Role::MenuBar
        | Role::MenuItem
        | Role::Popover
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Label
        | Role::StaticText
        | Role::Terminal
        | Role::Landmark
        | Role::Alert => false,
    };
    text_role.then_some(Kind {
        editable,
        secret: snap.role == Role::PasswordEntry,
        multiline: snap.states.contains(StateSet::MULTILINE),
    })
}

pub(crate) fn decode(text: Option<&gorca_model::LazyText>) -> Option<String> {
    let t = text?;
    std::str::from_utf8(&t.msg).ok().map(str::to_owned)
}

/// An insert as the window must splice it. Gecko reports `len` in UTF-16 units and the text
/// without the phantom U+FEFF it adds after each astral scalar in the string it hands out
/// (`DOMtoATK.h`); the window holds that string, so the phantoms the event left out go back in.
/// A text that already has `len` scalars (every other toolkit) is untouched.
fn with_phantoms(text: String, len: u32) -> String {
    let have = text.chars().count();
    let mut missing = (len as usize).saturating_sub(have);
    if missing == 0 {
        return text;
    }
    let mut out = String::with_capacity(text.len() + missing * 3);
    for c in text.chars() {
        out.push(c);
        if missing > 0 && u32::from(c) > 0xFFFF {
            out.push(gorca_text::seg::PHANTOM);
            missing -= 1;
        }
    }
    out
}

/// Result of [`Reader::text_ancestry`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ancestry {
    Inside,
    Outside,
    Unknown,
}

impl Reader {
    /// Share the counter the app actors bump when they could not queue a text event for us.
    #[must_use]
    pub fn with_text_dropped(mut self, dropped: Arc<AtomicU64>) -> Self {
        self.text.dropped = Some(dropped);
        self
    }

    pub fn text_counters(&self) -> TextCounters {
        self.text.stats
    }

    /// Does the latest remembered utterance come from a password field (PW-3)?
    pub fn last_spoken_is_sensitive(&self) -> bool {
        match (&self.cmd.last_spoken, self.text.sensitive_utt) {
            (Some(u), Some(id)) => u.id == id,
            (Some(_) | None, Some(_) | None) => false,
        }
    }

    pub(crate) fn text_active(&self) -> bool {
        matches!(self.mode, Mode::Focus | Mode::PassThrough { .. })
    }

    /// Review ended: caret, edit and selection events were dropped while it lasted, so the
    /// tracked window is stale. It is refetched on demand.
    pub(crate) fn text_resync_after_review(&mut self, fx: &mut ReaderEffects) {
        if !self.text.caret.is_off() && self.text_active() {
            self.text_step(In::Dirty, fx);
        }
    }

    fn text_cfg(&self) -> Cfg {
        let s = &self.cmd.effective;
        Cfg {
            typed_chars: s.echo.typed_characters,
            typed_words: s.echo.typed_words,
            inserted_text: s.echo.inserted_text,
            edge: std::time::Duration::from_millis(u64::from(s.text.caret_edge_ms)),
            late: std::time::Duration::from_millis(u64::from(s.text.caret_late_ms)),
        }
    }

    /// Run one input through the caret machine and turn its output into effects.
    fn text_step(&mut self, input: In, fx: &mut ReaderEffects) {
        let now = self.now;
        let now_ms = u64::try_from(now.saturating_duration_since(self.text.origin).as_millis())
            .unwrap_or(u64::MAX);
        let cfg = self.text_cfg();
        let insert_slack = self
            .text
            .caret
            .tracked()
            .map_or(0, |t| self.dir.text_insert_slack(t.root.app));
        let epoch_now = self.epoch.current();
        let mut out = std::mem::take(&mut self.text.out);
        out.clear();
        let state = std::mem::take(&mut self.text.caret);
        let next = {
            let mut cx = Ctx {
                now,
                now_ms,
                epoch_now,
                cfg,
                insert_slack,
                next_req: &mut self.text.next_req,
                stats: &mut self.text.stats,
                out: &mut out,
            };
            state.step(input, &mut cx)
        };
        self.text.caret = next;
        for o in out.drain(..) {
            match &o {
                Out::Say(s) => {
                    tracing::debug!(target: "gorca_text", pieces = s.pieces.len(), reason = ?s.reason, "say");
                }
                Out::Fetch { op, .. } => {
                    tracing::debug!(target: "gorca_text", ?op, "fetch");
                }
            }
            match o {
                Out::Say(s) => {
                    let u = self.text_utterance(&s);
                    fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                }
                Out::Fetch {
                    req,
                    op,
                    class,
                    epoch,
                } => fx.push(ReaderEffect::TextFetch {
                    req,
                    op,
                    class,
                    epoch,
                }),
            }
        }
        self.text.out = out;
        if let Some(d) = self.text.caret.deadline() {
            fx.push(ReaderEffect::ArmTimer(d));
        }
    }

    pub(crate) fn verbalize_opts(&self) -> VerbalizeOpts {
        let s = &self.cmd.effective.speech;
        VerbalizeOpts {
            punct: punct(s.punctuation),
            caps: caps(s.caps),
            lang: Lang::from_code(&s.language),
        }
    }

    /// The utterance as it goes to the speech lane: the text the applications gave (names,
    /// values, descriptions, notifications, OSD) has its symbols named by the current
    /// punctuation level and language, like text read from a document. The server speaks with
    /// punctuation off, so this is the only place a symbol gets its name. Segments that are
    /// already verbalised or are the reader's own words (`Said`) and roles and states pass.
    pub fn symbolize(&self, mut u: Utterance) -> Utterance {
        let o = self.verbalize_opts();
        for seg in &mut u.segments {
            if !matches!(
                seg.kind,
                gorca_model::SegmentKind::Text
                    | gorca_model::SegmentKind::Value
                    | gorca_model::SegmentKind::Description
            ) || seg.text.chars().all(|c| c.is_alphanumeric() || c == ' ')
            {
                continue;
            }
            let said = self.text.verbalizer.unit(&seg.text, How::Text, &o);
            if *said != *seg.text {
                seg.text = Arc::from(said);
            }
        }
        u.segments.retain(|s| !s.text.is_empty());
        u
    }

    /// The options for a single character that `mark_capital` will mark: the capital word is
    /// left out, `mark_capital` puts it before the letter.
    pub(crate) fn char_opts(&self) -> VerbalizeOpts {
        let mut o = self.verbalize_opts();
        o.caps.word = false;
        o
    }

    /// Verbalise one piece of text speech (VB-4: on the Reader, never on the speech lane).
    pub(crate) fn speak_pieces(&self, pieces: &[caret::Piece]) -> String {
        self.speak_pieces_with(pieces, self.verbalize_opts())
    }

    fn speak_pieces_with(&self, pieces: &[caret::Piece], o: VerbalizeOpts) -> String {
        let mut text = String::new();
        for p in pieces {
            if !text.is_empty() {
                text.push(' ');
            }
            let part = match (p.how, p.spell) {
                (Some(_), Spell::Letters) => self.text.verbalizer.spell(&p.text, false, &o),
                (Some(_), Spell::Phonetic) => self.text.verbalizer.spell(&p.text, true, &o),
                (Some(how), Spell::No) => self.text.verbalizer.unit(&p.text, how, &o),
                (None, Spell::No | Spell::Letters | Spell::Phonetic) => p.text.clone(),
            };
            text.push_str(&part);
        }
        text
    }

    fn text_utterance(&mut self, s: &Spoken) -> Utterance {
        // A single character (typed, a character move, an erasure): a capital gets its
        // indication.
        let single_char =
            matches!(s.pieces.as_slice(), [p] if p.how == Some(How::Char) && p.spell == Spell::No);
        let text = if single_char {
            self.speak_pieces_with(&s.pieces, self.char_opts())
        } else {
            self.speak_pieces(&s.pieces)
        };
        let id = self.utt_id();
        let mut u = message(&text, id, s.epoch);
        u.prio = s.prio;
        if single_char {
            self.mark_capital(&mut u);
        }
        // Typing echo is not kept in the speech history.
        if matches!(
            s.reason,
            gorca_text::echo::Reason::Typed
                | gorca_text::echo::Reason::TypedWord
                | gorca_text::echo::Reason::Erased
        ) {
            self.cmd.quick.echo_ids.push(id);
        }
        if s.sensitive {
            self.text.sensitive_utt = Some(id);
        }
        u
    }

    // ---- focus ---------------------------------------------------------------------------

    /// The focus presentation of `snap` finished: track it if it is a text object.
    pub(crate) fn text_track(&mut self, snap: &NodeSnapshot, fx: &mut ReaderEffects) {
        if !self.text_active() {
            return;
        }
        // Byte offsets (VTE; a capability the connector reports, not a name) are the terminal
        // layer's. Gecko's UTF-16 offsets are followed as they are (`TextUnit::window_follows`).
        if !self.dir.text_window_follows(snap.key.app) {
            self.text_untrack("app not followed");
            return;
        }
        // A terminal has its own arm (M6): the caret machine would speak a screen repaint as
        // inserted text.
        if snap.role == Role::Terminal {
            self.text_untrack_caret();
            self.term_track(snap, fx);
            return;
        }
        self.term_untrack("text object focused");
        match trackable(snap) {
            Some(kind) => {
                let was_off = self.text.caret.is_off();
                self.text_step(
                    In::Track {
                        node: snap.key,
                        kind,
                    },
                    fx,
                );
                if was_off && !self.text.caret.is_off() {
                    self.text.tracked_this_turn = true;
                }
            }
            None => self.text_untrack("not trackable"),
        }
    }

    /// Focus moved on (or the object is gone): forget the window, zeroing it for a secret one.
    pub(crate) fn text_untrack(&mut self, why: &'static str) {
        self.term_untrack(why);
        self.text.early_caret = None;
        self.text_untrack_caret();
    }

    fn text_untrack_caret(&mut self) {
        if self.text.caret.is_off() {
            return;
        }
        let mut sink = ReaderEffects::new();
        self.text_step(In::Untrack, &mut sink);
    }

    /// The tracked object was removed, or its application is gone: stop tracking it.
    pub(crate) fn text_node_gone(
        &mut self,
        node: Option<NodeKey>,
        app: Option<gorca_model::AppId>,
    ) {
        self.term_node_gone(node, app);
        let Some(t) = self.text.caret.tracked() else {
            return;
        };
        let (current, root) = (t.node, t.root);
        if node == Some(root) || app == Some(root.app) {
            self.text_untrack("tracked object gone");
        } else if node == Some(current) && current != root {
            // The paragraph the caret was in is gone (a merge): back to the focused object.
            let mut sink = ReaderEffects::new();
            self.text_step(
                In::Retarget {
                    node: root,
                    at: None,
                },
                &mut sink,
            );
        }
    }

    // ---- keys ----------------------------------------------------------------------------

    /// A key that reaches the application. Returns whether the text layer speaks it itself, so
    /// the generic key echo stays silent (no double echo, TI-3).
    pub(crate) fn text_key(&mut self, intent: &KeyIntent, fx: &mut ReaderEffects) -> bool {
        if self.term_tracked() {
            return self.term_key(intent, fx);
        }
        if !intent.event.pressed || self.text.caret.is_off() || !self.text_active() {
            return false;
        }
        let Some(key) = classify_text_key(&intent.event) else {
            return false;
        };
        tracing::debug!(target: "gorca_text", key = ?key, "text key");
        self.text.stats.keys_seen += 1;
        if matches!(key, gorca_input_api::TextKey::Typed { .. }) {
            self.text.stats.keys_typed += 1;
        }
        let owns = self.text.caret.owns_key(key);
        self.text_step(
            In::Key {
                key,
                epoch: intent.epoch,
            },
            fx,
        );
        owns
    }

    // ---- events --------------------------------------------------------------------------

    /// Did the app actors lose text events since this was last asked? If so the layers that
    /// trust their window or screen are told, and the answer is true. Asked on every text
    /// message and by a terminal review step: a loss followed by silence would otherwise go
    /// unnoticed for good.
    pub(crate) fn text_losses(&mut self, fx: &mut ReaderEffects) -> bool {
        let Some(d) = &self.text.dropped else {
            return false;
        };
        let n = d.load(Ordering::Relaxed);
        if n == self.text.dropped_seen {
            return false;
        }
        self.text.dropped_seen = n;
        self.text.stats.events_dropped = n;
        self.term_events_lost(fx);
        if !self.text.caret.is_off() {
            self.text_step(In::Dirty, fx);
        }
        true
    }

    pub(crate) fn on_text(&mut self, m: TextMsg, fx: &mut ReaderEffects) {
        self.text_losses(fx);
        match &m {
            TextMsg::Caret { node, offset } => {
                tracing::debug!(target: "gorca_text", ?node, offset, "caret event");
            }
            TextMsg::Edit { node, delta } => {
                tracing::debug!(target: "gorca_text", ?node, ?delta, "edit event");
            }
            TextMsg::Selection { node } => {
                tracing::debug!(target: "gorca_text", ?node, "selection event");
            }
            TextMsg::Fetched { .. } | TextMsg::Progress(_) => {}
        }
        match m {
            TextMsg::Caret { node, offset } => {
                if self.web_caret(node, offset, fx) {
                    return;
                }
                if self.term_is(node) {
                    self.term_caret(offset, fx);
                } else if !self.text_active() {
                    // Chromium places the caret of a contenteditable before the focus (and so
                    // the switch to focus mode) is handled: keep the event for the moment the
                    // window of the field is known (`replay_early_caret`).
                    self.text.early_caret = Some((node, offset, self.now));
                } else if self.text_accepts(node, Some(offset), fx) {
                    self.text.early_caret = None;
                    self.text_step(In::Caret { offset }, fx);
                } else if self.awaiting_hosts(node) {
                    // The window of the rich editable's root is still on its way (it says
                    // whether the paragraphs are its own): keep the event for `hosts`.
                    self.text.early_caret = Some((node, offset, self.now));
                }
            }
            TextMsg::Edit { node, delta } => {
                if self.term_is(node) {
                    self.term_edit(delta, fx);
                } else if self.text_active() && self.text_accepts(node, None, fx) {
                    let edit = match delta {
                        TextDelta::Insert { offset, len, text } => Edit::Insert {
                            offset,
                            len,
                            text: decode(text.as_ref()).map(|t| with_phantoms(t, len)),
                        },
                        TextDelta::Delete { offset, len, text } => Edit::Delete {
                            offset,
                            len,
                            text: decode(text.as_ref()),
                        },
                        TextDelta::Replaced { .. } => Edit::Replaced,
                    };
                    self.text_step(In::Edit(edit), fx);
                }
            }
            TextMsg::Selection { node } => {
                if self.text_active() && self.text_accepts(node, None, fx) {
                    self.text_step(In::Selection, fx);
                }
            }
            TextMsg::Fetched { req, epoch, result } => self.on_text_fetched(req, epoch, result, fx),
            TextMsg::Progress(p) => self.say_all_progress(p, fx),
        }
    }

    /// An editable root is tracked, has not yet learned whether it hosts paragraphs, and
    /// `node` is another node of the same application.
    fn awaiting_hosts(&self, node: NodeKey) -> bool {
        self.text.caret.tracked().is_some_and(|t| {
            t.kind.editable && !t.hosts && t.node != node && t.root.app == node.app
        })
    }

    /// Does an event of `node` belong to the tracked object? The node itself, yes. In a rich
    /// editable (contenteditable; its own text is made of embedded objects) the caret, the edits
    /// and the selection live on the paragraph the caret is in, a descendant of the focused
    /// object: the machine then moves to that node (CE-1). `at` is the caret offset when the
    /// event is a caret event.
    fn text_accepts(&mut self, node: NodeKey, at: Option<u32>, fx: &mut ReaderEffects) -> bool {
        let Some(t) = self.text.caret.tracked() else {
            return false;
        };
        if t.node == node {
            return true;
        }
        if node.app != t.root.app || !t.kind.editable || !t.hosts {
            return false;
        }
        if node != t.root {
            match (self.text_ancestry(t.root, node), at.is_some()) {
                // Outside the editable, or (Unknown) a text change or selection of a node
                // the mirror does not know: some other part of the page (a clock, a chat log)
                // that must not steal the editable. Only a caret event may redirect tracking
                // on an unknown node: Gecko sends caret-moved for the focused editable's
                // descendants only.
                (Ancestry::Outside, _) | (Ancestry::Unknown, false) => return false,
                (Ancestry::Unknown, true) | (Ancestry::Inside, _) => {}
            }
        }
        self.text.stats.retargets += 1;
        self.text_step(In::Retarget { node, at }, fx);
        true
    }

    /// Where does `node` hang relative to `root`, as far as the mirror knows? `Unknown` is an
    /// ancestry the mirror does not hold (a paragraph just created, or a node of another part of
    /// the page that was never mirrored).
    fn text_ancestry(&self, root: NodeKey, node: NodeKey) -> Ancestry {
        let mut cur = node;
        for _ in 0..MAX_ANCESTRY {
            let snap = match self.dir.lookup(cur) {
                gorca_model::Lookup::Hit(s) => s,
                gorca_model::Lookup::Miss | gorca_model::Lookup::Gone => return Ancestry::Unknown,
            };
            match snap.parent {
                Some(p) if p == root => return Ancestry::Inside,
                Some(p) => cur = p,
                None => return Ancestry::Outside,
            }
        }
        Ancestry::Outside
    }

    fn on_text_fetched(
        &mut self,
        req: TextReqId,
        epoch: Epoch,
        result: Result<Reply, BackendError>,
        fx: &mut ReaderEffects,
    ) {
        let _ = epoch;
        match &result {
            Ok(Reply::Window(w)) => {
                tracing::debug!(target: "gorca_text", base = w.base, chars = w.text.chars().count(), len = w.len, caret = ?w.caret, "window reply");
            }
            Ok(Reply::Unit(u)) => {
                tracing::debug!(target: "gorca_text", range = ?u.range, chars = u.text.chars().count(), "unit reply");
            }
            Ok(_) => {
                tracing::debug!(target: "gorca_text", "other reply");
            }
            Err(e) => {
                tracing::debug!(target: "gorca_text", error = ?e, "fetch failed");
            }
        }
        if self.term_owns_fetch(req) {
            let r = match result {
                Ok(r) => Ok(r),
                Err(BackendError::Timeout) => {
                    self.text.stats.fetch_timeout += 1;
                    Err(())
                }
                Err(
                    BackendError::Gone
                    | BackendError::NotSupported
                    | BackendError::Busy
                    | BackendError::Protocol(_)
                    | BackendError::Unresolved,
                ) => Err(()),
            };
            self.term_fetched(req, r, fx);
            return;
        }
        if let Some((want, key_epoch)) = self.text.format_req
            && want == req
        {
            self.text.format_req = None;
            self.text_formatting_reply(result, key_epoch, fx);
            return;
        }
        if let SayAll::Running(run) = &self.text.say_all
            && run.fetch == Some(req)
        {
            self.say_all_fetched(result, fx);
            return;
        }
        let r = match result {
            Ok(r) => Ok(r),
            Err(BackendError::Timeout) => {
                self.text.stats.fetch_timeout += 1;
                Err(())
            }
            Err(
                BackendError::Gone
                | BackendError::NotSupported
                | BackendError::Busy
                | BackendError::Protocol(_)
                | BackendError::Unresolved,
            ) => Err(()),
        };
        self.text_step(In::Fetched { req, result: r }, fx);
        self.replay_early_caret(fx);
    }

    /// The caret event of a rich editable that came before its focus (see `on_text`) is
    /// handled once the fill of the root says it hosts embedded objects: the same as if the
    /// browser had sent it after the focus.
    fn replay_early_caret(&mut self, fx: &mut ReaderEffects) {
        let Some((node, offset, at)) = self.text.early_caret else {
            return;
        };
        if self.now.saturating_duration_since(at) > EARLY_CARET_WINDOW {
            self.text.early_caret = None;
            return;
        }
        let ready = self.text.caret.tracked().is_some_and(|t| t.hosts);
        if ready {
            self.text.early_caret = None;
            if self.text_active() && self.text_accepts(node, Some(offset), fx) {
                self.text_step(In::Caret { offset }, fx);
            }
        }
    }

    pub(crate) fn text_timer(&mut self, fx: &mut ReaderEffects) {
        self.term_timer(fx);
        if !self.text.caret.is_off() {
            self.text_step(In::Timer, fx);
        }
        // A reading the speech lane has said nothing about for too long is over (the backstop
        // for a lost progress report); it never moves the caret by itself afterwards.
        let expired = matches!(&self.text.say_all, SayAll::Running(r) if r.expired(self.now));
        if expired {
            self.text.stats.say_all_watchdog += 1;
            self.say_all_finish(false, fx);
        } else {
            self.arm_say_all_watchdog(fx);
        }
    }

    fn arm_say_all_watchdog(&self, fx: &mut ReaderEffects) {
        if let SayAll::Running(run) = &self.text.say_all {
            fx.push(ReaderEffect::ArmTimer(run.deadline()));
        }
    }

    // ---- commands ------------------------------------------------------------------------

    /// `text.say_*`, `text.spell_*` and `info.current_line`: the unit at the caret
    /// (ADR-0007 12).
    pub(crate) fn text_say_unit(
        &mut self,
        say: Say,
        spell: Spell,
        epoch: Epoch,
        fx: &mut ReaderEffects,
    ) {
        if self.term_tracked() {
            self.term_say_unit(say, spell, epoch, fx);
            return;
        }
        if self.text.caret.is_off() || !self.text_active() {
            self.say(t(M::TextNotInField), fx);
            return;
        }
        self.text_step(In::Say { say, spell, epoch }, fx);
    }

    /// May the native text layer answer a command now? In Review (and Learn) mode another
    /// layer owns reading, and in Browse mode the document does: refuse with a short cue
    /// instead of reading the entry that merely still holds the focus. This is the hook M7a
    /// uses: its browse commands route to the document before they reach the commands below.
    fn text_mode_allows(&mut self, fx: &mut ReaderEffects) -> bool {
        if self.text_active() {
            return true;
        }
        self.say(t(M::TextNotAvailableMode), fx);
        false
    }

    /// `info.formatting`: ask for the attribute run under the caret and speak it when it comes.
    pub(crate) fn text_say_formatting(&mut self, epoch: Epoch, fx: &mut ReaderEffects) {
        if !self.text_mode_allows(fx) {
            return;
        }
        let Some(t) = self.text.caret.tracked() else {
            self.say(t(M::TextNotInField), fx);
            return;
        };
        let at = t.caret.unwrap_or(0);
        // Behind the last character there is no run: ask about the one in front of it.
        let at = if t.win.len() == Some(at) {
            at.saturating_sub(1)
        } else {
            at
        };
        let node = t.node;
        self.text.next_req += 1;
        let req = TextReqId(self.text.next_req);
        self.text.format_req = Some((req, epoch));
        fx.push(ReaderEffect::TextFetch {
            req,
            op: TextOp::Format { node, offset: at },
            class: BudgetClass::Interactive,
            epoch,
        });
    }

    fn text_formatting_reply(
        &mut self,
        result: Result<Reply, BackendError>,
        epoch: Epoch,
        fx: &mut ReaderEffects,
    ) {
        let text = match result {
            Ok(Reply::Format(f)) => {
                let d = formatting::describe(&f);
                if d.is_empty() {
                    t(M::TextNoFormatting).to_owned()
                } else {
                    d
                }
            }
            Ok(Reply::Window(_) | Reply::Unit(_) | Reply::Selections(_) | Reply::Done) => {
                t(M::TextNoFormatting).to_owned()
            }
            Err(BackendError::NotSupported) => t(M::TextFormattingUnavailable).to_owned(),
            Err(
                BackendError::Timeout
                | BackendError::Gone
                | BackendError::Busy
                | BackendError::Protocol(_)
                | BackendError::Unresolved,
            ) => t(M::TextNoFormatting).to_owned(),
        };
        let id = self.utt_id();
        let mut u = message(&text, id, epoch);
        u.prio = Prio::Interrupt;
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    pub(crate) fn text_say_selection(&mut self, epoch: Epoch, fx: &mut ReaderEffects) {
        if !self.text_mode_allows(fx) {
            return;
        }
        let Some(t) = self.text.caret.tracked() else {
            self.say(t(M::TextNotInField), fx);
            return;
        };
        let secret = t.kind.secret;
        let sel = caret::selected_text(t);
        let pieces = match sel {
            None => vec![caret::Piece {
                how: None,
                text: gorca_i18n::t(M::TextNoSelection).to_owned(),
                spell: Spell::No,
            }],
            Some((text, n)) if text.is_empty() => vec![caret::Piece {
                how: None,
                text: msg!(TextCharsSelected, n = n),
                spell: Spell::No,
            }],
            Some((text, _)) => vec![caret::Piece {
                how: Some(How::Text),
                text,
                spell: Spell::No,
            }],
        };
        let text = self.speak_pieces(&pieces);
        let id = self.utt_id();
        let mut u = message(&text, id, epoch);
        u.prio = Prio::Interrupt;
        if secret {
            self.text.sensitive_utt = Some(id);
        }
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    // ---- say-all -------------------------------------------------------------------------

    pub(crate) fn say_all_start(&mut self, epoch: Epoch, fx: &mut ReaderEffects) {
        // Browse mode reads the page from the virtual cursor (browse design 9).
        if matches!(self.mode, Mode::Browse { .. }) {
            let _ = self.web_say_all_start(epoch, fx);
            return;
        }
        if !self.text_mode_allows(fx) {
            return;
        }
        let Some(t) = self.text.caret.tracked() else {
            self.say(t(M::TextNotInField), fx);
            return;
        };
        let from = t.caret.unwrap_or(0);
        let run = Run::new(t.node, epoch, from, t.kind.secret, self.now);
        self.text.say_all = SayAll::Running(run);
        self.say_all_pump(fx);
    }

    /// Carry out what a reading asks. True when it was filled from a local source (a document)
    /// and the reading should be pumped again at once.
    fn say_all_apply(
        &mut self,
        acts: smallvec::SmallVec<[Act; 4]>,
        fx: &mut ReaderEffects,
    ) -> bool {
        let mut refilled = false;
        for a in acts {
            match a {
                Act::Fetch { around } => {
                    let SayAll::Running(run) = &mut self.text.say_all else {
                        continue;
                    };
                    match run.target {
                        Target::Native(node) => {
                            self.text.next_req += 1;
                            let req = TextReqId(self.text.next_req);
                            run.fetch = Some(req);
                            fx.push(ReaderEffect::TextFetch {
                                req,
                                op: TextOp::Window {
                                    node,
                                    around: Some(around.saturating_add(say_all::FETCH_RADIUS)),
                                    radius: say_all::FETCH_RADIUS,
                                },
                                class: BudgetClass::Prefetch,
                                epoch: run.epoch,
                            });
                        }
                        Target::Doc(doc) => {
                            // The page is the Reader's own: no fetch, no wait.
                            let win = self.web_flat_window(doc, around);
                            if let SayAll::Running(run) = &mut self.text.say_all {
                                match win {
                                    Some((base, text, len)) => run.fill(base, &text, len),
                                    None => run.fill(0, "", 0),
                                }
                                refilled = true;
                            }
                        }
                    }
                }
                Act::Submit {
                    utt: id,
                    text,
                    first,
                    range,
                } => {
                    let SayAll::Running(run) = &self.text.say_all else {
                        continue;
                    };
                    let (epoch, secret, target) = (run.epoch, run.secret, run.target);
                    let u = match target {
                        Target::Native(_) => {
                            let o = self.verbalize_opts();
                            let spoken = self.text.verbalizer.unit(&text, How::Text, &o);
                            let mut u = message(&spoken, id, epoch);
                            u.prio = Prio::Text;
                            u
                        }
                        Target::Doc(doc) => {
                            match self.web_chunk_utterance(doc, &range, id, epoch) {
                                Some(u) => u,
                                None => continue,
                            }
                        }
                    };
                    if secret {
                        self.text.sensitive_utt = Some(id);
                    }
                    self.text.stats.say_all_chunks += 1;
                    fx.push(ReaderEffect::SayAllChunk {
                        intent: Box::new(SpeechIntent { utterance: u }),
                        first,
                    });
                }
            }
        }
        refilled
    }

    pub(crate) fn say_all_pump(&mut self, fx: &mut ReaderEffects) {
        let acts = {
            let SayAll::Running(run) = &mut self.text.say_all else {
                return;
            };
            let next = &mut self.next_utt;
            run.pump(|| {
                *next += 1;
                UttId(*next)
            })
        };
        if self.say_all_apply(acts, fx) {
            // A document filled the buffer in place: prepare the chunks it holds.
            self.say_all_pump(fx);
            return;
        }
        self.arm_say_all_watchdog(fx);
        let done = matches!(&self.text.say_all, SayAll::Running(r) if r.is_done());
        if done {
            self.say_all_finish(true, fx);
        }
    }

    fn say_all_fetched(&mut self, result: Result<Reply, BackendError>, fx: &mut ReaderEffects) {
        match result {
            Ok(Reply::Window(fill)) => {
                if let SayAll::Running(run) = &mut self.text.say_all {
                    run.fill(fill.base, &fill.text, fill.len);
                }
                self.say_all_pump(fx);
            }
            Ok(Reply::Unit(_) | Reply::Selections(_) | Reply::Format(_) | Reply::Done) | Err(_) => {
                // The text is not coming: end the reading where it stands.
                self.text.stats.say_all_starved += 1;
                self.say_all_finish(false, fx);
            }
        }
    }

    fn say_all_progress(&mut self, p: LaneProgress, fx: &mut ReaderEffects) {
        let now = self.now;
        let out = match &mut self.text.say_all {
            SayAll::Running(run) => run.progress(p, now),
            SayAll::Idle => return,
        };
        // The caret follows the chunk being spoken, so a key that stops the reading acts from
        // where the reading is, and gorca's own move is long done when the key reaches the
        // application (no race with it, M1 of the review).
        if let Some(at) = out.place {
            self.say_all_place(at, fx);
        }
        if out.pump {
            self.say_all_pump(fx);
        } else if matches!(&self.text.say_all, SayAll::Running(r) if r.is_done()) {
            self.say_all_finish(true, fx);
        }
        self.arm_say_all_watchdog(fx);
    }

    /// Put the caret at `offset`; the caret event this causes is gorca's own.
    fn say_all_place(&mut self, offset: u32, fx: &mut ReaderEffects) {
        let SayAll::Running(run) = &self.text.say_all else {
            return;
        };
        let target = run.target;
        let node = match target {
            // The virtual cursor follows the reading whatever the caret setting says.
            Target::Doc(doc) => {
                self.web_follow(doc, offset, fx);
                return;
            }
            Target::Native(node) => node,
        };
        if !self.cmd.effective.text.say_all_moves_caret {
            return;
        }
        let epoch = self.epoch.current();
        self.text.next_req += 1;
        let req = TextReqId(self.text.next_req);
        self.text.caret.note_own_move(offset, self.now);
        fx.push(ReaderEffect::TextFetch {
            req,
            op: TextOp::SetCaret { node, offset },
            class: BudgetClass::Prefetch,
            epoch,
        });
    }

    /// End the reading. A reading that ran to the end of the text leaves the caret there (when
    /// the setting says so); one that was stopped or went silent never moves it: the caret is
    /// already at the chunk that was being spoken.
    fn say_all_finish(&mut self, natural: bool, fx: &mut ReaderEffects) {
        let SayAll::Running(run) = std::mem::take(&mut self.text.say_all) else {
            return;
        };
        if !natural {
            return;
        }
        let at = run.end_caret();
        let node = match run.target {
            Target::Doc(doc) => {
                // The end of the page: the virtual cursor stays on the last chunk read.
                self.web_follow(doc, at, fx);
                return;
            }
            Target::Native(node) => node,
        };
        if !self.cmd.effective.text.say_all_moves_caret {
            return;
        }
        self.text.next_req += 1;
        let req = TextReqId(self.text.next_req);
        self.text.caret.note_own_move(at, self.now);
        fx.push(ReaderEffect::TextFetch {
            req,
            op: TextOp::SetCaret { node, offset: at },
            class: BudgetClass::Prefetch,
            epoch: self.epoch.current(),
        });
    }

    /// After every message: a reading ends the moment something newer happens (SA-4).
    pub(crate) fn text_after(&mut self, fx: &mut ReaderEffects) {
        if self.text.say_all.stopped_by(self.epoch.current()) {
            self.say_all_finish(false, fx);
        }
        let fresh = std::mem::take(&mut self.text.tracked_this_turn);
        self.text_mode_changed(fresh, fx);
        self.text.tracked_this_turn = false;
    }

    /// The text layer works in focus and pass-through mode only; events that arrive in browse
    /// or review mode are dropped. Coming back, what the layer holds is stale (the field kept
    /// changing while the virtual cursor read elsewhere), so the window is dropped, and a
    /// field that got the focus while the mode was browse starts being tracked now (M7a: Enter
    /// on a field focuses it and switches to focus mode in the same breath; Insert+Space
    /// switches by hand on a field that already has the focus).
    ///
    /// `fresh_track`: the tracking started in this very turn, so its window request is already
    /// current; dirtying it would discard the reply that carries the structural facts (does the
    /// root host embedded objects) every later paragraph event depends on.
    fn text_mode_changed(&mut self, fresh_track: bool, fx: &mut ReaderEffects) {
        let active = self.text_active();
        if active == self.text.was_active {
            return;
        }
        self.text.was_active = active;
        if !active {
            if !self.text.caret.is_off() {
                self.text_step(In::Dirty, fx);
            }
            return;
        }
        let snap = self.presented.as_ref().map(|p| Arc::clone(&p.snap));
        match (self.text.caret.is_off(), snap) {
            (true, Some(s)) if s.states.contains(StateSet::FOCUSED) => self.text_track(&s, fx),
            (false, _) if fresh_track => {}
            (false, _) => self.text_step(In::Dirty, fx),
            (true, _) => {}
        }
    }
}

#[cfg(test)]
mod tests;
