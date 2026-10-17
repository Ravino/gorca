//! The terminal arm of the Reader (milestone M6, ADR-0001 T-2, ADR-0008 tier W3).
//!
//! The policy lives in the `ext-vte` extension (prompt detection, new output against repaint,
//! typed-character suppression, history reading, password prompts); this module is its host.
//! It owns what the extension must not: the clock, the screen reads (`TextOp::Screen`, run by
//! the runtime under a deadline), the speech (verbalised, stamped with an epoch) and the keys.
//! The extension is reached only through the [`TermSession`] seam: the extension host's slot
//! (the WASM component, `AppExt::terminal`) or the native build the daemon linked in
//! ([`crate::TermProvider`]). The core imports no extension crate.
//!
//! The core never asks what application a terminal is: a node with role `Terminal` is one
//! (WE-1) and the manifest of the extension decides whose terminals it serves. Keys go to the
//! extension unclassified ([`RawKey`]): which key is a readline motion is shell policy. Quirks
//! of one terminal widget live in connector data (`Toolkit::text_unit`, the `Screen` query that
//! avoids VTE's unreliable character count).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_ext_api::terminal::{
    Ms, RawKey, TermAction, TermConfig, TermCounters, TermEdit, TermInput, TermSnapshot, Unit,
    Urgency,
};
use gorca_input_api::{KeyEvent, KeyIntent, Modifiers, static_keycode};
use gorca_model::{Epoch, NodeKey, NodeSnapshot, Prio, Role, TextDelta, Utterance};
use gorca_rt::BudgetClass;
use gorca_speech::How;

use crate::SpeechIntent;
use crate::TermSession;
use crate::present::message;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};
use crate::text::caret::Reply;
use crate::text::{TextOp, TextReqId};

/// The shortest time between two on-demand re-trackings of one terminal.
const RETRACK_INTERVAL: Duration = Duration::from_millis(500);

