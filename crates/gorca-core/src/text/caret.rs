//! The caret state machine (ADR-0007 section 5): one tracked text object, the slot that binds a
//! movement key to the caret event it causes, typing echo, edit handling and selection.
//!
//! [`CaretState::step`] is pure: it takes the input and a [`Ctx`] (clock, settings, counters,
//! output) and returns the next state. Time is passed in, so tests drive it with a fake clock.
//! It never awaits and never touches IPC: what must be fetched leaves as [`Out::Fetch`] and the
//! reply returns as [`In::Fetched`].
//!
//! Hot paths (character and word echo, typing echo) are answered from the [`TextWindow`] with no
//! IPC (TI-1).

use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_input_api::{ClipOp, KeyUnit, TextDir, TextKey};
use gorca_model::{
    Epoch, NodeKey, Prio, TextGranularity, TextSelections, TextUnitAt, TextWindowFill,
};
use gorca_rt::BudgetClass;
use gorca_speech::How;
use gorca_text::caret::{self as tc, Presented, Say};
use gorca_text::coalesce::{Splice, SpliceKind, compose};
use gorca_text::echo::{
    EXPECT_TTL_MS, EXPECT_TTL_QUIRK_MS, ExpectRing, InsertAction, InsertSource, Reason,
    TypedWordBuffer, on_insert, word_before,
};
use gorca_text::sel;
use gorca_text::space::{Lookup, Need, TextSpace};
use smallvec::SmallVec;

use super::native::{NativeText, TextWindow};
use super::{TextCounters, TextOp, TextReqId};

/// Scalars fetched on each side of the caret for the window (TW-1).
pub const WINDOW_RADIUS: u32 = 2048;
/// A caret event this close behind an insert or delete at the same offset is its echo (CR-4).
const ABSORB: Duration = Duration::from_millis(50);
const ABSORB_MS: u64 = 50;
/// A caret event this long before its key (lanes are separate threads) is still correlated
/// (EV-4).
pub const HOLD: Duration = Duration::from_millis(5);
/// After tracking starts, the first caret event of a descendant of a rich editable is the
/// browser placing the caret at the focus (CE-1), not a move.
const PLACING: Duration = Duration::from_millis(600);
/// Caret moves made by gorca itself are remembered this long (say-all, review).
const OWN_MOVE: Duration = Duration::from_millis(300);
/// How long a presentation fetch may take before the machine gives up on it.
const FETCH_GRACE: Duration = Duration::from_millis(150);
/// Unexplained inserts (a log appending) are spoken at most this often (EV-5).
const INSERT_GAP: Duration = Duration::from_millis(250);
/// Text spoken for a selection longer than this is counted instead (SL-3).
const MAX_SPOKEN: u32 = sel::MAX_SPOKEN_SCALARS;

/// What kind of text object is tracked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Kind {
    pub editable: bool,
    /// A password field: spoken like any other, never logged (PW-1).
    pub secret: bool,
    pub multiline: bool,
}

/// Settings the machine reads.
#[derive(Copy, Clone, Debug)]
pub struct Cfg {
    pub typed_chars: bool,
    pub typed_words: bool,
    pub inserted_text: bool,
    pub edge: Duration,
    pub late: Duration,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            typed_chars: true,
            typed_words: false,
            inserted_text: true,
            edge: Duration::from_millis(100),
            late: Duration::from_millis(400),
        }
    }
}

/// One piece of a spoken text: running text to verbalise, or a fixed phrase.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Piece {
    pub how: Option<How>,
    pub text: String,
    pub spell: Spell,
}

/// Is a unit read as it stands, spelled out, or spelled with the phonetic alphabet
/// (`text.spell_*`, `info.current_line` pressed twice or three times)?
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Spell {
    No,
    Letters,
    Phonetic,
}

impl Piece {
    fn unit(how: How, text: String) -> Self {
        Self {
            how: Some(how),
            text,
            spell: Spell::No,
        }
    }

    fn phrase(text: &str) -> Self {
        Self {
            how: None,
            text: text.to_owned(),
            spell: Spell::No,
        }
    }
}

/// A text utterance, before verbalisation.
#[derive(Clone, Eq, PartialEq)]
pub struct Spoken {
    pub pieces: SmallVec<[Piece; 2]>,
    pub reason: Reason,
    pub epoch: Epoch,
    pub prio: Prio,
    /// Built from a password field: never logged (PW-2).
    pub sensitive: bool,
}

impl std::fmt::Debug for Spoken {
    // A secret utterance shows its length only (PW-2).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut d = f.debug_struct("Spoken");
        if self.sensitive {
            let chars: usize = self.pieces.iter().map(|p| p.text.chars().count()).sum();
            d.field("chars", &chars);
        } else {
            d.field("pieces", &self.pieces);
        }
        d.field("reason", &self.reason)
            .field("epoch", &self.epoch)
            .field("prio", &self.prio)
            .field("sensitive", &self.sensitive)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub enum Out {
    Say(Spoken),
    Fetch {
        req: TextReqId,
        op: TextOp,
        class: BudgetClass,
        epoch: Epoch,
    },
}

/// An edit of the tracked object. Its text may be a typed password: `Debug` shows lengths.
#[derive(Clone)]
pub enum Edit {
    Insert {
        offset: u32,
        len: u32,
        text: Option<String>,
    },
    Delete {
        offset: u32,
        len: u32,
        text: Option<String>,
    },
    /// Something replaced the text wholesale, or too many edits to follow.
    Replaced,
}

pub enum In {
    /// Focus landed on a text object.
    Track {
        node: NodeKey,
        kind: Kind,
    },
    /// Focus left it, or it is gone.
    Untrack,
    Key {
        key: TextKey,
        epoch: Epoch,
    },
    Caret {
        offset: u32,
    },
    Edit(Edit),
    Selection,
    /// A command asks for the unit at the caret (`text.say_char`, `info.current_line`, ...).
    Say {
        say: Say,
        spell: Spell,
        epoch: Epoch,
    },
    Fetched {
        req: TextReqId,
        result: Result<Reply, ()>,
    },
    Timer,
    /// Events for this object were lost upstream: the window cannot be trusted.
    Dirty,
    /// A caret, edit or selection event came from another node of the same rich editable
    /// (contenteditable puts them on the paragraph, CE-1): read that node from now on. `at` is
    /// the caret offset the event reported, when it was a caret event.
    Retarget {
        node: NodeKey,
        at: Option<u32>,
    },
}

/// A fetch reply, as the machine uses it.
#[derive(Clone, Debug)]
pub enum Reply {
    Window(TextWindowFill),
    Unit(TextUnitAt),
    Selections(TextSelections),
    /// The formatting of a run (`info.formatting`): answered to the Reader, not the machine.
    Format(gorca_model::TextFormat),
    Done,
}

/// Where the machine puts its output and what it reads.
pub struct Ctx<'a> {
    pub now: Instant,
    pub now_ms: u64,
    /// The Reader's current epoch.
    pub epoch_now: Epoch,
    pub cfg: Cfg,
    /// The app's insert-offset slack, from its connector's quirk table (0: none; see `insert`).
    pub insert_slack: u32,
    pub next_req: &'a mut u64,
    pub stats: &'a mut TextCounters,
    pub out: &'a mut Vec<Out>,
}

impl Ctx<'_> {
    fn req(&mut self) -> TextReqId {
        *self.next_req += 1;
        TextReqId(*self.next_req)
    }

    fn fetch(
        &mut self,
        t: &mut Tracked,
        op: TextOp,
        class: BudgetClass,
        epoch: Epoch,
    ) -> TextReqId {
        let req = self.req();
        self.stats.fetches += 1;
        if t.revs.len() >= 8 {
            t.revs.remove(0);
        }
        t.revs.push((req, t.win.rev()));
        self.out.push(Out::Fetch {
            req,
            op,
            class,
            epoch,
        });
        req
    }

    fn say(&mut self, s: Spoken) {
        self.out.push(Out::Say(s));
    }
}

/// A caret event waiting a moment for the key that caused it (EV-4).
#[derive(Copy, Clone, Debug)]
struct Held {
    off: u32,
    at: Instant,
}

impl std::fmt::Debug for Edit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = |t: &Option<String>| t.as_ref().map(|s| s.chars().count());
        match self {
            Edit::Insert { offset, len, text } => f
                .debug_struct("Insert")
                .field("offset", offset)
                .field("len", len)
                .field("chars", &n(text))
                .finish(),
            Edit::Delete { offset, len, text } => f
                .debug_struct("Delete")
                .field("offset", offset)
                .field("len", len)
                .field("chars", &n(text))
                .finish(),
            Edit::Replaced => f.write_str("Replaced"),
        }
    }
}

