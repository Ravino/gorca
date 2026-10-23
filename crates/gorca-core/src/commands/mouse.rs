// SPDX-License-Identifier: GPL-3.0-or-later
//! Mouse emulation in the Reader (ADR-0025): the runtime gate of the keypad chords, click at the
//! pointer, drag lock with the 120 s limit, and the answers of the `mouse.*` commands while
//! pointer emulation is not usable.
//!
//! Nothing here does I/O (EX-1, MI-2). A command produces `ReaderEffect::Pointer(req)`; the runtime
//! runs it against the `PointerSink` with a deadline and the answer comes back as
//! `ControlMsg::PointerReply`. Plain clicks and lock toggles are acknowledged at dispatch or on
//! the answer from a table lookup, within the key budget.
//!
//! MO-4 scope: click at the pointer (`mouse.{left,right,middle}_click`), the two locks,
//! `mouse.say_position`. Routing to and from the review cursor and the item clicks (`*_item`,
//! `route_to_review`, `review_to_pointer`) need the AT-SPI resolver (MO-5) and answer "No location".

use std::time::Instant;

use gorca_i18n::{MsgId as M, msg, t};
use gorca_output_api::{Button, ButtonAct, OutcomeKind, PointerAvail, PointerError};

use super::holds::{Hold, HoldCmd, HoldEvent, ReleaseWhy};
use super::pointer_gate::{GateOut, PointerMsg};
use super::pointer_req::{
    AtAct, ItemAction, PointerAtReq, PointerDone, PointerOp, PointerReply, PointerReq, PtrReqId,
};
use crate::focus::FocusState;
use crate::mode::Mode;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};
use gorca_model::NodeKey;
use gorca_output_api::IfCovered;

/// Replies still awaited; bounded so a lost reply cannot grow the table.
const MAX_PENDING: usize = 32;

/// The two lockable buttons.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct Holds {
    pub left: Hold,
    pub right: Hold,
}

impl Holds {
    fn of(&mut self, b: Button) -> Option<&mut Hold> {
        match b {
            Button::Left => Some(&mut self.left),
            Button::Right => Some(&mut self.right),
            Button::Middle => None,
        }
    }

    fn any_needs_release(&self) -> bool {
        self.left.needs_release() || self.right.needs_release()
    }

    fn lost(&mut self) {
        self.left.lost();
        self.right.lost();
    }
}

/// The `mouse.*` commands, one variant each (the handler table maps `CoreCmd` onto them).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum MouseCmd {
    LeftClick,
    RightClick,
    MiddleClick,
    LeftClickItem,
    RightClickItem,
    LeftLockToggle,
    RightLockToggle,
    RouteToReview,
    ReviewToPointer,
    SayPosition,
}

/// What a request in flight was for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Pending {
    Click(Button),
    Hold(Button),
    Position {
        screen: bool,
    },
    /// Route the pointer to the review position.
    Route,
    /// An item click (action first, then the pointer) with this button.
    Item(Button),
    /// A best-effort release (emulation switched off): nobody waits for it.
    Quiet,
}

impl Reader {
    // ---- the gate -------------------------------------------------------------------

    /// Availability or `mouse.enabled` changed (`ControlMsg::Pointer`).
    pub(crate) fn on_pointer(&mut self, m: PointerMsg, now: Instant, fx: &mut ReaderEffects) {
        let before = self.cmd.pointer.avail().can_call();
        let out = self.cmd.pointer.step(m, now);
        // Emulation went away: the buttons are not ours any more (the helper releases on its own).
        // Emulation switched off while usable: release best effort first.
        if let PointerMsg::Enabled(false) = m {
            if before && self.cmd.holds.any_needs_release() {
                self.send_pointer(PointerOp::ReleaseAll, Pending::Quiet, fx);
            }
            self.cmd.holds.lost();
        }
        let a = self.cmd.pointer.avail();
        if !a.can_call() && a != PointerAvail::Unresponsive {
            self.cmd.holds.lost();
        }
        self.apply_gate(out, fx);
    }

