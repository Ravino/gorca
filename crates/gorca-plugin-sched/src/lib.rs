// SPDX-License-Identifier: GPL-3.0-or-later
//! The statistical plugin scheduler, SPS (ADR-0026 section 2.4.7).
//!
//! Pure: integer state owned by one writer, no clock reads (time is an input), no randomness, no
//! I/O, no locks. The same input log gives bit-equal [`Plan`]s (SPS-6); `tools/research/
//! plugin_sched/sps.py` is the reference and `tests/replay.rs` replays a log recorded from it.
//!
//! Three decisions per keypress:
//! 1. [`Scheduler::on_key_down`]: which plugin workers to pre-wake (spin), under a CPU token
//!    bucket ([`SpinBucket`]);
//! 2. [`Scheduler::plan`]: placement, fuel share, deadline `d_i` and hold `h_i` of every
//!    triggered job, and the writer stopping point `t*`;
//! 3. [`Scheduler::observe`] / [`Scheduler::observe_context`]: what the replies taught us.
//!
//! What it never decides: whether a job exists (PF-4), what it computes (PF-1/PF-2), or whether
//! a reply is admitted (PF-7). The absolute clamp of the wait ([`wait_end_us`]) makes PF-8
//! independent of every statistic in here (SPS-1).
//!
//! Units: times in whole microseconds, probabilities and values in Q16 ([`Q`]).

#![allow(
    // Every value is a bounded microsecond count, a Q16 fraction or a small index.
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless
)]

pub mod hist;
pub mod log;

use hist::{EDGES, Hist, INF_US, Pmf, ewma_q16};
pub use hist::{NB, Q};

/// Most plugins one scheduler tracks (a scope has at most this many).
pub const MAX_PLUGINS: usize = 32;
/// Context slots per plugin (`KeyClass` uses six; the replay corpus uses eight).
pub const NCTX: usize = 8;
/// Largest number of hot queues of the queue placement model.
pub const MAX_QUEUES: usize = 16;

/// What the input layer says about a key-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyClass {
    /// Arrow or paging key outside text entry.
    Nav,
    /// Tab or Shift+Tab.
    Tab,
    /// Return or Enter.
    Enter,
    /// An arrow key inside a text field.
    EditArrow,
    /// A character key.
    Type,
    /// Anything else.
    Other,
}

impl KeyClass {
    /// Index into the per-plugin activation table.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Nav => 0,
            Self::Tab => 1,
            Self::Enter => 2,
            Self::EditArrow => 3,
            Self::Type => 4,
            Self::Other => 5,
        }
    }

    /// Typing keys: the LABELS fallback does not pre-wake on them.
    #[must_use]
    pub const fn is_typing(self) -> bool {
        matches!(self, Self::Type | Self::EditArrow)
    }
}

/// The manifest class: a prior and a hard constraint (section 2.4.7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    /// `class = "interactive"`.
    Interactive,
    /// No class.
    Unlabelled,
    /// `class = "refinement"`: never waited for, never pre-woken (L-1).
    Refinement,
}

impl Label {
    const fn value_prior(self) -> i64 {
        match self {
            Self::Interactive => Q,
            Self::Unlabelled => Q / 2,
            Self::Refinement => Q / 4,
        }
    }

    const fn alpha_prior(self) -> i64 {
        match self {
            Self::Interactive => Q / 2,
            Self::Unlabelled => Q / 4,
            Self::Refinement => Q / 8,
        }
    }

    /// May the writer wait for it?
    #[must_use]
    pub const fn wait_eligible(self) -> bool {
        !matches!(self, Self::Refinement)
    }

    /// May it be pre-woken or take a hot place?
    #[must_use]
    pub const fn spin_eligible(self) -> bool {
        !matches!(self, Self::Refinement)
    }

    const fn weight(self) -> i64 {
        match self {
            Self::Interactive => 2,
            Self::Unlabelled | Self::Refinement => 1,
        }
    }
}

/// Trust tier (L-2, L-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// First party (T2).
    First,
    /// A third party the user trusts.
    Trusted,
    /// Any other third party (T3).
    Third,
}

impl Tier {
    const fn index(self) -> usize {
        match self {
            Self::First => 0,
            Self::Trusted => 1,
            Self::Third => 2,
        }
    }

    const fn value_cap(self) -> i64 {
        match self {
            Self::First => Q,
            Self::Trusted => 3 * Q / 4,
            Self::Third => Q / 2,
        }
    }

    /// Hot-queue places per keypress.
    const fn spin_cap(self) -> usize {
        match self {
            Self::First | Self::Trusted => 16,
            Self::Third => 1,
        }
    }

    /// The longest a tier may hold the writer.
    const fn wait_cap_us(self) -> i64 {
        match self {
            Self::First | Self::Trusted => 10_000,
            Self::Third => 100,
        }
    }
}

/// Owner override (L-3; first party only, applied before the statistics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Override {
    /// Statistics decide.
    #[default]
    None,
    /// Always in the wait set (within the window).
    AlwaysWait,
    /// Never waited for.
    NeverWait,
}

/// The state a job started in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Start {
    /// The worker was already running (pre-woken).
    Hot,
    /// The worker was parked.
    Cold,
}