/// The Reader's terminal state: at most one tracked terminal, the focused one.
#[derive(Debug, Default)]
pub struct TermState {
    tracked: Option<Tracked>,
    /// Counters of terminals that were tracked before (added to the live one's on read).
    retired: TermCounters,
    /// Screen reads started, answered and lost, for the e2e gates.
    pub reads: TermReads,
    /// Fail closed (VP): a secret prompt was seen, by the extension or by the core's own keyword
    /// check of the screen it read, and no Enter or cancel has ended it. While it holds, everything
    /// spoken for the terminal is marked sensitive, and a typed key the extension did not answer
    /// (fault, busy, disabled) is claimed and spoken by the core, marked sensitive.
    secret_hold: bool,
    /// Numbers the trackings: a screen generation is only comparable within one tracking.
    next_serial: u64,
    /// The last on-demand re-tracking on a key (see `Reader::focus_audit_on_key`).
    retrack_at: Option<(NodeKey, Instant)>,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TermReads {
    pub started: u64,
    pub answered: u64,
    pub failed: u64,
    /// Events the app actor could not queue for us (the screen is then read again).
    pub events_lost: u64,
    /// Review steps that waited for a fresh screen read (events lost, focus not protected, or the
    /// terminal not tracked).
    pub review_syncs: u64,
    /// Trackings restarted by a key press in the focused terminal that had lost its tracking.
    pub retracked_on_key: u64,
}

#[derive(Debug)]
struct Tracked {
    node: NodeKey,
    /// Which tracking this is (every `term_track` makes a new session whose generation counts
    /// from its own origin).
    serial: u64,
    sess: Box<dyn TermSession>,
    /// The screen read in flight.
    fetch: Option<TextReqId>,
    /// The epoch of the last key that reached the terminal: speech it causes carries it.
    key_epoch: Epoch,
}

fn add(a: TermCounters, b: TermCounters) -> TermCounters {
    TermCounters {
        fetches: a.fetches + b.fetches,
        fetch_failed: a.fetch_failed + b.fetch_failed,
        settled_local: a.settled_local + b.settled_local,
        resync_mismatch: a.resync_mismatch + b.resync_mismatch,
        output_spoken: a.output_spoken + b.output_spoken,
        output_lines_spoken: a.output_lines_spoken + b.output_lines_spoken,
        output_lines_skipped: a.output_lines_skipped + b.output_lines_skipped,
        repaints: a.repaints + b.repaints,
        typed_echoed: a.typed_echoed + b.typed_echoed,
        typed_explained: a.typed_explained + b.typed_explained,
        history_read: a.history_read + b.history_read,
        secret_prompts: a.secret_prompts + b.secret_prompts,
    }
}

/// The key as the extension sees it: keysym, typed scalar, modifiers (locks removed) and the
/// lowercase letter of the physical key, so the Control and Alt shortcuts work in every layout
/// (as `gorca_input_api::classify_text_key` does). No shell knowledge lives here.
pub fn raw_key(ev: &KeyEvent) -> RawKey {
    let m = ev.modifiers.without_locks();
    let phys = (b'a'..=b'z')
        .find(|c| {
            let name = [*c];
            std::str::from_utf8(&name)
                .ok()
                .and_then(static_keycode)
                .is_some_and(|code| code == ev.keycode)
        })
        .unwrap_or(0);
    RawKey {
        keysym: ev.keysym,
        unichar: ev.unichar,
        phys,
        shift: m.contains(Modifiers::SHIFT),
        ctrl: m.contains(Modifiers::CONTROL),
        alt: m.contains(Modifiers::MOD1),
        logo: m.intersects(Modifiers::MOD4),
    }
}

/// The terminal session of last resort: no policy, so it says nothing and claims nothing, but it
/// keeps reading the screen (on focus and, rate limited, after changes) so that the core's own
/// secret-prompt check (`TermState::secret_hold`) still sees a password prompt.
#[derive(Debug, Default)]
struct WatchOnly {
    armed: bool,
}

impl TermSession for WatchOnly {
    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        match input {
            TermInput::Focus => out.push(TermAction::FetchScreen),
            TermInput::Edit(_) | TermInput::Caret(_) if !self.armed => {
                self.armed = true;
                out.push(TermAction::ArmTimer(now + 150));
            }
            TermInput::Tick if self.armed => {
                self.armed = false;
                out.push(TermAction::FetchScreen);
            }
            TermInput::Key(_)
            | TermInput::Raw(_)
            | TermInput::Edit(_)
            | TermInput::Caret(_)
            | TermInput::Screen { .. }
            | TermInput::ScreenFailed
            | TermInput::Tick
            | TermInput::Blur => {}
        }
    }
    fn counters(&self) -> TermCounters {
        TermCounters::default()
    }
    fn snapshot(&self) -> Option<TermSnapshot> {
        None
    }
    fn degraded(&self) -> bool {
        true
    }
}

/// Words of a password prompt, matched as substrings of the lowercased line (the core's own
/// fail-closed table: broader than the extension's expression, it only ever over-marks).
const SECRET_SUBSTR: &[&str] = &[
    "pass",
    "secret",
    "token",
    "credential",
    "verification code",
    "contrase\u{f1}a",
    "senha",
    "\u{43f}\u{430}\u{440}\u{43e}\u{43b}",
    "\u{441}\u{435}\u{43a}\u{440}\u{435}\u{442}",
    "\u{43a}\u{43e}\u{434}\u{43e}\u{432}\u{430}\u{44f} \u{444}\u{440}\u{430}\u{437}\u{430}",
    "\u{43a}\u{43e}\u{434} \u{43f}\u{43e}\u{434}\u{442}\u{432}\u{435}\u{440}\u{436}\u{434}\u{435}\u{43d}\u{438}\u{44f}",
    "\u{43f}\u{438}\u{43d}-\u{43a}\u{43e}\u{434}",
    "\u{43f}\u{438}\u{43d}\u{43a}\u{43e}\u{434}",
];

/// Does `line` read like a prompt for a secret: a keyword and a trailing colon or question mark?
fn is_secret_prompt(line: &str) -> bool {
    let l = line.trim_end().to_lowercase();
    if !l.ends_with([':', '?', '\u{ff1a}', '\u{ff1f}']) {
        return false;
    }
    SECRET_SUBSTR.iter().any(|w| l.contains(w))
        || l.split(|c: char| !c.is_alphanumeric())
            .any(|w| w == "pin" || w == "otp")
}

