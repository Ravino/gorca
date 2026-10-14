//! The review commands as Reader handlers (ADR-0006 4, 5, 6, 7, 8).
//!
//! A command either finishes in the same call (a mirror hit, a valid model: no IPC) or
//! **suspends**: it records a [`Pending`], sends one effect, and resumes when the reply arrives
//! (RS-1: at most one review operation is pending). Commands pressed meanwhile queue (at most 8)
//! and run after it; only the last one speaks. Nothing here awaits (R3).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_connector_api::ActionReq;
use gorca_model::{Lookup, NodeKey, NodeSnapshot, PropMask, Role, StateSet};
use smallvec::SmallVec;

use super::build::{NavFetchError, ProbeItem, RrVerdict};
use super::cursor::{BuildId, FlatPos, ModelId, NavReqId, ReviewCursor, ReviewFrom, ReviewKind};
use super::cursor::{ReviewOrigin, ReviewPos};
use super::flat::{FlatMove, anchor_at_node, flat_step, line_text, nearest_line, pos_at, reanchor};
use super::model::{Completeness, LineModel};
use super::objnav::{NavMove, NavOp, StepResult, step, step_degraded};
use super::segment::{Dir, SimpleUnits, Spell, TextUnits, Unit, char_text, slice, spell_letters};
use super::validity::{Validity, validity};
use super::{BuildRequest, OpState, ReviewMsg};
use crate::SpeechIntent;
use crate::commands::CoreCmd;
use crate::mode::{Mode, ModeMsg};
use crate::present::{PresentContext, flat_zone_text};
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// Safety margin on top of a task's own deadline before the Reader gives up waiting.
const GRACE: Duration = Duration::from_millis(40);
/// A model build gets this long before the Reader degrades (the task publishes at 120 ms).
const BUILD_WAIT: Duration = Duration::from_millis(500);
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
}