/// Tunables (section 2.4.7.11).
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// The writer budget `D`, from the start of the turn.
    pub d_us: i64,
    /// Reserved for apply + fuse after the wait.
    pub fuse_us: i64,
    /// Default snapshot + send cost when the turn gives none.
    pub snap_us: i64,
    /// Price of the writer's delay, Q16 value units per microsecond.
    pub lam_q16: i64,
    /// Minimum expected gain for a plugin to count toward K.
    pub theta_spin_q16: i64,
    /// K multiplier on the expected number of triggered plugins.
    pub k_mult_q16: i64,
    /// Most workers pre-woken per key.
    pub k_max: usize,
    /// Spin bucket size, microseconds of spin CPU.
    pub spin_bucket_us: i64,
    /// Bucket refill, microseconds of spin per second of wall time.
    pub spin_rate_us_per_s: i64,
    /// Charged per pre-woken worker at key-down.
    pub spin_est_us: i64,
    /// A pre-woken worker spins at most this long past its window.
    pub spin_tail_us: i64,
    /// Pre-wake window.
    pub prewake_window_us: u64,
    /// Activation EWMA shift.
    pub act_shift: u32,
    /// Usefulness EWMA shift.
    pub val_shift: u32,
    /// On-time replies that forgive one offence.
    pub offence_r: u32,
    /// Exclusion cap, activations.
    pub excl_max: u32,
    /// Hot-place denials before promotion.
    pub age_max: u32,
    /// Added to the union-bound deadline.
    pub margin_us: i64,
    /// Replies before a plugin's own histogram is trusted.
    pub warmup: u32,
    /// Per-call fuel cap, wall-time equivalent.
    pub cap_fuel_us: i64,
    /// Fuel units per microsecond.
    pub kappa: i64,
    /// Reproduce the reference's refund on a negative bucket (`sps.py` computes a negative K
    /// there and charges it). Only the cross-language replay sets this; the product never does:
    /// an empty bucket means K = 0 (section 2.4.7.3).
    pub reference_negative_bucket: bool,
    /// Reproduce the reference's key-down, which does not skip plugins excluded from the wait
    /// set (ADR section 2.4.7.3 says it should; the product does). Only the replay sets this.
    pub reference_prewake_ignores_exclusion: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            d_us: 300,
            fuse_us: 8,
            snap_us: 6,
            lam_q16: 3,
            theta_spin_q16: Q / 100,
            k_mult_q16: Q,
            k_max: 6,
            spin_bucket_us: 200_000,
            spin_rate_us_per_s: 150_000,
            spin_est_us: 3_000,
            spin_tail_us: 40,
            prewake_window_us: 50_000,
            act_shift: 5,
            val_shift: 5,
            offence_r: 32,
            excl_max: 256,
            age_max: 8,
            margin_us: 8,
            warmup: 8,
            cap_fuel_us: 200,
            kappa: 21_739,
            reference_negative_bucket: false,
            reference_prewake_ignores_exclusion: false,
        }
    }
}

/// The CPU token bucket of the key-down pre-wake (section 2.4.7.3), one per process.
#[derive(Clone, Copy, Debug)]
pub struct SpinBucket {
    us: i64,
    last_us: u64,
    cap: i64,
    rate: i64,
}

impl SpinBucket {
    /// A full bucket of `cap_us` refilling at `rate_us_per_s`.
    #[must_use]
    pub fn new(cap_us: i64, rate_us_per_s: i64) -> Self {
        Self {
            us: cap_us,
            last_us: 0,
            cap: cap_us,
            rate: rate_us_per_s,
        }
    }

    /// The default bucket: 200 ms, 150 ms per second.
    #[must_use]
    pub fn standard() -> Self {
        let p = Params::default();
        Self::new(p.spin_bucket_us, p.spin_rate_us_per_s)
    }

    /// Refill for the time passed.
    pub fn refill(&mut self, now_us: u64) {
        let dt = now_us.saturating_sub(self.last_us) as i64;
        self.last_us = self.last_us.max(now_us);
        self.us = self.cap.min(
            self.us
                .saturating_add(dt.saturating_mul(self.rate) / 1_000_000),
        );
    }

    /// Spin microseconds available.
    #[must_use]
    pub fn available_us(&self) -> i64 {
        self.us
    }

    /// Charge `us` (the floor is `-cap`).
    pub fn charge(&mut self, us: i64) {
        self.us = (self.us - us).max(-self.cap);
    }

    /// Correct an estimate: `est - actual` goes back (may be negative).
    pub fn settle(&mut self, est_us: i64, actual_us: i64) {
        self.us = self.cap.min((-self.cap).max(self.us + est_us - actual_us));
    }
}

/// Why the scheduler fell back to the fixed LABELS policy (kind only, section 2.4.7.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackKind {
    /// A histogram has no mass after warm-up.
    HistEmpty,
    /// The stopping point lies past the window.
    StopPastWindow,
    /// A queue's grants exceed its pool.
    GrantOverPool,
    /// A counter overflowed.
    CounterOverflow,
}

/// Per-plugin state (section 2.4.7.1).
#[derive(Clone, Debug)]
pub struct PluginStats {
    label: Label,
    tier: Tier,
    over: Override,
    pub(crate) exec: Hist,
    useful: i64,
    act: [i64; NCTX],
    offences: u32,
    excluded: u32,
    ontime: u32,
    denied: u32,
}

impl PluginStats {
    fn new(label: Label, tier: Tier, over: Override) -> Self {
        Self {
            label,
            tier,
            over,
            exec: Hist::new(),
            useful: label.value_prior(),
            act: [label.alpha_prior(); NCTX],
            offences: 0,
            excluded: 0,
            ontime: 0,
            denied: 0,
        }
    }

    fn value(&self) -> i64 {
        self.useful.min(self.tier.value_cap())
    }

    /// Replies seen.
    #[must_use]
    pub fn replies(&self) -> u32 {
        self.exec.n()
    }

    /// Activations left in the wait exclusion.
    #[must_use]
    pub fn excluded(&self) -> u32 {
        self.excluded
    }

    /// Consecutive hot-place denials.
    #[must_use]
    pub fn denied(&self) -> u32 {
        self.denied
    }

    /// Offence level.
    #[must_use]
    pub fn offence_level(&self) -> u32 {
        self.offences
    }

    fn waitable(&self) -> bool {
        self.label.wait_eligible() && self.excluded == 0 && self.over != Override::NeverWait
    }
}

/// A started-state histogram with its cached mass function.
#[derive(Clone, Debug, Default)]
struct StartStats {
    h: Hist,
    pmf: Pmf,
}

