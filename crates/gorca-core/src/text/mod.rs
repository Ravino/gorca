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
pub mod native;
pub mod say_all;

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
use gorca_speech::{Caps as VCaps, How, LaneProgress, Punct as VPunct, VerbalizeOpts, Verbalizer};
use gorca_text::caret::Say;

use crate::SpeechIntent;
use crate::mode::Mode;
use crate::present::message;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};
use caret::{CaretState, Cfg, Ctx, Edit, In, Kind, Out, Reply, Spell, Spoken};
use say_all::{Act, Run, SayAll};

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
    /// Move the caret (say-all stopping).
    SetCaret {
        node: NodeKey,
        offset: u32,
    },
}

impl TextOp {
    pub fn node(&self) -> NodeKey {
        match self {
            TextOp::Window { node, .. }
            | TextOp::AtOffset { node, .. }
            | TextOp::Selections { node }
            | TextOp::SetCaret { node, .. } => *node,
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
}

/// The Reader's text state.
#[derive(Debug)]
pub struct TextState {
    pub caret: CaretState,
    pub say_all: SayAll,
    pub stats: TextCounters,
    next_req: u64,
    origin: Instant,
    verbalizer: Verbalizer,
    /// Events the app actor could not queue for us: the window cannot be trusted.
    dropped: Option<Arc<AtomicU64>>,
    dropped_seen: u64,
    /// The utterance of the latest secret text, so the copy command can refuse it (PW-3).
    sensitive_utt: Option<UttId>,
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
    match c {
        gorca_config::Caps::Pitch => VCaps::Pitch,
        gorca_config::Caps::Say => VCaps::Say,
        gorca_config::Caps::Off => VCaps::Off,
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

fn decode(text: Option<&gorca_model::LazyText>) -> Option<String> {
    let t = text?;
    std::str::from_utf8(&t.msg).ok().map(str::to_owned)
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

    fn text_active(&self) -> bool {
        matches!(self.mode, Mode::Focus | Mode::PassThrough { .. })
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
                next_req: &mut self.text.next_req,
                stats: &mut self.text.stats,
                out: &mut out,
            };
            state.step(input, &mut cx)
        };
        self.text.caret = next;
        for o in out.drain(..) {
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

    fn verbalize_opts(&self) -> VerbalizeOpts {
        let s = &self.cmd.effective.speech;
        VerbalizeOpts {
            punct: punct(s.punctuation),
            caps: caps(s.caps),
        }
    }

    /// Verbalise one piece of text speech (VB-4: on the Reader, never on the speech lane).
    pub(crate) fn speak_pieces(&self, pieces: &[caret::Piece]) -> String {
        let o = self.verbalize_opts();
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
        let text = self.speak_pieces(&s.pieces);
        let id = self.utt_id();
        let mut u = message(&text, id, s.epoch);
        u.prio = s.prio;
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
        // Text counted in UTF-16 units (a capability the connector reports, not a name) is not
        // ours until M3b converts offsets: tracking it would speak the wrong characters.
        if !self.dir.text_scalar(snap.key.app) {
            self.text_untrack();
            return;
        }
        match trackable(snap) {
            Some(kind) => self.text_step(
                In::Track {
                    node: snap.key,
                    kind,
                },
                fx,
            ),
            None => self.text_untrack(),
        }
    }

    /// Focus moved on (or the object is gone): forget the window, zeroing it for a secret one.
    pub(crate) fn text_untrack(&mut self) {
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
        let hit = self
            .text
            .caret
            .tracked()
            .is_some_and(|t| node == Some(t.node) || app == Some(t.node.app));
        if hit {
            self.text_untrack();
        }
    }

    // ---- keys ----------------------------------------------------------------------------

    /// A key that reaches the application. Returns whether the text layer speaks it itself, so
    /// the generic key echo stays silent (no double echo, TI-3).
    pub(crate) fn text_key(&mut self, intent: &KeyIntent, fx: &mut ReaderEffects) -> bool {
        if !intent.event.pressed || self.text.caret.is_off() || !self.text_active() {
            return false;
        }
        let Some(key) = classify_text_key(&intent.event) else {
            return false;
        };
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

    pub(crate) fn on_text(&mut self, m: TextMsg, fx: &mut ReaderEffects) {
        if let Some(d) = &self.text.dropped {
            let n = d.load(Ordering::Relaxed);
            if n != self.text.dropped_seen {
                self.text.dropped_seen = n;
                self.text.stats.events_dropped = n;
                if !self.text.caret.is_off() {
                    self.text_step(In::Dirty, fx);
                }
            }
        }
        match m {
            TextMsg::Caret { node, offset } => {
                if self.text_is_tracked(node) && self.text_active() {
                    self.text_step(In::Caret { offset }, fx);
                }
            }
            TextMsg::Edit { node, delta } => {
                if self.text_is_tracked(node) && self.text_active() {
                    let edit = match delta {
                        TextDelta::Insert { offset, len, text } => Edit::Insert {
                            offset,
                            len,
                            text: decode(text.as_ref()),
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
                if self.text_is_tracked(node) && self.text_active() {
                    self.text_step(In::Selection, fx);
                }
            }
            TextMsg::Fetched { req, epoch, result } => self.on_text_fetched(req, epoch, result, fx),
            TextMsg::Progress(p) => self.say_all_progress(p, fx),
        }
    }

    fn text_is_tracked(&self, node: NodeKey) -> bool {
        self.text.caret.tracked().is_some_and(|t| t.node == node)
    }

    fn on_text_fetched(
        &mut self,
        req: TextReqId,
        epoch: Epoch,
        result: Result<Reply, BackendError>,
        fx: &mut ReaderEffects,
    ) {
        let _ = epoch;
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
    }

    pub(crate) fn text_timer(&mut self, fx: &mut ReaderEffects) {
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
        if self.text.caret.is_off() || !self.text_active() {
            self.say("not in a text field", fx);
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
        self.say("not available in this mode", fx);
        false
    }

    pub(crate) fn text_say_selection(&mut self, epoch: Epoch, fx: &mut ReaderEffects) {
        if !self.text_mode_allows(fx) {
            return;
        }
        let Some(t) = self.text.caret.tracked() else {
            self.say("not in a text field", fx);
            return;
        };
        let secret = t.kind.secret;
        let sel = caret::selected_text(t);
        let pieces = match sel {
            None => vec![caret::Piece {
                how: None,
                text: "no selection".to_owned(),
                spell: Spell::No,
            }],
            Some((text, n)) if text.is_empty() => vec![caret::Piece {
                how: None,
                text: format!("{n} characters selected"),
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
        if !self.text_mode_allows(fx) {
            return;
        }
        let Some(t) = self.text.caret.tracked() else {
            self.say("not in a text field", fx);
            return;
        };
        let from = t.caret.unwrap_or(0);
        let run = Run::new(t.node, epoch, from, t.kind.secret, self.now);
        self.text.say_all = SayAll::Running(run);
        self.say_all_pump(fx);
    }

    fn say_all_apply(&mut self, acts: smallvec::SmallVec<[Act; 4]>, fx: &mut ReaderEffects) {
        for a in acts {
            match a {
                Act::Fetch { around } => {
                    let SayAll::Running(run) = &mut self.text.say_all else {
                        continue;
                    };
                    self.text.next_req += 1;
                    let req = TextReqId(self.text.next_req);
                    run.fetch = Some(req);
                    fx.push(ReaderEffect::TextFetch {
                        req,
                        op: TextOp::Window {
                            node: run.node,
                            around: Some(around.saturating_add(say_all::FETCH_RADIUS)),
                            radius: say_all::FETCH_RADIUS,
                        },
                        class: BudgetClass::Prefetch,
                        epoch: run.epoch,
                    });
                }
                Act::Submit {
                    utt: id,
                    text,
                    first,
                    ..
                } => {
                    let SayAll::Running(run) = &self.text.say_all else {
                        continue;
                    };
                    let (epoch, secret) = (run.epoch, run.secret);
                    let o = self.verbalize_opts();
                    let spoken = self.text.verbalizer.unit(&text, How::Text, &o);
                    let mut u = message(&spoken, id, epoch);
                    u.prio = Prio::Text;
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
    }

    fn say_all_pump(&mut self, fx: &mut ReaderEffects) {
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
        self.say_all_apply(acts, fx);
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
            Ok(Reply::Unit(_) | Reply::Selections(_) | Reply::Done) | Err(_) => {
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
        if !self.cmd.effective.text.say_all_moves_caret {
            return;
        }
        let SayAll::Running(run) = &self.text.say_all else {
            return;
        };
        let (node, epoch) = (run.node, self.epoch.current());
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
        if !natural || !self.cmd.effective.text.say_all_moves_caret {
            return;
        }
        let target = run.end_caret();
        self.text.next_req += 1;
        let req = TextReqId(self.text.next_req);
        self.text.caret.note_own_move(target, self.now);
        fx.push(ReaderEffect::TextFetch {
            req,
            op: TextOp::SetCaret {
                node: run.node,
                offset: target,
            },
            class: BudgetClass::Prefetch,
            epoch: self.epoch.current(),
        });
    }

    /// After every message: a reading ends the moment something newer happens (SA-4).
    pub(crate) fn text_after(&mut self, fx: &mut ReaderEffects) {
        if self.text.say_all.stopped_by(self.epoch.current()) {
            self.say_all_finish(false, fx);
        }
    }
}

#[cfg(test)]
mod tests;
