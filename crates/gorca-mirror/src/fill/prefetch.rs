//! The product fill policy (B-1 adoption of arm C, decision 2026-10-14): lazy everywhere, a
//! prefetch of the focus window, an eager `CacheDump` only where the connector verified it
//! complete *and* the configuration asks for it (off by default).
//!
//! * `FocusSettled{n}`: the ancestry of `n` when its chain is not mirrored (one round per level),
//!   then a window of siblings around `n` (`FillOp::Siblings`: the connector finds `n`'s index in
//!   the one child-list reply it needs anyway, so there is no probing). The window is a *run*: a
//!   contiguous stretch of one parent's children that the mirror holds fresh. Near a run's edge
//!   the run grows by one more window; a run that went dirty is refetched when the focus is in it.
//! * Look-ahead: the `LOOK` siblings ahead of the focus that this policy fetched more than
//!   `LOOK_AGE` ago (or whose name/role went stale) are read again, one request: a silent rename
//!   (a recycled GTK4 list row) is corrected before the user lands on the node (B-1 M-6). Without
//!   it M-6 is R0's 0.27; the states of the node a focus move leaves are deliberately not a
//!   reason to refetch (that made the first version cost 45 calls per focus).
//! * `ChildrenChanged` on a parent that has a run: the run is dirty; when the focus sits in it,
//!   the window is refetched after a debounce. No age-based refresh: silent changes are the
//!   Reader's RV-1 business, and a window activation refreshes the focus window once.
//! * No crawl of other apps, no whole-window subtree walk, no timers while idle.
//! * `cx.dump_complete` apps with `PrefetchConfig::shell_dump`: lazy until one `CacheDump` after
//!   the first focus and `DUMP_QUIET`. Disabled by default until the Shell main-loop stall of a
//!   dump is measured (B-1 judgement finding 2).
//! * Bounds: at most `MAX_OUT_PREFETCH` own requests outstanding, nothing to a `Hung` app, only
//!   Interactive when `Slow`, nothing in the background when `Flooded`, backoff after a failure,
//!   bounded state (`MAX_RUNS` runs of at most `RUN_CAP` keys), `WakeAt` only while work is
//!   pending.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::mem::size_of;
use std::time::{Duration, Instant};

use gorca_ingress::DirtySet;
use gorca_model::{ChildList, Ifaces, NodeEvent, NodeKey, NodeSnapshot, PropMask, Role};
use gorca_rt::BudgetClass;
use smallvec::SmallVec;

use super::{
    DemandOutcome, DemandWhy, FillCtx, FillEffect, FillEffects, FillInput, FillOp, FillPolicy,
    FillResult, HealthView, RefuseReason,
};

/// Siblings before the focus (earlier in the child list) in a fresh window.
const BEFORE: u32 = 1;
/// Siblings after the focus in a fresh window.
const AFTER: u32 = 6;
/// Siblings one run extension adds.
const EXTEND: u32 = 6;
/// This close to the end of a run, in the direction the focus travels, the run grows ...
const EDGE_TRAVEL: usize = 3;
/// ... and this close to the other end (a fresh window holds `BEFORE` entries before the focus,
/// so a fresh run is not an edge case).
const EDGE_OTHER: usize = 1;
/// A parent with more children than this is read by index (`GetIndexInParent` +
/// `GetChildAtIndex`), never listed whole (B-1 review F-2).
const INDEXED_ABOVE: usize = 256;
/// What a sibling ahead of the focus must carry fresh: the fields a silent change corrupts.
/// `STATES` (and `VALUE`) are left out on purpose: every focus move invalidates the states of the
/// node it leaves, and a refetch per focus step is the 100-calls-per-focus cost of the bake-off
/// arm (the Reader re-reads a stale state itself).
const NEAR_NEED: PropMask = PropMask::NAME.union(PropMask::ROLE);
/// The siblings this far ahead of the focus are kept fresh: a silent rename (a recycled GTK4
/// list row) is corrected before the user lands on the node, not 2-5 ms after (B-1 M-6).
const LOOK: usize = 3;
/// A sibling ahead of the focus older than this (in the mirror, by this policy's own fetch) is
/// read again. A walk of 30 ms steps re-reads one node per step; a standing focus costs nothing.
const LOOK_AGE: Duration = Duration::from_millis(500);
const MAX_RUNS: usize = 48;
const RUN_CAP: usize = 96;
const ANCESTRY_DEPTH: u8 = 24;
const MAX_CHAIN: usize = 24;
const MAX_OUT_PREFETCH: usize = 3;
/// Own request bookkeeping is dropped after this many times the class deadline: by then the
/// driver has long answered (it replies to every request, `Timeout` included).
const OUT_TTL_FACTOR: u32 = 2;
const DUMP_QUIET: Duration = Duration::from_millis(500);
/// The same node is not asked for a window or an ancestry twice within this.
const ASK_GAP: Duration = Duration::from_secs(3);
const FOCUS_RETRIES: u8 = 4;
const BACKOFF: Duration = Duration::from_secs(1);
/// A dirty run is refetched this long after the last event ...
const DEBOUNCE: Duration = Duration::from_millis(60);
/// ... and never more often than this.
const MIN_REFETCH: Duration = Duration::from_millis(300);
/// A window activation refreshes the focus run if it is older than this.
const ACTIVATE_AGE: Duration = Duration::from_secs(2);
/// Minimum time between two refetches of the same renamed focus.
const RENAME_GAP: Duration = Duration::from_millis(100);
const OUT_CAP: usize = 8;
const DIRTY_CAP: usize = 4;
const RING_CAP: usize = 8;