impl StartStats {
    fn add(&mut self, x: u32) -> bool {
        let ok = self.h.add(x);
        self.pmf = self.h.pmf();
        ok
    }
}

/// A triggered job, as the writer knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Triggered {
    /// Plugin index in the scheduler.
    pub pid: usize,
    /// The actor is still running an earlier job: dispatched, but not in the eligible set.
    pub busy: bool,
}

/// Where a job runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// On a pre-armed hot queue (queue index; always 0 with own-thread placement).
    Hot(u8),
    /// On a parked thread.
    Parked,
}

/// How hot places exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaceMode {
    /// `k` shared hot queues, list-scheduled (the reference model, `sps.py`).
    Queues(usize),
    /// Every plugin owns its worker: a job is hot iff its worker was pre-armed.
    Own,
}

/// Time facts of the turn.
#[derive(Clone, Copy, Debug)]
pub struct TurnTimes {
    /// Now, microseconds on the caller's clock.
    pub now_us: u64,
    /// Measured time since the start of the writer turn; `None` uses the default reserve.
    pub spent_us: Option<u32>,
}

/// The decision for one job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Assignment {
    /// Plugin index.
    pub pid: usize,
    /// Placement.
    pub place: Place,
    /// Predicted queue offset on the hot queue.
    pub offset_us: u32,
    /// `d_i`, from the send.
    pub deadline_us: u32,
    /// Fuel share, units.
    pub grant_fuel: u64,
    /// `e99 + margin`: an offence needs more exec than this.
    pub exec_allow_us: u32,
    /// `d_i` was cut to the writer window or the tier cap: no offence possible.
    pub clipped: bool,
    /// How long the writer waits for this job; 0 = not waited.
    pub hold_us: u32,
}

impl Assignment {
    const EMPTY: Self = Self {
        pid: 0,
        place: Place::Parked,
        offset_us: 0,
        deadline_us: 0,
        grant_fuel: 0,
        exec_allow_us: 0,
        clipped: false,
        hold_us: 0,
    };

    /// The start state this placement predicts.
    #[must_use]
    pub fn predicted_start(&self) -> Start {
        match self.place {
            Place::Hot(_) => Start::Hot,
            Place::Parked => Start::Cold,
        }
    }
}

/// The plan of one dispatch epoch.
#[derive(Clone, Debug)]
pub struct Plan {
    assign: [Assignment; MAX_PLUGINS],
    len: usize,
    /// The largest hold: how long the writer waits at most after the send.
    pub t_star_us: u32,
    /// The plan was made under the LABELS fallback.
    pub fallback: Option<FallbackKind>,
}

impl Plan {
    fn empty() -> Self {
        Self {
            assign: [Assignment::EMPTY; MAX_PLUGINS],
            len: 0,
            t_star_us: 0,
            fallback: None,
        }
    }

    /// The assignments, in dispatch order (third parties last).
    #[must_use]
    pub fn assignments(&self) -> &[Assignment] {
        &self.assign[..self.len]
    }

    /// The assignment of a plugin.
    #[must_use]
    pub fn of(&self, pid: usize) -> Option<&Assignment> {
        self.assignments().iter().find(|a| a.pid == pid)
    }

    /// Plugins the writer waits for (`hold > 0`), in assignment order.
    pub fn wait_set(&self) -> impl Iterator<Item = &Assignment> {
        self.assignments().iter().filter(|a| a.hold_us > 0)
    }

    fn push(&mut self, a: Assignment) {
        if self.len < MAX_PLUGINS {
            self.assign[self.len] = a;
            self.len += 1;
        }
    }
}

/// What the scheduler has learned about one plugin and the host (diagnostics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Replies seen.
    pub replies: u32,
    /// Guest time, median (upper bucket edge).
    pub exec_p50_us: u32,
    /// Guest time, 99th percentile.
    pub exec_p99_us: u32,
    /// Start latency of a pre-armed worker, median.
    pub hot_start_p50_us: u32,
    /// Start latency of a pre-armed worker, 99th percentile.
    pub hot_start_p99_us: u32,
    /// Start latency of a parked worker, median.
    pub cold_start_p50_us: u32,
    /// Start latency of a parked worker, 99th percentile.
    pub cold_start_p99_us: u32,
    /// Value, Q16.
    pub value_q16: i64,
    /// Activations left in the wait exclusion.
    pub excluded: u32,
    /// Offence level.
    pub offences: u32,
}

/// What a key-down asks of the pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PreWake {
    /// Workers to arm.
    pub threads: u8,
    /// Which plugins' workers (bit per plugin index).
    pub who: u32,
    /// Arm until this instant, microseconds on the caller's clock.
    pub until_us: u64,
}

/// One reply, as the scheduler sees it (section 2.4.7.6).
#[derive(Clone, Copy, Debug)]
pub struct Observation<'a> {
    /// Plugin index.
    pub pid: usize,
    /// The state the job really started in.
    pub state: Start,
    /// Thread start latency (queue offset excluded).
    pub start_us: u32,
    /// Guest wall time.
    pub exec_us: u32,
    /// Send to reply.
    pub resp_us: u32,
    /// The assignment the job was dispatched with.
    pub assignment: &'a Assignment,
    /// The writer waited for it.
    pub waited: bool,
    /// Its facts changed what was published (L-4; first form: it emitted a fact).
    pub useful: bool,
}

#[derive(Clone, Copy, Debug)]
struct Armed {
    /// When the worker was armed (microseconds, the caller's clock).
    at: u64,
    /// Until when.
    until: u64,
    /// What the bucket was charged for it.
    est: i64,
}

/// The scheduler of one scope.
#[derive(Debug)]
pub struct Scheduler {
    p: Params,
    plugins: Vec<PluginStats>,
    start_hot: StartStats,
    start_cold: StartStats,
    turn: u64,
    pending_ctx: Option<usize>,
    armed: [Option<Armed>; MAX_PLUGINS],
    settle_us: i64,
    fallback: Option<FallbackKind>,
    recorder: Option<log::Ring>,
}