/// The one suspended review operation (RS-1).
#[derive(Debug)]
pub(crate) struct Pending {
    pub what: What,
    pub cmd: RCmd,
    pub rep: u8,
    pub attempts: u8,
    pub deadline: Instant,
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
        self.expire_late(now, fx);
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
    }

    /// Focus moved while review is active: review follows it by default (ADR-0006 4.1).
    pub(crate) fn review_focus_changed(&mut self, fx: &mut ReaderEffects) {
        if matches!(self.mode, Mode::Review { .. }) && self.review.focus_follows {
            self.review.op = OpState::Idle;
            self.on_mode(ModeMsg::ExitReview, fx);
        }
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
                let deadline = now + wait + GRACE;
                self.review.op = OpState::Pending(Box::new(Pending {
                    what,
                    cmd: rc,
                    rep,
                    attempts: attempts.saturating_add(1),
                    deadline,
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

    fn cursor(&self) -> Option<ReviewCursor> {
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
            self.say("no focus", fx);
            return None;
        };
        let from = match self.mode {
            Mode::Browse {
                doc,
                cursor,
                sticky,
            } => ReviewFrom::Browse {
                doc,
                cursor,
                sticky,
            },
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
        match rc {
            RCmd::Toggle => {
                if matches!(self.mode, Mode::Review { .. }) {
                    self.review.op = OpState::Idle;
                    self.on_mode(ModeMsg::ExitReview, fx);
                    self.say("review off", fx);
                } else if self.cursor_or_enter(fx).is_some() {
                    self.say("review on", fx);
                }
                Step::Done
            }
            RCmd::SimpleToggle => {
                self.review.simple = !self.review.simple;
                let t = if self.review.simple {
                    "simple review on"
                } else {
                    "simple review off"
                };
                self.say(t, fx);
                Step::Done
            }
            RCmd::FocusFollowsToggle => {
                self.review.focus_follows = !self.review.focus_follows;
                let t = if self.review.focus_follows {
                    "review follows focus on"
                } else {
                    "review follows focus off"
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
        self.say("review position lost", fx);
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
                        self.say(&format!("{}, no name", v.snap.role.spoken()), fx);
                    } else {
                        self.say(&spell_letters(name), fx);
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
                    self.say("not responding", fx);
                    Step::Done
                } else {
                    self.nav_suspend(NavOp::Node { node: k })
                }
            }
            Lookup::Gone => {
                self.say("object is gone", fx);
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
                    self.say("not responding", fx);
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
        let serves = |m: &Arc<LineModel>| {
            m.app == nav.app && (m.surface == nav || m.by_node.contains_key(&nav))
        };
        if let Some(m) = self.review.cur.as_ref().filter(|m| serves(m)) {
            return Pick::Ready(Arc::clone(m), true);
        }
        if let Some(m) = self.review.prev.as_ref().filter(|m| serves(m)) {
            return Pick::Ready(Arc::clone(m), false);
        }
        match self.surface_of(nav) {
            Ok(s) => {
                if let Some(m) = self.review.cur.as_ref().filter(|m| m.surface == s) {
                    return Pick::Ready(Arc::clone(m), true);
                }
                if let Some(m) = self.review.prev.as_ref().filter(|m| m.surface == s) {
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
            if let Some(q) = reanchor(m, p.anchor).or_else(|| nearest_line(m, p.anchor.y_mid)) {
                return Some(q);
            }
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
        let (model, is_cur) = match self.pick_model(nav) {
            Pick::Ready(m, c) => (m, c),
            Pick::Build(s) => {
                if attempts >= self.review.limits.nav_suspensions {
                    self.say("review unavailable", fx);
                    return Step::Done;
                }
                return self.build_suspend(s, None, None);
            }
            Pick::Ancestry(op) => {
                if attempts >= self.review.limits.nav_suspensions {
                    self.say("review unavailable", fx);
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
        // Validity on every step: O(1) when nothing was written, O(nodes) lock-free reads else.
        let checked = if is_cur { self.review.checked_rev } else { 0 };
        let (verdict, rev) = validity(&model, &handle, checked);
        if is_cur {
            self.review.checked_rev = rev;
        }
        let Some(pos) = self.flat_pos(&model, &cur) else {
            self.say("blank", fx);
            return Step::Done;
        };
        let units = SimpleUnits;
        let out = flat_step(&model, &pos, mv, &units);
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
        // Speak.
        let mut text = out.said.clone();
        if rep >= 2
            && matches!(
                mv,
                FlatMove::CurLine | FlatMove::CurWord | FlatMove::CurChar
            )
        {
            let how = match (rep, mv) {
                (3.., FlatMove::CurChar) => Spell::CodePoint,
                (3.., _) => Spell::Phonetic,
                _ => Spell::Letters,
            };
            text = units.spell(&text, how);
        }
        if out.boundary
            && out.said == "Bottom"
            && matches!(model.completeness, Completeness::Truncated(_))
        {
            "Bottom, more content not shown".clone_into(&mut text);
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
        self.maybe_revalidate(&model, out.pos.line, is_cur, now, fx);
        Step::Done
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
        line: u32,
        is_cur: bool,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        if !is_cur {
            return;
        }
        let due = self
            .review
            .validated_at
            .is_none_or(|at| now.saturating_duration_since(at) > self.review.geometry_ttl);
        if !due {
            return;
        }
        self.review.validated_at = Some(now);
        let probe: Vec<ProbeItem> = model
            .line_zones(line as usize)
            .iter()
            .take(8)
            .map(|z| ProbeItem {
                node: z.node,
                rect: z.rect,
                lines: model.texts.get(&z.node).cloned(),
            })
            .collect();
        if probe.is_empty() {
            return;
        }
        let clip = model
            .zones
            .iter()
            .find_map(|z| z.rect)
            .map(|_| gorca_model::Rect {
                x: -10_000,
                y: -10_000,
                w: 20_000,
                h: 20_000,
            })
            .unwrap_or_default();
        fx.push(ReaderEffect::ReviewRevalidate {
            model: model.id,
            app: model.app,
            probe,
            clip,
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
                            "not responding"
                        }
                        super::build::BuildError::Gone | super::build::BuildError::Internal => {
                            "review unavailable"
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
            self.exec(p.cmd, p.rep, p.attempts, p.queued, now, fx);
        }
    }

    /// Make `model` the model of its surface and carry the cursor over (RA-1).
    fn install_model(&mut self, model: &Arc<LineModel>, now: Instant, fx: &mut ReaderEffects) {
        let same_surface = self
            .review
            .cur
            .as_ref()
            .is_some_and(|c| c.surface == model.surface);
        if same_surface {
            // A refresh of the current surface replaces it.
        } else {
            self.review.prev = self.review.cur.take();
        }
        self.review.cur = Some(Arc::clone(model));
        self.review.checked_rev = 0;
        self.review.validated_at = Some(now);
        if model.completeness == Completeness::GeometryFallback
            && !self.review.told_fallback.contains(&model.surface)
        {
            self.review.told_fallback.truncate(3);
            self.review.told_fallback.push(model.surface);
            self.announce("layout unavailable", fx);
        }
        if let Some(cur) = self.cursor()
            && cur.nav.app == model.app
            && let ReviewPos::Screen(Some(p)) = cur.pos
            && p.model != model.id
        {
            let np = reanchor(model, p.anchor).or_else(|| nearest_line(model, p.anchor.y_mid));
            self.set_cursor(ReviewCursor {
                nav: cur.nav,
                pos: ReviewPos::Screen(np),
            });
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
                    self.say("not responding", fx);
                    return Step::Done;
                }
                return self.nav_suspend(NavOp::Node { node: nav });
            }
            Lookup::Gone => {
                self.say("object is gone", fx);
                return Step::Done;
            }
        };
        let text = flat_zone_text(&snap);
        let units = SimpleUnits;
        let (new_offset, said, _boundary) = match mv {
            FlatMove::CurLine => (offset, text.clone(), false),
            FlatMove::PrevLine | FlatMove::Top | FlatMove::StartOfLine => {
                let edge = matches!(mv, FlatMove::PrevLine);
                (0, if edge { "Top".to_owned() } else { text.clone() }, edge)
            }
            FlatMove::NextLine | FlatMove::Bottom | FlatMove::EndOfLine => {
                let edge = matches!(mv, FlatMove::NextLine);
                (
                    0,
                    if edge {
                        "Bottom".to_owned()
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
                (
                    offset,
                    if unit == Unit::Char {
                        char_text(&s)
                    } else if s.is_empty() {
                        "blank".to_owned()
                    } else {
                        s
                    },
                    false,
                )
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
                        (
                            r.start,
                            if unit == Unit::Char { char_text(&s) } else { s },
                            false,
                        )
                    }
                    None => (
                        offset,
                        if dir == Dir::Fwd {
                            "Bottom".to_owned()
                        } else {
                            "Top".to_owned()
                        },
                        true,
                    ),
                }
            }
        };
        let spoken = if rep >= 2
            && matches!(
                mv,
                FlatMove::CurLine | FlatMove::CurWord | FlatMove::CurChar
            ) {
            spell_letters(&said)
        } else {
            said
        };
        self.say(if spoken.is_empty() { "blank" } else { &spoken }, fx);
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
            self.say("no focus", fx);
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
            Lookup::Miss | Lookup::Gone => self.say("focus", fx),
        }
    }

    fn exec_act(&mut self, stage: ActStage, fx: &mut ReaderEffects) -> Step {
        let Some(cur) = self.cursor() else {
            self.say("review is not active", fx);
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
                self.say("cannot move focus here", fx);
            }
            (ActStage::Activate, Err(_)) => self.say("cannot activate", fx),
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
            self.say("no focus", fx);
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
            "status bar".to_owned()
        } else {
            parts.join(", ")
        };
        let text = if rep >= 2 { spell_letters(&text) } else { text };
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
            None => self.say("no status bar found", fx),
        }
    }
}

fn zone_global(m: &LineModel, p: &FlatPos) -> usize {
    m.lines
        .get(p.line as usize)
        .map_or(0, |l| l.zones.start as usize + usize::from(p.zone))
}