/// An insert nobody explains yet: a key a few milliseconds behind it may (the application's
/// signal can beat the keyboard monitor's, as a caret can, EV-4). Spoken as inserted text when
/// [`HOLD`] passes without a key claiming it.
#[derive(Clone)]
struct HeldIns {
    offset: u32,
    text: String,
    /// Scalars beyond `text`: a coalesced insert longer than what is spoken.
    more: usize,
    /// When the first of the coalesced inserts arrived.
    at: Instant,
    /// Not spoken before this: [`HOLD`] after `at`, or the end of the flood gap (EV-5).
    not_before: Instant,
    /// The epoch of the speech that spoke it (`flushed_ins` only): a later key makes it stale.
    spoken_epoch: Epoch,
}

impl HeldIns {
    /// The length of the net insert in scalars.
    fn len(&self) -> u32 {
        to_u32(self.text.chars().count().saturating_add(self.more))
    }
}

/// A delete nobody explains yet: an Erase key a few milliseconds behind it may. Dropped silently
/// when [`HOLD`] passes (a delete without a key is not spoken, TE-5).
#[derive(Clone)]
struct HeldDel {
    text: String,
    at: Instant,
}

// Lengths only: held text may be a password (PW-2).
impl std::fmt::Debug for HeldIns {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldIns")
            .field("offset", &self.offset)
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for HeldDel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldDel")
            .field("chars", &self.text.chars().count())
            .finish_non_exhaustive()
    }
}

/// How long a key may trail the insert it explains and still not be spoken twice.
const CLAIM_SPOKEN: Duration = Duration::from_millis(150);

/// Only short inserts wait: a paste or a commit of a phrase is not one key's character.
const HELD_INSERT_MAX: usize = 8;

/// The caret event expected from an insert or delete just seen.
#[derive(Copy, Clone, Debug)]
struct Absorb {
    at: u32,
    /// A second acceptable caret offset: where the caret really is when the application
    /// reported the insert at the wrong offset (see [`insert`]).
    alt: Option<u32>,
    until: Instant,
}

/// A movement key that already spoke its boundary: a caret event may still come (CR-3b).
#[derive(Copy, Clone, Debug)]
struct Late {
    key: TextKey,
    epoch: Epoch,
    until: Instant,
}

/// The tracked text object.
#[derive(Clone)]
pub struct Tracked {
    /// The text object the machine reads now. For a rich editable (a contenteditable region)
    /// this is the paragraph the caret is in, which can differ from `root` (CE-1).
    pub node: NodeKey,
    /// The focused object the tracking started on. Never changes.
    pub root: NodeKey,
    /// The root's own text holds embedded objects (U+FFFC): its caret lives in descendants.
    pub hosts: bool,
    /// When tracking started: a descendant's first caret event right after it is the focus
    /// placing the caret, not the user moving it.
    pub since: Option<Instant>,
    pub kind: Kind,
    pub caret: Option<u32>,
    /// The last fetched selection set with its texts.
    sel: Vec<(Range<u32>, Arc<str>)>,
    pub win: TextWindow,
    expect: ExpectRing,
    words: TypedWordBuffer,
    /// A word closed by the separator just typed, for when the window cannot give it (TE-4).
    word_fallback: Option<String>,
    absorb: Option<Absorb>,
    /// A selection event before this is the collapse a plain key caused: not announced (SL-3).
    sel_quiet: Option<Instant>,
    last_insert_say: Option<Instant>,
    own_moves: SmallVec<[(u32, Instant); 4]>,
    late: Option<Late>,
    held: Option<Held>,
    held_ins: Option<HeldIns>,
    /// An insert that was spoken as unexplained text, for a late key that explains it.
    flushed_ins: Option<HeldIns>,
    held_del: Option<HeldDel>,
    /// The window revision each outstanding request was issued under.
    revs: SmallVec<[(TextReqId, u64); 8]>,
}

impl std::fmt::Debug for Tracked {
    // Lengths only: a password field's text must not reach a log (PW-2).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracked")
            .field("node", &self.node)
            .field("kind", &self.kind)
            .field("caret", &self.caret)
            .field("sel_ranges", &self.sel.len())
            .field("window", &self.win)
            .field("expect", &self.expect.len())
            .finish_non_exhaustive()
    }
}

impl Tracked {
    pub fn new(node: NodeKey, kind: Kind) -> Self {
        Self {
            node,
            root: node,
            hosts: false,
            since: None,
            kind,
            caret: None,
            sel: Vec::new(),
            win: TextWindow::new(),
            expect: ExpectRing::new(),
            words: TypedWordBuffer::new(),
            word_fallback: None,
            absorb: None,
            sel_quiet: None,
            last_insert_say: None,
            own_moves: SmallVec::new(),
            late: None,
            held: None,
            held_ins: None,
            flushed_ins: None,
            held_del: None,
            revs: SmallVec::new(),
        }
    }

    /// Typed characters still waiting for their insert.
    #[cfg(test)]
    pub fn expect_len(&self) -> usize {
        self.expect.len()
    }

    /// Overwrite what a secret field held (PW-4).
    fn scrub(&mut self) {
        self.win.zero();
        self.win.invalidate();
        self.expect.clear();
        self.words.reset();
        self.sel.clear();
        // Held and spoken-but-remembered text: overwrite, then drop (PW-4).
        for h in [self.held_ins.take(), self.flushed_ins.take()]
            .into_iter()
            .flatten()
        {
            let mut t = h.text.into_bytes();
            t.fill(0);
            std::hint::black_box(&t);
        }
        if let Some(h) = self.held_del.take() {
            let mut t = h.text.into_bytes();
            t.fill(0);
            std::hint::black_box(&t);
        }
        if let Some(w) = self.word_fallback.take() {
            let mut t = w.into_bytes();
            t.fill(0);
            std::hint::black_box(&t);
        }
    }

    fn spoken(
        &self,
        pieces: SmallVec<[Piece; 2]>,
        reason: Reason,
        epoch: Epoch,
        prio: Prio,
    ) -> Spoken {
        Spoken {
            pieces,
            reason,
            epoch,
            prio,
            sensitive: self.kind.secret,
        }
    }

    fn one(&self, how: How, text: String, reason: Reason, epoch: Epoch, prio: Prio) -> Spoken {
        self.one_spelled(how, text, Spell::No, reason, epoch, prio)
    }

    fn one_spelled(
        &self,
        how: How,
        text: String,
        spell: Spell,
        reason: Reason,
        epoch: Epoch,
        prio: Prio,
    ) -> Spoken {
        let text = match text.char_indices().nth(MAX_UTTERANCE_SCALARS) {
            Some((cut, _)) => text[..cut].to_owned(),
            None => text,
        };
        let mut p = SmallVec::new();
        p.push(Piece {
            how: Some(how),
            text,
            spell,
        });
        self.spoken(p, reason, epoch, prio)
    }

    fn take_rev(&mut self, req: TextReqId) -> Option<u64> {
        let i = self.revs.iter().position(|(r, _)| *r == req)?;
        Some(self.revs.remove(i).1)
    }

    fn is_own_move(&mut self, off: u32, now: Instant) -> bool {
        self.own_moves
            .retain(|(_, at)| now.saturating_duration_since(*at) < OWN_MOVE);
        if let Some(i) = self.own_moves.iter().position(|(o, _)| *o == off) {
            self.own_moves.remove(i);
            true
        } else {
            false
        }
    }

    fn absorbs(&mut self, off: u32, now: Instant) -> bool {
        match self.absorb {
            Some(a) if (a.at == off || a.alt == Some(off)) && now <= a.until => {
                self.absorb = None;
                true
            }
            Some(a) if now > a.until => {
                self.absorb = None;
                false
            }
            Some(_) | None => false,
        }
    }

    fn sel_text(r: &Range<u32>, set: &[(Range<u32>, Arc<str>)]) -> Option<String> {
        let (outer, text) = set
            .iter()
            .find(|(o, _)| o.start <= r.start && r.end <= o.end)?;
        if text.is_empty() {
            return None;
        }
        Some(
            text.chars()
                .skip((r.start - outer.start) as usize)
                .take((r.end - r.start) as usize)
                .collect(),
        )
    }
}

/// What the machine is waiting for after a presentation needed data.
#[derive(Clone, Copy, Debug)]
pub enum Want {
    /// Speak `say` at `at` from the window once it is refilled; `refilled` is set after the
    /// first refill so a unit larger than the window falls back to the toolkit's answer.
    Unit {
        say: Say,
        at: u32,
        reason: Reason,
        refilled: bool,
        spell: Spell,
    },
    /// Speak the text of the toolkit's answer to `GetStringAtOffset`.
    Reply {
        say: Say,
        reason: Reason,
        spell: Spell,
    },
    /// Speak the selection change; `fallback` is the caret unit to speak instead if the
    /// selection cannot be read (SL fallback).
    Selection {
        fallback: Option<(Say, u32)>,
        all: bool,
    },
}