/// Is the screen row under the caret a secret prompt?
fn caret_row_is_secret_prompt(screen: &str, caret: Option<u32>) -> bool {
    let rows = gorca_ext_api::terminal::Rows::new(screen);
    let (row, _) = rows.locate(caret.unwrap_or(0));
    is_secret_prompt(rows.line(row))
}

/// A one-line description of an input for the debug log: kinds and offsets, never text. A key is
/// never named: it may be a password character.
fn input_kind(i: &TermInput) -> String {
    match i {
        TermInput::Key(_) | TermInput::Raw(_) => "key".to_owned(),
        TermInput::Edit(TermEdit::Insert { offset, len, text }) => {
            format!("insert at {offset} len {len} payload {}", text.is_some())
        }
        TermInput::Edit(TermEdit::Delete { offset, len }) => {
            format!("delete at {offset} len {len}")
        }
        TermInput::Edit(TermEdit::Replaced) => "replaced".to_owned(),
        TermInput::Caret(o) => format!("caret {o}"),
        TermInput::Screen { text, caret } => {
            format!("screen {} chars caret {caret:?}", text.chars().count())
        }
        TermInput::ScreenFailed => "screen failed".to_owned(),
        TermInput::Tick => "tick".to_owned(),
        TermInput::Focus => "focus".to_owned(),
        TermInput::Blur => "blur".to_owned(),
    }
}

impl Reader {
    /// Is a terminal the tracked text object?
    pub(crate) fn term_tracked(&self) -> bool {
        self.text.term.tracked.is_some()
    }

    pub(crate) fn term_is(&self, node: NodeKey) -> bool {
        self.text
            .term
            .tracked
            .as_ref()
            .is_some_and(|t| t.node == node)
    }

    /// The counters of every terminal tracked so far.
    pub fn terminal_counters(&self) -> TermCounters {
        let live = self
            .text
            .term
            .tracked
            .as_ref()
            .map_or_else(TermCounters::default, |t| t.sess.counters());
        add(self.text.term.retired, live)
    }

    pub fn terminal_reads(&self) -> TermReads {
        self.text.term.reads
    }

    /// The extension's current idea of the focused terminal's screen, for review.
    pub(crate) fn term_snapshot(&self, node: NodeKey) -> Option<TermSnapshot> {
        let t = self.text.term.tracked.as_ref().filter(|t| t.node == node)?;
        let mut s = t.sess.snapshot()?;
        // Unique over the whole run: a model built from an earlier tracking of the same terminal
        // (Alt+Tab away and back) must never look current because the counts happen to agree.
        s.generation = (t.serial << 32) | (s.generation & 0xffff_ffff);
        Some(s)
    }

    /// The presented snapshot of `node` when it is the focused terminal but not tracked (its
    /// tracking was skipped or lost; review starts it on demand).
    pub(crate) fn term_untracked_focus(&self, node: NodeKey) -> Option<Arc<NodeSnapshot>> {
        let p = self.presented.as_ref()?;
        (p.node == node
            && p.snap.role == Role::Terminal
            && !self.term_is(node)
            && self.dir.text_window_follows(node.app))
        .then(|| Arc::clone(&p.snap))
    }

    /// Start tracking the presented terminal `snap` for review (its first screen read is under
    /// way). Review mode does not speak, so the extension's own speech is silent.
    pub(crate) fn term_adopt(&mut self, snap: &NodeSnapshot, fx: &mut ReaderEffects) {
        self.text.term.reads.review_syncs += 1;
        self.term_track(snap, fx);
    }

    /// May the held screen of `node` be missing output? True when events were lost, or when its
    /// application does not hold the protection (its text events are dropped, uncounted); the
    /// screen is then read again, and the caller waits for the answer.
    pub(crate) fn term_review_untrusted(&mut self, node: NodeKey, fx: &mut ReaderEffects) -> bool {
        if !self.term_is(node) {
            return false;
        }
        let lost = self.text_losses(fx);
        let unprotected = self
            .focused_app
            .as_ref()
            .is_some_and(|f| !f.protects(node.app));
        if unprotected && !lost {
            self.term_events_lost(fx);
        }
        if lost || unprotected {
            self.text.term.reads.review_syncs += 1;
        }
        lost || unprotected
    }