/// What a prefetched node must carry to serve a focus presentation (the Reader's fields).
const LIGHT: PropMask = PropMask::NAME
    .union(PropMask::VALUE)
    .union(PropMask::ROLE)
    .union(PropMask::STATES);
/// Ancestors also carry their child count (where-am-I).
const ANCESTORS: PropMask = LIGHT.union(PropMask::CHILDREN);

/// Policy settings, resolved by the binary (the core never reads the environment).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PrefetchConfig {
    /// Dump a verified-complete app (today only gnome-shell) with one `Cache.GetItems` after the
    /// first focus. Off until the compositor main-loop stall of a dump is measured.
    pub shell_dump: bool,
}

/// Fx-style hasher: the keys are small integers, SipHash would dominate a step at the caps.
#[derive(Default)]
struct Fx(u64);

impl Fx {
    fn add(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.add(u64::from(*b));
        }
    }
    fn write_u32(&mut self, i: u32) {
        self.add(u64::from(i));
    }
    fn write_u64(&mut self, i: u64) {
        self.add(i);
    }
}

type Map<K, V> = HashMap<K, V, BuildHasherDefault<Fx>>;

/// A contiguous stretch of one parent's children, in child order.
#[derive(Debug)]
struct Run {
    keys: Vec<NodeKey>,
    /// When each key was last fetched by this policy (parallel to `keys`).
    stamps: Vec<Instant>,
    at: Instant,
    dirty: bool,
    used: u64,
    /// The run starts at the parent's first child / ends at its last.
    at_start: bool,
    at_end: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Dump {
    /// Not eligible, refuted, unsupported or not configured: lazy only.
    Off,
    /// Eligible, no focus presented yet.
    Idle,
    /// Wait until `at` (no focus move for `DUMP_QUIET`), then ask.
    Armed {
        at: Instant,
    },
    InFlight,
    /// Mirrored at `at`.
    Done {
        at: Instant,
    },
}

#[derive(Debug)]
struct Out {
    op: FillOp,
    class: BudgetClass,
    at: Instant,
}

/// The product policy. See the module documentation.
#[derive(Debug)]
pub struct FocusPrefetch {
    /// Parent -> its run.
    runs: Map<NodeKey, Run>,
    /// Node -> the parent whose run holds it.
    member: Map<NodeKey, NodeKey>,
    used: u64,
    out: SmallVec<[Out; 8]>,
    /// The last focus and its ancestors up to its window (or as far as the mirror knows).
    chain: SmallVec<[NodeKey; MAX_CHAIN]>,
    last_focus: Option<NodeKey>,
    retry_focus: Option<(NodeKey, u8)>,
    recent_anc: SmallVec<[(NodeKey, Instant); RING_CAP]>,
    recent_win: SmallVec<[(NodeKey, Instant); RING_CAP]>,
    /// A dirty run of the focus is refetched at this time.
    refetch_due: Option<Instant>,
    /// Parents whose child list is too long to list (a listing was refused, or the mirror knows
    /// the count): their windows are read by index.
    big: SmallVec<[NodeKey; RING_CAP]>,
    /// Parents that reported a change while a window for them was in flight: the run that reply
    /// installs predates the change and starts dirty.
    inflight_dirty: SmallVec<[NodeKey; DIRTY_CAP]>,
    /// The parent and node of the last focus (by key: indices shift when a run grows backwards), and
    /// which way the focus travels (look-ahead direction).
    prev_pos: Option<(NodeKey, NodeKey)>,
    back: bool,
    /// Parents whose run went dirty since the last refetch (at most `DIRTY_CAP`; the rest stay
    /// dirty and are refreshed when the focus returns).
    dirty_parents: SmallVec<[NodeKey; DIRTY_CAP]>,
    /// The focus was renamed: refetch it, at most once per `RENAME_GAP`.
    rename: Option<NodeKey>,
    last_rename: Option<(NodeKey, Instant)>,
    dump: Dump,
    backoff_until: Option<Instant>,
}

impl Default for FocusPrefetch {
    fn default() -> Self {
        Self::new(PrefetchConfig::default())
    }
}

/// The Reader's `focus_need` (B-1 5.2) as far as a prefetch is concerned.
fn need_of(n: &NodeSnapshot) -> PropMask {
    let mut need = LIGHT;
    if !n.interfaces.is_empty() && !n.interfaces.intersects(Ifaces::VALUE | Ifaces::TEXT) {
        need.remove(PropMask::VALUE);
    }
    if n.name.as_deref().is_none_or(str::is_empty) {
        need |= PropMask::RELATIONS;
    }
    need
}

fn child_count(n: &NodeSnapshot) -> Option<usize> {
    match &n.children {
        ChildList::Known(l) => Some(l.len()),
        ChildList::Count(c) => usize::try_from(*c).ok(),
        ChildList::Unknown => None,
    }
}

fn is_window(r: Role) -> bool {
    matches!(r, Role::Window | Role::Dialog)
}

fn recently(ring: &[(NodeKey, Instant)], k: NodeKey, now: Instant, gap: Duration) -> bool {
    ring.iter()
        .any(|(n, at)| *n == k && now.saturating_duration_since(*at) < gap)
}

fn remember(ring: &mut SmallVec<[(NodeKey, Instant); RING_CAP]>, k: NodeKey, now: Instant) {
    if ring.len() >= RING_CAP {
        ring.remove(0);
    }
    ring.push((k, now));
}

impl FocusPrefetch {
    pub fn new(cfg: PrefetchConfig) -> Self {
        Self {
            runs: Map::default(),
            member: Map::default(),
            used: 0,
            out: SmallVec::new(),
            chain: SmallVec::new(),
            last_focus: None,
            retry_focus: None,
            recent_anc: SmallVec::new(),
            recent_win: SmallVec::new(),
            refetch_due: None,
            big: SmallVec::new(),
            inflight_dirty: SmallVec::new(),
            prev_pos: None,
            back: false,
            dirty_parents: SmallVec::new(),
            rename: None,
            last_rename: None,
            dump: if cfg.shell_dump {
                Dump::Idle
            } else {
                Dump::Off
            },
            backoff_until: None,
        }
    }