/// The caret state of the focused object (ADR-0007 section 5).
#[derive(Debug, Default)]
pub enum CaretState {
    /// Focus is not a tracked text object.
    #[default]
    Off,
    /// Tracking a text object; nothing pending.
    Idle { t: Box<Tracked> },
    /// A text key was pressed on this object; waiting for its effect (CR-1).
    Awaiting {
        t: Box<Tracked>,
        key: TextKey,
        epoch: Epoch,
        edge_at: Instant,
        late_until: Instant,
    },
    /// A presentation needs data that is not in the window; one fetch in flight.
    Fetching {
        t: Box<Tracked>,
        req: TextReqId,
        want: Want,
        epoch: Epoch,
        deadline: Instant,
    },
}

/// Return and Tab act on a text view; in an entry they activate or move the focus.
fn is_line_key(ch: char) -> bool {
    matches!(ch, '\n' | '\t')
}

/// Text spoken at once is cut here: a line of a minified file must not become one utterance.
const MAX_UTTERANCE_SCALARS: usize = 4000;

fn say_of(unit: KeyUnit) -> Say {
    tc::say_for_key(match unit {
        KeyUnit::Char => tc::KeyUnit::Char,
        KeyUnit::Word => tc::KeyUnit::Word,
        KeyUnit::Line => tc::KeyUnit::Line,
        KeyUnit::Paragraph => tc::KeyUnit::Paragraph,
        KeyUnit::LineEdge => tc::KeyUnit::LineEdge,
        KeyUnit::DocEdge => tc::KeyUnit::DocEdge,
        KeyUnit::Page => tc::KeyUnit::Page,
    })
}

fn how_of(say: Say) -> How {
    match say {
        Say::Char => How::Char,
        Say::Word => How::Word,
        Say::Line | Say::Paragraph => How::Line,
    }
}

