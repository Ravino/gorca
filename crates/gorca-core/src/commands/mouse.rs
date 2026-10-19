// SPDX-License-Identifier: GPL-3.0-or-later
//! Mouse emulation in the Reader (ADR-0025): the runtime gate of the keypad chords and the answers
//! of the `mouse.*` commands while pointer emulation is not usable.
//!
//! MO-3 scope: gate, grab sets and the "not available" answers. The clicks themselves (pointer
//! actor effects, hold machines, resolver) arrive with MO-4/MO-5; until the daemon feeds
//! [`PointerMsg`], the availability stays `Absent`, the gate stays closed, the keypad types, and
//! the gorca+ chords say why the mouse is not available.

use std::time::Instant;

use gorca_i18n::{MsgId as M, msg, t};
use gorca_output_api::PointerAvail;

use super::pointer_gate::{GateOut, PointerMsg};
use super::table::CoreCmd;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// The file the "allow pointer" message names (the owner's allow-list, MI-9: gorca never writes it).
const ALLOW_LIST_FILE: &str = "~/.config/gorca/shell-helper.json";

impl Reader {
    /// Availability or `mouse.enabled` changed (`ControlMsg::Pointer`).
    pub(crate) fn on_pointer(&mut self, m: PointerMsg, now: Instant, fx: &mut ReaderEffects) {
        let out = self.cmd.pointer.step(m, now);
        self.apply_gate(out, fx);
    }

    /// The Reader's timer: a damped opening may be due (RG-5).
    pub(crate) fn pointer_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let out = self.cmd.pointer.tick(now);
        self.apply_gate(out, fx);
    }

    /// A gate change is a new generation of the existing session's `SetKeyGrabs` (RG-4): the same
    /// effect as any other grab-set change, never a reconnect, never `GrabKeyboard`.
    fn apply_gate(&mut self, out: GateOut, fx: &mut ReaderEffects) {
        if let Some(open) = out.changed {
            debug_assert_eq!(open, self.cmd.pointer.is_open());
            self.keys.open_gates = self.cmd.pointer.open_bits();
            fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
        }
        if let Some(at) = out.wake {
            fx.push(ReaderEffect::ArmTimer(at));
        }
    }

    /// The text for a gated command while the gate is closed, learn mode and the answer to a
    /// gorca+ chord alike.
    pub(crate) fn mouse_unavailable_text(&self) -> Option<String> {
        let g = &self.cmd.pointer;
        if !g.enabled() {
            return Some(t(M::MouseUnavailableOff).to_owned());
        }
        match g.avail() {
            PointerAvail::Ready { .. } => None,
            // Keys stay grabbed while the helper is slow (C-3); the command says why it does nothing.
            PointerAvail::Unresponsive => Some(t(M::MouseNotResponding).to_owned()),
            PointerAvail::Absent | PointerAvail::Squatted => {
                Some(t(M::MouseUnavailableHelper).to_owned())
            }
            PointerAvail::TooOld { .. } => Some(t(M::MouseUnavailableOld).to_owned()),
            PointerAvail::NoCap => Some(msg!(MouseUnavailableCap, file = ALLOW_LIST_FILE)),
            PointerAvail::Denied => Some(t(M::MouseUnavailableDenied).to_owned()),
        }
    }

    pub(crate) fn mouse_command(&mut self, c: CoreCmd, fx: &mut ReaderEffects) {
        debug_assert!(c.spec().gate.is_some());
        if let Some(text) = self.mouse_unavailable_text() {
            self.say(&text, fx);
        } else {
            // Usable: the click path is MO-4. Nothing to do yet (and unreachable in a running daemon
            // until the pointer actor feeds `PointerMsg`).
            tracing::debug!(
                cmd = c.name(),
                "mouse command with a usable helper: not wired yet"
            );
        }
    }
}