    /// The Reader's timer: a damped opening may be due (RG-5) and a held button may be due for
    /// release (120 s).
    pub(crate) fn pointer_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let out = self.cmd.pointer.tick(now);
        self.apply_gate(out, fx);
        for b in [Button::Left, Button::Right] {
            // A press or release whose reply never came: give up (the next toggle sends ReleaseAll).
            if self.cmd.holds.of(b).is_some_and(|h| h.fail_stale(now)) {
                self.say(t(M::MouseNotResponding), fx);
            }
            let req = self.next_ptr_id();
            let cmd = self
                .cmd
                .holds
                .of(b)
                .and_then(|h| h.expire(now, req).map(|c| (c, req)));
            if let Some((HoldCmd::Release, req)) = cmd {
                self.send_with_id(req, PointerOp::Release(b), Pending::Hold(b), fx);
            }
        }
        self.arm_hold_timer(fx);
    }

    fn arm_hold_timer(&self, fx: &mut ReaderEffects) {
        let next = [
            self.cmd.holds.left.deadline(),
            self.cmd.holds.right.deadline(),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(at) = next {
            fx.push(ReaderEffect::ArmTimer(at));
        }
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

    /// The text for a gated command while pointer emulation is not usable: learn mode and the
    /// answer to a gorca+ chord alike.
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
            PointerAvail::NoCap => Some(self.no_cap_text()),
            PointerAvail::Denied => Some(t(M::MouseUnavailableDenied).to_owned()),
        }
    }

    fn no_cap_text(&self) -> String {
        msg!(
            MouseUnavailableCap,
            hint = self.cmd.pointer_hint.as_deref().unwrap_or("")
        )
        .trim_end()
        .to_owned()
    }

    // ---- commands -------------------------------------------------------------------

    pub(crate) fn mouse_command(&mut self, c: MouseCmd, repeat: u8, fx: &mut ReaderEffects) {
        // The item clicks try the application's own action first, which needs no pointer: they say why the mouse
        // is not available only when the action path has nothing to do and the pointer is needed.
        let item_click = matches!(c, MouseCmd::LeftClickItem | MouseCmd::RightClickItem);
        if !item_click && let Some(text) = self.mouse_unavailable_text() {
            self.say(&text, fx);
            return;
        }
        match c {
            MouseCmd::LeftClick => self.mouse_click(Button::Left, fx),
            MouseCmd::RightClick => self.mouse_click(Button::Right, fx),
            MouseCmd::MiddleClick => self.mouse_click(Button::Middle, fx),
            MouseCmd::LeftLockToggle => self.mouse_lock(Button::Left, fx),
            MouseCmd::RightLockToggle => self.mouse_lock(Button::Right, fx),
            MouseCmd::SayPosition => {
                self.send_pointer(
                    PointerOp::Position,
                    Pending::Position {
                        screen: repeat >= 2,
                    },
                    fx,
                );
            }
            // The resolver (review position, item extents, window matching) arrives with MO-5.
            MouseCmd::RouteToReview => self.mouse_route(fx),
            MouseCmd::LeftClickItem => self.mouse_item_click(Button::Left, fx),
            MouseCmd::RightClickItem => self.mouse_item_click(Button::Right, fx),
            MouseCmd::ReviewToPointer => self.say(t(M::MouseNoLocation), fx),
        }
    }

    /// NVDA: click at the current pointer position, speaking first. A click of a button that is
    /// locked releases it instead (ADR-0025 2.6).
    fn mouse_click(&mut self, b: Button, fx: &mut ReaderEffects) {
        if self.cmd.holds.of(b).is_some_and(|h| h.is_held()) {
            self.mouse_lock(b, fx);
            return;
        }
        let text = match b {
            Button::Left => t(M::MouseLeftClick),
            Button::Right => t(M::MouseRightClick),
            Button::Middle => t(M::MouseMiddleClick),
        };
        self.say(text, fx);
        self.send_pointer(PointerOp::Click(b, ButtonAct::Click), Pending::Click(b), fx);
    }

    fn mouse_lock(&mut self, b: Button, fx: &mut ReaderEffects) {
        let req = self.next_ptr_id();
        let now = self.now;
        let Some(cmd) = self.cmd.holds.of(b).map(|h| h.toggle(req, now)) else {
            return;
        };
        match cmd {
            HoldCmd::Press => self.send_with_id(req, PointerOp::Press(b), Pending::Hold(b), fx),
            HoldCmd::Release => {
                self.send_with_id(req, PointerOp::Release(b), Pending::Hold(b), fx);
            }
            HoldCmd::ReleaseAll => {
                self.send_with_id(req, PointerOp::ReleaseAll, Pending::Hold(b), fx);
            }
            HoldCmd::Busy => self.say(t(M::MouseBusy), fx),
        }
    }

    /// Orca: click the review item. `mouse.item_click = action_first` (default) asks the application's own click
    /// (left) or context-menu (right) action first and uses the pointer only when it has none; `pointer` always
    /// uses the pointer. Spoken after the outcome (ADR-0025 EX-4).
    fn mouse_item_click(&mut self, b: Button, fx: &mut ReaderEffects) {
        use gorca_config::{IfCoveredPolicy, ItemClick};
        let Some((node, offset)) = self.review_target() else {
            self.say(t(M::MouseNoLocation), fx);
            return;
        };
        let (policy, covered) = {
            let m = &self.cmd.effective.mouse;
            (m.item_click, m.if_covered)
        };
        let item = match (policy, b) {
            (ItemClick::Pointer, _) | (ItemClick::ActionFirst, Button::Middle) => None,
            (ItemClick::ActionFirst, Button::Right) => Some(ItemAction::ContextMenu),
            (ItemClick::ActionFirst, Button::Left) => Some(ItemAction::Click),
        };
        if item.is_none()
            && let Some(text) = self.mouse_unavailable_text()
        {
            self.say(&text, fx);
            return;
        }
        let id = self.next_ptr_id();
        self.cmd.ptr_pending.insert(id, Pending::Item(b));
        fx.push(ReaderEffect::PointerAt(PointerAtReq {
            id,
            node,
            // The action path ignores the character; the pointer path aims at it.
            offset,
            act: AtAct::Click(b),
            if_covered: match covered {
                IfCoveredPolicy::Refuse => IfCovered::Refuse,
                IfCoveredPolicy::Click => IfCovered::Proceed,
            },
            item,
        }));
    }

    /// NVDA: move the mouse to the navigator (the review position, else the focused object).
    fn mouse_route(&mut self, fx: &mut ReaderEffects) {
        let Some((node, offset)) = self.review_target() else {
            self.say(t(M::MouseNoLocation), fx);
            return;
        };
        let id = self.next_ptr_id();
        self.cmd.ptr_pending.insert(id, Pending::Route);
        fx.push(ReaderEffect::PointerAt(PointerAtReq {
            id,
            node,
            offset,
            act: AtAct::Move,
            // Moving onto something that is covered is harmless; the reply says so.
            if_covered: IfCovered::Proceed,
            item: None,
        }));
    }

    /// The node (and character offset) the review position stands for: in review mode the cursor's
    /// zone (a text line gives the character, an object its centre), in browse mode the browse cursor's
    /// node, else the focused object.
    pub(crate) fn review_target(&self) -> Option<(NodeKey, Option<u32>)> {
        use crate::review::cursor::ReviewPos;
        use crate::review::model::ZoneKind;
        if let Mode::Review { cursor, .. } = self.mode {
            return match cursor.pos {
                ReviewPos::Object { .. } | ReviewPos::Screen(None) => Some((cursor.nav, None)),
                ReviewPos::Screen(Some(p)) => {
                    let m = self.review.cur.as_ref().filter(|m| m.id == p.model)?;
                    let (_, z) = crate::review::flat::zone_at(m, p.line, p.zone)?;
                    let off = match &z.kind {
                        ZoneKind::Text { src } => Some(src.start.saturating_add(p.offset)),
                        ZoneKind::Object | ZoneKind::Name => None,
                    };
                    Some((z.node, off))
                }
            };
        }
        if matches!(self.mode, Mode::Browse { .. }) {
            // The virtual cursor, never the focus: in a browser the focus is the document or a
            // stale control. No answer is a refusal, not a click on the document centre.
            return self.web_cursor_key().map(|k| (k, None));
        }
        match self.focus {
            FocusState::Focused { node, .. } => Some((node, None)),
            FocusState::NoFocus | FocusState::Settling { .. } | FocusState::Orphaned { .. } => None,
        }
    }

    fn next_ptr_id(&mut self) -> PtrReqId {
        self.cmd.ptr_next += 1;
        PtrReqId(self.cmd.ptr_next)
    }

    fn send_pointer(&mut self, op: PointerOp, why: Pending, fx: &mut ReaderEffects) {
        let id = self.next_ptr_id();
        self.send_with_id(id, op, why, fx);
    }

    fn send_with_id(&mut self, id: PtrReqId, op: PointerOp, why: Pending, fx: &mut ReaderEffects) {
        if self.cmd.ptr_pending.len() >= MAX_PENDING {
            // A reply never came for the oldest: forget them all rather than grow.
            self.cmd.ptr_pending.clear();
        }
        self.cmd.ptr_pending.insert(id, why);
        fx.push(ReaderEffect::Pointer(PointerReq { id, op }));
    }

    // ---- replies --------------------------------------------------------------------

    pub(crate) fn on_pointer_reply(
        &mut self,
        r: PointerReply,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let Some(why) = self.cmd.ptr_pending.remove(&r.id) else {
            return; // forgotten (overflow) or a stale duplicate
        };
        match why {
            Pending::Quiet => {}
            Pending::Click(b) => self.click_reply(b, &r, fx),
            Pending::Hold(b) => self.hold_reply(b, &r, now, fx),
            Pending::Item(b) => match r.result {
                // Done by the application's own action, or by the pointer.
                Ok(PointerDone::Acted) => self.say(item_text(b), fx),
                Ok(PointerDone::Outcome(o)) => match o.kind {
                    OutcomeKind::Done => self.say(item_text(b), fx),
                    OutcomeKind::Expired => self.say(t(M::MouseNotResponding), fx),
                    OutcomeKind::RefusedOccluded => self.say(t(M::MouseCoveredShell), fx),
                    OutcomeKind::RefusedOutside => self.say(t(M::MouseNoLocation), fx),
                },
                Ok(PointerDone::Released(_) | PointerDone::Pos(_)) => {}
                Err(e) => self.say_pointer_error(&e, fx),
            },
            Pending::Route => match r.result {
                Ok(PointerDone::Outcome(o)) => {
                    // Moved (Proceed): say so, and that something covers the spot if it does.
                    if self.cmd.effective.mouse.announce_move {
                        self.say(t(M::MouseMoved), fx);
                    }
                    if o.occluded {
                        self.say_queued(t(M::MouseCoveredShell), fx);
                    }
                }
                Ok(PointerDone::Released(_) | PointerDone::Pos(_) | PointerDone::Acted) => {}
                Err(e) => self.say_pointer_error(&e, fx),
            },
            Pending::Position { screen } => match r.result {
                Ok(PointerDone::Pos(p)) => {
                    let text = if screen || p.window == 0 {
                        msg!(
                            MousePositionScreen,
                            x = px(p.x),
                            y = px(p.y),
                            n = p.monitor + 1
                        )
                    } else {
                        msg!(
                            MousePosition,
                            window = t(M::MouseWindow),
                            x = px(p.wx),
                            y = px(p.wy)
                        )
                    };
                    self.say(&text, fx);
                }
                Ok(PointerDone::Outcome(_) | PointerDone::Released(_) | PointerDone::Acted) => {}
                Err(e) => self.say_pointer_error(&e, fx),
            },
        }
        self.arm_hold_timer(fx);
    }

    fn click_reply(&mut self, _b: Button, r: &PointerReply, fx: &mut ReaderEffects) {
        match &r.result {
            Ok(PointerDone::Outcome(o)) => match o.kind {
                OutcomeKind::Done => {}
                // The Shell ran the call after its deadline: nothing was injected.
                OutcomeKind::Expired => self.say(t(M::MouseNotResponding), fx),
                OutcomeKind::RefusedOccluded | OutcomeKind::RefusedOutside => {
                    self.say(t(M::MouseCoveredShell), fx);
                }
            },
            Ok(PointerDone::Released(_) | PointerDone::Pos(_) | PointerDone::Acted) => {}
            Err(e) => self.say_pointer_error(e, fx),
        }
    }

    fn hold_reply(&mut self, b: Button, r: &PointerReply, now: Instant, fx: &mut ReaderEffects) {
        match &r.result {
            Ok(done) => {
                let held = match done {
                    PointerDone::Outcome(o) => o.holds(b),
                    PointerDone::Released(_) | PointerDone::Pos(_) | PointerDone::Acted => false,
                };
                let ev = self
                    .cmd
                    .holds
                    .of(b)
                    .and_then(|h| h.reply_ok(r.id, held, now));
                match ev {
                    Some(HoldEvent::Locked) => self.say(lock_text(b, true), fx),
                    Some(HoldEvent::Unlocked(ReleaseWhy::User)) => {
                        self.say(lock_text(b, false), fx);
                    }
                    Some(HoldEvent::Unlocked(ReleaseWhy::Expired)) => {
                        let name = match b {
                            Button::Right => t(M::MouseButtonRight),
                            Button::Left | Button::Middle => t(M::MouseButtonLeft),
                        };
                        self.say(&msg!(MouseLockExpired, button = name), fx);
                    }
                    None => {
                        // Not the state this reply was for, or a press that did not show as held.
                        if let Some(h) = self.cmd.holds.of(b)
                            && h.needs_release()
                        {
                            self.say(t(M::MouseNotResponding), fx);
                        }
                    }
                }
            }
            Err(e) => {
                if self.cmd.holds.of(b).is_some_and(|h| h.reply_err(r.id)) {
                    self.say_pointer_error(e, fx);
                }
            }
        }
    }

    fn say_pointer_error(&mut self, e: &PointerError, fx: &mut ReaderEffects) {
        let text = match e {
            PointerError::Locked => t(M::MouseLockedSession).to_owned(),
            PointerError::Busy | PointerError::RateLimited => t(M::MouseBusy).to_owned(),
            PointerError::NoWindow | PointerError::NoLocation => t(M::MouseNoLocation).to_owned(),
            PointerError::NotVisible => t(M::MouseWindowHidden).to_owned(),
            PointerError::Unavailable(a) => match a {
                PointerAvail::Unresponsive | PointerAvail::Ready { .. } => {
                    t(M::MouseNotResponding).to_owned()
                }
                PointerAvail::Absent | PointerAvail::Squatted => {
                    t(M::MouseUnavailableHelper).to_owned()
                }
                PointerAvail::TooOld { .. } => t(M::MouseUnavailableOld).to_owned(),
                PointerAvail::NoCap => self.no_cap_text(),
                PointerAvail::Denied => t(M::MouseUnavailableDenied).to_owned(),
            },
            PointerError::Denied => t(M::MouseUnavailableDenied).to_owned(),
            PointerError::Timeout
            | PointerError::NotResponding
            | PointerError::BadArgs(_)
            | PointerError::Failed(_) => t(M::MouseNotResponding).to_owned(),
        };
        self.say(&text, fx);
    }
}

/// A coordinate as a whole number for speech.
#[allow(clippy::cast_possible_truncation)] // clamped to +-1e6 first
fn px(v: f64) -> i32 {
    v.round().clamp(-1.0e6, 1.0e6) as i32
}

/// What an item click says once it is done.
fn item_text(b: Button) -> &'static str {
    match b {
        Button::Right => t(M::MouseRightClick),
        Button::Left => t(M::MouseLeftClick),
        Button::Middle => t(M::MouseMiddleClick),
    }
}

fn lock_text(b: Button, locked: bool) -> &'static str {
    match (b, locked) {
        (Button::Right, true) => t(M::MouseRightLocked),
        (Button::Right, false) => t(M::MouseRightUnlocked),
        (Button::Left | Button::Middle, true) => t(M::MouseLeftLocked),
        (Button::Left | Button::Middle, false) => t(M::MouseLeftUnlocked),
    }
}
