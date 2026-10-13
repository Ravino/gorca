//! The shared fill driver (B-1 SPEC 4.5): identical for every policy. It owns the policy, feeds it
//! inputs, turns its `Request` effects into bounded requests (in-flight limits per class, rate
//! limit, node and byte caps, de-duplication, health gate, `CacheDump` eligibility), applies
//! replies to the mirror with the late-reply rules, and tells the policy how each request ended.
//!
//! The driver is synchronous and performs no I/O: the app actor's async shell sends the
//! [`IssuedFill`]s it returns to the connector (under their deadlines) and feeds the replies
//! back through [`FillDriver::on_reply`]. It knows no connector type.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gorca_model::{AppId, NodeKey, SnapshotBatch};
use gorca_rt::BudgetClass;

use crate::fill::{
    AppClass, FillCtx, FillEffect, FillEffects, FillInput, FillOp, FillPolicy, FillReqId,
    FillResult, HealthView, RefuseReason, SlotView,
};
use crate::limits::FillLimits;
use crate::store::{ApplyOrigin, ApplyOutcome, MirrorRead, MirrorWriter, RemovalCause};

#[cfg(test)]
mod tests;

/// Policy steps while the app has work run at most this often (`Tick`).
pub const TICK_INTERVAL: Duration = Duration::from_millis(50);
/// A queued Prefetch/Background request older than this is dropped unsent.
const QUEUE_EXPIRY: Duration = Duration::from_millis(1_000);
/// Refusal notices delivered to the policy per top-level call; a policy that reacts to a refusal
/// with the same request cannot loop.
const MAX_NOTICES: usize = 64;

/// How a request ended, as the connector reported it (no connector type here).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FillError {
    /// `UnknownObject` / `Gone`: authoritative, the key is tombstoned.
    Gone,
    /// The connector has no path for the key: a miss, never `Gone`.
    Unresolved,
    Timeout,
    Busy,
    NotSupported,
    Protocol,
}

#[derive(Debug)]
pub enum FillReply {
    Ok(SnapshotBatch),
    Err(FillError),
}

/// A request the async shell must send.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssuedFill {
    pub id: FillReqId,
    pub op: FillOp,
    pub class: BudgetClass,
    /// The mirror write sequence at issue: the reply is applied against it (LR-1..3).
    pub issued_at: u64,
    pub deadline: Instant,
}

/// What the app actor must do about a reply's health implications.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HealthSignal {
    None,
    /// The app answered.
    Success,
    Timeout(BudgetClass),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReplyReport {
    pub health: HealthSignal,
    pub outcome: Option<ApplyOutcome>,
}

impl ReplyReport {
    const NONE: ReplyReport = ReplyReport {
        health: HealthSignal::None,
        outcome: None,
    };
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FillStats {
    pub issued: u64,
    pub deduped: u64,
    pub queued: u64,
    pub cancelled: u64,
    pub replies_ok: u64,
    pub replies_err: u64,
    pub refused: [u64; RefuseReason::COUNT],
    /// Notices not delivered to the policy because of [`MAX_NOTICES`].
    pub notices_dropped: u64,
    pub nodes_applied: u64,
    pub dropped_tombstoned: u64,
    pub dropped_foreign: u64,
    pub superseded: u64,
    pub kept_stale: u64,
    pub evicted: u64,
    /// A dump that came back implausible cleared the eligibility flag for good.
    pub dump_flag_cleared: u64,
    /// `memory_bytes()` of the policy exceeded `FILL_STATE_MAX_BYTES`.
    pub policy_over_budget: u64,
}

impl FillStats {
    pub fn refused_total(&self) -> u64 {
        self.refused.iter().sum()
    }
}

/// All-apps fill-rate cap (calls per second), shared by every driver.
#[derive(Debug)]
pub struct GlobalRate {
    start: Instant,
    per_window: u64,
    /// `(window index << 32) | calls in the window`.
    state: AtomicU64,
}

const WINDOW: Duration = Duration::from_millis(100);

impl GlobalRate {
    pub fn new(per_second: u32) -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            per_window: u64::from(per_second / 10).max(1),
            state: AtomicU64::new(0),
        })
    }

    pub fn try_take(&self, now: Instant) -> bool {
        let w = u64::try_from(
            now.saturating_duration_since(self.start).as_millis() / WINDOW.as_millis(),
        )
        .unwrap_or(u64::MAX);
        let mut cur = self.state.load(Ordering::Relaxed);
        loop {
            let (cw, n) = (cur >> 32, cur & 0xffff_ffff);
            let next = if cw != w {
                (w << 32) | 1
            } else if n >= self.per_window {
                return false;
            } else {
                cur + 1
            };
            match self
                .state
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(seen) => cur = seen,
            }
        }
    }
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Option<Instant>,
}