    /// May this class be sent at all in this health (MA-9, I-4) and backoff state?
    fn can(&self, cx: &FillCtx<'_>, class: BudgetClass) -> bool {
        let health = match cx.health {
            HealthView::Hung | HealthView::Gone => false,
            // Slow: only Interactive. Flooded: the same, since an app that floods us with events
            // gets no prefetch: every window is a read of its main loop that the flood already
            // keeps busy (B-1 review F-2).
            HealthView::Slow | HealthView::Flooded => matches!(class, BudgetClass::Interactive),
            HealthView::Healthy => true,
        };
        health
            && (matches!(class, BudgetClass::Interactive)
                || self.backoff_until.is_none_or(|t| cx.now >= t))
    }

    /// Queue one request if it is admitted by health, our own outstanding bound and the slots.
    fn ask(
        &mut self,
        cx: &FillCtx<'_>,
        out: &mut FillEffects,
        op: FillOp,
        class: BudgetClass,
    ) -> bool {
        if !self.can(cx, class) || self.out.iter().any(|o| o.op == op) || self.out.len() >= OUT_CAP
        {
            return false;
        }
        let same = |c: BudgetClass| self.out.iter().filter(|o| o.class == c).count();
        let admitted = match class {
            BudgetClass::Interactive => cx.slots.interactive > 0,
            BudgetClass::Prefetch => {
                cx.slots.prefetch > 0 && cx.slots.tokens > 0 && same(class) < MAX_OUT_PREFETCH
            }
            BudgetClass::Background | BudgetClass::Startup => {
                cx.slots.background > 0 && cx.slots.tokens > 0 && same(class) < 1
            }
        };
        if !admitted {
            return false;
        }
        self.out.push(Out {
            op: op.clone(),
            class,
            at: cx.now,
        });
        out.push(FillEffect::Request { op, class });
        true
    }

    fn on_events(&mut self, cx: &FillCtx<'_>, evs: &[NodeEvent]) {
        for ev in evs {
            match ev {
                NodeEvent::ChildrenChanged { node } => self.note_dirty(cx.now, *node),
                NodeEvent::PropertiesChanged { node, mask } => {
                    if cx.focus == Some(*node)
                        && mask.intersects(PropMask::NAME | PropMask::DESCRIPTION | PropMask::ROLE)
                        && !mask.intersects(PropMask::STATES | PropMask::VALUE)
                    {
                        self.rename = Some(*node);
                    }
                }
                NodeEvent::NodeRemoved { node } => {
                    if let Some(p) = self.member.get(node).copied() {
                        self.note_dirty(cx.now, p);
                    }
                }
                _ => {}
            }
        }
    }