    fn term_ms(&self) -> Ms {
        u64::try_from(
            self.now
                .saturating_duration_since(self.text.origin)
                .as_millis(),
        )
        .unwrap_or(u64::MAX)
    }

    fn term_config(&self) -> TermConfig {
        let s = &self.cmd.effective.terminal;
        TermConfig {
            settle_ms: s.settle_ms,
            burst_lines: s.burst_lines,
            prompt_markers: s.prompt_markers.clone(),
            prompt_regex: s.prompt_regex.clone(),
            secret_regex: s.secret_regex.clone(),
            ..TermConfig::default()
        }
    }

    /// The focus presentation of a terminal finished: track it and read its screen.
    pub(crate) fn term_track(&mut self, snap: &NodeSnapshot, fx: &mut ReaderEffects) {
        self.term_untrack("retrack");
        let cfg = self.term_config();
        let sess = self.term_open_real(snap.key, &cfg).or_else(|| {
            // An application with extensions whose terminal policy is gone (disabled after
            // repeated faults) is still watched for secret prompts, by the core alone: fail closed.
            self.ext
                .as_ref()
                .and_then(|src| src.for_app(snap.key.app))
                .map(|_| Box::new(WatchOnly::default()) as Box<dyn TermSession>)
        });
        let Some(sess) = sess else {
            return;
        };
        self.term_start(snap.key, sess, fx);
    }

    /// The extension host's terminal policy for this application (the WASM component), else the
    /// native build the daemon installed; neither: none (a busy or disabled slot gives none).
    fn term_open_real(&self, node: NodeKey, cfg: &TermConfig) -> Option<Box<dyn TermSession>> {
        self.ext
            .as_ref()
            .and_then(|src| src.for_app(node.app))
            .and_then(|a| a.terminal(cfg))
            .or_else(|| self.term_provider.as_ref().map(|p| p.open(cfg)))
    }

    fn term_start(&mut self, node: NodeKey, sess: Box<dyn TermSession>, fx: &mut ReaderEffects) {
        self.text.term.next_serial += 1;
        tracing::debug!(target: "gorca_terminal", ?node, serial = self.text.term.next_serial, "terminal tracked");
        self.text.term.tracked = Some(Tracked {
            node,
            serial: self.text.term.next_serial,
            sess,
            fetch: None,
            key_epoch: self.epoch.current(),
        });
        self.term_step(TermInput::Focus, fx);
        // The core reads the screen itself when the policy did not ask (it is down): its own
        // secret-prompt check needs it.
        if self
            .text
            .term
            .tracked
            .as_ref()
            .is_some_and(|t| t.fetch.is_none())
        {
            self.term_fetch(fx);
        }
    }

    /// A key press found the focused terminal untracked: its tracking was lost without a focus
    /// change (the first focus came before its extension attached, the policy slot was busy or
    /// down at that moment, a node-removed event, a mode that was not `Focus` when it settled).
    /// A terminal that holds the focus is tracked again, from the presented snapshot, at most
    /// once per `RETRACK_INTERVAL`. Returns true when it started. Never a guess about focus:
    /// the focus state must still be on the presented terminal.
    pub(crate) fn term_retrack_on_key(&mut self, fx: &mut ReaderEffects) -> bool {
        if !self.text_active() {
            return false;
        }
        // Tracked with a real policy: nothing to repair. Tracked by the stand-in (the policy slot
        // was busy or down at the focus): upgraded below once a real session can be opened.
        let degraded = self
            .text
            .term
            .tracked
            .as_ref()
            .is_some_and(|t| t.sess.degraded());
        if self.term_tracked() && !degraded {
            return false;
        }
        let Some(p) = &self.presented else {
            return false;
        };
        let focused_here = matches!(
            self.focus,
            crate::focus::FocusState::Focused { node, .. } if node == p.node
        );
        if p.snap.role != Role::Terminal
            || !focused_here
            || self.selection_node == Some(p.node)
            || !self.dir.text_window_follows(p.node.app)
        {
            return false;
        }
        if let Some((n, at)) = self.text.term.retrack_at
            && n == p.node
            && self.now.saturating_duration_since(at) < RETRACK_INTERVAL
        {
            return false;
        }
        let (node, snap) = (p.node, Arc::clone(&p.snap));
        if degraded {
            // Only a tracking for the focused terminal itself, and only when the policy is back:
            // otherwise the stand-in stays (it keeps the secret watch) and nothing restarts.
            if !self.term_is(node) {
                return false;
            }
            let cfg = self.term_config();
            let Some(sess) = self.term_open_real(node, &cfg) else {
                return false;
            };
            self.text.term.retrack_at = Some((node, self.now));
            self.text.term.reads.retracked_on_key += 1;
            tracing::debug!(target: "gorca_terminal", ?node, "terminal tracked without its policy: upgrading");
            // A secret prompt seen by the stand-in still holds: the new session starts from a
            // fresh read and must not speak a password prompt aloud meanwhile.
            let hold = self.text.term.secret_hold;
            self.term_untrack("upgrade");
            self.text.term.secret_hold = hold;
            self.term_start(node, sess, fx);
            return true;
        }
        tracing::debug!(target: "gorca_terminal", ?node, "focused terminal untracked at a key: tracking it again");
        self.term_track(&snap, fx);
        // Only a tracking that started uses up the interval: an attempt that found no policy
        // (slot absent or busy) must not hold back the next key's, the policy may be back.
        let started = self.term_tracked();
        if started {
            self.text.term.retrack_at = Some((node, self.now));
        }
        self.text.term.reads.retracked_on_key += 1;
        started
    }