impl Scheduler {
    /// One scheduler for `plugins` (at most [`MAX_PLUGINS`]; the rest are ignored).
    #[must_use]
    pub fn new(plugins: &[(Label, Tier)], p: Params) -> Self {
        Self::with_overrides(
            &plugins
                .iter()
                .map(|(l, t)| (*l, *t, Override::None))
                .collect::<Vec<_>>(),
            p,
        )
    }

    /// As [`Scheduler::new`] with owner overrides.
    #[must_use]
    pub fn with_overrides(plugins: &[(Label, Tier, Override)], p: Params) -> Self {
        Self {
            p,
            plugins: plugins
                .iter()
                .take(MAX_PLUGINS)
                .map(|(l, t, o)| {
                    // An override is the owner's, and only for first-party code.
                    let o = if *t == Tier::First {
                        *o
                    } else {
                        Override::None
                    };
                    PluginStats::new(*l, *t, o)
                })
                .collect(),
            start_hot: StartStats::default(),
            start_cold: StartStats::default(),
            turn: 0,
            pending_ctx: None,
            armed: [None; MAX_PLUGINS],
            settle_us: 0,
            fallback: None,
            recorder: None,
        }
    }

    /// Record the input log into a ring of `cap` entries (field replay, `GORCA_SCHED_LOG`).
    pub fn record(&mut self, cap: usize) {
        self.recorder = Some(log::Ring::new(cap));
    }

    /// The recorded input log, oldest first.
    #[must_use]
    pub fn recorded(&self) -> Option<&log::Ring> {
        self.recorder.as_ref()
    }

    /// Tunables.
    #[must_use]
    pub fn params(&self) -> &Params {
        &self.p
    }

    /// Plugins tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// No plugins tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// The stats of a plugin.
    #[must_use]
    pub fn stats(&self, pid: usize) -> Option<&PluginStats> {
        self.plugins.get(pid)
    }

    /// A one-line summary of what the scheduler has learned about plugin `pid` (diagnostics and
    /// the benchmark report; numbers only).
    #[must_use]
    pub fn summary(&self, pid: usize) -> Option<Summary> {
        let st = self.plugins.get(pid)?;
        let q = |h: &Hist, p: i64| if h.total() == 0 { 0 } else { h.quantile(p) };
        Some(Summary {
            replies: st.exec.n(),
            exec_p50_us: q(&st.exec, Q / 2),
            exec_p99_us: q(&st.exec, Q * 99 / 100),
            hot_start_p50_us: q(&self.start_hot.h, Q / 2),
            hot_start_p99_us: q(&self.start_hot.h, Q * 99 / 100),
            cold_start_p50_us: q(&self.start_cold.h, Q / 2),
            cold_start_p99_us: q(&self.start_cold.h, Q * 99 / 100),
            value_q16: st.value(),
            excluded: st.excluded,
            offences: st.offences,
        })
    }

    /// Set once an invariant check failed; the rest of the scope runs the LABELS policy.
    #[must_use]
    pub fn fallback(&self) -> Option<FallbackKind> {
        self.fallback
    }

    /// Force the LABELS policy (tests, and the daemon's `labels` switch).
    pub fn force_fallback(&mut self, kind: FallbackKind) {
        self.fallback = Some(kind);
    }

    /// Window left for plugins after the snapshot and the reserve for apply + fuse.
    fn window_us(&self, spent: Option<u32>, fuse: Option<u32>) -> i64 {
        let spent = spent.map_or(self.p.snap_us, i64::from);
        let fuse = fuse.map_or(self.p.fuse_us, i64::from);
        (self.p.d_us - fuse - spent).max(0)
    }

    fn start_stats(&self, s: Start) -> &StartStats {
        match s {
            Start::Hot => &self.start_hot,
            Start::Cold => &self.start_cold,
        }
    }

    /// Q16 lower bound of `P(start + exec < t)` for plugin `st` in state `s` at queue offset
    /// `offset` (the convolution of the pooled start mass with the plugin's own exec mass).
    fn f_state(&self, st: &PluginStats, s: Start, t_us: i64, offset: i64) -> i64 {
        let mut acc = 0;
        for (up, m) in self.start_stats(s).pmf.iter() {
            acc += m * st.exec.cdf_at(t_us - offset - i64::from(up)) / Q;
        }
        acc
    }

    // ---------------------------------------------------------------- (a) key-down

    /// A key went down. `class` comes from the input layer. Returns the workers to arm; the
    /// bucket is charged `spin_est` per worker (settled later against the real cost).
    pub fn on_key_down(
        &mut self,
        class: KeyClass,
        now_us: u64,
        bucket: &mut SpinBucket,
    ) -> PreWake {
        self.keydown_ctx(class.index(), class.is_typing(), now_us, bucket)
    }

    /// As [`Scheduler::on_key_down`] with a raw context index (replay corpora).
    pub fn keydown_ctx(
        &mut self,
        ctx: usize,
        typing: bool,
        now_us: u64,
        bucket: &mut SpinBucket,
    ) -> PreWake {
        let ctx = ctx.min(NCTX - 1);
        // A key that caused no focus change teaches `alpha ~ 0` (typing learns to wake nothing).
        if let Some(prev) = self.pending_ctx.take() {
            self.learn_context(prev, 0);
        }
        self.settle_armed(now_us, false);
        bucket.settle(self.settle_us, 0);
        self.settle_us = 0;
        bucket.refill(now_us);
        let out = if self.fallback.is_some() {
            self.labels_prewake(typing, now_us, bucket)
        } else {
            self.sps_prewake(ctx, now_us, bucket)
        };
        self.pending_ctx = Some(ctx);
        if let Some(r) = self.recorder.as_mut() {
            r.push(log::Rec::KeyDown {
                ctx: ctx as u8,
                typing,
                now_us,
                threads: out.threads,
                who: out.who,
            });
        }
        out
    }

