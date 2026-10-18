// SPDX-License-Identifier: GPL-3.0-or-later
//! The `Mode` arm: mode state machine steps and their effects.

use crate::mode::{ModeEffect, ModeMsg};
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

impl Reader {
    pub(crate) fn on_mode(&mut self, m: ModeMsg, fx: &mut ReaderEffects) {
        self.on_mode_epoch(m, true, fx);
    }

    /// [`Reader::on_mode`]; `advance` false drops the epoch advance of the step. A key press that
    /// causes the step has advanced the epoch itself (the input lane, `interrupt_on_keys`), and
    /// advancing again would make this key's echo, and any key typed before the Reader runs,
    /// older than the epoch: the speech lane drops stale utterances.
    pub(crate) fn on_mode_epoch(&mut self, m: ModeMsg, advance: bool, fx: &mut ReaderEffects) {
        let (next, effects) = self.mode.step(m, &self.keys);
        self.mode = next;
        for e in effects {
            match e {
                ModeEffect::AdvanceEpoch => {
                    if advance {
                        self.epoch.advance();
                    }
                }
                ModeEffect::SetGrabSet(_) => {
                    fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
                }
            }
        }
    }
}