    /// Focus moved on, or the terminal is gone: wipe what the extension knows (secrets included).
    pub(crate) fn term_untrack(&mut self, why: &'static str) {
        self.text.term.secret_hold = false;
        if let Some(mut t) = self.text.term.tracked.take() {
            tracing::debug!(target: "gorca_terminal", node = ?t.node, serial = t.serial, why, "terminal untracked");
            let mut sink = Vec::new();
            t.sess.step(0, TermInput::Blur, &mut sink);
            self.text.term.retired = add(self.text.term.retired, t.sess.counters());
        }
    }

    /// The tracked terminal was removed, or its application is gone.
    pub(crate) fn term_node_gone(
        &mut self,
        node: Option<NodeKey>,
        app: Option<gorca_model::AppId>,
    ) {
        let hit = self
            .text
            .term
            .tracked
            .as_ref()
            .is_some_and(|t| node == Some(t.node) || app == Some(t.node.app));
        if hit {
            self.term_untrack("node or app gone");
        }
    }

    /// A key that reaches the terminal. True when the extension speaks it itself, so the
    /// generic key echo stays silent (no double echo, TI-3).
    pub(crate) fn term_key(&mut self, intent: &KeyIntent, fx: &mut ReaderEffects) -> bool {
        if !intent.event.pressed || !self.text_active() {
            return false;
        }
        if let Some(t) = &mut self.text.term.tracked {
            t.key_epoch = intent.epoch;
            // Not a publish: the key is no proof of focus. A terminal whose application has not
            // earned the protection reads its focus state again (the earlier read failed).
            let node = t.node;
            self.reconfirm_on_key(node, fx);
        }
        self.text.stats.keys_seen += 1;
        self.term_step(TermInput::Raw(raw_key(&intent.event)), fx)
    }

    pub(crate) fn term_caret(&mut self, offset: u32, fx: &mut ReaderEffects) {
        self.term_step(TermInput::Caret(offset), fx);
    }

    pub(crate) fn term_edit(&mut self, delta: TextDelta, fx: &mut ReaderEffects) {
        let edit = match delta {
            TextDelta::Insert { offset, len, text } => TermEdit::Insert {
                offset,
                len,
                text: crate::text::decode(text.as_ref()),
            },
            TextDelta::Delete { offset, len, .. } => TermEdit::Delete { offset, len },
            TextDelta::Replaced { .. } => TermEdit::Replaced,
        };
        self.term_step(TermInput::Edit(edit), fx);
    }

    /// Events were lost upstream: the extension must read the screen again.
    pub(crate) fn term_events_lost(&mut self, fx: &mut ReaderEffects) {
        if self.term_tracked() {
            self.text.term.reads.events_lost += 1;
            self.term_step(TermInput::Edit(TermEdit::Replaced), fx);
        }
    }

