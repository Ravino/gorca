// SPDX-License-Identifier: GPL-3.0-or-later
//! The `Mode` arm: mode state machine steps and their effects.

use crate::mode::{ModeEffect, ModeMsg};
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

impl Reader {
    pub(crate) fn on_mode(&mut self, m: ModeMsg, fx: &mut ReaderEffects) {
        let (next, effects) = self.mode.step(m, &self.keys);
        self.mode = next;
        for e in effects {
            match e {
                ModeEffect::AdvanceEpoch => {
                    self.epoch.advance();
                }
                ModeEffect::SetGrabSet(_) => {
                    fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
                }
            }
        }
    }
}