impl Bucket {
    fn refill(&mut self, now: Instant, rate: u32) {
        let burst = f64::from((rate / 10).max(1));
        match self.last {
            None => self.tokens = burst,
            Some(t) => {
                let dt = now.saturating_duration_since(t).as_secs_f64();
                self.tokens = (self.tokens + dt * f64::from(rate)).min(burst);
            }
        }
        self.last = Some(now);
    }

    fn take(&mut self, now: Instant, rate: u32) -> bool {
        self.refill(now, rate);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[derive(Debug)]
struct Flight {
    op: FillOp,
    class: BudgetClass,
    cancelled: bool,
}

#[derive(Debug)]
struct Queued {
    id: FillReqId,
    op: FillOp,
    class: BudgetClass,
    at: Instant,
}

/// What the driver reads from the app actor on every call.
#[derive(Debug)]
pub struct DriverCx<'a> {
    pub now: Instant,
    pub health: HealthView,
    pub mirror: &'a dyn MirrorRead,
    /// The Reader's settled focus if it is in this app.
    pub focus: Option<NodeKey>,
}

fn slot_index(c: BudgetClass) -> usize {
    match c {
        BudgetClass::Interactive => 0,
        BudgetClass::Prefetch => 1,
        BudgetClass::Background | BudgetClass::Startup => 2,
    }
}

/// `Startup` is a connection class, not a fill class.
fn canonical(c: BudgetClass) -> BudgetClass {
    match c {
        BudgetClass::Startup => BudgetClass::Background,
        BudgetClass::Interactive | BudgetClass::Prefetch | BudgetClass::Background => c,
    }
}

pub struct FillDriver {
    app: AppId,
    class: AppClass,
    policy: Box<dyn FillPolicy>,
    limits: FillLimits,
    dump_complete: bool,
    next_id: u64,
    inflight: HashMap<FillReqId, Flight>,
    active_ops: HashSet<FillOp>,
    n_inflight: [usize; 3],
    queue: VecDeque<Queued>,
    bucket: Bucket,
    global: Option<Arc<GlobalRate>>,
    last_dump: Option<Instant>,
    wake_at: Option<Instant>,
    last_tick: Option<Instant>,
    outbox: Vec<IssuedFill>,
    stats: FillStats,
}

impl std::fmt::Debug for FillDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FillDriver")
            .field("app", &self.app)
            .field("policy", &self.policy.name())
            .field("inflight", &self.inflight.len())
            .field("queued", &self.queue.len())
            .finish_non_exhaustive()
    }
}