    /// A structure event on a parent that has a run: the run is dirty. After a debounce the
    /// focus run is refetched; the run of any other parent (or one whose focus is gone) is
    /// dropped and the parent re-read once, so its child count is not left stale.
    fn note_dirty(&mut self, now: Instant, parent: NodeKey) {
        if self.window_in_flight(parent)
            && !self.inflight_dirty.contains(&parent)
            && self.inflight_dirty.len() < DIRTY_CAP
        {
            self.inflight_dirty.push(parent);
        }
        let Some(run) = self.runs.get_mut(&parent) else {
            return;
        };
        run.dirty = true;
        let due = (now + DEBOUNCE).max(run.at + MIN_REFETCH);
        self.refetch_due = Some(self.refetch_due.map_or(due, |d| d.min(due)));
        if !self.dirty_parents.contains(&parent) && self.dirty_parents.len() < DIRTY_CAP {
            self.dirty_parents.push(parent);
        }
    }

    fn window_in_flight(&self, parent: NodeKey) -> bool {
        self.out
            .iter()
            .any(|o| matches!(&o.op, FillOp::Siblings { parent: p, .. } if *p == parent))
    }

    fn focus_parent(&self) -> Option<NodeKey> {
        self.last_focus.and_then(|f| self.member.get(&f).copied())
    }

    fn on_resync(&mut self, cx: &FillCtx<'_>, dirty: &DirtySet) {
        if dirty.is_all() {
            let parents: Vec<NodeKey> = self.runs.keys().copied().collect();
            for p in parents {
                self.note_dirty(cx.now, p);
            }
        } else {
            for n in dirty.iter() {
                self.note_dirty(cx.now, n);
            }
        }
        if let Dump::Done { at } = self.dump
            && cx.now.saturating_duration_since(at) >= cx.limits.dump_interval
        {
            self.dump = Dump::Armed {
                at: cx.now + DUMP_QUIET,
            };
        }
    }

    fn on_demand(
        &mut self,
        cx: &FillCtx<'_>,
        node: NodeKey,
        why: DemandWhy,
        outcome: DemandOutcome,
        out: &mut FillEffects,
    ) {
        let hit = matches!(outcome, DemandOutcome::Hit);
        match why {
            DemandWhy::WhereAmI => {
                if !hit || !self.walk_chain(cx, node) {
                    self.ask_ancestry(cx, node, BudgetClass::Interactive, out);
                }
            }
            DemandWhy::Focus
            | DemandWhy::StateChange
            | DemandWhy::NavStep
            | DemandWhy::Revalidate => {}
        }
    }

    fn on_focus(&mut self, cx: &FillCtx<'_>, node: NodeKey, out: &mut FillEffects) {
        self.last_focus = Some(node);
        match self.dump {
            Dump::Idle | Dump::Armed { .. } => {
                self.dump = Dump::Armed {
                    at: cx.now + DUMP_QUIET,
                };
            }
            Dump::Off | Dump::InFlight | Dump::Done { .. } => {}
        }
        if !self.refresh_around(cx, node, false, out) {
            // The Reader's reply may not be in the mirror yet: look again shortly.
            self.retry_focus = Some((node, FOCUS_RETRIES));
        }
    }

    /// The user is back: the focus window is refreshed once (silent changes while they were
    /// away), unless it was fetched a moment ago.
    fn on_window(&mut self, cx: &FillCtx<'_>, out: &mut FillEffects) {
        if cx.dump_complete && !matches!(self.dump, Dump::Off) {
            return;
        }
        if let Some(f) = self.last_focus {
            self.refresh_around(cx, f, true, out);
        }
    }

    /// Follow the parents of `node` through the mirror into `self.chain`; true when the chain
    /// reaches a window (or the application root) with names and roles mirrored.
    fn walk_chain(&mut self, cx: &FillCtx<'_>, node: NodeKey) -> bool {
        self.chain.clear();
        let mut cur = node;
        for _ in 0..MAX_CHAIN {
            self.chain.push(cur);
            let Some(v) = cx.mirror.view(cur).hit() else {
                return false;
            };
            if is_window(v.snap.role) || v.snap.role == Role::Application {
                return true;
            }
            match v.snap.parent {
                Some(p) => cur = p,
                None => return false,
            }
        }
        false
    }

    fn ask_ancestry(
        &mut self,
        cx: &FillCtx<'_>,
        node: NodeKey,
        class: BudgetClass,
        out: &mut FillEffects,
    ) {
        if matches!(class, BudgetClass::Prefetch)
            && recently(&self.recent_anc, node, cx.now, ASK_GAP)
        {
            return;
        }
        if self.ask(
            cx,
            out,
            FillOp::Ancestry {
                node,
                max_depth: ANCESTRY_DEPTH,
                want: ANCESTORS,
            },
            class,
        ) {
            remember(&mut self.recent_anc, node, cx.now);
        }
    }

