//! The `Key` arm: classification result to effect, plus key echo.

use std::sync::atomic::Ordering;
use std::time::Instant;

use gorca_input_api::{KeyClass, KeyIntent};

use crate::SpeechIntent;
use crate::mode::Mode;
use crate::present::{key_echo, key_echo_is_action, key_echo_text};
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

impl Reader {
    pub(crate) fn on_key(&mut self, intent: KeyIntent, now: Instant, fx: &mut ReaderEffects) {
        // E-4: adopt the epoch carried by the key press so the utterance answering it is never
        // cancelled by that same key press.
        self.epoch.observe(intent.epoch);
        if let Some(l) = &self.cmd.links {
            // The input lane watches this counter while learn mode holds the keyboard (LM-3).
            l.reader_ack.fetch_add(1, Ordering::AcqRel);
        }
        self.observe_locks(&intent, fx);
        match intent.class {
            KeyClass::Command(cmd) | KeyClass::Watched(cmd) => {
                self.run_command(cmd, &intent, now, fx);
            }
            KeyClass::Described(cmd) => self.describe(&intent, cmd, fx),
            KeyClass::Ambiguous { committed } => {
                self.ambiguous_keys += 1;
                self.pass_key_seen(&intent, fx);
                // A chord that matches a command but whose fate Mutter's rules leave unknown is
                // never executed (it might also have reached the application). Tell the user to
                // press it again, so silence is not mistaken for a broken screen reader. Only
                // for a real command chord, never for a modifier or an unbound key.
                if committed.is_some() && intent.kind == gorca_input_api::PressKind::Press {
                    self.cmd.ambiguous_cues += 1;
                    self.say("again", fx);
                }
            }
            KeyClass::ModifierPassThrough => {
                // LM-5: the double tap still reaches the application in learn mode.
                if matches!(self.mode, Mode::InputHelp { .. }) {
                    self.say("gorca key passed to application", fx);
                }
            }
            KeyClass::PassToApp => {
                self.pass_key_seen(&intent, fx);
                self.focus_audit_on_key(&intent, fx);
                // The text layer speaks typed characters and erasures of an editable object
                // itself (ADR-0007 TE-1, TE-5): the generic echo would double them.
                let text_owns = self.text_key(&intent, fx);
                if !text_owns
                    && intent.event.pressed
                    && self.cmd.effective.echo.typed_characters
                    && let Some(text) = key_echo_text(
                        intent.event.unichar,
                        intent.event.keysym,
                        intent.event.modifiers,
                    )
                    && (!key_echo_is_action(&text) || self.cmd.effective.echo.command_keys)
                {
                    let id = self.utt_id();
                    let mut u = key_echo(&text, id, intent.epoch);
                    self.mark_capital(&mut u);
                    fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                }
            }
            KeyClass::Swallowed | KeyClass::Injected => {}
        }
    }

    /// A key that is going to the application reaches a Reader whose picture of the focus may
    /// have been lost without a focus event (a popup or dialog closed, a lookup failed, an
    /// extension was not attached yet): every state that waits for the next focus event to be
    /// repaired is checked here, so one key press is enough, not an Alt+Tab. A terminal that
    /// holds the focus but is not tracked is tracked again; a web application that is not
    /// browsing resolves its focus again. Both are rate limited and never guess the focus: they
    /// start from the focus state the Reader already holds.
    pub(crate) fn focus_audit_on_key(&mut self, intent: &KeyIntent, fx: &mut ReaderEffects) {
        if !intent.event.pressed || !self.text_active() {
            return;
        }
        let crate::focus::FocusState::Focused {
            node, presented, ..
        } = self.focus
        else {
            return;
        };
        if self.selection_node == Some(node) {
            return;
        }
        // The focus event came, but its presentation never finished (the read failed twice, the
        // application was busy): nothing was tracked or announced. Present it again.
        if !presented && self.represent_unfinished_focus(node, fx) {
            return;
        }
        if !self.term_retrack_on_key(fx) {
            self.web_resync_on_key(node, fx);
        }
    }

    /// The focused node was never presented: ask for it again, once per `REPRESENT_INTERVAL`,
    /// and only when its first try is long over (it is not simply still in flight).
    fn represent_unfinished_focus(
        &mut self,
        node: gorca_model::NodeKey,
        fx: &mut ReaderEffects,
    ) -> bool {
        let now = self.now;
        let Some((n, at)) = self.last_focus else {
            return false;
        };
        if n != node || now.saturating_duration_since(at) < REPRESENT_AFTER {
            return false;
        }
        if let Some((rn, rat)) = self.focus_represent_at
            && rn == node
            && now.saturating_duration_since(rat) < REPRESENT_INTERVAL
        {
            return false;
        }
        self.focus_represent_at = Some((node, now));
        self.counters.focus_represented_on_key += 1;
        tracing::debug!(
            ?node,
            "focused node never presented: presenting it again at a key"
        );
        self.represent_focus(node, fx);
        true
    }
}

/// A focus whose presentation has not finished this long after the event is presented again by
/// the next key.
const REPRESENT_AFTER: std::time::Duration = std::time::Duration::from_millis(1500);
/// The shortest time between two such presentations.
const REPRESENT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
