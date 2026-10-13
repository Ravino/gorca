//! The `Key` arm: classification result to effect, plus key echo.

use std::sync::atomic::Ordering;
use std::time::Instant;

use gorca_input_api::{KeyClass, KeyIntent};

use crate::SpeechIntent;
use crate::mode::Mode;
use crate::present::{key_echo, key_echo_text};
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
        match intent.class {
            KeyClass::Command(cmd) | KeyClass::Watched(cmd) => {
                self.run_command(cmd, &intent, now, fx);
            }
            KeyClass::Described(cmd) => self.describe(&intent, cmd, fx),
            KeyClass::Ambiguous { .. } => {
                self.ambiguous_keys += 1;
                self.pass_key_seen(&intent, fx);
            }
            KeyClass::ModifierPassThrough => {
                // LM-5: the double tap still reaches the application in learn mode.
                if matches!(self.mode, Mode::InputHelp { .. }) {
                    self.say("gorca key passed to application", fx);
                }
            }
            KeyClass::PassToApp => {
                self.pass_key_seen(&intent, fx);
                if intent.event.pressed
                    && self.cmd.effective.echo.typed_characters
                    && let Some(text) = key_echo_text(
                        intent.event.unichar,
                        intent.event.keysym,
                        intent.event.modifiers,
                    )
                {
                    let id = self.utt_id();
                    let u = key_echo(&text, id, intent.epoch);
                    fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                }
            }
            KeyClass::Swallowed | KeyClass::Injected => {}
        }
    }
}