fn granularity(say: Say) -> TextGranularity {
    match say {
        Say::Char => TextGranularity::Char,
        Say::Word => TextGranularity::Word,
        Say::Line => TextGranularity::Line,
        Say::Paragraph => TextGranularity::Paragraph,
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The text of `say` at `at`, from the window, or what must be fetched first.
fn unit_text(t: &Tracked, say: Say, at: u32) -> Lookup<(How, String)> {
    let space = NativeText::new(&t.win, !t.kind.multiline);
    match tc::present(&space, at, say) {
        Lookup::Need(n) => Lookup::Need(n),
        Lookup::Ready(Presented::Blank) => Lookup::Ready((How::Char, String::new())),
        Lookup::Ready(Presented::Text(r)) => {
            let mut s = String::new();
            match space.read(r, &mut s) {
                Lookup::Ready(()) => Lookup::Ready((how_of(say), s)),
                Lookup::Need(n) => Lookup::Need(n),
            }
        }
    }
}

impl CaretState {
    pub fn tracked(&self) -> Option<&Tracked> {
        match self {
            CaretState::Off => None,
            CaretState::Idle { t }
            | CaretState::Awaiting { t, .. }
            | CaretState::Fetching { t, .. } => Some(t),
        }
    }

    pub fn tracked_mut(&mut self) -> Option<&mut Tracked> {
        match self {
            CaretState::Off => None,
            CaretState::Idle { t }
            | CaretState::Awaiting { t, .. }
            | CaretState::Fetching { t, .. } => Some(t),
        }
    }

    fn into_tracked(self) -> Option<Box<Tracked>> {
        match self {
            CaretState::Off => None,
            CaretState::Idle { t }
            | CaretState::Awaiting { t, .. }
            | CaretState::Fetching { t, .. } => Some(t),
        }
    }

    pub fn is_off(&self) -> bool {
        matches!(self, CaretState::Off)
    }

    /// Does this machine speak `key` itself, so the generic key echo must stay silent? A typed
    /// character in an editable object is echoed from the key (TE-1); BackSpace and Delete are
    /// spoken from the deletion (TE-5), not by name.
    pub fn owns_key(&self, key: TextKey) -> bool {
        let Some(t) = self.tracked() else {
            return false;
        };
        match key {
            TextKey::Typed { ch } => t.kind.editable && (t.kind.multiline || !is_line_key(ch)),
            TextKey::Erase { .. } => t.kind.editable,
            TextKey::Move { .. } | TextKey::SelectAll | TextKey::Clipboard(_) => false,
        }
    }

    /// The next time the machine wants to run without an input.
    pub fn deadline(&self) -> Option<Instant> {
        let own = match self {
            CaretState::Off => None,
            CaretState::Idle { t } => t.held.map(|h| h.at + HOLD),
            CaretState::Awaiting { t, edge_at, .. } => {
                Some(t.held.map_or(*edge_at, |h| (*edge_at).min(h.at + HOLD)))
            }
            CaretState::Fetching { deadline, .. } => Some(*deadline),
        };
        let ins = self.tracked().and_then(|t| {
            let a = t.held_ins.as_ref().map(|h| h.not_before);
            let b = t.held_del.as_ref().map(|h| h.at + HOLD);
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        });
        match (own, ins) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// gorca moved the caret itself (say-all, review): its caret event is not the user's.
    pub fn note_own_move(&mut self, offset: u32, now: Instant) {
        if let Some(t) = self.tracked_mut() {
            t.own_moves.push((offset, now));
            if t.own_moves.len() > 4 {
                t.own_moves.remove(0);
            }
        }
    }

    pub fn step(self, input: In, cx: &mut Ctx<'_>) -> CaretState {
        let state = self.expire(cx.now);
        match input {
            In::Track { node, kind } => state.track(node, kind, cx),
            In::Untrack => {
                if let Some(mut t) = state.into_tracked() {
                    t.scrub();
                }
                CaretState::Off
            }
            In::Key { key, epoch } => state.on_key(key, epoch, cx),
            In::Caret { offset } => state.on_caret(offset, cx),
            In::Edit(e) => state.on_edit(e, cx),
            In::Selection => state.on_selection(cx),
            In::Say { say, spell, epoch } => state.on_say(say, spell, epoch, cx),
            In::Fetched { req, result } => state.on_fetched(req, result, cx),
            In::Timer => state.on_timer(cx),
            In::Dirty => {
                let mut state = state;
                if let Some(t) = state.tracked_mut() {
                    t.win.invalidate();
                    cx.stats.window_dirty += 1;
                }
                state
            }
            In::Retarget { node, at } => state.retarget(node, at, cx),
        }
    }

    /// CE-1: the text object under the caret changed within one focused rich editable. What is
    /// bound to offsets of the old node (window, caret, selection, held edits) goes; what is
    /// bound to the user (the key slot, typed keys waiting for their insert) stays.
    fn retarget(mut self, node: NodeKey, at: Option<u32>, cx: &mut Ctx<'_>) -> CaretState {
        // A presentation in flight was for the old node.
        self = match self {
            CaretState::Fetching { t, .. } => CaretState::Idle { t },
            other @ (CaretState::Off | CaretState::Idle { .. } | CaretState::Awaiting { .. }) => {
                other
            }
        };
        let idle = matches!(self, CaretState::Idle { .. });
        let epoch = cx.epoch_now;
        let now = cx.now;
        let Some(t) = self.tracked_mut() else {
            return self;
        };
        if t.node == node {
            return self;
        }
        t.node = node;
        t.caret = None;
        t.win.invalidate();
        t.sel.clear();
        t.absorb = None;
        t.own_moves.clear();
        t.held = None;
        t.words.reset();
        t.word_fallback = None;
        for h in [t.held_ins.take(), t.flushed_ins.take()]
            .into_iter()
            .flatten()
        {
            let mut b = h.text.into_bytes();
            b.fill(0);
            std::hint::black_box(&b);
        }
        t.held_del = None;
        // Right after focus the caret event is the browser placing the caret, not the user
        // moving it: remember where it is and say nothing.
        let placing = idle
            && at.is_some()
            && t.since
                .is_some_and(|a| now.saturating_duration_since(a) < PLACING);
        if placing {
            t.caret = at;
        }
        if idle {
            // With a key waiting, the caret event that follows asks for exactly what it needs.
            cx.fetch(
                t,
                TextOp::Window {
                    node,
                    around: None,
                    radius: WINDOW_RADIUS,
                },
                BudgetClass::Prefetch,
                epoch,
            );
        }
        self
    }

    /// A slot that outlived its latest deadline is gone.
    fn expire(self, now: Instant) -> CaretState {
        match self {
            CaretState::Awaiting { t, late_until, .. } if now > late_until => {
                CaretState::Idle { t }
            }
            other @ (CaretState::Off
            | CaretState::Idle { .. }
            | CaretState::Awaiting { .. }
            | CaretState::Fetching { .. }) => other,
        }
    }

    fn track(self, node: NodeKey, kind: Kind, cx: &mut Ctx<'_>) -> CaretState {
        let mut t = match self.into_tracked() {
            Some(mut t) if t.node == node => {
                t.kind = kind;
                t
            }
            Some(mut old) => {
                old.scrub();
                Box::new(Tracked::new(node, kind))
            }
            None => Box::new(Tracked::new(node, kind)),
        };
        t.since = Some(cx.now);
        let epoch = cx.epoch_now;
        cx.fetch(
            &mut t,
            TextOp::Window {
                node,
                around: None,
                radius: WINDOW_RADIUS,
            },
            BudgetClass::Prefetch,
            epoch,
        );
        CaretState::Idle { t }
    }

    // ---- keys ----------------------------------------------------------------------------

    fn on_key(self, key: TextKey, epoch: Epoch, cx: &mut Ctx<'_>) -> CaretState {
        let Some(mut t) = self.into_tracked() else {
            return CaretState::Off;
        };
        match key {
            TextKey::Typed { ch } => {
                t.late = None;
                t.held = None;
                if t.kind.multiline || !is_line_key(ch) {
                    // The insert may already be here, ahead of its key.
                    let claimed = claim_held_insert(&mut t, ch, epoch, cx);
                    if t.kind.editable {
                        echo_typed(&mut t, ch, epoch, claimed, cx);
                    } else if claimed == Claim::No {
                        // Not ours to echo (the generic key echo speaks it), but an insert that
                        // follows is still the typed key's, not text from elsewhere (TE-2).
                        t.expect.set_ttl(expect_ttl(cx));
                        t.expect.push(ch, cx.now_ms);
                    }
                }
                CaretState::Idle { t }
            }
            TextKey::Move { .. }
            | TextKey::Erase { .. }
            | TextKey::SelectAll
            | TextKey::Clipboard(_) => {
                t.words.reset();
                t.word_fallback = None;
                t.late = None;
                // A key that is not a typed character does not explain the insert before it.
                flush_held_insert(&mut t, cx);
                if !matches!(key, TextKey::Move { select: true, .. }) {
                    // A collapse of the selection by this key is not announced (SL-3).
                    t.sel.clear();
                    t.sel_quiet = Some(cx.now + cx.cfg.late);
                }
                // The deletion may have beaten its key by a few ms: speak it now (TE-5).
                if matches!(key, TextKey::Erase { .. } | TextKey::Clipboard(ClipOp::Cut))
                    && let Some(h) = t.held_del.take()
                {
                    cx.stats.delete_held_claimed += 1;
                    speak_deleted(&mut t, h.text, epoch, cx);
                    return CaretState::Idle { t };
                }
                let edge_at = cx.now + cx.cfg.edge;
                let late_until = cx.now + cx.cfg.late.max(cx.cfg.edge);
                // The caret event may have beaten the key by a few ms (EV-4).
                let held = t.held.take();
                let state = CaretState::Awaiting {
                    t,
                    key,
                    epoch,
                    edge_at,
                    late_until,
                };
                match held {
                    Some(h) if matches!(key, TextKey::Move { .. }) => {
                        cx.stats.caret_held_correlated += 1;
                        state.on_caret(h.off, cx)
                    }
                    Some(_) | None => state,
                }
            }
        }
    }

    // ---- caret ---------------------------------------------------------------------------

    fn on_caret(self, off: u32, cx: &mut Ctx<'_>) -> CaretState {
        match self {
            CaretState::Off => CaretState::Off,
            // A selection fetch in flight keeps running: the selection event beat the caret
            // event (order differs between toolkits); the caret only moves the position.
            CaretState::Fetching {
                mut t,
                req,
                want: want @ Want::Selection { .. },
                epoch,
                deadline,
            } => {
                t.caret = Some(off);
                CaretState::Fetching {
                    t,
                    req,
                    want,
                    epoch,
                    deadline,
                }
            }
            CaretState::Fetching { t, .. } => CaretState::Idle { t }.on_caret(off, cx),
            CaretState::Idle { mut t } => {
                if t.is_own_move(off, cx.now) || t.absorbs(off, cx.now) {
                    t.caret = Some(off);
                    cx.stats.caret_absorbed += 1;
                    return CaretState::Idle { t };
                }
                // A caret event ahead of its insert (typing): the typed keys explain it.
                // Only while the newest typed key is fresh: a key whose insert never came (a
                // preedit, a refused character) must not swallow a later, unrelated move.
                if let Some(old) = t.caret
                    && t.expect
                        .newest_age_ms(cx.now_ms)
                        .is_some_and(|age| age <= ABSORB_MS)
                    && off > old
                    && (off - old) as usize <= t.expect.len()
                {
                    t.caret = Some(off);
                    cx.stats.caret_absorbed += 1;
                    return CaretState::Idle { t };
                }
                if t.caret == Some(off) {
                    return CaretState::Idle { t };
                }
                if let Some(l) = t.late
                    && cx.now <= l.until
                    && let TextKey::Move {
                        unit,
                        select: false,
                        ..
                    } = l.key
                {
                    t.late = None;
                    cx.stats.caret_late += 1;
                    let reason = Reason::CaretMove {
                        say: say_of(unit),
                        correlated: true,
                    };
                    return present(t, say_of(unit), off, reason, l.epoch, cx);
                }
                t.held = Some(Held { off, at: cx.now });
                CaretState::Idle { t }
            }
            CaretState::Awaiting {
                mut t,
                key,
                epoch,
                edge_at,
                late_until,
            } => match key {
                TextKey::Move {
                    unit,
                    dir,
                    select: false,
                } => {
                    // A caret that went the other way is not this key's (the previous key hit
                    // a boundary and this event is the next key's, which beat it): hold it for
                    // the key that may follow, like an unexplained caret in Idle.
                    let wrong_way = t.caret.is_some_and(|c| match dir {
                        TextDir::Next => off < c,
                        TextDir::Prev => off > c,
                    });
                    if wrong_way {
                        t.held = Some(Held { off, at: cx.now });
                        return CaretState::Awaiting {
                            t,
                            key,
                            epoch,
                            edge_at,
                            late_until,
                        };
                    }
                    cx.stats.caret_correlated += 1;
                    let say = say_of(unit);
                    let reason = Reason::CaretMove {
                        say,
                        correlated: true,
                    };
                    present(t, say, off, reason, epoch, cx)
                }
                TextKey::Move {
                    unit, select: true, ..
                } => {
                    cx.stats.caret_correlated += 1;
                    t.caret = Some(off);
                    let fallback = Some((say_of(unit), off));
                    select_fetch(
                        t,
                        Want::Selection {
                            fallback,
                            all: false,
                        },
                        epoch,
                        cx,
                    )
                }
                TextKey::SelectAll | TextKey::Erase { .. } | TextKey::Clipboard(_) => {
                    // The caret follows the selection or the edit; the edit speaks.
                    t.caret = Some(off);
                    CaretState::Awaiting {
                        t,
                        key,
                        epoch,
                        edge_at,
                        late_until,
                    }
                }
                TextKey::Typed { .. } => CaretState::Idle { t },
            },
        }
    }

    // ---- edits ---------------------------------------------------------------------------

    fn on_edit(self, e: Edit, cx: &mut Ctx<'_>) -> CaretState {
        let mut pending = None;
        let (mut t, slot) = match self {
            CaretState::Off => return CaretState::Off,
            CaretState::Awaiting {
                t,
                key,
                epoch,
                edge_at,
                late_until,
            } => (t, Some((key, epoch, edge_at, late_until))),
            CaretState::Idle { t } => (t, None),
            CaretState::Fetching {
                t,
                req,
                want,
                epoch,
                deadline,
            } => {
                // An edit does not cancel a presentation in flight; its reply still speaks.
                pending = Some((req, want, epoch, deadline));
                (t, None)
            }
        };
        let mut consumed = false;
        match e {
            Edit::Replaced => {
                t.win.invalidate();
                cx.stats.window_dirty += 1;
            }
            Edit::Insert { offset, len, text } => {
                insert(&mut t, offset, len, text, slot, cx);
                // A paste has spoken; the slot is spent.
                consumed = matches!(slot, Some((TextKey::Clipboard(ClipOp::Paste), ..)));
            }
            Edit::Delete { offset, len, text } => {
                consumed = delete(&mut t, offset, len, text, slot, cx);
            }
        }
        match slot {
            Some((key, epoch, edge_at, late_until)) if !consumed => CaretState::Awaiting {
                t,
                key,
                epoch,
                edge_at,
                late_until,
            },
            Some(_) | None => match pending {
                Some((req, want, epoch, deadline)) => CaretState::Fetching {
                    t,
                    req,
                    want,
                    epoch,
                    deadline,
                },
                None => CaretState::Idle { t },
            },
        }
    }

    fn on_say(self, say: Say, spell: Spell, epoch: Epoch, cx: &mut Ctx<'_>) -> CaretState {
        let Some(t) = self.into_tracked() else {
            return CaretState::Off;
        };
        let Some(at) = t.caret else {
            return CaretState::Idle { t };
        };
        let reason = Reason::CaretMove {
            say,
            correlated: true,
        };
        present_inner(t, say, at, reason, spell, epoch, false, cx)
    }

    // ---- selection -----------------------------------------------------------------------

    fn on_selection(self, cx: &mut Ctx<'_>) -> CaretState {
        match self {
            CaretState::Off => CaretState::Off,
            // The caret event of a Shift move already asked; this event brings nothing new.
            CaretState::Fetching {
                t,
                req,
                want: want @ Want::Selection { .. },
                epoch,
                deadline,
            } => CaretState::Fetching {
                t,
                req,
                want,
                epoch,
                deadline,
            },
            CaretState::Fetching { t, .. } => CaretState::Idle { t }.on_selection(cx),
            CaretState::Idle { t } => {
                if t.sel_quiet.is_some_and(|until| cx.now <= until) {
                    return CaretState::Idle { t };
                }
                // Uncorrelated (mouse, script): fetch and speak the change.
                let epoch = cx.epoch_now;
                select_fetch(
                    t,
                    Want::Selection {
                        fallback: None,
                        all: false,
                    },
                    epoch,
                    cx,
                )
            }
            CaretState::Awaiting {
                t,
                key,
                epoch,
                edge_at,
                late_until,
            } => match key {
                TextKey::Move { select: true, .. } => select_fetch(
                    t,
                    Want::Selection {
                        fallback: None,
                        all: false,
                    },
                    epoch,
                    cx,
                ),
                TextKey::SelectAll => select_fetch(
                    t,
                    Want::Selection {
                        fallback: None,
                        all: true,
                    },
                    epoch,
                    cx,
                ),
                // A collapse caused by this key is not announced (SL-3).
                TextKey::Move { select: false, .. }
                | TextKey::Erase { .. }
                | TextKey::Clipboard(_)
                | TextKey::Typed { .. } => CaretState::Awaiting {
                    t,
                    key,
                    epoch,
                    edge_at,
                    late_until,
                },
            },
        }
    }

    // ---- fetch replies ---------------------------------------------------------------------

    fn on_fetched(self, req: TextReqId, result: Result<Reply, ()>, cx: &mut Ctx<'_>) -> CaretState {
        match self {
            CaretState::Off => CaretState::Off,
            CaretState::Fetching {
                mut t,
                req: r,
                want,
                epoch,
                deadline,
            } if r == req => {
                let fresh = t.take_rev(req) == Some(t.win.rev());
                match (result, want) {
                    (
                        Ok(Reply::Window(fill)),
                        Want::Unit {
                            say,
                            at,
                            reason,
                            refilled,
                            spell,
                        },
                    ) => {
                        if fresh {
                            t.win.fill(&fill);
                            note_hosts(&mut t, &fill.text);
                            if t.caret.is_none() {
                                t.caret = fill.caret;
                            }
                        }
                        if refilled {
                            // The unit does not fit in a window: the toolkit's answer decides.
                            return reply_fetch(t, say, at, reason, spell, epoch, cx);
                        }
                        present_inner(t, say, at, reason, spell, epoch, true, cx)
                    }
                    (Ok(Reply::Unit(u)), Want::Reply { say, reason, spell }) => {
                        if fresh && say == Say::Line {
                            t.win.add_line(u.range.clone(), Arc::clone(&u.text));
                        }
                        let s = t.one_spelled(
                            how_of(say),
                            u.text.to_string(),
                            spell,
                            reason,
                            epoch,
                            Prio::Interrupt,
                        );
                        cx.say(s);
                        CaretState::Idle { t }
                    }
                    (Ok(Reply::Selections(s)), Want::Selection { all, .. }) => {
                        speak_selection(&mut t, s, all, epoch, cx);
                        CaretState::Idle { t }
                    }
                    (
                        Ok(
                            Reply::Done
                            | Reply::Window(_)
                            | Reply::Unit(_)
                            | Reply::Selections(_)
                            | Reply::Format(_),
                        ),
                        _,
                    ) => {
                        let _ = deadline;
                        CaretState::Idle { t }
                    }
                    (Err(()), want) => {
                        cx.stats.fetch_failed += 1;
                        fail(t, want, epoch, cx)
                    }
                }
            }
            CaretState::Fetching {
                mut t,
                req: r,
                want,
                epoch,
                deadline,
            } => {
                stray(&mut t, req, result);
                CaretState::Fetching {
                    t,
                    req: r,
                    want,
                    epoch,
                    deadline,
                }
            }
            CaretState::Idle { mut t } => {
                stray(&mut t, req, result);
                CaretState::Idle { t }
            }
            CaretState::Awaiting {
                mut t,
                key,
                epoch,
                edge_at,
                late_until,
            } => {
                stray(&mut t, req, result);
                CaretState::Awaiting {
                    t,
                    key,
                    epoch,
                    edge_at,
                    late_until,
                }
            }
        }
    }

    fn on_timer(mut self, cx: &mut Ctx<'_>) -> CaretState {
        if let Some(t) = self.tracked_mut() {
            flush_due(t, cx);
        }
        // A caret held inside an open slot that no key claimed is an unexplained move.
        self = match self {
            CaretState::Awaiting { t, .. } if t.held.is_some_and(|h| cx.now >= h.at + HOLD) => {
                CaretState::Idle { t }
            }
            other @ (CaretState::Off
            | CaretState::Idle { .. }
            | CaretState::Awaiting { .. }
            | CaretState::Fetching { .. }) => other,
        };
        match self {
            CaretState::Off => CaretState::Off,
            CaretState::Idle { mut t } => {
                if let Some(h) = t.held
                    && cx.now >= h.at + HOLD
                {
                    t.held = None;
                    cx.stats.caret_uncorrelated += 1;
                    // CR-4: one scalar on the same line is a character, anything else a line.
                    let same = t.caret.is_some_and(|c| t.win.same_line(c, h.off));
                    let old = t.caret.unwrap_or(h.off);
                    let dist = t
                        .win
                        .visible_between(old, h.off)
                        .unwrap_or_else(|| old.abs_diff(h.off));
                    let say = tc::say_for_uncorrelated_by(dist, same);
                    let reason = Reason::CaretMove {
                        say,
                        correlated: false,
                    };
                    let epoch = cx.epoch_now;
                    return present(t, say, h.off, reason, epoch, cx);
                }
                CaretState::Idle { t }
            }
            CaretState::Awaiting {
                mut t,
                key,
                epoch,
                edge_at,
                late_until,
            } => {
                if cx.now < edge_at {
                    return CaretState::Awaiting {
                        t,
                        key,
                        epoch,
                        edge_at,
                        late_until,
                    };
                }
                // CR-3: no caret event arrived: the key did not move the caret.
                cx.stats.caret_edge += 1;
                match key {
                    TextKey::Move {
                        unit,
                        select: false,
                        ..
                    } => {
                        t.late = Some(Late {
                            key,
                            epoch,
                            until: late_until,
                        });
                        match t.caret {
                            Some(at) => {
                                let say = say_of(unit);
                                present(t, say, at, Reason::Boundary, epoch, cx)
                            }
                            None => CaretState::Idle { t },
                        }
                    }
                    TextKey::SelectAll => select_fetch(
                        t,
                        Want::Selection {
                            fallback: None,
                            all: true,
                        },
                        epoch,
                        cx,
                    ),
                    TextKey::Move { select: true, .. }
                    | TextKey::Erase { .. }
                    | TextKey::Clipboard(_)
                    | TextKey::Typed { .. } => {
                        if now_before(cx.now, late_until) {
                            // The edit may still be on its way (slow app): keep the slot.
                            CaretState::Awaiting {
                                t,
                                key,
                                epoch,
                                edge_at: late_until,
                                late_until,
                            }
                        } else {
                            CaretState::Idle { t }
                        }
                    }
                }
            }
            CaretState::Fetching {
                t,
                req,
                want,
                epoch,
                deadline,
            } => {
                if cx.now < deadline {
                    return CaretState::Fetching {
                        t,
                        req,
                        want,
                        epoch,
                        deadline,
                    };
                }
                cx.stats.fetch_timeout += 1;
                fail(t, want, epoch, cx)
            }
        }
    }
}

fn now_before(now: Instant, deadline: Instant) -> bool {
    now < deadline
}

// ---- presentation ------------------------------------------------------------------------

/// Speak `say` at `at` from the window, or fetch what is missing and speak on the reply.
fn present(
    t: Box<Tracked>,
    say: Say,
    at: u32,
    reason: Reason,
    epoch: Epoch,
    cx: &mut Ctx<'_>,
) -> CaretState {
    present_inner(t, say, at, reason, Spell::No, epoch, false, cx)
}

#[allow(clippy::too_many_arguments)] // one presentation request; a struct would only rename them
fn present_inner(
    mut t: Box<Tracked>,
    say: Say,
    at: u32,
    reason: Reason,
    spell: Spell,
    epoch: Epoch,
    refilled: bool,
    cx: &mut Ctx<'_>,
) -> CaretState {
    t.caret = Some(at);
    match unit_text(&t, say, at) {
        Lookup::Ready((how, text)) => {
            cx.stats.window_hits += 1;
            let s = t.one_spelled(how, text, spell, reason, epoch, Prio::Interrupt);
            cx.say(s);
            CaretState::Idle { t }
        }
        Lookup::Need(Need::Line { at: line_at }) => {
            let node = t.node;
            let req = cx.fetch(
                &mut t,
                TextOp::AtOffset {
                    node,
                    offset: line_at,
                    unit: TextGranularity::Line,
                },
                BudgetClass::Interactive,
                epoch,
            );
            fetching(t, req, Want::Reply { say, reason, spell }, epoch, cx)
        }
        Lookup::Need(Need::Window { around }) => {
            if refilled {
                return reply_fetch(t, say, at, reason, spell, epoch, cx);
            }
            window_fetch(
                t,
                around,
                Want::Unit {
                    say,
                    at,
                    reason,
                    refilled: false,
                    spell,
                },
                epoch,
                cx,
            )
        }
        Lookup::Need(Need::Length) => window_fetch(
            t,
            at,
            Want::Unit {
                say,
                at,
                reason,
                refilled: false,
                spell,
            },
            epoch,
            cx,
        ),
    }
}

fn window_fetch(
    mut t: Box<Tracked>,
    around: u32,
    want: Want,
    epoch: Epoch,
    cx: &mut Ctx<'_>,
) -> CaretState {
    let node = t.node;
    let req = cx.fetch(
        &mut t,
        TextOp::Window {
            node,
            around: Some(around),
            radius: WINDOW_RADIUS,
        },
        BudgetClass::Interactive,
        epoch,
    );
    fetching(t, req, want, epoch, cx)
}

/// Ask the toolkit for the unit itself (a unit too large for the window).
fn reply_fetch(
    mut t: Box<Tracked>,
    say: Say,
    at: u32,
    reason: Reason,
    spell: Spell,
    epoch: Epoch,
    cx: &mut Ctx<'_>,
) -> CaretState {
    let node = t.node;
    let req = cx.fetch(
        &mut t,
        TextOp::AtOffset {
            node,
            offset: at,
            unit: granularity(say),
        },
        BudgetClass::Interactive,
        epoch,
    );
    fetching(t, req, Want::Reply { say, reason, spell }, epoch, cx)
}

fn select_fetch(mut t: Box<Tracked>, want: Want, epoch: Epoch, cx: &mut Ctx<'_>) -> CaretState {
    let node = t.node;
    let req = cx.fetch(
        &mut t,
        TextOp::Selections { node },
        BudgetClass::Interactive,
        epoch,
    );
    fetching(t, req, want, epoch, cx)
}

fn fetching(t: Box<Tracked>, req: TextReqId, want: Want, epoch: Epoch, cx: &Ctx<'_>) -> CaretState {
    CaretState::Fetching {
        t,
        req,
        want,
        epoch,
        deadline: cx.now + BudgetClass::Interactive.default_timeout() + FETCH_GRACE,
    }
}

/// A fetch failed or timed out: say what can still be said (ADR-0007 5.1).
fn fail(mut t: Box<Tracked>, want: Want, epoch: Epoch, cx: &mut Ctx<'_>) -> CaretState {
    match want {
        Want::Unit {
            say, at, reason, ..
        } => line_fallback(&mut t, say, at, reason, epoch, cx),
        Want::Reply { say, reason, .. } => {
            let at = t.caret.unwrap_or(0);
            line_fallback(&mut t, say, at, reason, epoch, cx);
        }
        Want::Selection { fallback, .. } => {
            cx.stats.sel_fetch_failed += 1;
            if let Some((say, at)) = fallback {
                let reason = Reason::CaretMove {
                    say,
                    correlated: true,
                };
                return present_inner(t, say, at, reason, Spell::No, epoch, true, cx);
            }
        }
    }
    t.held = None;
    CaretState::Idle { t }
}

/// Characters and words say nothing when the fetch failed; a line says what the window knows.
fn line_fallback(
    t: &mut Tracked,
    say: Say,
    at: u32,
    reason: Reason,
    epoch: Epoch,
    cx: &mut Ctx<'_>,
) {
    if matches!(say, Say::Line | Say::Paragraph)
        && let Some(line) = window_paragraph(t, at)
    {
        let s = t.one(How::Line, line, reason, epoch, Prio::Interrupt);
        cx.say(s);
    }
}

/// The hard line around `at` out of the window, when it holds all of it.
fn window_paragraph(t: &Tracked, at: u32) -> Option<String> {
    let w = &t.win;
    let rel = at.checked_sub(w.base())? as usize;
    let chars = w.chars();
    if rel > chars.len() {
        return None;
    }
    let start = chars[..rel]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |p| p + 1);
    let end = chars[rel..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |p| rel + p + 1);
    let whole_start = start > 0 || w.base() == 0;
    let whole_end = end < chars.len() || w.end() == w.len().unwrap_or(u32::MAX);
    (whole_start && whole_end).then(|| chars[start..end].iter().collect())
}

/// A reply the machine no longer waits for: a window fill or a line is still worth keeping, if
/// the window did not change since the request.
fn stray(t: &mut Tracked, req: TextReqId, result: Result<Reply, ()>) {
    let fresh = t.take_rev(req) == Some(t.win.rev());
    if !fresh {
        return;
    }
    match result {
        Ok(Reply::Window(fill)) => {
            t.win.fill(&fill);
            note_hosts(t, &fill.text);
            if t.caret.is_none() {
                t.caret = fill.caret;
            }
        }
        Ok(Reply::Unit(_) | Reply::Selections(_) | Reply::Format(_) | Reply::Done) | Err(()) => {}
    }
}

/// Text that is only embedded-object marks (U+FFFC): structure, not words.
fn is_structure(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '\u{FFFC}')
}

/// A rich editable's own text is made of embedded objects (U+FFFC), one per block child; the
/// caret lives in those children (CE-1).
fn note_hosts(t: &mut Tracked, text: &str) {
    if t.node == t.root && text.contains('\u{FFFC}') {
        t.hosts = true;
    }
}

// ---- typing ------------------------------------------------------------------------------

/// TE-1: the typed character is spoken from the key, before any text event.
fn echo_typed(t: &mut Tracked, ch: char, epoch: Epoch, claimed: Claim, cx: &mut Ctx<'_>) {
    if claimed == Claim::No {
        t.expect.set_ttl(expect_ttl(cx));
        t.expect.push(ch, cx.now_ms);
    }
    if cx.cfg.typed_words {
        let mut word = String::new();
        t.word_fallback = t.words.push(ch, &mut word).then_some(word);
    }
    if !cx.cfg.typed_chars {
        return;
    }
    if claimed == Claim::AlreadySpoken {
        // The insert was spoken as unexplained text a moment ago: once is enough (TI-3).
        return;
    }
    let mut pieces: SmallVec<[Piece; 2]> = SmallVec::new();
    if ch == '\n' {
        pieces.push(Piece::phrase("new line"));
    } else {
        pieces.push(Piece::unit(How::Char, ch.to_string()));
    }
    let s = t.spoken(pieces, Reason::Typed, epoch, Prio::Interrupt);
    cx.say(s);
    cx.stats.typed_echoed += 1;
}

/// An insert in the tracked object (TE-2, TE-3, TE-4).
/// Expectation lifetime: the long one only for an engine whose connector declares the
/// `gecko-insert-offset` quirk row (slack > 0): Firefox on a loaded host stalls for >1 s and
/// then delivers the typed keys' inserts coalesced. Every other toolkit keeps 1 s.
fn expect_ttl(cx: &Ctx<'_>) -> u64 {
    if cx.insert_slack > 0 {
        EXPECT_TTL_QUIRK_MS
    } else {
        EXPECT_TTL_MS
    }
}

fn insert(
    t: &mut Tracked,
    offset: u32,
    len: u32,
    text: Option<String>,
    slot: Option<(TextKey, Epoch, Instant, Instant)>,
    cx: &mut Ctx<'_>,
) {
    let caret_before = t.caret;
    t.absorb = Some(Absorb {
        at: offset.saturating_add(len),
        alt: None,
        until: cx.now + ABSORB,
    });
    let paste = matches!(slot, Some((TextKey::Clipboard(ClipOp::Paste), ..)));
    let source = if paste {
        InsertSource::Paste
    } else {
        InsertSource::Unknown
    };
    let Some(s) = text else {
        // The application sent no text: the typed keys explain as many scalars as they can.
        t.win.invalidate();
        cx.stats.window_dirty += 1;
        let _ = t.expect.forget(len as usize, cx.now_ms);
        return;
    };
    t.win.splice(offset, 0, &s);
    note_hosts(t, &s);
    if is_structure(&s) {
        // A block was added to a rich editable: the window knows, there is nothing to say.
        return;
    }
    // The skip over stale keys (a refused key before a typed one) needs positional evidence:
    // the insert lands at the caret, within the app's slack.
    let at_caret = caret_before.is_some_and(|c| offset.abs_diff(c) <= cx.insert_slack);
    t.expect.set_ttl(expect_ttl(cx));
    let action = on_insert(
        &mut t.expect,
        &s,
        cx.now_ms,
        source,
        cx.cfg.inserted_text,
        at_caret,
    );
    if !matches!(action, Some(InsertAction::Echoed { .. })) {
        // Text that is none of the typed keys' means they will not come: forget them.
        t.expect.clear();
    }
    // Text goes in at the caret (typing, an input method's commit). When the application
    // reports another offset (the connector's quirk row declares an engine that does: Gecko
    // now and then reports one or two too many for scripted or committed text, and for a
    // typed key's own insert it was seen four off, after astral characters), the window can no
    // longer be trusted: drop it (it is refilled when a unit needs it) and accept the caret
    // event where the caret really went, so the insert's own caret move is not spoken a second
    // time. A typed key's insert, explained by the key itself, moves the caret by exactly its
    // length whatever offset the application names; any other insert only within the slack.
    //
    // Accepted limit (R-3): because an echoed insert counts as misplaced at any offset, a typed
    // key's far-offset insert (a refused key plus a scripted append) can swallow a non-key
    // mouse caret move to caret+1 arriving within the 50 ms absorb window. Firefox only.
    let echoed = matches!(action, Some(InsertAction::Echoed { .. }));
    let misplaced = cx.insert_slack > 0
        && !paste
        && caret_before.is_some_and(|c| {
            offset != c && (echoed || (offset > c && offset - c <= cx.insert_slack))
        });
    if misplaced {
        tracing::debug!(target: "gorca_text", quirk = "gecko-insert-offset", offset, caret = ?caret_before, echoed, "insert offset quirk applied");
        if let (Some(c), Some(a)) = (caret_before, t.absorb.as_mut()) {
            a.alt = Some(c.saturating_add(len));
        }
        t.win.invalidate();
        cx.stats.window_dirty += 1;
    }
    match action {
        Some(InsertAction::Echoed { .. }) => {
            if cx.cfg.typed_words {
                speak_word(t, offset, &s, cx);
            }
        }
        Some(InsertAction::Speak { text, more, source }) => {
            if !paste && slot.is_none() {
                let n = to_u32(text.chars().count().saturating_add(more));
                // Edits that touch fold into one net insert (gorca-text), spoken once.
                if coalesce_held_insert(t, offset, n, cx) {
                    return;
                }
                let flooding = t
                    .last_insert_say
                    .is_some_and(|at| cx.now.saturating_duration_since(at) < INSERT_GAP);
                if flooding && let Some(h) = t.held_ins.as_mut() {
                    // Not touching, but the gap is on: the held one stays and counts the rest.
                    h.more = h.more.saturating_add(n as usize);
                    cx.stats.inserts_coalesced += 1;
                    return;
                }
                if n as usize <= HELD_INSERT_MAX || flooding {
                    // A short insert waits a moment for its key (see [`HeldIns`]); one that
                    // comes while an earlier insert was just spoken waits for the gap (EV-5).
                    flush_held_insert(t, cx);
                    let hold = cx.now + HOLD;
                    let not_before = t
                        .last_insert_say
                        .map_or(hold, |at| (at + INSERT_GAP).max(hold));
                    t.held_ins = Some(HeldIns {
                        offset,
                        text: text.to_owned(),
                        more,
                        at: cx.now,
                        not_before,
                        spoken_epoch: cx.epoch_now,
                    });
                    return;
                }
            }
            let epoch_prio = match slot {
                Some((_, e, ..)) if paste => (e, Prio::Interrupt),
                Some(_) | None => (cx.epoch_now, Prio::Text),
            };
            speak_inserted(t, text, more, source, paste, epoch_prio, cx);
        }
        None => {}
    }
}

/// Speak text that arrived without a key (TE-3). EV-5: a flood of unexplained inserts is
/// spoken once in a while, not every time.
fn speak_inserted(
    t: &mut Tracked,
    text: &str,
    more: usize,
    source: InsertSource,
    paste: bool,
    (epoch, prio): (Epoch, Prio),
    cx: &mut Ctx<'_>,
) -> bool {
    if !paste
        && t.last_insert_say
            .is_some_and(|at| cx.now.saturating_duration_since(at) < INSERT_GAP)
    {
        cx.stats.text_flood_coalesced += 1;
        return false;
    }
    let mut pieces: SmallVec<[Piece; 2]> = SmallVec::new();
    pieces.push(Piece::unit(How::Text, text.to_owned()));
    if more > 0 {
        pieces.push(Piece::phrase(&format!("and {more} more")));
    }
    t.last_insert_say = Some(cx.now);
    let sp = t.spoken(pieces, Reason::Inserted(source), epoch, prio);
    cx.say(sp);
    cx.stats.inserted_spoken += 1;
    true
}

/// The held insert is not explained by any key: speak it as inserted text, and remember it for
/// a key that turns up late.
fn flush_held_insert(t: &mut Tracked, cx: &mut Ctx<'_>) {
    if let Some(h) = t.held_ins.take() {
        let ep = (cx.epoch_now, Prio::Text);
        if speak_inserted(t, &h.text, h.more, InsertSource::Unknown, false, ep, cx) {
            t.flushed_ins = Some(HeldIns {
                at: cx.now,
                spoken_epoch: ep.0,
                ..h
            });
        }
    }
}

/// A new unexplained insert of `n` scalars at `offset` that touches the held one: the two are
/// one net insert (`gorca_text::coalesce::compose`), read back from the window, which already
/// has both. Returns whether it folded.
fn coalesce_held_insert(t: &mut Tracked, offset: u32, n: u32, cx: &mut Ctx<'_>) -> bool {
    let Some(h) = t.held_ins.as_ref() else {
        return false;
    };
    let Some(net) = compose(Splice::insert(h.offset, h.len()), Splice::insert(offset, n)) else {
        return false;
    };
    if net.kind() != SpliceKind::Insert {
        return false;
    }
    let Some(full) = t.win.read(net.written()) else {
        return false;
    };
    let total = full.chars().count();
    let (text, more) = gorca_text::echo::truncate_inserted(&full);
    let (text, more) = (text.to_owned(), more);
    debug_assert_eq!(total, text.chars().count() + more);
    if let Some(h) = t.held_ins.as_mut() {
        h.offset = net.at;
        h.text = text;
        h.more = more;
    }
    cx.stats.inserts_coalesced += 1;
    true
}

/// Held inserts and deletes that have waited long enough: the insert is spoken, the delete
/// dropped.
fn flush_due(t: &mut Tracked, cx: &mut Ctx<'_>) {
    if t.held_ins.as_ref().is_some_and(|h| cx.now >= h.not_before) {
        flush_held_insert(t, cx);
    }
    if t.held_del.as_ref().is_some_and(|h| cx.now >= h.at + HOLD) {
        t.held_del = None;
    }
}

/// What a typed key found of its own insert.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Claim {
    /// Nothing yet: the insert will follow.
    No,
    /// The insert was waiting (held): the key's echo is the insert's too.
    Held,
    /// The insert was spoken as unexplained text a moment ago: the key stays silent.
    AlreadySpoken,
    /// The insert was spoken as unexplained text, but before this key: the key's epoch made that
    /// speech stale (E-2), so it is cancelled or dropped. The key echoes itself; its insert
    /// has come, so nothing more is expected.
    Superseded,
}

