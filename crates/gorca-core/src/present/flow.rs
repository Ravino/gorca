//! The `Focus`, `Reply` and `Timer` arms: focus settling, fetch-on-miss, presentation and the
//! focused-application protection published once a focus is validated.

use std::sync::Arc;
use std::time::Instant;

use gorca_model::{NodeKey, NodeSnapshot, PropMask, StateSet};
use gorca_rt::BudgetClass;

use super::{PresentContext, message};
use crate::SpeechIntent;
use crate::focus::{FocusEffect, FocusMsg, FocusState, settle_delay};
use crate::reader::{
    DUPLICATE_FOCUS_WINDOW, FocusLaneMsg, QueryReply, Reader, ReaderEffect, ReaderEffects,
};

impl Reader {
    pub(crate) fn on_focus(&mut self, f: FocusLaneMsg, now: Instant, fx: &mut ReaderEffects) {
        match f {
            FocusLaneMsg::FocusChanged { node } => {
                self.selection_node = None;
                self.begin_focus(node, now, fx);
            }
            FocusLaneMsg::ItemSelected { node } => {
                self.selection_node = Some(node);
                self.begin_focus(node, now, fx);
            }
            FocusLaneMsg::NodeRemoved(n) => self.apply_focus(FocusMsg::NodeRemoved(n), fx),
            FocusLaneMsg::AppGone(a) => {
                if let Some(f) = &self.focused_app {
                    f.forget(a);
                }
                self.apply_focus(FocusMsg::AppGone(a), fx);
            }
            FocusLaneMsg::AppHung(a) => self.apply_focus(FocusMsg::AppHung(a), fx),
        }
    }

    pub(crate) fn begin_focus(&mut self, node: NodeKey, now: Instant, fx: &mut ReaderEffects) {
        if let Some((last, at)) = self.last_focus
            && last == node
            && now.saturating_duration_since(at) < DUPLICATE_FOCUS_WINDOW
        {
            self.duplicate_focus_dropped += 1;
            return;
        }
        self.last_focus = Some((node, now));
        // The epoch advances first, so stale speech cancels now, not after δ (E-2).
        let epoch = self.epoch.advance();
        let role = self.dir.lookup(node).hit().map(|n| n.role);
        let until = now + settle_delay(role);
        self.apply_focus(FocusMsg::FocusChanged { node, epoch, until }, fx);
        if let FocusState::Settling { until, .. } = self.focus {
            if until <= now {
                self.apply_focus(FocusMsg::TimerElapsed { now }, fx);
            } else {
                fx.push(ReaderEffect::ArmTimer(until));
            }
        }
    }

    pub(crate) fn apply_focus(&mut self, msg: FocusMsg, fx: &mut ReaderEffects) {
        let (next, effect) = self.focus.step(msg);
        self.focus = next;
        let Some(effect) = effect else { return };
        match effect {
            FocusEffect::BeginPresent { node, epoch } => {
                // Mirror read, lock-free. On a miss, fetch and wait for the reply.
                match self.dir.lookup(node) {
                    gorca_model::Lookup::Hit(snap) => self.present(&snap, fx),
                    gorca_model::Lookup::Miss => fx.push(ReaderEffect::Fetch {
                        node,
                        mask: PropMask::PRESENT,
                        class: BudgetClass::Interactive,
                        epoch,
                    }),
                    gorca_model::Lookup::Gone => {
                        self.apply_focus(FocusMsg::NodeRemoved(node), fx);
                    }
                }
            }
            FocusEffect::Speak { node, epoch } => {
                // Reached when `Presented` was stepped by `present`; the snapshot was already
                // turned into an utterance there, so nothing is left to do for `node`/`epoch`.
                let _ = (node, epoch);
            }
            FocusEffect::QueryActiveWindowFocus => fx.push(ReaderEffect::QueryActiveWindowFocus),
            FocusEffect::AnnounceNotResponding => {
                let id = self.utt_id();
                let u = message("application not responding", id, self.epoch.current());
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            }
        }
    }

    /// Step `Presented` for the snapshot's node and, if that produced the `Speak` effect, build
    /// the utterance from the snapshot at hand.
    pub(crate) fn present(&mut self, snap: &Arc<NodeSnapshot>, fx: &mut ReaderEffects) {
        let (next, effect) = self.focus.step(FocusMsg::Presented { node: snap.key });
        self.focus = next;
        match effect {
            Some(FocusEffect::Speak { epoch, .. }) => {
                let id = self.utt_id();
                let ctx = if self.selection_node == Some(snap.key) {
                    PresentContext::SelectedItem
                } else {
                    PresentContext::Focus
                };
                let u = self.presenters.present(ctx, snap, id, epoch);
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                // Protection from eviction is earned only here: the focus passed the state
                // machine, was presented, and the fetched node itself shows `FOCUSED`. A focus
                // event alone, from any application, proves nothing.
                if self.selection_node == Some(snap.key) {
                    // A selected item in a switcher: not real focus, so the protection of the
                    // application that does hold focus stays as it is.
                } else if let Some(f) = &self.focused_app {
                    if snap.states.contains(StateSet::FOCUSED)
                        && !snap.states.contains(StateSet::DEFUNCT)
                    {
                        f.publish(snap.key.app);
                    } else {
                        // The Reader's focus is here now and could not be validated: protection
                        // must not stay with whoever held it before (fail closed).
                        f.demote();
                    }
                }
            }
            Some(
                FocusEffect::BeginPresent { .. }
                | FocusEffect::QueryActiveWindowFocus
                | FocusEffect::AnnounceNotResponding,
            )
            | None => {}
        }
    }

    pub(crate) fn on_reply(&mut self, r: QueryReply, fx: &mut ReaderEffects) {
        // E-3: results from a worker with an older epoch are dropped by whoever receives them.
        if r.epoch < self.epoch.current() {
            self.stale_replies_dropped += 1;
            return;
        }
        // An answer to `info.where_am_i` and friends (CR-3), not a focus presentation.
        if self.take_pending_reply(r.node, r.epoch, r.result.as_ref().ok().cloned(), fx) {
            return;
        }
        match r.result {
            Ok(snap) => self.present(&snap, fx),
            // Timeout, Gone, ...: speak from the cache if there is anything (`may_refine`).
            Err(_) => {
                if let Some(snap) = self.dir.lookup(r.node).hit() {
                    self.present(&snap, fx);
                }
            }
        }
    }
}
