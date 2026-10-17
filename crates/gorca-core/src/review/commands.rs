//! The review commands as Reader handlers (ADR-0006 4, 5, 6, 7, 8).
//!
//! A command either finishes in the same call (a mirror hit, a valid model: no IPC) or
//! **suspends**: it records a [`Pending`], sends one effect, and resumes when the reply arrives
//! (RS-1: at most one review operation is pending). Commands pressed meanwhile queue (at most 8)
//! and run after it; only the last one speaks. Nothing here awaits (R3).

use gorca_i18n::{MsgId as M, msg, t};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_connector_api::ActionReq;
use gorca_model::{Epoch, Lookup, NodeKey, NodeSnapshot, PropMask, Role, StateSet};
use gorca_speech::How;
use smallvec::SmallVec;

use super::build::{NavFetchError, ProbeItem, RrVerdict};
use super::cursor::{BuildId, FlatPos, ModelId, NavReqId, ReviewCursor, ReviewFrom, ReviewKind};
use super::cursor::{ReviewOrigin, ReviewPos, hash_line};
use super::flat::{
    Carried, FlatMove, anchor_at_node, carry_terminal, flat_step, line_text, nearest_line, pos_at,
    reanchor,
};
use super::model::{Completeness, LineModel, LineSource};
use super::objnav::{NavMove, NavOp, StepResult, step, step_degraded};
use super::segment::{Dir, SimpleUnits, TermUnits, TextUnits, Unit, slice};
use super::terminal::held_model;
use super::validity::{Validity, validity};
use super::{BuildRequest, OpState, ReviewMsg, RrCheck};
use crate::SpeechIntent;
use crate::commands::CoreCmd;
use crate::mode::{Mode, ModeMsg};
use crate::present::{PresentContext, flat_zone_text};
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// Safety margin on top of a task's own deadline before the Reader gives up waiting.
const GRACE: Duration = Duration::from_millis(40);
/// A model build gets this long before the Reader degrades (the task publishes at 120 ms).
const BUILD_WAIT: Duration = Duration::from_millis(500);
/// How long a review step waits for a terminal's screen to be read again.
const TERM_WAIT: Duration = Duration::from_millis(400);
/// One review step, queue included, is over after this long: OB-3 and R4 allow the 120 ms Interactive
/// deadline plus 5 ms. Whatever is still unknown then is spoken degraded, with no further IPC.
const STEP_BUDGET: Duration = Duration::from_millis(125);
/// Commands that may wait behind a pending operation (ADR-0006 4.3).
const QUEUE_MAX: usize = 8;
/// Fraction of dirty nodes that makes a refresh a full rebuild (VD-3), in percent.
const DIRTY_FULL_PERCENT: usize = 25;

/// The review-side meaning of a command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum RCmd {
    Obj(NavMove),
    ObjCurrent,
    Flat(FlatMove),
    /// `true` = `review.mode_next`.
    ModeSwitch(bool),
    ToFocus,
    MoveFocus,
    Activate,
    Toggle,
    SimpleToggle,
    FocusFollowsToggle,
    StatusBar,
}

impl RCmd {
    /// Commands that act on the cursor wait for a pending operation; the others cancel it.
    fn cursor_relative(self) -> bool {
        match self {
            RCmd::Obj(_) | RCmd::ObjCurrent | RCmd::Flat(_) | RCmd::ModeSwitch(_) => true,
            RCmd::ToFocus
            | RCmd::MoveFocus
            | RCmd::Activate
            | RCmd::Toggle
            | RCmd::SimpleToggle
            | RCmd::FocusFollowsToggle
            | RCmd::StatusBar => false,
        }
    }
}

/// Every command this arm implements.
const COMMANDS: [(CoreCmd, RCmd); 29] = [
    (CoreCmd::ObjectParent, RCmd::Obj(NavMove::Parent)),
    (CoreCmd::ObjectFirstChild, RCmd::Obj(NavMove::FirstChild)),
    (CoreCmd::ObjectNext, RCmd::Obj(NavMove::Next)),
    (CoreCmd::ObjectPrev, RCmd::Obj(NavMove::Prev)),
    (CoreCmd::ObjectNextInFlow, RCmd::Obj(NavMove::NextInFlow)),
    (CoreCmd::ObjectPrevInFlow, RCmd::Obj(NavMove::PrevInFlow)),
    (CoreCmd::ObjectCurrent, RCmd::ObjCurrent),
    (CoreCmd::ReviewPrevLine, RCmd::Flat(FlatMove::PrevLine)),
    (CoreCmd::ReviewCurLine, RCmd::Flat(FlatMove::CurLine)),
    (CoreCmd::ReviewNextLine, RCmd::Flat(FlatMove::NextLine)),
    (CoreCmd::ReviewPrevWord, RCmd::Flat(FlatMove::PrevWord)),
    (CoreCmd::ReviewCurWord, RCmd::Flat(FlatMove::CurWord)),
    (CoreCmd::ReviewNextWord, RCmd::Flat(FlatMove::NextWord)),
    (CoreCmd::ReviewPrevChar, RCmd::Flat(FlatMove::PrevChar)),
    (CoreCmd::ReviewCurChar, RCmd::Flat(FlatMove::CurChar)),
    (CoreCmd::ReviewNextChar, RCmd::Flat(FlatMove::NextChar)),
    (CoreCmd::ReviewTop, RCmd::Flat(FlatMove::Top)),
    (CoreCmd::ReviewBottom, RCmd::Flat(FlatMove::Bottom)),
    (
        CoreCmd::ReviewStartOfLine,
        RCmd::Flat(FlatMove::StartOfLine),
    ),
    (CoreCmd::ReviewEndOfLine, RCmd::Flat(FlatMove::EndOfLine)),
    (CoreCmd::ReviewModeNext, RCmd::ModeSwitch(true)),
    (CoreCmd::ReviewModePrev, RCmd::ModeSwitch(false)),
    (CoreCmd::ReviewToFocus, RCmd::ToFocus),
    (CoreCmd::ReviewMoveFocus, RCmd::MoveFocus),
    (CoreCmd::ReviewActivate, RCmd::Activate),
    (CoreCmd::ReviewToggle, RCmd::Toggle),
    (CoreCmd::ReviewSimpleToggle, RCmd::SimpleToggle),
    (CoreCmd::ReviewFocusFollowsToggle, RCmd::FocusFollowsToggle),
    (CoreCmd::InfoStatusBar, RCmd::StatusBar),
];

