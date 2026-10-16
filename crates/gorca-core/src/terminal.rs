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

use std::time::Duration;

use gorca_ext_api::terminal::{
    Ms, RawKey, TermAction, TermConfig, TermCounters, TermEdit, TermInput, TermSnapshot, Unit,
    Urgency,
};
use gorca_input_api::{KeyEvent, KeyIntent, Modifiers, static_keycode};
use gorca_model::{Epoch, NodeKey, NodeSnapshot, Prio, TextDelta, Utterance};
use gorca_rt::BudgetClass;
use gorca_speech::How;

use crate::SpeechIntent;
use crate::TermSession;
use crate::present::message;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};
use crate::text::caret::Reply;
use crate::text::{TextOp, TextReqId};

/// The Reader's terminal state: at most one tracked terminal, the focused one.
#[derive(Debug, Default)]
pub struct TermState {
    tracked: Option<Tracked>,
    /// Counters of terminals that were tracked before (added to the live one's on read).
    retired: TermCounters,
    /// Screen reads started, answered and lost, for the e2e gates.
    pub reads: TermReads,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TermReads {
    pub started: u64,
    pub answered: u64,
    pub failed: u64,
    /// Events the app actor could not queue for us (the screen is then read again).
    pub events_lost: u64,
}

#[derive(Debug)]
struct Tracked {
    node: NodeKey,
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
        t.sess.snapshot()
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
        self.term_untrack();
        let cfg = self.term_config();
        // The extension host's terminal policy for this application (the WASM component), else
        // the native build the daemon installed; neither: this terminal is not tracked.
        let sess = self
            .ext
            .as_ref()
            .and_then(|src| src.for_app(snap.key.app))
            .and_then(|a| a.terminal(&cfg))
            .or_else(|| self.term_provider.as_ref().map(|p| p.open(&cfg)));
        let Some(sess) = sess else {
            return;
        };
        self.text.term.tracked = Some(Tracked {
            node: snap.key,
            sess,
            fetch: None,
            key_epoch: self.epoch.current(),
        });
        self.term_step(TermInput::Focus, fx);
    }

    /// Focus moved on, or the terminal is gone: wipe what the extension knows (secrets included).
    pub(crate) fn term_untrack(&mut self) {
        if let Some(mut t) = self.text.term.tracked.take() {
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
            self.term_untrack();
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
        let text = gorca_ext_api::terminal::unit_at(&snap.text, snap.caret, unit).unwrap_or_default();
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
        if sensitive {
            self.text.sensitive_utt = Some(id);
        }
        u
    }
}

#[cfg(test)]
mod tests;