impl FillDriver {
    pub fn new(
        app: AppId,
        class: AppClass,
        policy: Box<dyn FillPolicy>,
        limits: FillLimits,
        global: Option<Arc<GlobalRate>>,
    ) -> Self {
        Self {
            app,
            class,
            policy,
            limits,
            dump_complete: false,
            next_id: 1,
            inflight: HashMap::new(),
            active_ops: HashSet::new(),
            n_inflight: [0; 3],
            queue: VecDeque::new(),
            bucket: Bucket {
                tokens: 0.0,
                last: None,
            },
            global,
            last_dump: None,
            wake_at: None,
            last_tick: None,
            outbox: Vec::new(),
            stats: FillStats::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_policy_for_test(&mut self, p: Box<dyn FillPolicy>) {
        self.policy = p;
    }

    pub fn policy_name(&self) -> &'static str {
        self.policy.name()
    }

    pub fn policy_memory_bytes(&self) -> usize {
        self.policy.memory_bytes()
    }

    pub fn stats(&self) -> FillStats {
        self.stats
    }

    pub fn set_class(&mut self, class: AppClass) {
        self.class = class;
    }

    /// The connector's verified eligibility for `CacheDump` (B-1 2.2).
    pub fn set_dump_complete(&mut self, complete: bool) {
        self.dump_complete = complete;
    }

    pub fn dump_complete(&self) -> bool {
        self.dump_complete
    }

    pub fn in_flight(&self) -> usize {
        self.n_inflight.iter().sum()
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn has_work(&self) -> bool {
        !self.inflight.is_empty() || !self.queue.is_empty()
    }

    /// When the shell should call [`FillDriver::poll_timers`] next.
    pub fn next_wake(&self) -> Option<Instant> {
        let tick = if self.has_work() {
            Some(
                self.last_tick
                    .map_or_else(Instant::now, |t| t + TICK_INTERVAL),
            )
        } else {
            None
        };
        match (self.wake_at, tick) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Requests to send now. Cheap when empty.
    pub fn take_issued(&mut self) -> Vec<IssuedFill> {
        std::mem::take(&mut self.outbox)
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn slot_view(&mut self, cx: &DriverCx<'_>) -> SlotView {
        let total = self.in_flight();
        let max = self.limits.max_in_flight;
        let shared = max.saturating_sub(self.limits.interactive_reserved);
        let rate = self.rate(cx);
        self.bucket.refill(cx.now, rate);
        let clamp = |n: usize| u8::try_from(n).unwrap_or(u8::MAX);
        SlotView {
            interactive: clamp(max.saturating_sub(total)),
            prefetch: clamp(shared.saturating_sub(total)),
            background: clamp(
                shared.saturating_sub(total).min(
                    self.limits
                        .max_background
                        .saturating_sub(self.n_inflight[2]),
                ),
            ),
            tokens: self.bucket.tokens as u32,
        }
    }

    fn rate(&self, cx: &DriverCx<'_>) -> u32 {
        if cx.focus.is_some() {
            self.limits.rate_focused
        } else {
            self.limits.rate_non_focused
        }
    }

    /// Feed one input to the policy and execute its effects.
    pub fn on_input(&mut self, cx: &DriverCx<'_>, writer: &mut MirrorWriter, input: FillInput<'_>) {
        if matches!(cx.health, HealthView::Gone) {
            return;
        }
        let mut notices = VecDeque::new();
        let effects = self.step(cx, input);
        self.apply_effects(cx, writer, effects, &mut notices);
        self.deliver_notices(cx, writer, &mut notices);
        self.start_queued(cx, writer);
    }

    /// Run `Tick` / wake-ups that are due. Call when `next_wake()` has passed.
    pub fn poll_timers(&mut self, cx: &DriverCx<'_>, writer: &mut MirrorWriter) {
        let due = self.wake_at.is_some_and(|t| t <= cx.now)
            || (self.has_work()
                && self
                    .last_tick
                    .is_none_or(|t| cx.now.saturating_duration_since(t) >= TICK_INTERVAL));
        if due {
            self.wake_at = self.wake_at.filter(|t| *t > cx.now);
            self.last_tick = Some(cx.now);
            self.on_input(cx, writer, FillInput::Tick);
        }
        self.expire_queue(cx.now);
    }

    fn step(&mut self, cx: &DriverCx<'_>, input: FillInput<'_>) -> FillEffects {
        let slots = self.slot_view(cx);
        let mut out = FillEffects::new();
        let fctx = FillCtx {
            now: cx.now,
            app: self.app,
            app_class: self.class,
            health: cx.health,
            dump_complete: self.dump_complete,
            mirror: cx.mirror,
            focus: cx.focus,
            slots,
            limits: &self.limits,
        };
        self.policy.step(&fctx, input, &mut out);
        if self.policy.memory_bytes() > self.limits.state_max_bytes {
            self.stats.policy_over_budget += 1;
        }
        out
    }

    fn deliver_notices(
        &mut self,
        cx: &DriverCx<'_>,
        writer: &mut MirrorWriter,
        notices: &mut VecDeque<(FillReqId, FillOp, RefuseReason)>,
    ) {
        let mut budget = MAX_NOTICES;
        while let Some((id, op, why)) = notices.pop_front() {
            if budget == 0 {
                self.stats.notices_dropped += 1 + notices.len() as u64;
                notices.clear();
                break;
            }
            budget -= 1;
            let effects = self.step(
                cx,
                FillInput::Reply {
                    id,
                    op: &op,
                    result: FillResult::Refused(why),
                },
            );
            self.apply_effects(cx, writer, effects, notices);
        }
    }

    fn apply_effects(
        &mut self,
        cx: &DriverCx<'_>,
        writer: &mut MirrorWriter,
        effects: FillEffects,
        notices: &mut VecDeque<(FillReqId, FillOp, RefuseReason)>,
    ) {
        for e in effects {
            match e {
                FillEffect::Request { op, class } => {
                    let id = FillReqId(self.next_id);
                    self.next_id += 1;
                    let op = self.sanitize(op);
                    let class = if matches!(op, FillOp::CacheDump { .. }) {
                        BudgetClass::Background
                    } else {
                        canonical(class)
                    };
                    if let Err(why) = self.admit(cx, writer, id, &op, class) {
                        self.stats.refused[why.index()] += 1;
                        notices.push_back((id, op, why));
                    }
                }
                FillEffect::Cancel(id) => self.cancel(id),
                FillEffect::Evict(k) => {
                    if cx.focus != Some(k) && !writer.is_protected(k) {
                        writer.evict(k);
                    }
                }
                FillEffect::WakeAt(t) => {
                    self.wake_at = Some(self.wake_at.map_or(t, |w| w.min(t)));
                }
            }
        }
    }

    /// Clamp an op to the hard bounds of the section 7 table.
    fn sanitize(&self, op: FillOp) -> FillOp {
        let l = &self.limits;
        match op {
            FillOp::ChildSnapshots { node, range, want } => {
                let end = range.end.min(range.start.saturating_add(l.max_child_range));
                FillOp::ChildSnapshots {
                    node,
                    range: range.start..end.max(range.start),
                    want,
                }
            }
            FillOp::Ancestry {
                node,
                max_depth,
                want,
            } => FillOp::Ancestry {
                node,
                max_depth: max_depth.min(l.max_ancestry_depth),
                want,
            },
            FillOp::Subtree { root, max_nodes } => FillOp::Subtree {
                root,
                max_nodes: max_nodes.min(l.max_subtree_nodes),
            },
            FillOp::CacheDump { max_nodes } => FillOp::CacheDump {
                max_nodes: max_nodes.min(l.max_dump_nodes),
            },
            f @ FillOp::Fetch { .. } => f,
        }
    }

    fn has_slot(&self, class: BudgetClass) -> bool {
        let total = self.in_flight();
        let shared = self
            .limits
            .max_in_flight
            .saturating_sub(self.limits.interactive_reserved);
        match class {
            BudgetClass::Interactive => total < self.limits.max_in_flight,
            BudgetClass::Prefetch => total < shared,
            BudgetClass::Background | BudgetClass::Startup => {
                total < shared && self.n_inflight[2] < self.limits.max_background
            }
        }
    }

    fn admit(
        &mut self,
        cx: &DriverCx<'_>,
        writer: &MirrorWriter,
        id: FillReqId,
        op: &FillOp,
        class: BudgetClass,
    ) -> Result<(), RefuseReason> {
        if self.active_ops.contains(op) {
            self.stats.deduped += 1;
            return Ok(());
        }
        let interactive = matches!(class, BudgetClass::Interactive);
        match cx.health {
            HealthView::Hung | HealthView::Gone => return Err(RefuseReason::Health),
            HealthView::Slow if !interactive => return Err(RefuseReason::Health),
            HealthView::Flooded if matches!(class, BudgetClass::Background) => {
                return Err(RefuseReason::Health);
            }
            HealthView::Healthy | HealthView::Slow | HealthView::Flooded => {}
        }
        if matches!(op, FillOp::CacheDump { .. }) {
            if !self.dump_complete {
                return Err(RefuseReason::DumpIneligible);
            }
            if self
                .last_dump
                .is_some_and(|t| cx.now.saturating_duration_since(t) < self.limits.dump_interval)
            {
                return Err(RefuseReason::DumpTooSoon);
            }
        }
        if !interactive {
            let st = cx.mirror.stats();
            if st.live >= self.limits.max_nodes as u64 || st.bytes >= self.limits.max_bytes {
                return Err(RefuseReason::NodeCap);
            }
            let rate = self.rate(cx);
            if !self.bucket.take(cx.now, rate)
                || self.global.as_ref().is_some_and(|g| !g.try_take(cx.now))
            {
                return Err(RefuseReason::RateLimit);
            }
        }
        if self.has_slot(class) {
            self.issue(cx, writer, id, op.clone(), class);
            Ok(())
        } else if interactive {
            Err(RefuseReason::NoSlot)
        } else if self.queue.len() >= self.limits.max_pending {
            Err(RefuseReason::QueueFull)
        } else {
            self.active_ops.insert(op.clone());
            self.queue.push_back(Queued {
                id,
                op: op.clone(),
                class,
                at: cx.now,
            });
            self.stats.queued += 1;
            Ok(())
        }
    }

    fn issue(
        &mut self,
        cx: &DriverCx<'_>,
        writer: &MirrorWriter,
        id: FillReqId,
        op: FillOp,
        class: BudgetClass,
    ) {
        if matches!(op, FillOp::CacheDump { .. }) {
            self.last_dump = Some(cx.now);
        }
        self.n_inflight[slot_index(class)] += 1;
        self.active_ops.insert(op.clone());
        self.outbox.push(IssuedFill {
            id,
            op: op.clone(),
            class,
            issued_at: writer.seq(),
            deadline: cx.now + class.default_timeout(),
        });
        self.inflight.insert(
            id,
            Flight {
                op,
                class,
                cancelled: false,
            },
        );
        self.stats.issued += 1;
    }

    fn cancel(&mut self, id: FillReqId) {
        if let Some(pos) = self.queue.iter().position(|q| q.id == id)
            && let Some(q) = self.queue.remove(pos)
        {
            self.active_ops.remove(&q.op);
            self.stats.cancelled += 1;
        } else if let Some(f) = self.inflight.get_mut(&id) {
            f.cancelled = true;
            self.stats.cancelled += 1;
        }
    }

    fn expire_queue(&mut self, now: Instant) {
        while let Some(front) = self.queue.front() {
            if now.saturating_duration_since(front.at) < QUEUE_EXPIRY {
                break;
            }
            if let Some(q) = self.queue.pop_front() {
                self.active_ops.remove(&q.op);
                self.stats.refused[RefuseReason::Expired.index()] += 1;
            }
        }
    }

    /// Move queued requests into free slots.
    fn start_queued(&mut self, cx: &DriverCx<'_>, writer: &MirrorWriter) {
        self.expire_queue(cx.now);
        let mut i = 0;
        while i < self.queue.len() {
            let class = self.queue[i].class;
            if self.has_slot(class) {
                if let Some(q) = self.queue.remove(i) {
                    self.active_ops.remove(&q.op);
                    self.issue(cx, writer, q.id, q.op, q.class);
                }
            } else {
                i += 1;
            }
        }
    }

    /// A reply to a request this driver issued. Applies it to the mirror with the late-reply
    /// rules, tells the policy, and starts queued work. The caller feeds the returned health
    /// signal to the app's health machine.
    pub fn on_reply(
        &mut self,
        cx: &DriverCx<'_>,
        writer: &mut MirrorWriter,
        id: FillReqId,
        issued_at: u64,
        reply: FillReply,
    ) -> ReplyReport {
        let Some(flight) = self.inflight.remove(&id) else {
            return ReplyReport::NONE;
        };
        self.active_ops.remove(&flight.op);
        let slot = slot_index(flight.class);
        self.n_inflight[slot] = self.n_inflight[slot].saturating_sub(1);
        let mut keys: Vec<NodeKey> = Vec::new();
        let mut nodes = 0usize;
        let (report, result) = match reply {
            FillReply::Ok(mut batch) => {
                Self::cap_batch(&flight.op, &mut batch);
                nodes = batch.nodes.len();
                if nodes <= self.limits.max_subtree_nodes as usize {
                    keys.extend(batch.nodes.iter().map(|n| n.key));
                }
                let implausible = matches!(flight.op, FillOp::CacheDump { .. }) && nodes <= 2;
                let origin = if matches!(flight.op, FillOp::CacheDump { .. }) {
                    ApplyOrigin::CacheDump { issued_at }
                } else {
                    ApplyOrigin::Reply { issued_at }
                };
                let outcome = writer.apply(batch, origin);
                self.account(&outcome);
                self.stats.replies_ok += 1;
                if implausible && self.dump_complete {
                    self.dump_complete = false;
                    self.stats.dump_flag_cleared += 1;
                }
                (
                    ReplyReport {
                        health: HealthSignal::Success,
                        outcome: Some(outcome),
                    },
                    None,
                )
            }
            FillReply::Err(e) => {
                self.stats.replies_err += 1;
                let (health, result) = match e {
                    FillError::Gone => {
                        if let Some(k) = flight.op.target() {
                            writer.tombstone(k, cx.now, RemovalCause::UnknownObject);
                        }
                        (HealthSignal::Success, FillResultKind::Gone)
                    }
                    FillError::Unresolved => (HealthSignal::Success, FillResultKind::Unresolved),
                    FillError::Timeout => {
                        (HealthSignal::Timeout(flight.class), FillResultKind::Timeout)
                    }
                    FillError::Busy => (
                        HealthSignal::None,
                        FillResultKind::Refused(RefuseReason::NoSlot),
                    ),
                    FillError::NotSupported => {
                        (HealthSignal::Success, FillResultKind::NotSupported)
                    }
                    FillError::Protocol => (HealthSignal::Success, FillResultKind::Failed),
                };
                (
                    ReplyReport {
                        health,
                        outcome: None,
                    },
                    Some(result),
                )
            }
        };
        if !flight.cancelled && !matches!(cx.health, HealthView::Gone) {
            let outcome = report.outcome.unwrap_or_default();
            let fr = match result {
                None => FillResult::Applied {
                    keys: &keys,
                    nodes,
                    outcome,
                },
                Some(FillResultKind::Gone) => FillResult::Gone,
                Some(FillResultKind::Unresolved) => FillResult::Unresolved,
                Some(FillResultKind::Timeout) => FillResult::Timeout,
                Some(FillResultKind::NotSupported) => FillResult::NotSupported,
                Some(FillResultKind::Failed) => FillResult::Failed,
                Some(FillResultKind::Refused(r)) => FillResult::Refused(r),
            };
            let mut notices = VecDeque::new();
            let effects = self.step(
                cx,
                FillInput::Reply {
                    id,
                    op: &flight.op,
                    result: fr,
                },
            );
            self.apply_effects(cx, writer, effects, &mut notices);
            self.deliver_notices(cx, writer, &mut notices);
        }
        self.start_queued(cx, writer);
        report
    }

    /// The Reader's own fetch (the shared cold path) finished: apply its reply to the mirror.
    /// The policy is not told (it learns of the miss through `Demand`).
    pub fn on_external_reply(
        &mut self,
        cx: &DriverCx<'_>,
        writer: &mut MirrorWriter,
        node: NodeKey,
        issued_at: u64,
        reply: FillReply,
    ) -> ReplyReport {
        match reply {
            FillReply::Ok(batch) => {
                let outcome = writer.apply(batch, ApplyOrigin::Reply { issued_at });
                self.account(&outcome);
                ReplyReport {
                    health: HealthSignal::None,
                    outcome: Some(outcome),
                }
            }
            FillReply::Err(FillError::Gone) => {
                writer.tombstone(node, cx.now, RemovalCause::UnknownObject);
                ReplyReport::NONE
            }
            FillReply::Err(
                FillError::Unresolved
                | FillError::Timeout
                | FillError::Busy
                | FillError::NotSupported
                | FillError::Protocol,
            ) => ReplyReport::NONE,
        }
    }

    /// Bound a reply to what the op asked for; an app (or a buggy connector) cannot make us
    /// store more than the section 7 table allows.
    fn cap_batch(op: &FillOp, batch: &mut SnapshotBatch) {
        let cap = match op {
            FillOp::Fetch { .. } => 1,
            FillOp::ChildSnapshots { range, .. } => range.end.saturating_sub(range.start) as usize,
            FillOp::Ancestry { max_depth, .. } => usize::from(*max_depth) + 1,
            FillOp::Subtree { max_nodes, .. } | FillOp::CacheDump { max_nodes } => {
                *max_nodes as usize
            }
        };
        if batch.nodes.len() > cap {
            batch.nodes.truncate(cap);
            batch.truncated = true;
        }
    }

    fn account(&mut self, o: &ApplyOutcome) {
        self.stats.nodes_applied += u64::from(o.applied);
        self.stats.dropped_tombstoned += u64::from(o.dropped_tombstoned);
        self.stats.dropped_foreign += u64::from(o.dropped_foreign);
        self.stats.superseded += u64::from(o.superseded);
        self.stats.kept_stale += u64::from(o.kept_stale);
        self.stats.evicted += u64::from(o.evicted);
    }
}

enum FillResultKind {
    Gone,
    Unresolved,
    Timeout,
    NotSupported,
    Failed,
    Refused(RefuseReason),
}