/// A typed key whose character is the (start of the) held insert claims it; one whose
/// character was just spoken as an unexplained insert finds it already spoken.
fn claim_held_insert(t: &mut Tracked, ch: char, key_epoch: Epoch, cx: &mut Ctx<'_>) -> Claim {
    let (claim, offset) = if let Some(h) = t.held_ins.as_mut()
        && h.text.starts_with(ch)
    {
        let offset = h.offset;
        h.text = h.text.chars().skip(1).collect();
        h.offset += 1;
        if h.text.is_empty() {
            t.held_ins = None;
        }
        (Claim::Held, offset)
    } else if let Some(f) = t.flushed_ins.as_mut()
        && cx.now.saturating_duration_since(f.at) <= CLAIM_SPOKEN
        && f.text.starts_with(ch)
    {
        let offset = f.offset;
        // The insert beat its key (the buses are not ordered). Its speech belongs to an older
        // epoch than the key's, so the key silences or drops it: the key must echo itself.
        let claim = if f.spoken_epoch >= key_epoch {
            Claim::AlreadySpoken
        } else {
            Claim::Superseded
        };
        f.text = f.text.chars().skip(1).collect();
        f.offset += 1;
        if f.text.is_empty() {
            t.flushed_ins = None;
        }
        (claim, offset)
    } else {
        return Claim::No;
    };
    cx.stats.insert_held_claimed += 1;
    if cx.cfg.typed_words {
        let mut one = String::new();
        one.push(ch);
        speak_word(t, offset, &one, cx);
    }
    claim
}