    /// The answer to a screen read. False when `req` is not ours.
    pub(crate) fn term_fetched(
        &mut self,
        req: TextReqId,
        result: Result<Reply, ()>,
        fx: &mut ReaderEffects,
    ) -> bool {
        let Some(t) = &mut self.text.term.tracked else {
            return false;
        };
        if t.fetch != Some(req) {
            return false;
        }
        t.fetch = None;
        let input = match result {
            Ok(Reply::Window(w)) => {
                self.text.term.reads.answered += 1;
                // The core's own check, whatever the extension makes of the same screen.
                if caret_row_is_secret_prompt(&w.text, w.caret) {
                    self.text.term.secret_hold = true;
                }
                TermInput::Screen {
                    text: w.text.to_string(),
                    caret: w.caret,
                }
            }
            Ok(Reply::Unit(_) | Reply::Selections(_) | Reply::Format(_) | Reply::Done)
            | Err(()) => {
                self.text.term.reads.failed += 1;
                TermInput::ScreenFailed
            }
        };
        self.term_step(input, fx);
        self.review_term_replied(fx);
        true
    }

    /// Is `req` the screen read of the tracked terminal?
    pub(crate) fn term_owns_fetch(&self, req: TextReqId) -> bool {
        self.text
            .term
            .tracked
            .as_ref()
            .is_some_and(|t| t.fetch == Some(req))
    }