    /// Warm what the user is likely to reach next from `node`. False when `node` is not mirrored
    /// yet.
    fn refresh_around(
        &mut self,
        cx: &FillCtx<'_>,
        node: NodeKey,
        force: bool,
        out: &mut FillEffects,
    ) -> bool {
        let Some(v) = cx.mirror.view(node).hit() else {
            return false;
        };
        let complete = self.walk_chain(cx, node);
        // A dump-eligible app that is configured for it is lazy until its dump is in (no
        // sibling windows on the application main loop that is about to be dumped), and a dump
        // covers the node it carried.
        if cx.dump_complete {
            match self.dump {
                Dump::Done { .. } if !v.satisfies(need_of(&v.snap)) || v.aged => {}
                Dump::Off => {}
                Dump::Idle | Dump::Armed { .. } | Dump::InFlight | Dump::Done { .. } => {
                    return true;
                }
            }
        }
        // The ancestors up to the window, so a where-am-I is answered from the mirror: only the
        // part of the chain the mirror lacks (from its first unmirrored level, once per level).
        if !complete && let Some(top) = self.chain.last().copied() {
            self.ask_ancestry(cx, top, BudgetClass::Prefetch, out);
        }
        let Some(parent) = v.snap.parent else {
            // The ancestry brings the parent link; its reply continues from here.
            return true;
        };
        self.ensure_window(cx, node, parent, force, out);
        true
    }

    /// A `Siblings` request, already inside the driver's bounds (so what the policy remembers
    /// of it is exactly what comes back), read by index when the parent's list is long.
    fn siblings_op(
        &self,
        cx: &FillCtx<'_>,
        parent: NodeKey,
        node: NodeKey,
        before: u32,
        after: u32,
    ) -> FillOp {
        let cap = cx.limits.max_child_range;
        let before = before.min(cap);
        let after = after.min(cap.saturating_sub(before));
        let long = self.big.contains(&parent)
            || cx
                .mirror
                .view(parent)
                .hit()
                .and_then(|v| child_count(&v.snap))
                .is_some_and(|c| c > INDEXED_ABOVE);
        FillOp::Siblings {
            parent,
            node,
            before,
            after,
            indexed: long,
            want: LIGHT,
        }
    }

    /// The growth of a run, as clamped into the driver's bounds.
    fn extend_by(cx: &FillCtx<'_>) -> u32 {
        EXTEND.min(cx.limits.max_child_range)
    }

    /// Make sure the mirror holds fresh siblings around `node`: grow the run near its open edge,
    /// refetch it when stale, start one when there is none.
    fn ensure_window(
        &mut self,
        cx: &FillCtx<'_>,
        node: NodeKey,
        parent: NodeKey,
        force: bool,
        out: &mut FillEffects,
    ) {
        self.used += 1;
        let used = self.used;
        let in_run = self.member.get(&node) == Some(&parent);
        let Some(run) = self.runs.get_mut(&parent).filter(|_| in_run) else {
            // The first window of a parent: remember where the focus was, so that the next step
            // already knows which way it travels.
            self.prev_pos = Some((parent, node));
            self.ask_window(cx, parent, node, false, out);
            return;
        };
        run.used = used;
        let idx = run.keys.iter().position(|k| *k == node).unwrap_or(0);
        let len = run.keys.len();
        let age = cx.now.saturating_duration_since(run.at);
        let dirty = run.dirty;
        let stale = (dirty && age >= MIN_REFETCH) || (force && age > ACTIVATE_AGE);
        // Which way is the focus travelling in this run? Look ahead of it that way.
        // The previous focus is tracked by key: a run that grows backwards shifts every index.
        let prev_idx = match self.prev_pos {
            Some((p, k)) if p == parent => run.keys.iter().position(|x| *x == k),
            _ => None,
        };
        let back = match prev_idx {
            Some(i) if idx != i => idx < i,
            _ => self.back,
        };
        self.back = back;
        self.prev_pos = Some((parent, node));
        // The stretch of siblings ahead of the focus that is old or stale: one request.
        let span = if back {
            idx.saturating_sub(LOOK)..idx
        } else {
            idx + 1..(idx + LOOK + 1).min(len)
        };
        let mut ahead: Option<(usize, usize)> = None;
        for j in span {
            let (Some(k), Some(at)) = (run.keys.get(j), run.stamps.get(j)) else {
                break;
            };
            let old = cx.now.saturating_duration_since(*at) > LOOK_AGE
                || !cx
                    .mirror
                    .view(*k)
                    .hit()
                    .is_some_and(|v| v.satisfies(NEAR_NEED));
            if old {
                ahead = Some(ahead.map_or((j, j), |(f, l)| (f.min(j), l.max(j))));
            }
        }
        let ahead = ahead.and_then(|(f, l)| Some((*run.keys.get(f)?, u32::try_from(l - f).ok()?)));
        let first = run.keys.first().copied();
        let last = run.keys.last().copied();
        let (at_start, at_end) = (run.at_start, run.at_end);
        if stale {
            self.ask_window(cx, parent, node, dirty, out);
            return;
        }
        if dirty {
            let due = cx.now + MIN_REFETCH.saturating_sub(age);
            self.refetch_due = Some(self.refetch_due.map_or(due, |d| d.min(due)));
        }
        if let Some((anchor, after)) = ahead {
            let op = self.siblings_op(cx, parent, anchor, 0, after);
            self.ask(cx, out, op, BudgetClass::Prefetch);
        }
        let (edge_back, edge_fwd) = if back {
            (EDGE_TRAVEL, EDGE_OTHER)
        } else {
            (EDGE_OTHER, EDGE_TRAVEL)
        };
        if idx < edge_back
            && !at_start
            && let Some(first) = first
        {
            self.ask_extend(cx, parent, first, true, out);
        }
        if len - idx <= edge_fwd
            && !at_end
            && let Some(last) = last
        {
            self.ask_extend(cx, parent, last, false, out);
        }
    }