    fn arm(&mut self, who: u32, now_us: u64, bucket: &mut SpinBucket) -> PreWake {
        let until_us = now_us + self.p.prewake_window_us;
        let mut threads = 0u8;
        let mut mask = 0u32;
        for pid in 0..self.plugins.len() {
            if who & (1 << pid) != 0 {
                self.armed[pid] = Some(Armed {
                    at: now_us,
                    until: until_us,
                    est: self.p.spin_est_us,
                });
                bucket.charge(self.p.spin_est_us);
                threads += 1;
                mask |= 1 << pid;
            }
        }
        PreWake {
            threads,
            who: mask,
            until_us,
        }
    }

    fn sps_prewake(&mut self, ctx: usize, now_us: u64, bucket: &mut SpinBucket) -> PreWake {
        let dw = self.window_us(None, None);
        let no_host = self.start_hot.h.total() == 0 || self.start_cold.h.total() == 0;
        // (score, pid) of the plugins worth a hot start; `load` = expected valuable activations.
        let mut scored = [(0i64, 0usize); MAX_PLUGINS];
        let mut n = 0;
        let mut load = 0i64;
        for (pid, st) in self.plugins.iter().enumerate() {
            if !st.label.spin_eligible()
                || (st.excluded > 0 && !self.p.reference_prewake_ignores_exclusion)
            {
                continue;
            }
            let a = st.act[ctx];
            let gain = if no_host {
                Q / 2
            } else {
                (self.f_state(st, Start::Hot, dw, 0) - self.f_state(st, Start::Cold, dw, 0)).max(0)
            };
            let sc = a * st.value() / Q * gain / Q;
            if sc >= self.p.theta_spin_q16 {
                scored[n] = (sc, pid);
                n += 1;
                load += a;
            }
        }
        if n == 0 {
            return PreWake::default();
        }
        let ceil = (load * self.p.k_mult_q16 + Q * Q - 1) / (Q * Q);
        let by_bucket = bucket.available_us().div_euclid(self.p.spin_est_us.max(1));
        let want = n.min(self.p.k_max).min(ceil.max(1) as usize) as i64;
        let k = if self.p.reference_negative_bucket {
            // sps.py: a negative bucket gives a negative k, and charging it refunds the bucket.
            let k = want.min(by_bucket);
            if k < 0 {
                bucket.charge(k * self.p.spin_est_us);
            }
            k.max(0) as usize
        } else {
            want.min(by_bucket.max(0)) as usize
        };
        let scored = &mut scored[..n];
        // Best score first; ties by plugin index. At most one third-party worker (L-2).
        scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut who = 0u32;
        let mut third = 0;
        let mut taken = 0;
        for (_, pid) in scored.iter() {
            if taken >= k {
                break;
            }
            if self.plugins[*pid].tier == Tier::Third {
                if third >= Tier::Third.spin_cap() {
                    continue;
                }
                third += 1;
            }
            who |= 1 << pid;
            taken += 1;
        }
        self.arm(who, now_us, bucket)
    }

    /// The LABELS policy: pre-wake every interactive and unlabelled worker on non-typing keys.
    fn labels_prewake(&mut self, typing: bool, now_us: u64, bucket: &mut SpinBucket) -> PreWake {
        if typing {
            return PreWake::default();
        }
        let mut who = 0u32;
        let mut cost = 0;
        for (pid, st) in self.plugins.iter().enumerate() {
            if st.label.spin_eligible() && cost + self.p.spin_est_us <= bucket.available_us() {
                who |= 1 << pid;
                cost += self.p.spin_est_us;
            }
        }
        self.arm(who, now_us, bucket)
    }

    /// Settle pre-armed workers: those whose window passed (or all of them, once a focus change
    /// consumed them). The difference goes back to the bucket at the next key-down.
    fn settle_armed(&mut self, now_us: u64, all: bool) {
        for slot in &mut self.armed {
            if let Some(a) = *slot
                && (all || a.until <= now_us)
            {
                let spun = now_us.min(a.until).saturating_sub(a.at) as i64;
                self.settle_us += a.est - (spun + self.p.spin_tail_us);
                *slot = None;
            }
        }
    }

    /// The workers armed now (window not over), bit per plugin index.
    #[must_use]
    pub fn armed_mask(&self, now_us: u64) -> u32 {
        let mut m = 0;
        for (pid, a) in self.armed.iter().enumerate() {
            if a.is_some_and(|a| a.until > now_us) {
                m |= 1 << pid;
            }
        }
        m
    }

    // ---------------------------------------------------------------- (b) + (c) dispatch

    /// Plan one dispatch epoch: order, placement, fuel, deadline and hold of every triggered job.
    /// The caller then reports the key's outcome with [`Scheduler::end_key`].
    pub fn plan(&mut self, trig: &[Triggered], times: TurnTimes, mode: PlaceMode) -> Plan {
        self.plan_with(trig, times, mode, None)
    }

