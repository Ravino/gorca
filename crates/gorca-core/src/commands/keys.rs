//! The `Key` arm: classification result to effect, plus key echo.

use gorca_input_api::{KeyClass, KeyIntent};

use crate::SpeechIntent;
use crate::present::{key_echo, key_echo_text};
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

impl Reader {
    pub(crate) fn on_key(&mut self, intent: KeyIntent, fx: &mut ReaderEffects) {
        // E-4: adopt the epoch carried by the key press so the utterance answering it is never
        // cancelled by that same key press.
        self.epoch.observe(intent.epoch);
        match intent.class {
            KeyClass::Command(cmd) => fx.push(ReaderEffect::RunCommand(cmd)),
            KeyClass::Ambiguous { .. } => self.ambiguous_keys += 1,
            KeyClass::PassToApp => {
                if intent.event.pressed
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
            KeyClass::Injected => {}
        }
    }
}