    /// A fresh window around `node`. `bypass` skips the per-node gap (a dirty run, after its
    /// debounce).
    fn ask_window(
        &mut self,
        cx: &FillCtx<'_>,
        parent: NodeKey,
        node: NodeKey,
        bypass: bool,
        out: &mut FillEffects,
    ) {
        if !bypass && recently(&self.recent_win, node, cx.now, ASK_GAP) {
            return;
        }
        let op = self.siblings_op(cx, parent, node, BEFORE, AFTER);
        if self.ask(cx, out, op, BudgetClass::Prefetch) {
            remember(&mut self.recent_win, node, cx.now);
        }
    }

    /// Grow the run past `anchor`, its first (`backwards`) or last key.
    fn ask_extend(
        &mut self,
        cx: &FillCtx<'_>,
        parent: NodeKey,
        anchor: NodeKey,
        backwards: bool,
        out: &mut FillEffects,
    ) {
        let by = Self::extend_by(cx);
        let (before, after) = if backwards { (by, 0) } else { (0, by) };
        let op = self.siblings_op(cx, parent, anchor, before, after);
        self.ask(cx, out, op, BudgetClass::Prefetch);
    }

    fn drop_run(&mut self, parent: NodeKey) {
        if let Some(run) = self.runs.remove(&parent) {
            for k in &run.keys {
                if self.member.get(k) == Some(&parent) {
                    self.member.remove(k);
                }
            }
        }
    }

    /// Store `keys` as the run of `parent`, evicting the least recently used run when full.
    fn install_run(
        &mut self,
        parent: NodeKey,
        keys: &[NodeKey],
        flags: (bool, bool),
        now: Instant,
    ) {
        self.drop_run(parent);
        while self.runs.len() >= MAX_RUNS {
            let victim = self
                .runs
                .iter()
                .min_by_key(|(_, r)| r.used)
                .map(|(p, _)| *p);
            let Some(v) = victim else { break };
            self.drop_run(v);
        }
        self.used += 1;
        let was_dirty = self
            .inflight_dirty
            .iter()
            .position(|p| *p == parent)
            .map(|i| self.inflight_dirty.remove(i))
            .is_some();
        if was_dirty {
            let due = now + DEBOUNCE;
            self.refetch_due = Some(self.refetch_due.map_or(due, |d| d.min(due)));
            if !self.dirty_parents.contains(&parent) && self.dirty_parents.len() < DIRTY_CAP {
                self.dirty_parents.push(parent);
            }
        }
        let keys: Vec<NodeKey> = keys.iter().copied().take(RUN_CAP).collect();
        for k in &keys {
            self.member.insert(*k, parent);
        }
        self.runs.insert(
            parent,
            Run {
                stamps: vec![now; keys.len()],
                keys,
                at: now,
                dirty: was_dirty,
                used: self.used,
                at_start: flags.0,
                at_end: flags.1,
            },
        );
    }

    /// Grow an existing run with `keys` (the reply of an extension; `keys` includes the anchor).
    fn grow_run(
        &mut self,
        parent: NodeKey,
        anchor: NodeKey,
        keys: &[NodeKey],
        backwards: bool,
        asked: u32,
        now: Instant,
    ) {
        let anchored = self.runs.get(&parent).is_some_and(|run| {
            if backwards {
                run.keys.first() == Some(&anchor) && keys.last() == Some(&anchor)
            } else {
                run.keys.last() == Some(&anchor) && keys.first() == Some(&anchor)
            }
        });
        let Some(run) = self.runs.get_mut(&parent).filter(|_| anchored) else {
            // The list moved under us: start again from the next focus.
            self.drop_run(parent);
            return;
        };
        let fresh = keys.len().saturating_sub(1);
        let added: &[NodeKey] = if backwards {
            keys.get(..fresh).unwrap_or(&[])
        } else {
            keys.get(1..).unwrap_or(&[])
        };
        if backwards {
            let mut v = added.to_vec();
            v.append(&mut run.keys);
            run.keys = v;
            let mut st = vec![now; added.len()];
            st.append(&mut run.stamps);
            run.stamps = st;
            run.at_start = fresh < asked as usize;
        } else {
            run.keys.extend_from_slice(added);
            run.stamps.extend(std::iter::repeat_n(now, added.len()));
            run.at_end = fresh < asked as usize;
        }
        let mut gone: Vec<NodeKey> = Vec::new();
        while run.keys.len() > RUN_CAP {
            // Drop the end far from the growth.
            if backwards {
                run.at_end = false;
                run.stamps.pop();
                gone.extend(run.keys.pop());
            } else {
                run.at_start = false;
                run.stamps.remove(0);
                gone.push(run.keys.remove(0));
            }
        }
        for k in added {
            self.member.insert(*k, parent);
        }
        for k in gone {
            if self.member.get(&k) == Some(&parent) {
                self.member.remove(&k);
            }
        }
    }