    /// `text.say_*`, `text.spell_*` and `info.current_line` in a terminal: the unit at the
    /// caret of the screen the extension holds (no IPC).
    pub(crate) fn term_say_unit(
        &mut self,
        say: gorca_text::caret::Say,
        spell: crate::text::caret::Spell,
        epoch: Epoch,
        fx: &mut ReaderEffects,
    ) {
        use gorca_text::caret::Say;
        let node = self.text.term.tracked.as_ref().map(|t| t.node);
        let snap = node.and_then(|n| self.term_snapshot(n));
        let (Some(snap), true) = (snap, self.text_active()) else {
            self.say("not in a text field", fx);
            return;
        };
        let (unit, how) = match say {
            Say::Char => (Unit::Char, How::Char),
            Say::Word => (Unit::Word, How::Word),
            Say::Line | Say::Paragraph => (Unit::Line, How::Line),
        };
        let text =
            gorca_ext_api::terminal::unit_at(&snap.text, snap.caret, unit).unwrap_or_default();
        let piece = crate::text::caret::Piece {
            how: Some(how),
            text,
            spell,
        };
        let said = self.speak_pieces(&[piece]);
        let id = self.utt_id();
        let mut u = message(&said, id, epoch);
        u.prio = Prio::Interrupt;
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    pub(crate) fn term_timer(&mut self, fx: &mut ReaderEffects) {
        if self.term_tracked() {
            self.term_step(TermInput::Tick, fx);
        }
    }

    /// One input through the extension; its actions become effects. True when it claimed a key.
    fn term_step(&mut self, input: TermInput, fx: &mut ReaderEffects) -> bool {
        let now = self.term_ms();
        let is_typed = matches!(&input, TermInput::Raw(k) if k.types_a_character());
        let key = if let TermInput::Raw(k) = &input {
            Some(*k)
        } else {
            None
        };
        if tracing::enabled!(tracing::Level::DEBUG) {
            // What came in, without any text: kinds and offsets only.
            tracing::debug!(now, input = %input_kind(&input), "terminal input");
        }
        let mut acts: Vec<TermAction> = Vec::new();
        {
            let Some(t) = &mut self.text.term.tracked else {
                return false;
            };
            t.sess.step(now, input, &mut acts);
        }
        self.term_hold_secret(key.as_ref(), &mut acts);
        let mut claimed = false;
        let speak = self.text_active();
        // `echo.typed_characters` (and its toggle) governs the character a key types here as it
        // does everywhere else; the key is still claimed so the generic echo stays silent. The
        // extension's reply to a typed key is that one spoken character, nothing else.
        let silent_typed = is_typed && !self.cmd.effective.echo.typed_characters;
        if !acts.is_empty() {
            // `TermAction`'s Debug shows lengths, never text.
            tracing::debug!(actions = ?acts, speak, "terminal actions");
        }
        for a in acts {
            match a {
                TermAction::ClaimKey => claimed = true,
                TermAction::Say {
                    text,
                    unit,
                    urgency,
                    sensitive,
                } => {
                    if speak && !(silent_typed && unit == Unit::Char && urgency == Urgency::Key) {
                        let u = self.term_utterance(&text, unit, urgency, sensitive);
                        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                    }
                }
                TermAction::FetchScreen => self.term_fetch(fx),
                TermAction::ArmTimer(ms) => {
                    fx.push(ReaderEffect::ArmTimer(
                        self.text.origin + Duration::from_millis(ms),
                    ));
                }
            }
        }
        claimed
    }

    /// The fail-closed rule for secrets (see `TermState::secret_hold`), applied to what the
    /// extension answered: its own sensitive marking starts the hold, the hold marks every
    /// `Say`, a typed key it left unanswered is claimed and spoken here (marked sensitive), and
    /// Enter, Ctrl+C and Ctrl+D end the hold. Nothing here depends on the extension being alive.
    fn term_hold_secret(&mut self, key: Option<&RawKey>, acts: &mut Vec<TermAction>) {
        let down = !self
            .text
            .term
            .tracked
            .as_ref()
            .is_none_or(|t| t.sess.alive());
        let term = &mut self.text.term;
        if acts.iter().any(|a| {
            matches!(
                a,
                TermAction::Say {
                    sensitive: true,
                    ..
                }
            )
        }) {
            term.secret_hold = true;
        }
        // A policy that cannot answer cannot say whether this is a password: assume it is.
        if term.secret_hold || down {
            for a in acts.iter_mut() {
                if let TermAction::Say { sensitive, .. } = a {
                    *sensitive = true;
                }
            }
            if let Some(k) = key
                && k.types_a_character()
                && !acts
                    .iter()
                    .any(|a| matches!(a, TermAction::Say { .. } | TermAction::ClaimKey))
                && let Some(c) = char::from_u32(k.unichar)
            {
                acts.push(TermAction::ClaimKey);
                acts.push(TermAction::Say {
                    text: c.to_string(),
                    unit: Unit::Char,
                    urgency: Urgency::Key,
                    sensitive: true,
                });
            }
        }
        if let Some(k) = key {
            const RETURN: u32 = 0xff0d;
            const KP_ENTER: u32 = 0xff8d;
            let ends = matches!(k.keysym, RETURN | KP_ENTER)
                || (k.ctrl && !k.alt && matches!(k.phys, b'c' | b'd'));
            if ends {
                term.secret_hold = false;
            }
        }
    }

    fn term_fetch(&mut self, fx: &mut ReaderEffects) {
        let epoch = self.epoch.current();
        let Some(t) = &mut self.text.term.tracked else {
            return;
        };
        self.text.next_req += 1;
        let req = TextReqId(self.text.next_req);
        t.fetch = Some(req);
        self.text.term.reads.started += 1;
        fx.push(ReaderEffect::TextFetch {
            req,
            op: TextOp::Screen { node: t.node },
            class: BudgetClass::Interactive,
            epoch,
        });
    }

    /// Is a terminal secret prompt holding (everything spoken is sensitive)?
    pub(crate) fn term_secret_held(&self) -> bool {
        self.text.term.secret_hold
    }

    fn term_utterance(
        &mut self,
        text: &str,
        unit: Unit,
        urgency: Urgency,
        sensitive: bool,
    ) -> Utterance {
        let how = match unit {
            Unit::Char => How::Char,
            Unit::Word => How::Word,
            Unit::Line => How::Line,
            Unit::Text => How::Text,
        };
        let said = self.text.verbalizer.unit(text, how, &self.verbalize_opts());
        let id = self.utt_id();
        let (epoch, prio) = match urgency {
            Urgency::Key => (
                self.text
                    .term
                    .tracked
                    .as_ref()
                    .map_or_else(|| self.epoch.current(), |t| t.key_epoch),
                Prio::Interrupt,
            ),
            Urgency::Queued | Urgency::Output => (self.epoch.current(), Prio::Text),
        };
        let mut u = message(&said, id, epoch);
        u.prio = prio;
        if unit == Unit::Char {
            self.mark_capital(&mut u);
        }
        if sensitive {
            self.text.sensitive_utt = Some(id);
        }
        u
    }
}

#[cfg(test)]
pub(crate) mod tests;