    /// As [`Scheduler::plan`] with the measured reserve for admitting the replies and fusing
    /// after the wait (`None`: [`Params::fuse_us`]). The writer's total delay is bounded by `D`
    /// only if the wait leaves room for what follows it; the caller measures that cost.
    pub fn plan_with(
        &mut self,
        trig: &[Triggered],
        times: TurnTimes,
        mode: PlaceMode,
        fuse_us: Option<u32>,
    ) -> Plan {
        self.turn += 1;
        let trig = &trig[..trig.len().min(MAX_PLUGINS)];
        let mut mask = 0u32;
        for t in trig {
            if t.pid < self.plugins.len() {
                mask |= 1 << t.pid;
            }
        }
        let armed = self.armed_mask(times.now_us);
        // The focus change consumes the pre-armed workers: they run their job or park.
        self.settle_armed(times.now_us, true);
        let plan = if self.fallback.is_some() {
            self.plan_labels(trig, times, armed, fuse_us)
        } else {
            let plan = self.plan_sps(trig, times, mode, armed, fuse_us);
            match self.check(&plan, trig, self.window_us(times.spent_us, fuse_us)) {
                Some(kind) => {
                    self.fallback = Some(kind);
                    self.plan_labels(trig, times, armed, fuse_us)
                }
                None => plan,
            }
        };
        if let Some(r) = self.recorder.as_mut() {
            r.push(log::Rec::Plan {
                now_us: times.now_us,
                spent_us: times.spent_us.unwrap_or(u32::MAX),
                fuse_us: fuse_us.unwrap_or(u32::MAX),
                queues: match mode {
                    PlaceMode::Queues(k) => k as u8,
                    PlaceMode::Own => 255,
                },
                armed,
                mask,
                busy: trig
                    .iter()
                    .filter(|t| t.busy && t.pid < 32)
                    .fold(0, |m, t| m | (1u32 << t.pid)),
                hash: plan_hash(&plan),
            });
        }
        plan
    }

    /// The invariants of section 2.4.7.7.
    fn check(&self, plan: &Plan, trig: &[Triggered], dw: i64) -> Option<FallbackKind> {
        if i64::from(plan.t_star_us) > dw {
            return Some(FallbackKind::StopPastWindow);
        }
        for t in trig {
            if let Some(st) = self.plugins.get(t.pid)
                && st.exec.n >= self.p.warmup
                && st.exec.total() == 0
            {
                return Some(FallbackKind::HistEmpty);
            }
        }
        None
    }

    fn quant(h: &Hist, p: i64, empty: u32) -> u32 {
        if h.total() == 0 { empty } else { h.quantile(p) }
    }