/// TE-4: the word before the separator just typed.
fn speak_word(t: &mut Tracked, offset: u32, ins: &str, cx: &mut Ctx<'_>) {
    let Some(last) = ins.chars().last() else {
        return;
    };
    if !gorca_text::echo::is_separator(last) {
        return;
    }
    let sep = offset + to_u32(ins.chars().count()).saturating_sub(1);
    let from_window = sep
        .checked_sub(t.win.base())
        .and_then(|rel| word_before(t.win.chars(), rel as usize))
        .map(|r| t.win.chars()[r].iter().collect::<String>());
    let Some(word) = from_window.or_else(|| t.word_fallback.take()) else {
        return;
    };
    let s = t.one(How::Word, word, Reason::TypedWord, cx.epoch_now, Prio::Text);
    cx.say(s);
}

/// A delete in the tracked object. Returns whether it spent the key slot.
fn delete(
    t: &mut Tracked,
    offset: u32,
    len: u32,
    text: Option<String>,
    slot: Option<(TextKey, Epoch, Instant, Instant)>,
    cx: &mut Ctx<'_>,
) -> bool {
    let deleted = text.or_else(|| t.win.read(offset..offset.saturating_add(len)));
    t.win.splice(offset, len, "");
    // BackSpace moves the caret back by what it erased, Delete leaves it: when the application
    // reports the delete at another offset (see [`insert`]), the caret event is still its echo.
    let alt = match (t.caret, slot) {
        (Some(c), Some((TextKey::Erase { dir, .. }, ..))) => {
            let after = if dir == TextDir::Prev {
                c.saturating_sub(len)
            } else {
                c
            };
            if after != offset {
                t.win.invalidate();
                cx.stats.window_dirty += 1;
            }
            (cx.insert_slack > 0).then_some(after)
        }
        _ => None,
    };
    t.absorb = Some(Absorb {
        at: offset,
        alt,
        until: cx.now + ABSORB,
    });
    if deleted.as_deref().is_some_and(is_structure) {
        // A block left a rich editable: nothing was typed away.
        return false;
    }
    t.words.reset();
    t.word_fallback = None;
    let erases = matches!(
        slot,
        Some((TextKey::Erase { .. } | TextKey::Clipboard(ClipOp::Cut), ..))
    );
    let Some((_, epoch, ..)) = slot.filter(|_| erases) else {
        if slot.is_none()
            && let Some(d) = deleted.filter(|d| !d.is_empty())
        {
            // Nobody explains it yet: an Erase key may be a few ms behind.
            t.held_del = Some(HeldDel {
                text: d,
                at: cx.now,
            });
        }
        return false;
    };
    if let Some(d) = deleted.filter(|d| !d.is_empty()) {
        speak_deleted(t, d, epoch, cx);
    }
    true
}