/// `Some` for every command this arm implements.
pub(crate) fn map_command(c: CoreCmd) -> Option<RCmd> {
    COMMANDS.iter().find(|(k, _)| *k == c).map(|(_, r)| *r)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ActStage {
    Focus,
    Select,
    Activate,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum What {
    Nav(NavReqId),
    Build(BuildId),
    StatusBar(NavReqId),
    Act(NavReqId, ActStage),
    /// A terminal's screen is being read again before the review step speaks from it.
    Term,
}

/// The one suspended review operation (RS-1).
#[derive(Debug)]
pub(crate) struct Pending {
    pub what: What,
    pub cmd: RCmd,
    pub rep: u8,
    pub attempts: u8,
    pub deadline: Instant,
    /// The epoch of the key press this operation answers (R3: a newer epoch supersedes it).
    pub epoch: Epoch,
    pub queued: SmallVec<[(RCmd, u8); QUEUE_MAX]>,
}

/// How an execution ended.
enum Step {
    Done,
    Suspend {
        what: What,
        effect: ReaderEffect,
        wait: Duration,
    },
}

type Queue = SmallVec<[(RCmd, u8); QUEUE_MAX]>;

/// Which model serves a cursor.
enum Pick {
    Ready(Arc<LineModel>, bool),
    Build(gorca_model::NodeKey),
    Ancestry(NavOp),
    Gone,
}

fn is_surface_role(snap: &NodeSnapshot, parent_is_menubar: bool) -> bool {
    match snap.role {
        Role::Window | Role::Dialog | Role::Alert | Role::Popover => true,
        // SF-2: a showing menu is its own surface, unless it is a menu bar's pull-down title.
        Role::Menu => snap.states.contains(StateSet::SHOWING) && !parent_is_menubar,
        Role::Application
        | Role::Document
        | Role::DocumentWeb
        | Role::Panel
        | Role::Heading
        | Role::Paragraph
        | Role::Link
        | Role::Image
        | Role::List
        | Role::ListItem
        | Role::Table
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Button
        | Role::ToggleButton
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::TextEntry
        | Role::PasswordEntry
        | Role::SpinButton
        | Role::Slider
        | Role::ProgressBar
        | Role::MenuBar
        | Role::MenuItem
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Label
        | Role::StaticText
        | Role::Terminal
        | Role::Landmark
        | Role::Unknown => false,
    }
}

impl Reader {
    // ---- entry points -----------------------------------------------------------------------

    /// A review, object or `info.status_bar` command from the key arm.
    pub(crate) fn review_command(
        &mut self,
        c: CoreCmd,
        rep: u8,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let Some(rc) = map_command(c) else { return };
        // A pending operation past its deadline is over: dropped without a word, the new key
        // speaks (F-3, ADR-0001 R-1: superseded results are never spoken).
        if matches!(&self.review.op, OpState::Pending(p) if now >= p.deadline) {
            self.review.op = OpState::Idle;
            self.review.counters.review_step_timeout += 1;
        }
        if let OpState::Pending(p) = &mut self.review.op {
            if rc.cursor_relative() {
                if p.queued.len() >= QUEUE_MAX {
                    p.queued.pop();
                    self.review.counters.review_queue_overflow += 1;
                }
                p.queued.push((rc, rep));
                return;
            }
            // A command that does not depend on the cursor cancels the pending one.
            self.review.op = OpState::Idle;
        }
        self.review.step_started = Some(now);
        self.exec(rc, rep, 0, Queue::new(), now, fx);
    }

    pub(crate) fn review_message(&mut self, m: ReviewMsg, fx: &mut ReaderEffects) {
        let now = self.now;
        match m {
            ReviewMsg::NavFetched { req, result } => {
                let Some(p) = self.take_pending(|w| w == What::Nav(req)) else {
                    return;
                };
                self.resume(p, result.err(), now, fx);
            }
            ReviewMsg::Built { build, result } => self.on_built(build, result, now, fx),
            ReviewMsg::Revalidated { model, verdict } => {
                self.review_revalidated(model, &verdict, now, fx);
            }
            ReviewMsg::StatusBarFound { req, result } => {
                let Some(p) = self.take_pending(|w| w == What::StatusBar(req)) else {
                    return;
                };
                match result {
                    Ok(Some(bar)) => {
                        self.speak_status_bar(bar, p.rep, fx);
                    }
                    Ok(None) | Err(_) => self.status_bar_fallback(fx),
                }
                self.finish_queue(p.queued, now, fx);
            }
            ReviewMsg::ActionDone { req, result } => {
                let Some(p) = self.take_pending(|w| matches!(w, What::Act(r, _) if r == req))
                else {
                    return;
                };
                let What::Act(_, stage) = p.what else { return };
                self.on_action_done(stage, result, p, now, fx);
            }
        }
    }

    /// Timer: a pending operation whose reply never came ends degraded (ADR-0006 4.3).
    pub(crate) fn review_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        self.expire_late(now, fx);
        // The runtime keeps one timer slot and clears it when it fires: whatever is still
        // outstanding is armed again (F-3).
        if let OpState::Pending(p) = &self.review.op {
            fx.push(ReaderEffect::ArmTimer(p.deadline));
        }
    }

    /// Focus moved while review is active: review follows it by default (ADR-0006 4.1).
    pub(crate) fn review_focus_changed(&mut self, fx: &mut ReaderEffects) {
        if matches!(self.mode, Mode::Review { .. }) && self.review.focus_follows {
            self.review.op = OpState::Idle;
            self.on_mode(ModeMsg::ExitReview, fx);
        }
    }

    /// Drop the pending review operation (review is being left).
    pub(crate) fn review_cancel_pending(&mut self) {
        self.review.op = OpState::Idle;
    }

    pub(crate) fn review_app_gone(&mut self, app: gorca_model::AppId, fx: &mut ReaderEffects) {
        self.review.forget_app(app);
        let in_app = matches!(self.mode, Mode::Review { cursor, .. } if cursor.nav.app == app);
        if in_app {
            self.review.counters.review_surface_gone += 1;
            self.review.op = OpState::Idle;
            self.on_mode(ModeMsg::ExitReview, fx);
        }
    }

    fn expire_late(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let late = matches!(&self.review.op, OpState::Pending(p) if now >= p.deadline);
        if !late {
            return;
        }
        let Some(p) = self.take_pending(|_| true) else {
            return;
        };
        self.review.counters.review_step_timeout += 1;
        // The degraded step (OB-4); queued commands are dropped.
        let attempts = self.review.limits.nav_suspensions;
        self.exec(p.cmd, p.rep, attempts, Queue::new(), now, fx);
    }

    fn take_pending(&mut self, pick: impl Fn(What) -> bool) -> Option<Pending> {
        match &self.review.op {
            OpState::Pending(p) if pick(p.what) => {}
            OpState::Pending(_) | OpState::Idle => return None,
        }
        match std::mem::replace(&mut self.review.op, OpState::Idle) {
            OpState::Pending(p) => Some(*p),
            OpState::Idle => None,
        }
    }

    /// Continue a command whose fetch finished (or failed).
    fn resume(
        &mut self,
        p: Pending,
        err: Option<NavFetchError>,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        if self.superseded(&p) {
            return;
        }
        let attempts = match err {
            None => p.attempts,
            Some(e) => {
                if e == NavFetchError::Timeout {
                    self.review.counters.review_step_timeout += 1;
                }
                // No more fetching for this step: the degraded move (OB-4).
                self.review.limits.nav_suspensions
            }
        };
        self.exec(p.cmd, p.rep, attempts, p.queued, now, fx);
    }

    /// A result whose key press was followed by another one (any key advances the epoch) and with
    /// nothing queued is stale: it is dropped, never spoken (R3, ADR-0001 R-1).
    fn superseded(&mut self, p: &Pending) -> bool {
        let stale = p.queued.is_empty() && self.epoch.current() > p.epoch;
        if stale {
            self.review.counters.review_superseded += 1;
        }
        stale
    }

    /// OB-3: the step's whole budget is spent; nothing more is fetched for it or its queue.
    fn budget_spent(&self, now: Instant) -> bool {
        self.review
            .step_started
            .is_some_and(|s| now >= s + STEP_BUDGET)
    }

    fn exec(
        &mut self,
        rc: RCmd,
        rep: u8,
        attempts: u8,
        queued: Queue,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        // With commands waiting behind it, this one is superseded: it runs, but stays silent.
        let step = if queued.is_empty() {
            self.exec_inner(rc, rep, attempts, now, fx)
        } else {
            let mut tmp = ReaderEffects::new();
            let step = self.exec_inner(rc, rep, attempts, now, &mut tmp);
            fx.extend(
                tmp.into_iter()
                    .filter(|e| !matches!(e, ReaderEffect::Speak(_))),
            );
            step
        };
        match self.settle(step, (rc, rep, attempts), queued, now, fx) {
            None => {}
            Some(rest) => self.finish_queue(rest, now, fx),
        }
    }

    /// A finished step lets the queue run (`Some(queue)`); a suspended one keeps it.
    fn settle(
        &mut self,
        step: Step,
        (rc, rep, attempts): (RCmd, u8, u8),
        queued: Queue,
        now: Instant,
        fx: &mut ReaderEffects,
    ) -> Option<Queue> {
        match step {
            Step::Done => Some(queued),
            Step::Suspend { what, effect, wait } => {
                let mut deadline = now + wait + GRACE;
                if !matches!(what, What::Build(_))
                    && let Some(s) = self.review.step_started
                {
                    // All suspensions of one step share one budget (OB-3, F-2).
                    let end = (s + STEP_BUDGET).max(now + Duration::from_millis(1));
                    deadline = deadline.min(end);
                }
                self.review.op = OpState::Pending(Box::new(Pending {
                    what,
                    cmd: rc,
                    rep,
                    attempts: attempts.saturating_add(1),
                    deadline,
                    epoch: self.epoch.current(),
                    queued,
                }));
                fx.push(effect);
                fx.push(ReaderEffect::ArmTimer(deadline));
                None
            }
        }
    }

    /// Run the commands pressed while an operation was pending; only the last one speaks.
    fn finish_queue(&mut self, mut queued: Queue, now: Instant, fx: &mut ReaderEffects) {
        while !queued.is_empty() {
            let (c, r) = queued.remove(0);
            let last = queued.is_empty();
            let mut tmp = ReaderEffects::new();
            let step = self.exec_inner(c, r, 0, now, &mut tmp);
            for e in tmp {
                // Superseded results are never spoken (ADR-0001 R-1).
                if matches!(e, ReaderEffect::Speak(_)) && !last {
                    continue;
                }
                fx.push(e);
            }
            let rest = std::mem::take(&mut queued);
            match self.settle(step, (c, r, 0), rest, now, fx) {
                None => return,
                Some(q) => queued = q,
            }
        }
    }

    // ---- cursor helpers ---------------------------------------------------------------------

    pub(super) fn cursor(&self) -> Option<ReviewCursor> {
        match self.mode {
            Mode::Review { cursor, .. } => Some(cursor),
            Mode::Focus
            | Mode::Browse { .. }
            | Mode::PassThrough { .. }
            | Mode::InputHelp { .. } => None,
        }
    }

    fn set_cursor(&mut self, cursor: ReviewCursor) {
        // A cursor move is not a mode change: no epoch advance, no grab-set churn.
        if let Mode::Review { origin, .. } = self.mode {
            self.mode = Mode::Review { origin, cursor };
        }
    }

    /// The cursor, entering review at the focus when it is not active (ADR-0006 4.1).
    fn cursor_or_enter(&mut self, fx: &mut ReaderEffects) -> Option<ReviewCursor> {
        if let Some(c) = self.cursor() {
            return Some(c);
        }
        if matches!(self.mode, Mode::PassThrough { .. } | Mode::InputHelp { .. }) {
            return None;
        }
        let Some(node) = self.current_node() else {
            self.say(t(M::NoFocus), fx);
            return None;
        };
        let from = match self.mode {
            Mode::Browse { doc, sticky } => ReviewFrom::Browse { doc, sticky },
            Mode::Focus
            | Mode::Review { .. }
            | Mode::PassThrough { .. }
            | Mode::InputHelp { .. } => ReviewFrom::Focus,
        };
        let cursor = ReviewCursor::at(node, self.review.default_kind);
        self.on_mode(
            ModeMsg::EnterReview {
                origin: ReviewOrigin { node, from },
                cursor,
            },
            fx,
        );
        Some(cursor)
    }

    /// The navigator, or a live stand-in when it was removed (ADR-0006 13).
    fn live_nav(&mut self, cur: &ReviewCursor) -> Option<NodeKey> {
        match self.dir.view(cur.nav) {
            Lookup::Hit(_) | Lookup::Miss => Some(cur.nav),
            Lookup::Gone => {
                self.review.counters.review_nav_gone += 1;
                let origin = match self.mode {
                    Mode::Review { origin, .. } => Some(origin.node),
                    Mode::Focus
                    | Mode::Browse { .. }
                    | Mode::PassThrough { .. }
                    | Mode::InputHelp { .. } => None,
                };
                let alive = |k: &NodeKey| !matches!(self.dir.view(*k), Lookup::Gone);
                let stand_in = origin
                    .filter(alive)
                    .or_else(|| self.current_node().filter(alive));
                if let Some(k) = stand_in {
                    self.set_cursor(ReviewCursor::at(k, cur.kind()));
                }
                stand_in
            }
        }
    }

    /// Move the navigator (RC-1): the position follows it.
    fn move_nav(&mut self, k: NodeKey) {
        let Some(cur) = self.cursor() else { return };
        let pos = match cur.pos {
            ReviewPos::Object { .. } => ReviewPos::Object {
                offset: 0,
                text_line: None,
            },
            ReviewPos::Screen(_) => {
                let p = self
                    .review
                    .cur
                    .as_ref()
                    .filter(|m| m.app == k.app)
                    .and_then(|m| self.anchor_for(m, k));
                ReviewPos::Screen(p)
            }
        };
        self.set_cursor(ReviewCursor { nav: k, pos });
    }

    /// RC-1: the first zone of `k`, else of its nearest ancestor, else of its first descendant.
    fn anchor_for(&self, m: &LineModel, k: NodeKey) -> Option<FlatPos> {
        if let Some(p) = anchor_at_node(m, k) {
            return Some(p);
        }
        let mut cur = k;
        for _ in 0..16 {
            let Lookup::Hit(v) = self.dir.view(cur) else {
                break;
            };
            let Some(parent) = v.snap.parent else { break };
            if let Some(p) = anchor_at_node(m, parent) {
                return Some(p);
            }
            cur = parent;
        }
        let mut stack = vec![k];
        let mut seen = 0;
        while let Some(n) = stack.pop() {
            seen += 1;
            if seen > 64 {
                break;
            }
            if let Some(p) = anchor_at_node(m, n) {
                return Some(p);
            }
            if let Lookup::Hit(v) = self.dir.view(n)
                && let gorca_model::ChildList::Known(kids) = &v.snap.children
            {
                stack.extend(kids.iter().rev().copied());
            }
        }
        None
    }

    // ---- executing ---------------------------------------------------------------------------

    fn exec_inner(
        &mut self,
        rc: RCmd,
        rep: u8,
        attempts: u8,
        now: Instant,
        fx: &mut ReaderEffects,
    ) -> Step {
        let attempts = if self.budget_spent(now) {
            attempts.max(self.review.limits.nav_suspensions)
        } else {
            attempts
        };
        match rc {
            RCmd::Toggle => {
                if matches!(self.mode, Mode::Review { .. }) {
                    self.review.op = OpState::Idle;
                    self.on_mode(ModeMsg::ExitReview, fx);
                    self.say(t(M::ReviewOff), fx);
                } else if self.cursor_or_enter(fx).is_some() {
                    self.say(t(M::ReviewOn), fx);
                }
                Step::Done
            }
            RCmd::SimpleToggle => {
                self.review.simple = !self.review.simple;
                let t = if self.review.simple {
                    t(M::ReviewSimpleOn)
                } else {
                    t(M::ReviewSimpleOff)
                };
                self.say(t, fx);
                Step::Done
            }
            RCmd::FocusFollowsToggle => {
                self.review.focus_follows = !self.review.focus_follows;
                let t = if self.review.focus_follows {
                    t(M::ReviewFollowsFocusOn)
                } else {
                    t(M::ReviewFollowsFocusOff)
                };
                self.say(t, fx);
                Step::Done
            }
            RCmd::ToFocus => {
                self.exec_to_focus(fx);
                Step::Done
            }
            RCmd::MoveFocus => self.exec_act(ActStage::Focus, fx),
            RCmd::Activate => self.exec_act(ActStage::Activate, fx),
            RCmd::StatusBar => self.exec_status_bar(rep, attempts, fx),
            RCmd::ObjCurrent => {
                let Some(cur) = self.cursor_or_enter(fx) else {
                    return Step::Done;
                };
                let Some(nav) = self.live_nav(&cur) else {
                    self.exit_lost(fx);
                    return Step::Done;
                };
                self.speak_object(nav, rep, attempts, fx)
            }
            RCmd::Obj(mv) => self.exec_obj(mv, attempts, fx),
            RCmd::ModeSwitch(_) => self.exec_mode_switch(rc, rep, attempts, now, fx),
            RCmd::Flat(mv) => self.exec_flat(mv, rep, attempts, now, fx),
        }
    }

    fn exit_lost(&mut self, fx: &mut ReaderEffects) {
        self.say(t(M::ReviewPositionLost), fx);
        self.on_mode(ModeMsg::ExitReview, fx);
    }

    fn nav_suspend(&mut self, op: NavOp) -> Step {
        let req = NavReqId(self.review.id());
        Step::Suspend {
            what: What::Nav(req),
            effect: ReaderEffect::ReviewNav { req, op },
            wait: super::INTERACTIVE,
        }
    }

    /// Speak the object `k` as focus does; suspend when the mirror cannot give it.
    fn speak_object(&mut self, k: NodeKey, rep: u8, attempts: u8, fx: &mut ReaderEffects) -> Step {
        match self.dir.view(k) {
            Lookup::Hit(v) => {
                let epoch = self.epoch.current();
                if rep >= 2 {
                    let name = v.snap.name.as_deref().unwrap_or("");
                    if name.is_empty() {
                        self.say(&msg!(InfoNoName, role = v.snap.role.spoken()), fx);
                    } else {
                        let spelled = self.review_spell(name);
                        self.say(&spelled, fx);
                    }
                    return Step::Done;
                }
                let id = self.utt_id();
                let u = self
                    .presenters
                    .present(PresentContext::Focus, &v.snap, id, epoch);
                let mut u = u;
                u.may_refine |= v.aged || !v.stale.is_empty();
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                fx.push(ReaderEffect::Demand {
                    node: k,
                    why: gorca_mirror::fill::DemandWhy::NavStep,
                    outcome: gorca_mirror::fill::DemandOutcome::Hit,
                });
                Step::Done
            }
            Lookup::Miss => {
                if attempts >= self.review.limits.nav_suspensions {
                    self.say(t(M::ReviewNotResponding), fx);
                    Step::Done
                } else {
                    self.nav_suspend(NavOp::Node { node: k })
                }
            }
            Lookup::Gone => {
                self.say(t(M::ReviewObjectGone), fx);
                Step::Done
            }
        }
    }

    fn exec_obj(&mut self, mv: NavMove, attempts: u8, fx: &mut ReaderEffects) -> Step {
        let Some(cur) = self.cursor_or_enter(fx) else {
            return Step::Done;
        };
        let Some(nav) = self.live_nav(&cur) else {
            self.exit_lost(fx);
            return Step::Done;
        };
        let limits = self.review.limits;
        let result = if attempts >= limits.nav_suspensions {
            self.review.counters.objnav_degraded += 1;
            match step_degraded(&*self.dir, nav, mv, limits) {
                StepResult::Empty | StepResult::Stop(_) => {
                    self.say(t(M::ReviewNotResponding), fx);
                    return Step::Done;
                }
                found @ StepResult::Found(_) => found,
            }
        } else {
            step(&*self.dir, nav, mv, self.review.simple, limits)
        };
        match result {
            StepResult::Found(k) => {
                self.move_nav(k);
                self.speak_object(k, 1, attempts, fx)
            }
            StepResult::Empty => {
                self.say(mv.empty_text(), fx);
                Step::Done
            }
            StepResult::Stop(objnav_stop) => match objnav_stop {
                super::objnav::Stop::Miss(op) => self.nav_suspend(op),
                super::objnav::Stop::Cap(_) => {
                    self.review.counters.objnav_visit_cap += 1;
                    self.say(mv.empty_text(), fx);
                    Step::Done
                }
            },
        }
    }

    // ---- surfaces and models ----------------------------------------------------------------

    /// The surface that contains `nav` (ADR-0006 6.2), from the mirror alone.
    fn surface_of(&self, nav: NodeKey) -> Result<NodeKey, NavOp> {
        let mut cur = nav;
        let mut top_below_app = nav;
        for _ in 0..64 {
            let v = match self.dir.view(cur) {
                Lookup::Hit(v) => v,
                Lookup::Miss | Lookup::Gone => return Err(NavOp::Ancestry { node: nav }),
            };
            let parent_menubar = v.snap.parent.is_some_and(
                |p| matches!(self.dir.view(p), Lookup::Hit(pv) if pv.snap.role == Role::MenuBar),
            );
            if is_surface_role(&v.snap, parent_menubar) || v.snap.role == Role::Application {
                // For the Shell and other windowless trees, the container below the root.
                return Ok(if v.snap.role == Role::Application {
                    top_below_app
                } else {
                    cur
                });
            }
            top_below_app = cur;
            match v.snap.parent {
                Some(p) if v.snap.have.contains(PropMask::PARENT) => cur = p,
                Some(_) | None => {
                    return if v.snap.have.contains(PropMask::PARENT) {
                        Ok(cur)
                    } else {
                        Err(NavOp::Ancestry { node: nav })
                    };
                }
            }
        }
        Ok(top_below_app)
    }

    fn pick_model(&self, nav: NodeKey) -> Pick {
        // A terminal's model is true only while the tracked screen has the generation it was
        // built from (`term_review_model`); it has no zones to check, so served from here it
        // would stay "valid" for ever.
        let serves = |m: &Arc<LineModel>| {
            m.source != LineSource::Terminal
                && m.app == nav.app
                && (m.surface == nav || m.by_node.contains_key(&nav))
        };
        if let Some(m) = self.review.cur.as_ref().filter(|m| serves(m)) {
            return Pick::Ready(Arc::clone(m), true);
        }
        if let Some(m) = self.review.prev.as_ref().filter(|m| serves(m)) {
            return Pick::Ready(Arc::clone(m), false);
        }
        match self.surface_of(nav) {
            Ok(s) => {
                let of_surface =
                    |m: &&Arc<LineModel>| m.surface == s && m.source != LineSource::Terminal;
                if let Some(m) = self.review.cur.as_ref().filter(of_surface) {
                    return Pick::Ready(Arc::clone(m), true);
                }
                if let Some(m) = self.review.prev.as_ref().filter(of_surface) {
                    return Pick::Ready(Arc::clone(m), false);
                }
                if matches!(self.dir.view(s), Lookup::Gone) {
                    return Pick::Gone;
                }
                Pick::Build(s)
            }
            Err(op) => Pick::Ancestry(op),
        }
    }

    /// Wait for the terminal's screen read (`review_term_replied` resumes the step).
    fn term_wait(&mut self) -> Step {
        Step::Suspend {
            what: What::Term,
            effect: ReaderEffect::ArmTimer(self.now + TERM_WAIT + GRACE),
            wait: TERM_WAIT,
        }
    }

    /// The terminal's extension took an answer to a screen read: a review step that waited for it
    /// goes on.
    pub(crate) fn review_term_replied(&mut self, fx: &mut ReaderEffects) {
        let Some(p) = self.take_pending(|w| w == What::Term) else {
            self.review_term_answered(fx);
            return;
        };
        let now = self.now;
        self.resume(p, None, now, fx);
    }

    /// A terminal review key spoke from the held screen while the screen was being read again:
    /// when the answer comes, the spoken line is checked against it (RV-1).
    pub(crate) fn review_term_answered(&mut self, fx: &mut ReaderEffects) {
        if matches!(self.review.op, OpState::Idle) {
            self.term_review_refresh(fx);
        }
    }

    fn build_suspend(
        &mut self,
        surface: NodeKey,
        prev: Option<Arc<LineModel>>,
        dirty: Option<Arc<[NodeKey]>>,
    ) -> Step {
        let build = BuildId(self.review.id());
        self.review.counters.review_builds += 1;
        Step::Suspend {
            what: What::Build(build),
            effect: ReaderEffect::ReviewBuild(BuildRequest {
                build,
                surface,
                prev,
                dirty,
                interactive: true,
            }),
            wait: BUILD_WAIT,
        }
    }

    /// The position of `cur` in `m`: kept, re-anchored (RA-1) or anchored at the navigator.
    fn flat_pos(&self, m: &LineModel, cur: &ReviewCursor) -> Option<FlatPos> {
        if let ReviewPos::Screen(Some(p)) = cur.pos {
            if p.model == m.id {
                return Some(p);
            }
            if m.source == LineSource::Terminal {
                if let Some(Carried::At(q)) = carry_terminal(None, m, &p.anchor, p.offset, None) {
                    return Some(q);
                }
            } else if let Some(q) =
                reanchor(m, p.anchor).or_else(|| nearest_line(m, p.anchor.y_mid))
            {
                return Some(q);
            }
        }
        // A terminal starts at the line the caret is on.
        if m.source == LineSource::Terminal
            && let Some(l) = m.focus_line
            && let Some(p) = pos_at(m, l, 0, 0)
        {
            return Some(p);
        }
        self.anchor_for(m, cur.nav).or_else(|| pos_at(m, 0, 0, 0))
    }

    fn exec_flat(
        &mut self,
        mv: FlatMove,
        rep: u8,
        attempts: u8,
        now: Instant,
        fx: &mut ReaderEffects,
    ) -> Step {
        let Some(cur) = self.cursor_or_enter(fx) else {
            return Step::Done;
        };
        let Some(nav) = self.live_nav(&cur) else {
            self.exit_lost(fx);
            return Step::Done;
        };
        if let ReviewPos::Object { offset, .. } = cur.pos {
            return self.exec_object_text(&cur, nav, offset, mv, rep, attempts, fx);
        }
        // A terminal's screen is held by its extension: no build, no IPC (ADR-0006 LS-1). When
        // that screen may be missing output (events lost, or the application's events are being
        // dropped because it is not the protected one) or the terminal is not tracked at all, it
        // is read first and the step waits for it, once.
        // With a held screen the key speaks from it at once and the read it started corrects
        // the line afterwards if it was wrong (ADR-0018, ADR-0001 R3); only a terminal with no
        // screen at all is waited for.
        let mut speak_first = false;
        if attempts == 0 {
            if let Some(snap) = self.term_untracked_focus(nav) {
                self.term_adopt(&snap, fx);
                return self.term_wait();
            }
            if self.term_review_untrusted(nav, fx) {
                if self.term_snapshot(nav).is_none() {
                    return self.term_wait();
                }
                speak_first = true;
            }
        }
        let picked = match self.term_review_model(nav, now, fx) {
            Some(m) => Pick::Ready(m, true),
            None => self.pick_model(nav),
        };
        // Installing a rebuilt screen carried the cursor over: read it again.
        let cur = self.cursor().unwrap_or(cur);
        let (model, is_cur) = match picked {
            Pick::Ready(m, c) => (m, c),
            Pick::Build(s) => {
                if attempts >= self.review.limits.nav_suspensions {
                    self.say(t(M::ReviewUnavailable), fx);
                    return Step::Done;
                }
                return self.build_suspend(s, None, None);
            }
            Pick::Ancestry(op) => {
                if attempts >= self.review.limits.nav_suspensions {
                    self.say(t(M::ReviewUnavailable), fx);
                    return Step::Done;
                }
                return self.nav_suspend(op);
            }
            Pick::Gone => {
                self.review.counters.review_surface_gone += 1;
                self.on_mode(ModeMsg::ExitReview, fx);
                return Step::Done;
            }
        };
        let Some(handle) = self.dir.handle(model.app) else {
            self.review.counters.review_surface_gone += 1;
            self.on_mode(ModeMsg::ExitReview, fx);
            return Step::Done;
        };
        if model.source == LineSource::Terminal
            && let ReviewPos::Screen(Some(p)) = cur.pos
            && let Some(held) = self.review.held.clone()
            && held.id == p.model
            && held.surface == model.surface
        {
            self.exec_held(&held, &model, &cur, p, mv, rep, fx);
            return Step::Done;
        }
        // Validity on every step: O(1) when nothing was written, O(nodes) lock-free reads else.
        let checked = if is_cur { self.review.checked_rev } else { 0 };
        let (verdict, rev) = validity(&model, &handle, checked);
        if is_cur {
            self.review.checked_rev = rev;
        }
        let Some(pos) = self.flat_pos(&model, &cur) else {
            self.say(t(M::WordBlank), fx);
            return Step::Done;
        };
        let out = if model.source == LineSource::Terminal {
            flat_step(&model, &pos, mv, &TermUnits)
        } else {
            flat_step(&model, &pos, mv, &SimpleUnits)
        };
        if let Validity::Dirty(dirty) = &verdict {
            let target: SmallVec<[NodeKey; 8]> = model
                .line_zones(out.pos.line as usize)
                .iter()
                .map(|z| z.node)
                .collect();
            let on_target = target.iter().any(|n| dirty.contains(n));
            let many = dirty.len() * 100 > model.checks.len() * DIRTY_FULL_PERCENT;
            let list: Arc<[NodeKey]> = Arc::from(dirty.as_slice());
            // VD-3 wins over VD-2: when most of the surface changed (a flood, a page switch) the
            // old model is spoken from and rebuilt in the background.
            if on_target && !many && attempts == 0 {
                // VD-2: the line to read is not true any more; wait for the refresh.
                let d = if many { None } else { Some(list) };
                return self.build_suspend(model.surface, Some(Arc::clone(&model)), d);
            }
            if on_target {
                self.review.counters.review_stale_spoken += 1;
            }
            let d = if many { None } else { Some(list) };
            self.background_refresh(&model, d, now, fx);
        }
        // Speak: every unit is named like text read from a document, in the current language and
        // at the current level; a character is always named, punctuation included.
        let mut text = self.review_voice(mv, out.boundary, rep, &out.said);
        if out.boundary
            && out.said == t(M::ReviewBottom)
            && matches!(model.completeness, Completeness::Truncated(_))
        {
            t(M::ReviewBottomTruncated).clone_into(&mut text);
        }
        self.say(&text, fx);
        if out.boundary {
            self.set_cursor(ReviewCursor {
                nav: cur.nav,
                pos: ReviewPos::Screen(Some(pos)),
            });
        } else {
            // RC-2: the navigator follows what was just read.
            let node = model
                .zones
                .get(zone_global(&model, &out.pos))
                .map_or(nav, |z| z.node);
            self.set_cursor(ReviewCursor {
                nav: node,
                pos: ReviewPos::Screen(Some(out.pos)),
            });
        }
        // RR-1: geometry changes without events (scroll, resize) are caught after the fact.
        self.maybe_revalidate(&model, &out.pos, is_cur, now, fx);
        if speak_first && let Some(l) = model.lines.get(out.pos.line as usize) {
            // RV-1: the screen is being read again; a different line gets corrected.
            self.review.rr_check = Some(RrCheck {
                model: model.id,
                anchor: out.pos.anchor,
                text: Arc::clone(&l.text),
                epoch: self.epoch.current(),
            });
        }
        Step::Done
    }

    /// A terminal cursor on a line that scrolled off the screen (ADR-0018): word and character
    /// moves go on reading the line it was on; anything that leaves the line lands on the top
    /// row of the screen and says so.
    #[allow(clippy::too_many_arguments)]
    fn exec_held(
        &mut self,
        held: &Arc<LineModel>,
        screen: &Arc<LineModel>,
        cur: &ReviewCursor,
        p: FlatPos,
        mv: FlatMove,
        rep: u8,
        fx: &mut ReaderEffects,
    ) {
        let intra = !matches!(
            mv,
            FlatMove::PrevLine | FlatMove::NextLine | FlatMove::Top | FlatMove::Bottom
        );
        if intra {
            let out = flat_step(held, &p, mv, &TermUnits);
            if !out.boundary {
                let text = self.review_voice(mv, false, rep, &out.said);
                self.say(&text, fx);
                self.set_cursor(ReviewCursor {
                    nav: cur.nav,
                    pos: ReviewPos::Screen(Some(out.pos)),
                });
                return;
            }
        }
        self.review.held = None;
        let Some(top) = pos_at(screen, 0, 0, 0) else {
            self.say(t(M::WordBlank), fx);
            return;
        };
        let (pos, said, scrolled) = match mv {
            FlatMove::Top | FlatMove::Bottom => {
                let o = flat_step(screen, &top, mv, &TermUnits);
                (o.pos, o.said, false)
            }
            FlatMove::PrevLine
            | FlatMove::CurLine
            | FlatMove::NextLine
            | FlatMove::PrevWord
            | FlatMove::CurWord
            | FlatMove::NextWord
            | FlatMove::PrevChar
            | FlatMove::CurChar
            | FlatMove::NextChar
            | FlatMove::StartOfLine
            | FlatMove::EndOfLine => (top, line_text(screen, 0), true),
        };
        let line = self.review_voice(FlatMove::CurLine, false, rep, &said);
        let text = if scrolled {
            format!("{}, {line}", t(M::ReviewScrolled))
        } else {
            line
        };
        self.say(&text, fx);
        self.set_cursor(ReviewCursor {
            nav: cur.nav,
            pos: ReviewPos::Screen(Some(pos)),
        });
    }

    fn background_refresh(
        &mut self,
        model: &Arc<LineModel>,
        dirty: Option<Arc<[NodeKey]>>,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        if let Some((_, at)) = self.review.bg_build
            && now.saturating_duration_since(at) < Duration::from_secs(2)
        {
            return;
        }
        let build = BuildId(self.review.id());
        self.review.bg_build = Some((build, now));
        self.review.counters.review_builds += 1;
        fx.push(ReaderEffect::ReviewBuild(BuildRequest {
            build,
            surface: model.surface,
            prev: Some(Arc::clone(model)),
            dirty,
            interactive: false,
        }));
    }

    fn maybe_revalidate(
        &mut self,
        model: &Arc<LineModel>,
        pos: &FlatPos,
        is_cur: bool,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        if !is_cur || model.source == LineSource::Terminal {
            return;
        }
        let line = pos.line;
        let due = self
            .review
            .validated_at
            .is_none_or(|at| now.saturating_duration_since(at) > self.review.geometry_ttl);
        if !due {
            return;
        }
        self.review.validated_at = Some(now);
        // One probe per node, with the extents the model recorded for the *node* (a text zone's
        // own rectangle is its line's), compared by the task with a fresh read (F-5).
        let mut probe: Vec<ProbeItem> = Vec::new();
        for z in model.line_zones(line as usize) {
            if probe.len() >= 8 || probe.iter().any(|p| p.node == z.node) {
                continue;
            }
            probe.push(ProbeItem {
                node: z.node,
                rect: model.node_rect(z.node),
                lines: model.texts.get(&z.node).cloned(),
            });
        }
        if probe.is_empty() {
            return;
        }
        // What was just spoken, to correct it if the content turns out to be different (F-4).
        self.review.rr_check = Some(RrCheck {
            model: model.id,
            anchor: pos.anchor,
            text: Arc::clone(&model.lines[line as usize].text),
            epoch: self.epoch.current(),
        });
        fx.push(ReaderEffect::ReviewRevalidate {
            model: model.id,
            app: model.app,
            probe,
            clip: model.clip,
        });
    }

    fn review_revalidated(
        &mut self,
        model: ModelId,
        verdict: &RrVerdict,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let Some(cur) = self.review.cur.clone().filter(|m| m.id == model) else {
            return;
        };
        match verdict {
            RrVerdict::Same | RrVerdict::Failed => {
                self.review.validated_at = Some(now);
                self.review.rr_check = None;
            }
            RrVerdict::Changed { .. } => {
                // The fresh extents are in the mirror: the next validity check finds the model
                // dirty. Read the text again in the background.
                self.review.bg_build = None;
                self.background_refresh(&cur, None, now, fx);
            }
        }
    }

    fn on_built(
        &mut self,
        build: BuildId,
        result: Result<Arc<LineModel>, super::build::BuildError>,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let waiting =
            matches!(&self.review.op, OpState::Pending(p) if p.what == What::Build(build));
        if matches!(self.review.bg_build, Some((b, _)) if b == build) {
            self.review.bg_build = None;
        }
        let model = match result {
            Ok(m) => m,
            Err(e) => {
                self.review.counters.review_build_failed += 1;
                if waiting && let Some(p) = self.take_pending(|_| true) {
                    let text = match e {
                        super::build::BuildError::Hung | super::build::BuildError::Refused => {
                            t(M::ReviewNotResponding)
                        }
                        super::build::BuildError::Gone | super::build::BuildError::Internal => {
                            t(M::ReviewUnavailable)
                        }
                    };
                    self.say(text, fx);
                    self.finish_queue(p.queued, now, fx);
                }
                return;
            }
        };
        self.install_model(&model, now, fx);
        if waiting && let Some(p) = self.take_pending(|_| true) {
            if self.superseded(&p) {
                return;
            }
            self.exec(p.cmd, p.rep, p.attempts, p.queued, now, fx);
        }
    }

    /// RR-1 / RV-1: the line just spoken was read from a model that turned out to be out of date;
    /// when its text now differs and the user has not moved on, say the corrected line.
    fn correct_spoken_line(&mut self, model: &Arc<LineModel>, fx: &mut ReaderEffects) {
        let Some(chk) = self.review.rr_check.clone() else {
            return;
        };
        if chk.model == model.id || self.epoch.current() != chk.epoch {
            if self.epoch.current() != chk.epoch {
                self.review.rr_check = None;
            }
            return;
        }
        self.review.rr_check = None;
        if chk.anchor.node.app != model.app {
            return;
        }
        // A terminal is corrected from where its cursor went (`carry_terminal_cursor`), so the
        // line spoken and the cursor never disagree.
        let found = if model.source == LineSource::Terminal {
            match self.cursor().map(|c| c.pos) {
                Some(ReviewPos::Screen(Some(p))) if p.model == model.id => Some(p),
                Some(ReviewPos::Screen(_) | ReviewPos::Object { .. }) | None => None,
            }
        } else {
            reanchor(model, chk.anchor).or_else(|| nearest_line(model, chk.anchor.y_mid))
        };
        let Some(p) = found else {
            return;
        };
        let now_text = line_text(model, p.line);
        if now_text.as_str() != &*chk.text {
            self.review.counters.review_corrections += 1;
            let said = self.review_voice(FlatMove::CurLine, false, 1, &now_text);
            self.say(&said, fx);
        }
    }

    /// Make `model` the model of its surface and carry the cursor over (RA-1).
    pub(super) fn install_model(
        &mut self,
        model: &Arc<LineModel>,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let same_surface = self
            .review
            .cur
            .as_ref()
            .is_some_and(|c| c.surface == model.surface);
        // The screen the cursor was made on, for the scroll estimate of a terminal.
        let before = self.review.cur.clone().filter(|_| same_surface);
        let hint = self.review.scroll.rows_for(model.built_rev);
        if model.source == LineSource::Terminal {
            self.review.scroll = super::ScrollHint::fresh(model.built_rev >> 32);
        }
        if same_surface {
            // A refresh of the current surface replaces it.
        } else {
            self.review.prev = self.review.cur.take();
            self.review.held = None;
        }
        self.review.cur = Some(Arc::clone(model));
        self.review.checked_rev = 0;
        if model.source != LineSource::Terminal {
            self.correct_spoken_line(model, fx);
        }
        self.review.validated_at = Some(now);
        if model.completeness == Completeness::GeometryFallback
            && !self.review.told_fallback.contains(&model.surface)
        {
            self.review.told_fallback.truncate(3);
            self.review.told_fallback.push(model.surface);
            self.announce(t(M::ReviewLayoutUnavailable), fx);
        }
        if let Some(cur) = self.cursor()
            && cur.nav.app == model.app
            && let ReviewPos::Screen(Some(p)) = cur.pos
            && p.model != model.id
        {
            if model.source == LineSource::Terminal {
                self.carry_terminal_cursor(&cur, p, before.as_deref(), model, hint, now);
                // A line that left the screen is read from its held text: nothing to correct.
                if self.review.held.is_none() {
                    self.correct_spoken_line(model, fx);
                } else {
                    self.review.rr_check = None;
                }
                return;
            }
            let np = reanchor(model, p.anchor).or_else(|| nearest_line(model, p.anchor.y_mid));
            self.set_cursor(ReviewCursor {
                nav: cur.nav,
                pos: ReviewPos::Screen(np),
            });
        }
    }

    /// The cursor `cur` at `p` follows its line to the rebuilt screen `model` (ADR-0018): by
    /// content, keeping the offset; a line that left the screen is kept as a one-line model.
    fn carry_terminal_cursor(
        &mut self,
        cur: &ReviewCursor,
        p: FlatPos,
        before: Option<&LineModel>,
        model: &Arc<LineModel>,
        hint: Option<u32>,
        now: Instant,
    ) {
        let held = self.review.held.clone().filter(|h| h.id == p.model);
        let old = before.filter(|o| o.id == p.model);
        // A held line comes back only if its text is on the screen again.
        let back = held.is_none()
            || model
                .lines
                .iter()
                .any(|l| hash_line(&l.text) == p.anchor.line_hash);
        let carried = if back {
            carry_terminal(old, model, &p.anchor, p.offset, hint)
        } else {
            Some(Carried::Gone)
        };
        // A line spoken from a screen that may have been stale, and a re-read that shares nothing
        // with it: the screen changed under the user. The row is kept and its new line corrected.
        let carried = match carried {
            Some(Carried::Replaced) if self.review.rr_check.is_some() => {
                nearest_line(model, p.anchor.y_mid)
                    .and_then(|np| pos_at(model, np.line, 0, p.offset))
                    .map(Carried::At)
            }
            Some(Carried::Replaced) => Some(Carried::Gone),
            other => other,
        };
        let moved = match (&held, carried) {
            (_, None) => Some(None),
            (_, Some(Carried::At(np))) => {
                self.review.held = None;
                Some(Some(np))
            }
            // Still not on the screen: the cursor stays on the held line.
            (Some(_), Some(Carried::Gone | Carried::Replaced)) => None,
            (None, Some(Carried::Gone | Carried::Replaced)) => {
                let text = old
                    .and_then(|o| o.lines.get(p.line as usize))
                    .map(|l| Arc::clone(&l.text));
                match text {
                    Some(t) => {
                        let h = Arc::new(held_model(model.app, model.surface, &t, now));
                        let np = pos_at(&h, 0, 0, p.offset);
                        self.review.held = Some(h);
                        Some(np)
                    }
                    None => Some(nearest_line(model, p.anchor.y_mid)),
                }
            }
        };
        if let Some(np) = moved {
            self.set_cursor(ReviewCursor {
                nav: cur.nav,
                pos: ReviewPos::Screen(np),
            });
        }
    }

    /// Test probe of the state model: the line a terminal review cursor is on, the offset in
    /// it, and whether that exact text is a row of the current screen model.
    #[cfg(test)]
    pub(crate) fn review_probe(&self) -> Option<(String, u32, bool)> {
        let ReviewPos::Screen(Some(p)) = self.cursor()?.pos else {
            return None;
        };
        let cur = self.review.cur.as_ref()?;
        let m = [Some(cur), self.review.held.as_ref()]
            .into_iter()
            .flatten()
            .find(|m| m.id == p.model)?;
        if m.source != LineSource::Terminal {
            return None;
        }
        let text = m.lines.get(p.line as usize)?.text.to_string();
        let on_screen =
            cur.source == LineSource::Terminal && cur.lines.iter().any(|l| *l.text == *text);
        Some((text, p.offset, on_screen))
    }

    /// Test probe: is a review step suspended on a terminal's screen read?
    #[cfg(test)]
    pub(crate) fn review_waits_for_terminal(&self) -> bool {
        matches!(&self.review.op, OpState::Pending(p) if p.what == What::Term)
    }

    /// `text` spelled letter by letter, every character named in the current language.
    fn review_spell(&self, text: &str) -> String {
        self.text
            .verbalizer
            .spell(text, false, &self.verbalize_opts())
    }

    /// What is spoken for the unit a review move read: names it by the current language and
    /// punctuation level (a character is always named, VB-1), spells it when the command was
    /// repeated. Boundary words ("Top", "Bottom") pass untouched.
    pub(super) fn review_voice(&self, mv: FlatMove, boundary: bool, rep: u8, said: &str) -> String {
        if boundary {
            return said.to_owned();
        }
        let o = self.verbalize_opts();
        let v = &self.text.verbalizer;
        let how = match mv {
            FlatMove::PrevChar | FlatMove::CurChar | FlatMove::NextChar => How::Char,
            FlatMove::PrevWord | FlatMove::CurWord | FlatMove::NextWord => How::Word,
            FlatMove::PrevLine
            | FlatMove::CurLine
            | FlatMove::NextLine
            | FlatMove::Top
            | FlatMove::Bottom
            | FlatMove::StartOfLine
            | FlatMove::EndOfLine => How::Line,
        };
        let spelled = matches!(
            mv,
            FlatMove::CurLine | FlatMove::CurWord | FlatMove::CurChar
        ) && rep >= 2;
        if spelled {
            v.spell(said, rep >= 3, &o)
        } else {
            v.unit(said, how, &o)
        }
    }

    // ---- object review over the navigator's text -----------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn exec_object_text(
        &mut self,
        cur: &ReviewCursor,
        nav: NodeKey,
        offset: u32,
        mv: FlatMove,
        rep: u8,
        attempts: u8,
        fx: &mut ReaderEffects,
    ) -> Step {
        let snap = match self.dir.view(nav) {
            Lookup::Hit(v) => v.snap,
            Lookup::Miss => {
                if attempts >= self.review.limits.nav_suspensions {
                    self.say(t(M::ReviewNotResponding), fx);
                    return Step::Done;
                }
                return self.nav_suspend(NavOp::Node { node: nav });
            }
            Lookup::Gone => {
                self.say(t(M::ReviewObjectGone), fx);
                return Step::Done;
            }
        };
        let text = flat_zone_text(&snap);
        let units = SimpleUnits;
        let (new_offset, said, boundary) = match mv {
            FlatMove::CurLine => (offset, text.clone(), false),
            FlatMove::PrevLine | FlatMove::Top | FlatMove::StartOfLine => {
                let edge = matches!(mv, FlatMove::PrevLine);
                (
                    0,
                    if edge {
                        t(M::ReviewTop).to_owned()
                    } else {
                        text.clone()
                    },
                    edge,
                )
            }
            FlatMove::NextLine | FlatMove::Bottom | FlatMove::EndOfLine => {
                let edge = matches!(mv, FlatMove::NextLine);
                (
                    0,
                    if edge {
                        t(M::ReviewBottom).to_owned()
                    } else {
                        text.clone()
                    },
                    edge,
                )
            }
            FlatMove::CurWord | FlatMove::CurChar => {
                let unit = if mv == FlatMove::CurWord {
                    Unit::Word
                } else {
                    Unit::Char
                };
                let s = units
                    .at(&text, offset, unit)
                    .map(|r| slice(&text, &r))
                    .unwrap_or_default();
                (offset, s, false)
            }
            FlatMove::NextWord | FlatMove::PrevWord | FlatMove::NextChar | FlatMove::PrevChar => {
                let unit = if matches!(mv, FlatMove::NextWord | FlatMove::PrevWord) {
                    Unit::Word
                } else {
                    Unit::Char
                };
                let dir = if matches!(mv, FlatMove::NextWord | FlatMove::NextChar) {
                    Dir::Fwd
                } else {
                    Dir::Back
                };
                match units.next(&text, offset, unit, dir) {
                    Some(r) => {
                        let s = slice(&text, &r);
                        (r.start, s, false)
                    }
                    None => (
                        offset,
                        if dir == Dir::Fwd {
                            t(M::ReviewBottom).to_owned()
                        } else {
                            t(M::ReviewTop).to_owned()
                        },
                        true,
                    ),
                }
            }
        };
        let spoken = self.review_voice(mv, boundary, rep, &said);
        self.say(
            if spoken.is_empty() {
                t(M::WordBlank)
            } else {
                &spoken
            },
            fx,
        );
        self.set_cursor(ReviewCursor {
            nav: cur.nav,
            pos: ReviewPos::Object {
                offset: new_offset,
                text_line: None,
            },
        });
        Step::Done
    }

    fn exec_mode_switch(
        &mut self,
        rc: RCmd,
        rep: u8,
        attempts: u8,
        now: Instant,
        fx: &mut ReaderEffects,
    ) -> Step {
        let Some(cur) = self.cursor_or_enter(fx) else {
            return Step::Done;
        };
        // Object and Screen: next and previous both toggle (Document joins in M7).
        let kind = match cur.kind() {
            ReviewKind::Object => ReviewKind::Screen,
            ReviewKind::Screen => ReviewKind::Object,
        };
        let _ = rc;
        self.set_cursor(ReviewCursor::at(cur.nav, kind));
        match kind {
            ReviewKind::Screen => {
                let anchored = self
                    .review
                    .cur
                    .clone()
                    .filter(|m| m.app == cur.nav.app)
                    .and_then(|m| self.anchor_for(&m, cur.nav));
                if let Some(p) = anchored {
                    self.set_cursor(ReviewCursor {
                        nav: cur.nav,
                        pos: ReviewPos::Screen(Some(p)),
                    });
                }
                self.exec_flat(FlatMove::CurLine, rep, attempts, now, fx)
            }
            ReviewKind::Object => self.speak_object(cur.nav, 1, attempts, fx),
        }
    }

    // ---- focus and actions --------------------------------------------------------------------

    fn exec_to_focus(&mut self, fx: &mut ReaderEffects) {
        let node = self.current_node();
        self.review.op = OpState::Idle;
        if matches!(self.mode, Mode::Review { .. }) {
            self.on_mode(ModeMsg::ExitReview, fx);
        }
        let Some(node) = node else {
            self.say(t(M::NoFocus), fx);
            return;
        };
        match self.dir.view(node) {
            Lookup::Hit(v) => {
                let id = self.utt_id();
                let epoch = self.epoch.current();
                let u = self
                    .presenters
                    .present(PresentContext::Focus, &v.snap, id, epoch);
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            }
            Lookup::Miss | Lookup::Gone => self.say(t(M::ReviewFocus), fx),
        }
    }

    fn exec_act(&mut self, stage: ActStage, fx: &mut ReaderEffects) -> Step {
        let Some(cur) = self.cursor() else {
            self.say(t(M::ReviewNotActive), fx);
            return Step::Done;
        };
        let Some(nav) = self.live_nav(&cur) else {
            self.exit_lost(fx);
            return Step::Done;
        };
        let act = match stage {
            ActStage::Focus => ActionReq::Focus { node: nav },
            ActStage::Select => ActionReq::Select { node: nav },
            ActStage::Activate => ActionReq::DoDefault { node: nav },
        };
        let req = NavReqId(self.review.id());
        Step::Suspend {
            what: What::Act(req, stage),
            effect: ReaderEffect::ReviewAct { req, act },
            wait: super::INTERACTIVE,
        }
    }

    fn on_action_done(
        &mut self,
        stage: ActStage,
        result: Result<(), NavFetchError>,
        p: Pending,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        match (stage, result) {
            // Success is confirmed by an event, not by the reply (ADR-0006 7).
            (_, Ok(())) => {}
            (ActStage::Focus, Err(_)) => {
                // GTK4 refuses GrabFocus (F-4): try selecting the item in its parent.
                let queued = p.queued;
                self.exec_act_stage(ActStage::Select, queued, now, fx);
                return;
            }
            (ActStage::Select, Err(_)) => {
                self.review.counters.review_move_focus_unsupported += 1;
                self.say(t(M::ReviewCannotMoveFocus), fx);
            }
            (ActStage::Activate, Err(_)) => self.say(t(M::ReviewCannotActivate), fx),
        }
        self.finish_queue(p.queued, now, fx);
    }

    fn exec_act_stage(
        &mut self,
        stage: ActStage,
        queued: Queue,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let step = self.exec_act(stage, fx);
        if let Some(rest) = self.settle(step, (RCmd::MoveFocus, 1, 0), queued, now, fx) {
            self.finish_queue(rest, now, fx);
        }
    }

    // ---- status bar (ADR-0006 8) ----------------------------------------------------------

    fn exec_status_bar(&mut self, rep: u8, attempts: u8, fx: &mut ReaderEffects) -> Step {
        let Some(node) = self.current_node() else {
            self.say(t(M::NoFocus), fx);
            return Step::Done;
        };
        let surface = match self.surface_of(node) {
            Ok(s) => s,
            Err(op) => {
                if attempts >= self.review.limits.nav_suspensions {
                    self.status_bar_fallback(fx);
                    return Step::Done;
                }
                return self.nav_suspend(op);
            }
        };
        // 1. Mirror search, zero IPC.
        if let Some(bar) = self.find_status_bar_in_mirror(surface) {
            self.speak_status_bar(bar, rep, fx);
            return Step::Done;
        }
        if attempts == 0 {
            // 2. One Collection call.
            let req = NavReqId(self.review.id());
            return Step::Suspend {
                what: What::StatusBar(req),
                effect: ReaderEffect::ReviewStatusBar { req, root: surface },
                wait: super::INTERACTIVE,
            };
        }
        self.status_bar_fallback(fx);
        Step::Done
    }

    fn find_status_bar_in_mirror(&self, surface: NodeKey) -> Option<NodeKey> {
        let mut queue = std::collections::VecDeque::from([surface]);
        let mut seen = 0usize;
        while let Some(k) = queue.pop_front() {
            seen += 1;
            if seen > 2_048 {
                return None;
            }
            let Lookup::Hit(v) = self.dir.view(k) else {
                continue;
            };
            if v.snap.role == Role::StatusBar && v.snap.states.contains(StateSet::SHOWING) {
                return Some(k);
            }
            if let gorca_model::ChildList::Known(kids) = &v.snap.children {
                queue.extend(kids.iter().copied());
            }
        }
        None
    }

    fn speak_status_bar(&mut self, bar: NodeKey, rep: u8, fx: &mut ReaderEffects) {
        let mut parts: Vec<String> = Vec::new();
        if let Lookup::Hit(v) = self.dir.view(bar) {
            if let Some(n) = v.snap.name.as_deref().filter(|n| !n.is_empty()) {
                parts.push(n.to_owned());
            }
            let mut stack: Vec<NodeKey> = match &v.snap.children {
                gorca_model::ChildList::Known(k) => k.iter().rev().copied().collect(),
                gorca_model::ChildList::Count(_) | gorca_model::ChildList::Unknown => Vec::new(),
            };
            let mut seen = 0;
            while let Some(k) = stack.pop() {
                seen += 1;
                if seen > 64 {
                    break;
                }
                let Lookup::Hit(c) = self.dir.view(k) else {
                    continue;
                };
                if !c.snap.states.contains(StateSet::SHOWING) {
                    continue;
                }
                let t = flat_zone_text(&c.snap);
                let leaf =
                    !matches!(&c.snap.children, gorca_model::ChildList::Known(k) if !k.is_empty());
                if leaf && !t.is_empty() {
                    parts.push(t);
                }
                if let gorca_model::ChildList::Known(kids) = &c.snap.children {
                    stack.extend(kids.iter().rev().copied());
                }
            }
        }
        let text = if parts.is_empty() {
            t(M::ReviewStatusBar).to_owned()
        } else {
            parts.join(", ")
        };
        let text = if rep >= 2 {
            self.review_spell(&text)
        } else {
            text
        };
        self.say(&text, fx);
    }

    /// 3. The bottom line of the surface's model, else 4. nothing found.
    fn status_bar_fallback(&mut self, fx: &mut ReaderEffects) {
        let last = self
            .review
            .cur
            .as_ref()
            .filter(|m| {
                self.current_node()
                    .is_some_and(|n| m.app == n.app && !m.lines.is_empty())
            })
            .map(|m| {
                line_text(
                    m,
                    u32::try_from(m.lines.len().saturating_sub(1)).unwrap_or(0),
                )
            });
        match last {
            Some(t) => self.say(&t, fx),
            None => self.say(t(M::ReviewNoStatusBar), fx),
        }
    }
}

fn zone_global(m: &LineModel, p: &FlatPos) -> usize {
    m.lines
        .get(p.line as usize)
        .map_or(0, |l| l.zones.start as usize + usize::from(p.zone))
}