    fn plan_sps(
        &mut self,
        trig: &[Triggered],
        times: TurnTimes,
        mode: PlaceMode,
        armed: u32,
        fuse_us: Option<u32>,
    ) -> Plan {
        let p = self.p;
        let n = trig.len();
        let dw = self.window_us(times.spent_us, fuse_us);
        let q99 = Q * 99 / 100;
        let q90 = Q * 9 / 10;
        let (hot, cold) = (&self.start_hot.h, &self.start_cold.h);
        let hot_q99 = i64::from(Self::quant(hot, q99, 8));
        let cold_q99 = i64::from(Self::quant(cold, q99, 256));
        let cold_q90 = i64::from(Self::quant(cold, q90, 128));
        let hot_q90 = i64::from(Self::quant(hot, q90, 4));
        let hot_q50 = i64::from(Self::quant(hot, Q / 2, 4));

        // 1. Order: third parties last; promoted first; weighted SPT; rotating tie.
        let mut order = [(0usize, (0i64, 0i64, 0i64, 0i64, 0i64, 0usize)); MAX_PLUGINS];
        let mut m = 0;
        for t in trig {
            let Some(st) = self.plugins.get(t.pid) else {
                continue;
            };
            let e50 = i64::from(Self::quant(&st.exec, Q / 2, 32)).max(1);
            let promoted = st.denied >= p.age_max;
            let eligible = st.waitable() && !t.busy;
            let key = if eligible {
                st.value() * st.label.weight() / e50
            } else {
                0
            };
            let tie = ((t.pid as u64 + self.turn) % (n.max(1) as u64)) as i64;
            order[m] = (
                t.pid,
                (
                    i64::from(st.tier == Tier::Third),
                    i64::from(!promoted),
                    if promoted { -i64::from(st.denied) } else { 0 },
                    -key,
                    tie,
                    t.pid,
                ),
            );
            m += 1;
        }
        order[..m].sort_unstable_by_key(|a| a.1);

        // 2. Place by list scheduling (queues) or by the pre-armed worker (own threads).
        let k_hot = match mode {
            PlaceMode::Queues(k) => k.min(MAX_QUEUES),
            PlaceMode::Own => armed.count_ones() as usize,
        };
        let mut free = [0i64; MAX_QUEUES];
        let mut free99 = [0i64; MAX_QUEUES];
        let mut hot_used = [0usize; 3];
        let mut plan = Plan::empty();
        for (pid, _) in &order[..m] {
            let pid = *pid;
            let busy = trig.iter().any(|t| t.pid == pid && t.busy);
            let st = &self.plugins[pid];
            let e90 = i64::from(Self::quant(&st.exec, q90, 64));
            let e99 = i64::from(Self::quant(&st.exec, q99, INF_US));
            let tier = st.tier.index();
            let mut place = Place::Parked;
            let (mut off, mut off99) = (0i64, 0i64);
            if !busy && k_hot > 0 && st.label.spin_eligible() && hot_used[tier] < st.tier.spin_cap()
            {
                match mode {
                    PlaceMode::Queues(_) => {
                        let k = (0..k_hot).min_by_key(|j| (free[*j], *j)).unwrap_or(0);
                        if free[k] + hot_q90 <= cold_q90 {
                            place = Place::Hot(k as u8);
                            off = free[k];
                            off99 = free99[k];
                            free[k] += e90;
                            free99[k] += e99.min(p.cap_fuel_us);
                            hot_used[tier] += 1;
                        }
                    }
                    PlaceMode::Own => {
                        if armed & (1 << pid) != 0 {
                            place = Place::Hot(0);
                            hot_used[tier] += 1;
                        }
                    }
                }
            }
            let d = match place {
                Place::Parked => cold_q99 + e99 + p.margin_us,
                Place::Hot(_) => off99 + hot_q99 + e99 + p.margin_us,
            };
            if !busy && st.label.wait_eligible() && st.label.spin_eligible() && st.excluded == 0 {
                // Aging counts denials only while shared hot queues exist. With a worker per plugin
                // the place is the pre-armed worker itself: there is nothing to promote into.
                let denied =
                    if matches!(place, Place::Hot(_)) || k_hot == 0 || mode == PlaceMode::Own {
                        0
                    } else {
                        (st.denied + 1).min(255)
                    };
                self.plugins[pid].denied = denied;
            }
            let st = &self.plugins[pid];
            let cap_t = dw.min(st.tier.wait_cap_us());
            plan.push(Assignment {
                pid,
                place,
                offset_us: off as u32,
                deadline_us: d.min(cap_t) as u32,
                grant_fuel: 0,
                exec_allow_us: (e99 + p.margin_us) as u32,
                clipped: d > cap_t,
                hold_us: 0,
            });
        }

        // 3.-5. Writer stopping point and wait set. Eligible: label, not excluded, not busy, and
        // not a third party whose deadline was clipped.
        let mut elig = [false; MAX_PLUGINS];
        for (i, a) in plan.assignments().iter().enumerate() {
            let st = &self.plugins[a.pid];
            let busy = trig.iter().any(|t| t.pid == a.pid && t.busy);
            elig[i] = st.waitable() && !busy && !(a.clipped && st.tier == Tier::Third);
        }
        let mut grid = [0u32; 2 * MAX_PLUGINS + 20];
        let mut g = 0;
        for e in EDGES.iter().filter(|e| **e > 0 && i64::from(**e) <= dw) {
            grid[g] = *e;
            g += 1;
        }
        grid[g] = dw as u32;
        g += 1;
        for (i, a) in plan.assignments().iter().enumerate() {
            if elig[i] && g < grid.len() {
                grid[g] = a.deadline_us;
                g += 1;
            }
        }
        let grid = &mut grid[..g];
        grid.sort_unstable();
        let (mut best_t, mut best_obj) = (0i64, 0i64);
        let mut prev = u32::MAX;
        for t in grid.iter().copied() {
            if t == prev {
                continue;
            }
            prev = t;
            let t = i64::from(t);
            let mut obj = -p.lam_q16 * t;
            for (i, a) in plan.assignments().iter().enumerate() {
                if !elig[i] {
                    continue;
                }
                let st = &self.plugins[a.pid];
                let f = if st.exec.n < p.warmup {
                    if t >= i64::from(a.deadline_us) { Q } else { 0 }
                } else {
                    self.f_state(
                        st,
                        a.predicted_start(),
                        t.min(i64::from(a.deadline_us)),
                        i64::from(a.offset_us),
                    )
                };
                obj += st.value() * f / Q;
            }
            if obj > best_obj {
                best_t = t;
                best_obj = obj;
            }
        }
        let mut t_star = 0u32;
        for (i, &is_elig) in elig.iter().enumerate().take(plan.len) {
            let a = plan.assign[i];
            let st = &self.plugins[a.pid];
            if st.over == Override::AlwaysWait && is_elig {
                plan.assign[i].hold_us = a.deadline_us;
            } else if is_elig {
                let h = best_t.min(i64::from(a.deadline_us));
                let f = if st.exec.n < p.warmup {
                    Q
                } else {
                    self.f_state(st, a.predicted_start(), h, i64::from(a.offset_us))
                };
                if h > 0 && st.value() * f / Q >= p.lam_q16 * h {
                    plan.assign[i].hold_us = h as u32;
                }
            }
            t_star = t_star.max(plan.assign[i].hold_us);
        }
        plan.t_star_us = t_star;

        // Fuel: weighted progressive filling per hot queue; a parked job runs alone at the cap.
        let cap = p.cap_fuel_us * p.kappa;
        let pool0 = (dw - hot_q50).max(0) * p.kappa;
        match mode {
            PlaceMode::Queues(_) => {
                for k in 0..k_hot {
                    let mut pool = pool0;
                    let mut wsum: i64 = 0;
                    for a in plan.assignments() {
                        if a.place == Place::Hot(k as u8) {
                            wsum += self.plugins[a.pid].label.weight();
                        }
                    }
                    // Queue order = predicted offset, then plugin index.
                    let mut idx = [0usize; MAX_PLUGINS];
                    let mut c = 0;
                    for (i, a) in plan.assignments().iter().enumerate() {
                        if a.place == Place::Hot(k as u8) {
                            idx[c] = i;
                            c += 1;
                        }
                    }
                    idx[..c]
                        .sort_unstable_by_key(|i| (plan.assign[*i].offset_us, plan.assign[*i].pid));
                    for i in &idx[..c] {
                        let w = self.plugins[plan.assign[*i].pid].label.weight();
                        let g = cap.min(pool * w / wsum.max(1));
                        plan.assign[*i].grant_fuel = g as u64;
                        wsum -= w;
                        pool -= g;
                    }
                }
            }
            PlaceMode::Own => {
                for i in 0..plan.len {
                    if matches!(plan.assign[i].place, Place::Hot(_)) {
                        plan.assign[i].grant_fuel = cap.min(pool0) as u64;
                    }
                }
            }
        }
        for i in 0..plan.len {
            if plan.assign[i].place == Place::Parked {
                plan.assign[i].grant_fuel = cap as u64;
            }
        }
        plan
    }

    /// The fixed LABELS policy: wait for every non-refinement job up to the window; workers are
    /// hot iff pre-armed; no statistics.
    fn plan_labels(
        &mut self,
        trig: &[Triggered],
        times: TurnTimes,
        armed: u32,
        fuse_us: Option<u32>,
    ) -> Plan {
        let p = self.p;
        let dw = self.window_us(times.spent_us, fuse_us);
        let cap = p.cap_fuel_us * p.kappa;
        let mut plan = Plan::empty();
        let mut t_star = 0u32;
        // Third parties last.
        for third in [false, true] {
            for t in trig {
                let Some(st) = self.plugins.get(t.pid) else {
                    continue;
                };
                if (st.tier == Tier::Third) != third {
                    continue;
                }
                let hot = armed & (1 << t.pid) != 0 && st.label.spin_eligible() && !t.busy;
                let hold = if st.label.wait_eligible() && !t.busy && st.over != Override::NeverWait
                {
                    dw.min(st.tier.wait_cap_us()) as u32
                } else {
                    0
                };
                t_star = t_star.max(hold);
                plan.push(Assignment {
                    pid: t.pid,
                    place: if hot { Place::Hot(0) } else { Place::Parked },
                    offset_us: 0,
                    deadline_us: dw as u32,
                    grant_fuel: cap as u64,
                    exec_allow_us: INF_US,
                    clipped: true,
                    hold_us: hold,
                });
            }
        }
        plan.t_star_us = t_star;
        plan.fallback = self.fallback;
        plan
    }