/// TE-5: the deleted text, spoken with the key's epoch.
fn speak_deleted(t: &mut Tracked, text: String, epoch: Epoch, cx: &mut Ctx<'_>) {
    let how = if text.chars().count() == 1 {
        How::Char
    } else {
        How::Text
    };
    let s = t.one(how, text, Reason::Erased, epoch, Prio::Interrupt);
    cx.say(s);
    cx.stats.erased_spoken += 1;
}

// ---- selection ---------------------------------------------------------------------------

/// Characters in `r`: the window's count when it holds them (a phantom U+FEFF is not a
/// character), else the offsets' own difference.
fn visible_count(win: &TextWindow, r: &Range<u32>) -> u32 {
    win.visible_between(r.start, r.end)
        .unwrap_or_else(|| r.end.saturating_sub(r.start))
}

fn count_phrase(n: u32) -> String {
    if n == 1 {
        "1 character".to_owned()
    } else {
        format!("{n} characters")
    }
}

/// SL-3: speak what became selected and what was unselected.
fn speak_selection(
    t: &mut Tracked,
    now: TextSelections,
    all_requested: bool,
    epoch: Epoch,
    cx: &mut Ctx<'_>,
) {
    let old = std::mem::take(&mut t.sel);
    let old_ranges: SmallVec<[Range<u32>; 4]> = old.iter().map(|(r, _)| r.clone()).collect();
    let new_ranges: SmallVec<[Range<u32>; 4]> = now.ranges.iter().map(|(r, _)| r.clone()).collect();
    let delta = sel::diff(&old_ranges, &new_ranges);
    t.sel = now.ranges;
    if delta.is_empty() {
        return;
    }
    let mut pieces: SmallVec<[Piece; 2]> = SmallVec::new();
    if !delta.added.is_empty() {
        let whole = t.win.len().is_some_and(|l| {
            l > 0 && sel::normalize(&new_ranges).as_slice() == std::slice::from_ref(&(0..l))
        });
        if whole && (all_requested || delta.added_len() == t.win.len().unwrap_or(0)) {
            let n = t.win.len().unwrap_or(0);
            let n = visible_count(&t.win, &(0..n));
            pieces.push(Piece::phrase(&format!("all selected, {}", count_phrase(n))));
        } else {
            push_changed(&t.win, &delta.added, &t.sel, "selected", &mut pieces);
        }
    }
    if !delta.removed.is_empty() {
        if !pieces.is_empty() {
            pieces.push(Piece::phrase(","));
        }
        push_changed(&t.win, &delta.removed, &old, "unselected", &mut pieces);
    }
    let s = t.spoken(pieces, Reason::Selection, epoch, Prio::Interrupt);
    cx.say(s);
    cx.stats.selection_spoken += 1;
}