    /// The reply re-read siblings that are already in the run: note when.
    fn refresh_run(&mut self, parent: NodeKey, keys: &[NodeKey], now: Instant) {
        let Some(run) = self.runs.get_mut(&parent) else {
            return;
        };
        for k in keys {
            if let Some(i) = run.keys.iter().position(|x| x == k)
                && let Some(st) = run.stamps.get_mut(i)
            {
                *st = now;
            }
        }
    }

    fn on_reply(
        &mut self,
        cx: &FillCtx<'_>,
        op: &FillOp,
        result: &FillResult<'_>,
        out: &mut FillEffects,
    ) {
        self.out.retain(|o| &o.op != op);
        if let FillOp::Siblings {
            parent, indexed, ..
        } = op
        {
            // A listing that was refused (or failed) on a parent not read by index: from now
            // on this parent is read by index.
            if matches!(result, FillResult::Failed) && !*indexed && !self.big.contains(parent) {
                if self.big.len() >= RING_CAP {
                    self.big.remove(0);
                }
                self.big.push(*parent);
            }
        }
        let is_dump = matches!(op, FillOp::CacheDump { .. });
        match result {
            FillResult::Applied { keys, .. } => self.on_applied(cx, op, keys, out),
            FillResult::Gone | FillResult::Unresolved => self.on_failed(cx, op, false),
            FillResult::Timeout | FillResult::Failed => self.on_failed(cx, op, true),
            FillResult::NotSupported | FillResult::Refused(RefuseReason::DumpIneligible) => {
                if is_dump {
                    self.dump = Dump::Off;
                }
            }
            FillResult::Refused(why) => {
                if is_dump {
                    let wait = if matches!(why, RefuseReason::DumpTooSoon) {
                        cx.limits.dump_interval
                    } else {
                        DUMP_QUIET * 4
                    };
                    self.dump = Dump::Armed { at: cx.now + wait };
                }
            }
            FillResult::Cancelled => {}
        }
        // The run installed (if any) took the change that arrived in flight; none left to take.
        if let FillOp::Siblings { parent, .. } = op
            && !self.window_in_flight(*parent)
        {
            self.inflight_dirty.retain(|p| p != parent);
        }
    }

    fn on_failed(&mut self, cx: &FillCtx<'_>, op: &FillOp, backoff: bool) {
        if backoff {
            self.backoff_until = Some(cx.now + BACKOFF);
        }
        match op {
            FillOp::CacheDump { .. } => {
                self.dump = Dump::Armed {
                    at: cx.now + cx.limits.dump_interval,
                };
            }
            FillOp::Siblings { parent, .. } => {
                if !backoff {
                    self.drop_run(*parent);
                }
            }
            FillOp::Fetch { .. }
            | FillOp::ChildSnapshots { .. }
            | FillOp::Ancestry { .. }
            | FillOp::Subtree { .. } => {}
        }
    }

    fn on_applied(
        &mut self,
        cx: &FillCtx<'_>,
        op: &FillOp,
        keys: &[NodeKey],
        out: &mut FillEffects,
    ) {
        match op {
            FillOp::Siblings {
                parent,
                node,
                before,
                after,
                ..
            } => {
                let Some(pos) = keys.iter().position(|k| k == node) else {
                    // The node is no longer a child of `parent` (or was lost): no run.
                    self.drop_run(*parent);
                    return;
                };
                match (*before > 0, *after > 0) {
                    (true, true) => {
                        let flags = (
                            pos < *before as usize,
                            keys.len().saturating_sub(pos + 1) < *after as usize,
                        );
                        self.install_run(*parent, keys, flags, cx.now);
                    }
                    (true, false) => self.grow_run(*parent, *node, keys, true, *before, cx.now),
                    (false, true) if *after == Self::extend_by(cx) => {
                        self.grow_run(*parent, *node, keys, false, *after, cx.now);
                    }
                    (false, true | false) => self.refresh_run(*parent, keys, cx.now),
                }
            }
            FillOp::Ancestry { node, .. } => {
                if self.last_focus == Some(*node) {
                    self.refresh_around(cx, *node, false, out);
                }
            }
            FillOp::CacheDump { .. } => self.dump = Dump::Done { at: cx.now },
            FillOp::Fetch { .. } | FillOp::ChildSnapshots { .. } | FillOp::Subtree { .. } => {}
        }
    }