    // ---------------------------------------------------------------- observations

    /// The pending key caused a focus change that triggered exactly the plugins in `triggered`
    /// (bit per plugin index); no-op when no key is pending. A key whose focus change never
    /// comes is flushed by the next [`Scheduler::on_key_down`] with an empty set.
    pub fn end_key(&mut self, triggered: u32) {
        if let Some(ctx) = self.pending_ctx {
            self.observe_context(ctx, triggered);
        }
    }

    /// The key (context) pending an outcome.
    #[must_use]
    pub fn pending_ctx(&self) -> Option<usize> {
        self.pending_ctx
    }

    /// The key with context `ctx` triggered exactly the plugins in `triggered` (bit per plugin
    /// index).
    pub fn observe_context(&mut self, ctx: usize, triggered: u32) {
        let ctx = ctx.min(NCTX - 1);
        self.pending_ctx = None;
        self.learn_context(ctx, triggered);
        if let Some(r) = self.recorder.as_mut() {
            r.push(log::Rec::Context {
                ctx: ctx as u8,
                triggered,
            });
        }
    }

    fn learn_context(&mut self, ctx: usize, triggered: u32) {
        for (pid, st) in self.plugins.iter_mut().enumerate() {
            let x = if triggered & (1 << pid) != 0 { Q } else { 0 };
            st.act[ctx] = ewma_q16(st.act[ctx], x, self.p.act_shift);
        }
    }

    /// Forget which workers are armed without settling their cost (the runtime says nothing
    /// was armed; replay of a reference log that settles through explicit calls).
    pub fn disarm_all(&mut self) {
        self.armed = [None; MAX_PLUGINS];
        self.settle_us = 0;
    }

    /// A reply was published: update the histograms, usefulness, offence and exclusion state.
    pub fn observe(&mut self, o: &Observation<'_>) {
        let Some(st) = self.plugins.get_mut(o.pid) else {
            return;
        };
        let p = self.p;
        let a = o.assignment;
        let mut ok = match o.state {
            Start::Hot => self.start_hot.add(o.start_us),
            Start::Cold => self.start_cold.add(o.start_us),
        };
        ok &= st.exec.add(o.exec_us);
        st.useful = ewma_q16(st.useful, if o.useful { Q } else { 0 }, p.val_shift);
        let bound = if o.waited { a.hold_us } else { a.deadline_us };
        let late = o.resp_us > bound;
        let own_fault = o.exec_us > a.exec_allow_us && !a.clipped;
        st.excluded = st.excluded.saturating_sub(1);
        if late && o.waited && own_fault {
            st.offences = st.offences.saturating_add(1);
            st.excluded = p.excl_max.min(1u32 << st.offences.min(30));
            st.ontime = 0;
        } else if !late {
            st.ontime += 1;
            if st.ontime >= p.offence_r && st.offences > 0 {
                st.offences -= 1;
                st.ontime = 0;
            }
        }
        if !ok {
            self.fallback = Some(FallbackKind::CounterOverflow);
        }
        if let Some(r) = self.recorder.as_mut() {
            r.push(log::Rec::Observe {
                pid: o.pid as u8,
                hot: o.state == Start::Hot,
                start_us: o.start_us,
                exec_us: o.exec_us,
                resp_us: o.resp_us,
                deadline_us: a.deadline_us,
                hold_us: a.hold_us,
                exec_allow_us: a.exec_allow_us,
                clipped: a.clipped,
                waited: o.waited,
                useful: o.useful,
            });
        }
    }
}

/// FNV-1a over the decisions of a plan.
#[must_use]
pub fn plan_hash(plan: &Plan) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut put = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for a in plan.assignments() {
        put(a.pid as u64);
        put(match a.place {
            Place::Hot(k) => 1 + u64::from(k),
            Place::Parked => 0,
        });
        put(u64::from(a.offset_us));
        put(u64::from(a.deadline_us));
        put(a.grant_fuel);
        put(u64::from(a.hold_us));
    }
    put(u64::from(plan.t_star_us));
    h
}

/// The absolute clamp of the wait (section 2.4.7.4 step 7): the deadline handed to the wait
/// primitive. `t_snap_done_us` is the measured time after the snapshot and the send, `t0_us` the
/// start of the writer turn. Whatever `t_star_us` is, the result never exceeds
/// `t0 + D - fuse_reserve` (SPS-1).
#[must_use]
pub fn wait_end_us(
    t0_us: u64,
    t_snap_done_us: u64,
    t_star_us: u64,
    d_us: u64,
    fuse_reserve_us: u64,
) -> u64 {
    let limit = t0_us.saturating_add(d_us.saturating_sub(fuse_reserve_us));
    t_snap_done_us.saturating_add(t_star_us).min(limit)
}

/// The end of the writer's wait given the holds and the reply times of the wait set
/// (`reply_us` is `None` for a reply that never comes), all from the send:
/// `max over W of min(reply, hold)`. Used by the property tests of SPS-1/SPS-2.
#[must_use]
pub fn wait_end_of(holds_replies: &[(u32, Option<u32>)]) -> u32 {
    holds_replies
        .iter()
        .map(|(h, r)| r.map_or(*h, |r| r.min(*h)))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