fn push_changed(
    win: &TextWindow,
    ranges: &[Range<u32>],
    set: &[(Range<u32>, Arc<str>)],
    word: &str,
    pieces: &mut SmallVec<[Piece; 2]>,
) {
    let total = ranges
        .iter()
        .fold(0u32, |a, r| a.saturating_add(visible_count(win, r)));
    // The fetched text of a long selection is left out; a small change to it is still in the
    // window.
    let texts: Option<Vec<String>> = ranges
        .iter()
        .map(|r| {
            Tracked::sel_text(r, set).or_else(|| {
                (r.end.saturating_sub(r.start) <= MAX_SPOKEN)
                    .then(|| win.read(r.clone()))
                    .flatten()
            })
        })
        .collect();
    match texts {
        Some(texts) if total <= MAX_SPOKEN => {
            pieces.push(Piece::unit(How::Text, texts.join(" ")));
            pieces.push(Piece::phrase(word));
        }
        Some(_) | None => pieces.push(Piece::phrase(&format!("{} {word}", count_phrase(total)))),
    }
}

/// `info.selection` (SL-4): what is selected now, from the last fetched set.
pub fn selected_text(t: &Tracked) -> Option<(String, u32)> {
    let total = t
        .sel
        .iter()
        .fold(0u32, |a, (r, _)| a.saturating_add(visible_count(&t.win, r)));
    if total == 0 {
        return None;
    }
    let text: String = t
        .sel
        .iter()
        .filter(|(_, s)| !s.is_empty())
        .map(|(_, s)| s.as_ref())
        .collect::<Vec<_>>()
        .join(" ");
    Some((text, total))
}