    fn pump(&mut self, cx: &FillCtx<'_>, out: &mut FillEffects) {
        let now = cx.now;
        if let Some((n, left)) = self.retry_focus {
            if cx.mirror.view(n).hit().is_some() {
                self.retry_focus = None;
                if self.last_focus == Some(n) {
                    self.refresh_around(cx, n, false, out);
                }
            } else if left == 0 || self.last_focus != Some(n) {
                self.retry_focus = None;
            } else {
                self.retry_focus = Some((n, left - 1));
            }
        }
        if self.refetch_due.is_some_and(|t| t <= now) {
            self.refetch_due = None;
            let parents = std::mem::take(&mut self.dirty_parents);
            let focus_parent = self.focus_parent();
            let focus_ok = self
                .last_focus
                .is_some_and(|f| cx.mirror.view(f).hit().is_some());
            for p in parents {
                if focus_ok && focus_parent == Some(p) {
                    continue;
                }
                self.drop_run(p);
                self.ask(
                    cx,
                    out,
                    FillOp::Fetch {
                        node: p,
                        want: ANCESTORS,
                    },
                    BudgetClass::Prefetch,
                );
            }
            if let Some(f) = self.last_focus {
                self.refresh_around(cx, f, false, out);
            }
        }
        if let Some(n) = self.rename {
            let wait = self
                .last_rename
                .filter(|(k, _)| *k == n)
                .map(|(_, at)| at + RENAME_GAP)
                .filter(|t| *t > now);
            if wait.is_none() {
                self.rename = None;
                self.last_rename = Some((n, now));
                self.ask(
                    cx,
                    out,
                    FillOp::Fetch {
                        node: n,
                        want: LIGHT,
                    },
                    BudgetClass::Interactive,
                );
            }
        }
        if let Dump::Armed { at } = self.dump
            && now >= at
            && self.ask(
                cx,
                out,
                FillOp::CacheDump {
                    max_nodes: cx.limits.max_dump_nodes,
                },
                BudgetClass::Background,
            )
        {
            self.dump = Dump::InFlight;
        }
    }

    fn schedule(&self, cx: &FillCtx<'_>, out: &mut FillEffects) {
        let now = cx.now;
        let mut wake: Option<Instant> = None;
        let mut want = |t: Instant| wake = Some(wake.map_or(t, |w| w.min(t)));
        if let Dump::Armed { at } = self.dump
            && !matches!(cx.health, HealthView::Hung | HealthView::Gone)
        {
            want(at.max(now));
        }
        if let Some(t) = self.refetch_due {
            want(t.max(now));
        }
        if let Some(n) = self.rename
            && let Some((k, at)) = self.last_rename
            && k == n
        {
            want((at + RENAME_GAP).max(now));
        }
        if self.retry_focus.is_some() {
            want(now + Duration::from_millis(25));
        }
        if let Some(t) = wake {
            out.push(FillEffect::WakeAt(t));
        }
    }
}

impl FillPolicy for FocusPrefetch {
    fn name(&self) -> &'static str {
        "focus-prefetch"
    }

    fn step(&mut self, cx: &FillCtx<'_>, input: FillInput<'_>, out: &mut FillEffects) {
        // The connector's claim can be refuted mid-run (IC-22): lazy from then on.
        if !cx.dump_complete && !matches!(self.dump, Dump::Off) {
            self.dump = Dump::Off;
        }
        self.out.retain(|o| {
            cx.now.saturating_duration_since(o.at) < o.class.default_timeout() * OUT_TTL_FACTOR
        });
        match input {
            FillInput::Appeared { .. } | FillInput::Ready | FillInput::Tick => {}
            FillInput::Events(evs) => self.on_events(cx, evs),
            FillInput::Resync { dirty } => self.on_resync(cx, dirty),
            FillInput::Demand { node, why, outcome } => self.on_demand(cx, node, why, outcome, out),
            FillInput::FocusSettled { node } => self.on_focus(cx, node, out),
            FillInput::WindowActivated { .. } => self.on_window(cx, out),
            FillInput::Reply { op, result, .. } => self.on_reply(cx, op, &result, out),
        }
        self.pump(cx, out);
        self.schedule(cx, out);
    }

    fn memory_bytes(&self) -> usize {
        // Every SmallVec is capped at its inline size by construction, so only the maps and the
        // run key vectors live on the heap.
        let keys: usize = self
            .runs
            .values()
            .map(|r| {
                r.keys.capacity() * size_of::<NodeKey>()
                    + r.stamps.capacity() * size_of::<Instant>()
            })
            .sum();
        let table = |cap: usize, entry: usize| cap * (entry + 1);
        size_of::<Self>()
            + keys
            + table(self.runs.capacity(), size_of::<(NodeKey, Run)>())
            + table(self.member.capacity(), size_of::<(NodeKey, NodeKey)>())
    }
}
