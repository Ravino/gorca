// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-app ingress: a bounded lock-free MPSC ring in front of a consumer-side coalescing table
//! (B-2 candidate A).
//!
//! * `push` appends the event to a bounded `ArrayQueue` (one CAS, no lock, no allocation) and
//!   wakes the consumer when it is idle. It does no hashing and no coalescing.
//! * The coalescing (SPEC 4.2) is applied by whoever folds the ring into the table: the
//!   consumer in `drain()`, or a producer that finds the ring full (bounded chunk) or that
//!   serves an app already holding more than a ring's worth of slots. The fold applies the
//!   records in ring order, so a single producer observes exactly the sequence of states of the
//!   executable model, wherever the fold happens.
//! * Hard bounds: the ring holds at most [`RING_CAP`] records; the table is bounded by
//!   `max_keys`, `hard_cap` and the shared [`SlotBudget`] (SPEC 3.6, 3.7).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};

use crossbeam_queue::ArrayQueue;
use gorca_model::{NodeEvent, NodeKey, RawEvent};
use gorca_rt::Notify;

use crate::dirty::DirtySet;
use crate::table::{Meta, Shed, Table, Taken};

/// ADR-0001 section 6.3. `ActiveDescendant` is an addition: the ADR's list omits it although
/// `NodeEvent::ActiveDescendantChanged` exists; it is last-writer-wins per container.
///
/// The set of keys the coalescer merges on. The table indexes them by (node, class) directly;
/// this enum names them for documentation and for consumers of the key set.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CoalesceKey {
    /// Last-writer-wins, one slot per app.
    Focus,
    /// OR the `PropMask`; value fetched lazily.
    Props(NodeKey),
    /// Collapse. TBD: ancestor `ChildrenChanged` subsuming descendants (depth-limited check).
    Children(NodeKey),
    /// `TextDelta` merge.
    Text(NodeKey),
    /// Last-writer-wins.
    Caret(NodeKey),
    Selection(NodeKey),
    ActiveDescendant(NodeKey),
    /// Never coalesced, but rate-capped per drain.
    Announcement(u64),
    Window(NodeKey),
    Document(NodeKey),
    Removed(NodeKey),
}

/// Default [`IngressConfig::hard_cap`].
pub const HARD_CAP: usize = 65_536;

/// Records the lock-free ring holds before a producer folds some into the table.
pub const RING_CAP: usize = 1024;
/// Records a producer folds when it finds the ring full (bounds its critical section).
const HELP_CHUNK: usize = 1024;
/// Budget units a producer takes from the shared counter at once for its ring (and the slack
/// kept when units are given back), so a push usually touches only per-app atomics: one
/// compare-and-swap on the global counter per push made four producers wait on each other.
const RING_CREDIT_BLOCK: usize = 16;
/// Records folded between two returns of ring charges.
const FOLD_GROUP: usize = 16;
/// Failed attempts a producer spins, then yields, before it takes the lock and folds itself.
const SPIN_TRIES: u32 = 256;
const YIELD_TRIES: u32 = 8;
/// Records the consumer folds per lock acquisition while draining.
const DRAIN_CHUNK: usize = 256;
/// An app holding at least this many slots folds at push time (it is already degraded, and the
/// global accounting stays exact for it).
const EAGER_AT: usize = RING_CAP;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IngressConfig {
    /// Distinct keys per app before the app enters `Flooded` (R-3). ADR: 4096.
    pub max_keys: usize,
    /// Announcements kept per drain; extra ones are dropped and counted.
    pub announcement_cap: usize,
    /// More deltas than this on one node collapse to `Replaced` (ADR: 64).
    pub max_text_deltas: usize,
    /// Hard bound on pending slots, lossless classes included (PC-7, SPEC 4.4 rule H), and on
    /// the text deltas queued beyond the first of each slot (SPEC AU-4). A new key beyond it
    /// sets the dirty set to "everything" instead of queueing.
    pub hard_cap: usize,
    /// Per-app reserved minimum `R` (AU-3): the first `R` pending units of an app (table slots,
    /// text deltas and ring records together, focus excluded) are charged to the budget without
    /// competing for the pool, so an app holding at most `R` is never affected by others.
    /// Sized above `announcement_cap` so announcements stay protected next to other events.
    pub reserved_slots: usize,
}

/// Default [`IngressConfig::reserved_slots`].
pub const RESERVED_SLOTS: usize = 32;

impl Default for IngressConfig {
    fn default() -> Self {
        Self {
            max_keys: 4096,
            announcement_cap: 16,
            max_text_deltas: 64,
            hard_cap: HARD_CAP,
            reserved_slots: RESERVED_SLOTS,
        }
    }
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct IngressStats {
    pub pushed: u64,
    /// Events merged into an existing slot or superseded.
    pub coalesced: u64,
    /// Events turned into a dirty bit because the app was `Flooded`.
    pub dirtied: u64,
    pub announcements_dropped: u64,
    /// Number of focus events folded into the single focus slot (R-1 statistics).
    pub focus_sequence: u64,
    pub flooded: bool,
}

/// What one drain delivers to the app actor.
#[derive(Debug, Default)]
pub struct Drained {
    /// Surviving events, in order of first occurrence (R-2).
    pub events: Vec<RawEvent>,
    /// Present when the app was `Flooded`: nodes to resync lazily.
    pub dirty: Option<DirtySet>,
    pub stats: IngressStats,
    /// Large buffers of the drained table, freed when the batch is dropped so the draining
    /// thread does not pay for `munmap` (SPEC AU-6).
    #[allow(dead_code)]
    pub(crate) garbage: Garbage,
}

#[derive(Debug, Default)]
#[allow(dead_code)]
pub(crate) struct Garbage {
    pub(crate) nodes: Option<crate::table::NodeMap>,
    pub(crate) meta: Vec<Meta>,
    pub(crate) aux: Vec<smallvec::SmallVec<[gorca_model::TextDelta; 2]>>,
    /// The table of an app that was preempted while the reaper was backed up (ADR-0004 section
    /// 8): freed here, with the batch, on the thread that drains the victim.
    pub(crate) shed: Vec<Shed>,
}

/// Process-wide bound on pending slots across many ingresses (hostile many-app floods).
///
/// Shared by every ingress created with [`ingress_with_budget`]. Each queued slot, each queued
/// text delta beyond the first of a slot and each record still in the lock-free ring is charged
/// once, and returned when it is drained, superseded, turned into a dirty mark, or when the
/// ingress is dropped (AU-1): the sum over all ingresses of their pending units is exactly
/// [`SlotBudget::used`], ring included, whenever the ingresses are observed.
///
/// Two kinds of units (AU-3, AU-7). The **reserve**: every app may hold up to
/// [`IngressConfig::reserved_slots`] (`R`) units, table and ring together, outside the pool;
/// a per-app counter, moved by compare-and-swap, never lets it pass `R`. The **pool**: above
/// `R`, a unit is taken with a compare-and-swap that never passes `limit`; the newest `Focus`
/// slot of an app may exceed it by one. Hence, however many threads push,
/// `used() <= limit + apps * (R + 1)` at every instant (both counts live in one atomic word).
/// An app holding at most `R` units is never affected by others. An app above `R` can only be
/// preempted (flooded, dirty-all, newest Focus kept), never silently dropped from.
/// An app holding fewer than 64 units that finds the pool exhausted preempts another app that
/// holds more than it and more than `R`; the victim's buffers go to the reaper thread, or to the
/// victim's next drain when the reaper is backed up (AU-6, ADR-0004 section 8).
#[derive(Debug)]
pub struct SlotBudget {
    /// Low 32 bits: pool units. High 32 bits: reserve units of all apps.
    units: AtomicU64,
    limit: usize,
    /// Apps whose weight exceeded `R` at their last update: the only possible victims.
    over: Mutex<OverSet>,
    over_len: AtomicUsize,
    /// Frees the buffers of preempted tables; ends when the budget is dropped.
    reaper: Option<SyncSender<Shed>>,
    reaper_fallbacks: AtomicU64,
    inline_frees: AtomicU64,
}

/// The apps that hold more than `R`, behind a mutex that is only ever held for a few loads and
/// stores: no `Arc` is dropped, no other lock taken and nothing allocated-and-freed under it
/// except the vector's own growth.
#[derive(Debug, Default)]
struct OverSet {
    /// `(Shared::id, app)`; the id lets an entry be found even when its slot hint went stale.
    apps: Vec<(usize, Weak<Shared>)>,
    cursor: usize,
}

const POOL_MASK: u64 = 0xFFFF_FFFF;
/// Largest pool the packed counter can represent.
const POOL_MAX: usize = (u32::MAX - 1) as usize;
const RESERVE_ONE: u64 = 1 << 32;
/// Candidates looked at by one preemption: victim choice is O(1), the largest of a window.
const VICTIM_SAMPLE: usize = 64;

/// Default global pending-slot budget (about 25 MB of slots).
pub const GLOBAL_SLOT_BUDGET: usize = 262_144;

/// Garbage handed to the reaper before the hand-over falls back to the victim's next drain.
const REAPER_QUEUE: usize = 256;

const NOT_OVER: usize = usize::MAX;

/// Preempted tables an app carries to its next drain when the reaper is backed up.
pub const MAX_PARKED: usize = 4;

impl SlotBudget {
    /// A pool of `limit` units on top of the per-app reserves. Starts the reaper thread
    /// (`gorca-reaper`); it ends when the budget is dropped.
    pub fn new(limit: usize) -> Arc<Self> {
        Self::build(limit, true)
    }

    fn build(limit: usize, with_reaper: bool) -> Arc<Self> {
        let (tx, rx) = sync_channel::<Shed>(REAPER_QUEUE);
        let reaper = with_reaper
            .then(|| {
                std::thread::Builder::new()
                    .name("gorca-reaper".to_owned())
                    .spawn(move || while rx.recv().map(drop).is_ok() {})
                    .ok()
                    .map(|_| tx)
            })
            .flatten();
        Arc::new(Self {
            units: AtomicU64::new(0),
            limit: limit.min(POOL_MAX),
            over: Mutex::new(OverSet::default()),
            over_len: AtomicUsize::new(0),
            reaper,
            reaper_fallbacks: AtomicU64::new(0),
            inline_frees: AtomicU64::new(0),
        })
    }

    /// Slots currently charged to the budget (the sum over all ingresses of their pending units:
    /// table slots, queued text deltas and records in the ring), reserve and pool together.
    pub fn used(&self) -> usize {
        let v = self.units.load(Ordering::Relaxed);
        usize::try_from((v & POOL_MASK) + (v >> 32)).unwrap_or(usize::MAX)
    }

    /// The configured pool limit.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Times a preempted table could not be handed to the reaper and went to the victim's
    /// next drain instead.
    pub fn reaper_fallbacks(&self) -> u64 {
        self.reaper_fallbacks.load(Ordering::Relaxed)
    }

    /// Preempted tables that had to be freed on the preempting thread because the victim already
    /// carried [`MAX_PARKED`] of them (a stalled app preempted over and over while the reaper is
    /// backed up).
    pub fn inline_frees(&self) -> u64 {
        self.inline_frees.load(Ordering::Relaxed)
    }

    /// Units currently taken from the pool (without the per-app reserves).
    pub fn pool_used(&self) -> usize {
        (self.units.load(Ordering::Relaxed) & POOL_MASK) as usize
    }

    pub(crate) fn pool_full(&self) -> bool {
        self.pool_used() >= self.limit
    }

    /// Take up to `n` reserve units for an app whose reserve counter is `app` (capped at `cap`).
    /// One compare-and-swap on the app's counter and one add on the global word.
    pub(crate) fn take_reserve(&self, app: &AtomicUsize, cap: usize, n: usize) -> usize {
        let mut cur = app.load(Ordering::Relaxed);
        loop {
            let take = n.min(cap.saturating_sub(cur));
            if take == 0 {
                return 0;
            }
            match app.compare_exchange_weak(cur, cur + take, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => {
                    self.units
                        .fetch_add(take as u64 * RESERVE_ONE, Ordering::AcqRel);
                    return take;
                }
                Err(now) => cur = now,
            }
        }
    }

    /// Take up to `n` pool units in one compare-and-swap, never passing the limit.
    pub(crate) fn take_pool(&self, n: usize) -> usize {
        let mut cur = self.units.load(Ordering::Relaxed);
        loop {
            let take = n.min(self.limit.saturating_sub((cur & POOL_MASK) as usize));
            if take == 0 {
                return 0;
            }
            match self.units.compare_exchange_weak(
                cur,
                cur + take as u64,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return take,
                Err(now) => cur = now,
            }
        }
    }

    /// Take one pool unit if the pool is not exhausted.
    pub(crate) fn try_charge(&self) -> bool {
        self.take_pool(1) == 1
    }

    /// Charge the newest focus slot of an app that finds the pool exhausted. The overshoot is
    /// at most one unit per app (the `+ 1` of the bound).
    pub(crate) fn force_charge(&self) {
        self.units.fetch_add(1, Ordering::AcqRel);
    }

    /// Return `pool` pool units and `reserve` units of the app counter `app`.
    pub(crate) fn release_split(&self, pool: usize, reserve: usize, app: &AtomicUsize) {
        // The global word first: it never exceeds the sum of the app counters, so a unit being
        // handed back is never counted twice by a take that lands in between.
        let d = pool as u64 + reserve as u64 * RESERVE_ONE;
        if d > 0 {
            let prev = self.units.fetch_sub(d, Ordering::AcqRel);
            debug_assert!(
                (prev & POOL_MASK) >= pool as u64 && (prev >> 32) >= reserve as u64,
                "budget underflow"
            );
        }
        if reserve > 0 {
            app.fetch_sub(reserve, Ordering::AcqRel);
        }
    }

    fn over(&self) -> MutexGuard<'_, OverSet> {
        self.over.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn over_insert(&self, sh: &Shared) {
        let mut o = self.over();
        if sh.over_slot.load(Ordering::Relaxed) != NOT_OVER {
            return;
        }
        sh.over_slot.store(o.apps.len(), Ordering::Relaxed);
        o.apps.push((sh.id, sh.this.clone()));
        self.over_len.store(o.apps.len(), Ordering::Relaxed);
    }

    fn over_remove(&self, sh: &Shared) {
        // Declared before the guard, so it is dropped after it: if this is the last strong
        // reference, `Drop for Shared` calls back into this function and must find the mutex free.
        // An app that was never over `R` (nearly all of them) is not in the set: no lock, no scan.
        if sh.over_slot.load(Ordering::Relaxed) == NOT_OVER {
            return;
        }
        let moved: Option<Arc<Shared>>;
        let mut o = self.over();
        let hint = sh.over_slot.swap(NOT_OVER, Ordering::Relaxed);
        let slot = match o.apps.get(hint) {
            Some((id, _)) if *id == sh.id => Some(hint),
            // The hint went stale (a dying neighbour could not be told it moved): search.
            _ => o.apps.iter().position(|(id, _)| *id == sh.id),
        };
        let Some(slot) = slot else {
            return;
        };
        drop(o.apps.swap_remove(slot));
        moved = o.apps.get(slot).and_then(|(_, w)| w.upgrade());
        if let Some(m) = &moved {
            m.over_slot.store(slot, Ordering::Relaxed);
        }
        self.over_len.store(o.apps.len(), Ordering::Relaxed);
    }

    /// The largest of a window of the apps that hold more than `R`, other than `me`. Looks at no
    /// more than `VICTIM_SAMPLE` entries and takes no lock of an app, so it costs the same
    /// whatever the number of apps, and nothing when nobody is over (SPEC AU-6).
    fn pick_victim(&self, me: usize) -> Option<Arc<Shared>> {
        if self.over_len.load(Ordering::Relaxed) == 0 {
            return None;
        }
        // The upgraded handles are dropped after the mutex is released: dropping the last one
        // runs `Drop for Shared`, which takes the mutex.
        let mut window: smallvec::SmallVec<[Arc<Shared>; VICTIM_SAMPLE]> =
            smallvec::SmallVec::new();
        {
            let mut o = self.over();
            let n = o.apps.len();
            if n == 0 {
                return None;
            }
            let start = o.cursor % n;
            o.cursor = o.cursor.wrapping_add(VICTIM_SAMPLE);
            for i in 0..n.min(VICTIM_SAMPLE) {
                if let Some((id, w)) = o.apps.get((start + i) % n)
                    && *id != me
                    && let Some(s) = w.upgrade()
                {
                    window.push(s);
                }
            }
        }
        let best = window
            .iter()
            .enumerate()
            .max_by_key(|(_, s)| s.weight())
            .map(|(i, _)| i)?;
        Some(window.swap_remove(best))
    }

    /// Free `g` on the reaper thread; when that is backed up (or gone) park it with the victim
    /// for its next drain. Never freed on the caller's critical path.
    fn dispose(&self, g: Shed, owner: &Shared) {
        if !g.is_large() {
            return;
        }
        let rejected = match &self.reaper {
            Some(tx) => match tx.try_send(g) {
                Ok(()) => None,
                Err(TrySendError::Full(g) | TrySendError::Disconnected(g)) => Some(g),
            },
            None => Some(g),
        };
        if let Some(g) = rejected {
            self.reaper_fallbacks.fetch_add(1, Ordering::Relaxed);
            owner.park(g);
        }
    }

    /// Take the slots of the largest holder (other than `me`) back, if it holds more than
    /// `floor` units. Returns false when there is nobody to take from. Only `try_lock` is used
    /// on the victim, so two preempting apps cannot deadlock; `tries` bounds how long a busy
    /// victim is waited for (its critical sections are short and wait on nothing).
    pub(crate) fn preempt(&self, me: usize, floor: usize, tries: u32) -> bool {
        for attempt in 0..tries.max(1) {
            if !self.pool_full() {
                return true;
            }
            let Some(v) = self.pick_victim(me) else {
                return false;
            };
            if v.weight() <= floor {
                return false;
            }
            let shed = match v.table.try_lock() {
                Ok(mut g) => Some(v.shed(&mut g)),
                Err(TryLockError::Poisoned(p)) => Some(v.shed(&mut p.into_inner())),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some((freed, garbage)) = shed {
                self.dispose(garbage, &v);
                return freed > 0;
            }
            if attempt % 8 == 7 {
                std::thread::yield_now();
            } else {
                std::hint::spin_loop();
            }
        }
        false
    }
}

/// Keeps a hot atomic on its own cache lines.
#[derive(Debug, Default)]
#[repr(align(128))]
struct Padded<T>(T);

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

#[derive(Debug)]
struct Shared {
    id: usize,
    cfg: IngressConfig,
    ring: ArrayQueue<RawEvent>,
    /// A wake-up for the consumer is outstanding (set by the first push after a drain).
    armed: AtomicBool,
    /// The receiver is gone: pushes are discarded and charge nothing (SPEC AU-2).
    closed: AtomicBool,
    /// Budget units the table holds, for the eager path and for preemption.
    held_hint: AtomicUsize,
    /// Units taken from the budget on behalf of the ring (AU-1, AU-3), packed: low 32 bits the
    /// credit, high 32 bits how much of it came from the reserve. At least the number of
    /// records in the ring (give or take the producers racing for the last unit), at most a
    /// couple of blocks more; equal to it whenever the ingress is observed (`settle`).
    ring_credit: Padded<AtomicU64>,
    /// Reserve units this app holds, table and ring together, never above `R`.
    app_reserve: Arc<AtomicUsize>,
    /// Position in the budget's set of over-reserve apps, or `NOT_OVER`.
    over_slot: AtomicUsize,
    this: Weak<Shared>,
    /// Table of this app, preempted while the reaper was backed up (freed by the next drain).
    parked: Mutex<Vec<Shed>>,
    wake: Notify,
    table: Mutex<Table>,
    /// Meta vector recycled between drains (consumer only).
    spare: Mutex<Vec<Meta>>,
    budget: Option<Arc<SlotBudget>>,
}

impl Shared {
    /// What a preempting app sees of this one: table units plus ring records.
    fn weight(&self) -> usize {
        self.held_hint.load(Ordering::Relaxed) + self.ring.len()
    }

    /// Give back credit beyond the records in the ring plus `slack`, once it exceeds that by
    /// two slacks (the cache line is written by the producers too, so surplus returns in bulk).
    /// Pool units go back first, so the app keeps its reserve. Lock-free and safe to race: only
    /// the thread whose compare-and-swap wins releases units.
    fn trim_credit(&self, slack: usize) {
        let Some(b) = &self.budget else { return };
        let queued = self.ring.len();
        let v = self.ring_credit.0.load(Ordering::Acquire);
        let (credit, res) = ((v & POOL_MASK) as usize, (v >> 32) as usize);
        if credit > queued + 2 * slack {
            let keep = queued + slack;
            let k = credit - keep;
            let pool_part = k.min(credit - res);
            let res_part = k - pool_part;
            let new = keep as u64 | ((res - res_part) as u64) << 32;
            if self
                .ring_credit
                .0
                .compare_exchange(v, new, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                b.release_split(pool_part, res_part, &self.app_reserve);
            }
        }
    }

    /// Take `n` units of the ring credit for the records just popped: reserve units first.
    /// Returns `(pool, reserve)`; fewer than `n` when producers raced the credit down.
    fn take_credit(&self, n: usize) -> Option<(usize, usize)> {
        self.budget.as_ref()?;
        let mut v = self.ring_credit.0.load(Ordering::Acquire);
        loop {
            let (credit, res) = ((v & POOL_MASK) as usize, (v >> 32) as usize);
            let take = n.min(credit);
            if take == 0 {
                return None;
            }
            let res_take = take.min(res);
            let new = (credit - take) as u64 | ((res - res_take) as u64) << 32;
            match self.ring_credit.0.compare_exchange_weak(
                v,
                new,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some((take - res_take, res_take)),
                Err(now) => v = now,
            }
        }
    }

    /// Make `used()` exact for this ingress: credit equals the records in the ring.
    fn settle(&self) {
        self.trim_credit(0);
    }

    /// Make sure there is credit for one more record in the ring, leasing a block from the
    /// budget (reserve first) when it is used up. False when not even one unit is available.
    fn cover(&self, b: &SlotBudget) -> bool {
        let v = self.ring_credit.0.load(Ordering::Acquire);
        if self.ring.len() < (v & POOL_MASK) as usize {
            return true;
        }
        let res = b.take_reserve(
            &self.app_reserve,
            self.cfg.reserved_slots,
            RING_CREDIT_BLOCK,
        );
        let pool = b.take_pool(RING_CREDIT_BLOCK - res);
        if res + pool == 0 {
            return false;
        }
        self.ring_credit
            .0
            .fetch_add((res + pool) as u64 | (res as u64) << 32, Ordering::AcqRel);
        self.update_over();
        true
    }

    /// Keep the budget's set of over-reserve apps (the possible victims) up to date.
    fn update_over(&self) {
        let Some(b) = &self.budget else { return };
        let over = self.weight() > self.cfg.reserved_slots;
        if over == (self.over_slot.load(Ordering::Relaxed) != NOT_OVER) {
            return;
        }
        if over {
            b.over_insert(self);
        } else {
            b.over_remove(self);
        }
    }

    /// Keep a preempted table until this app's next drain, at most [`MAX_PARKED`] of them: a
    /// stalled app preempted again and again while the reaper is backed up must not accumulate
    /// memory without bound. Beyond that the table is freed right here, on the preempting
    /// thread. That breaks the 250 us bound (AU-6) for such a push, and only in this doubly
    /// pathological case (reaper queue full and a victim that never drains); unbounded memory
    /// would be worse than one slow push, and the event is counted (`inline_frees`).
    fn park(&self, g: Shed) {
        let mut p = self.parked.lock().unwrap_or_else(PoisonError::into_inner);
        if p.len() < MAX_PARKED {
            p.push(g);
            return;
        }
        drop(p);
        if let Some(b) = &self.budget {
            b.inline_frees.fetch_add(1, Ordering::Relaxed);
        }
        drop(g);
    }

    /// Discard every record of the ring (receiver gone, or preemption). Returns how many.
    fn discard_ring(&self) -> usize {
        let mut n = 0;
        while self.ring.pop().is_some() {
            n += 1;
        }
        self.settle();
        n
    }

    /// Preemption of this app (AU-3): its ring and table are given up, flooded and fully dirty.
    /// The table's buffers are returned to the caller to be freed outside every lock (AU-6).
    fn shed(&self, t: &mut Table) -> (usize, Shed) {
        // The newest Focus is lossless (ADR-0004 section 8): a Focus still in the ring survives
        // the preemption like the one in the table.
        let mut ring = 0;
        let mut focus = None;
        while let Some(ev) = self.ring.pop() {
            ring += 1;
            if matches!(ev, NodeEvent::FocusChanged { .. }) {
                focus = Some(ev);
            }
        }
        self.settle();
        let (freed, garbage) = t.shed(focus);
        self.held_hint.store(t.held(), Ordering::Relaxed);
        self.update_over();
        (freed + ring, garbage)
    }

    fn lock(&self) -> MutexGuard<'_, Table> {
        // A panic while holding the lock cannot leave the table in a state worse than a stale
        // slot: every operation keeps the counters consistent step by step.
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fold up to `limit` records of the ring into `t`. Returns how many were folded.
    fn fold_into(&self, t: &mut Table, limit: usize) -> usize {
        let mut n = 0;
        while n < limit {
            // Small groups: the prepaid units of a group go back with two atomic operations
            // instead of two per record (the counters are hot, producers add to them), and
            // before the table charges what it keeps, so a full pool cannot refuse a record
            // that merely changes form.
            let mut group: smallvec::SmallVec<[RawEvent; FOLD_GROUP]> = smallvec::SmallVec::new();
            while group.len() < FOLD_GROUP.min(limit - n) {
                let Some(ev) = self.ring.pop() else { break };
                group.push(ev);
            }
            if group.is_empty() {
                break;
            }
            // The credit of the records just taken moves to the table (reserve first), which
            // spends it on the slots it creates and gives back what it does not use: no unit
            // is released and charged again, so a full pool cannot refuse a record of an app
            // that is within its reserve (AU-3).
            if let Some((pool, res)) = self.take_credit(group.len()) {
                t.give_credit(pool, res);
            }
            n += group.len();
            for ev in group {
                t.push(&self.cfg, ev);
            }
            t.return_credit();
        }
        // O(n) check, sampled and limited to small tables so debug builds keep bounded pushes.
        debug_assert!(
            n == 0 || t.live() > 2048 || !t.pushed_total().is_multiple_of(509) || t.check().is_ok(),
            "{:?}",
            t.check()
        );
        self.trim_credit(RING_CREDIT_BLOCK);
        self.held_hint.store(t.held(), Ordering::Relaxed);
        if n > 0 {
            self.update_over();
        }
        n
    }

    /// The pool refused the ring charge: apply `ev` to the table right away, behind the records
    /// already queued (a producer in this state is a flood; it pays with the lock).
    fn push_direct(&self, ev: RawEvent) {
        let mut t = self.lock();
        self.fold_all(&mut t);
        if !self.closed.load(Ordering::Relaxed) {
            // The reserve and the pool are what this record competes for: give back idle credit.
            self.trim_credit(0);
            t.push(&self.cfg, ev);
            self.held_hint.store(t.held(), Ordering::Relaxed);
            self.update_over();
        }
        drop(t);
        self.arm();
    }

    fn fold_all(&self, t: &mut Table) {
        let snapshot = self.ring.len();
        self.fold_into(t, snapshot);
    }

    /// Set the wake flag and notify if it was not set.
    fn arm(&self) {
        if !self.armed.load(Ordering::SeqCst) && !self.armed.swap(true, Ordering::SeqCst) {
            self.wake.notify_one();
        }
    }
}

/// Create a connected sender and receiver pair for one app.
pub fn ingress(cfg: IngressConfig) -> (IngressSender, IngressReceiver) {
    ingress_inner(cfg, None)
}

/// Like [`ingress`], charging every pending slot to a budget shared with other ingresses.
pub fn ingress_with_budget(
    cfg: IngressConfig,
    budget: &Arc<SlotBudget>,
) -> (IngressSender, IngressReceiver) {
    ingress_inner(cfg, Some(Arc::clone(budget)))
}

fn ingress_inner(
    cfg: IngressConfig,
    budget: Option<Arc<SlotBudget>>,
) -> (IngressSender, IngressReceiver) {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let app_reserve = Arc::new(AtomicUsize::new(0));
    let shared = Arc::new_cyclic(|this| Shared {
        id,
        cfg,
        ring: ArrayQueue::new(RING_CAP),
        armed: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        held_hint: AtomicUsize::new(0),
        ring_credit: Padded(AtomicU64::new(0)),
        app_reserve: Arc::clone(&app_reserve),
        over_slot: AtomicUsize::new(NOT_OVER),
        this: this.clone(),
        parked: Mutex::new(Vec::new()),
        wake: Notify::new(),
        table: Mutex::new(Table::new(id, budget.clone(), app_reserve)),
        spare: Mutex::new(Vec::new()),
        budget,
    });
    (
        IngressSender {
            shared: Arc::clone(&shared),
        },
        IngressReceiver { shared },
    )
}

/// Producer side, held by the drain / connector. Cloneable.
#[derive(Clone, Debug)]
pub struct IngressSender {
    shared: Arc<Shared>,
}

/// Consumer side, held by the app actor.
#[derive(Debug)]
pub struct IngressReceiver {
    shared: Arc<Shared>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // Records pushed after the receiver was dropped, and their charges, die with the ring.
        let v = self.ring_credit.0.load(Ordering::Relaxed);
        let (credit, res) = ((v & POOL_MASK) as usize, (v >> 32) as usize);
        if let Some(b) = &self.budget {
            b.release_split(credit - res, res, &self.app_reserve);
            b.over_remove(self);
        }
    }
}

impl IngressSender {
    /// The receiver is gone (the application was retired or evicted): pushes are discarded. The
    /// drain uses this to drop a stale cached route and ask the router again.
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Acquire)
    }

    /// O(1) amortised, never awaits (C-1). One CAS on the ring in the common case; when the
    /// ring is full the caller folds a bounded chunk of it into the table first. Events pushed
    /// after the receiver was dropped are discarded.
    pub fn push(&self, ev: RawEvent) {
        let sh = &*self.shared;
        if sh.closed.load(Ordering::Relaxed) {
            return;
        }
        if let Some(b) = &sh.budget {
            // The record is charged before it is queued, so `used()` covers the ring (AU-1).
            if !sh.cover(b) {
                sh.push_direct(ev);
                return;
            }
        }
        let mut ev = ev;
        let mut waited = 0u32;
        loop {
            match sh.ring.push(ev) {
                Ok(()) => break,
                Err(back) => {
                    ev = back;
                    // Whoever holds the lock is folding, which frees ring slots one record at a
                    // time: wait for a slot, not for the end of its whole critical section. The
                    // wait is bounded (a spinning producer could keep a lock holder of lower
                    // priority off the CPU): spin, then yield, then take the lock and fold.
                    waited += 1;
                    let guard = match sh.table.try_lock() {
                        Ok(g) => Some(g),
                        Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
                        Err(TryLockError::WouldBlock) if waited <= SPIN_TRIES => {
                            std::hint::spin_loop();
                            None
                        }
                        Err(TryLockError::WouldBlock) if waited <= SPIN_TRIES + YIELD_TRIES => {
                            std::thread::yield_now();
                            None
                        }
                        Err(TryLockError::WouldBlock) => Some(sh.lock()),
                    };
                    if let Some(mut t) = guard {
                        sh.fold_into(&mut t, HELP_CHUNK);
                    }
                    if sh.closed.load(Ordering::Relaxed) {
                        return;
                    }
                }
            }
        }
        if sh.closed.load(Ordering::Relaxed) {
            // The receiver went away meanwhile: nobody will fold this record.
            sh.discard_ring();
            return;
        }
        if sh.held_hint.load(Ordering::Relaxed) >= EAGER_AT {
            // An app holding this much is already degraded: fold now so that the budget and
            // `pending()` are exact for it. If the consumer is draining, the ring is enough.
            let guard = match sh.table.try_lock() {
                Ok(g) => Some(g),
                Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some(mut t) = guard {
                sh.fold_all(&mut t);
            }
        }
        sh.arm();
    }
}

impl IngressReceiver {
    /// Take everything pending. Non-blocking. Leaves the ingress empty and out of `Flooded`.
    pub fn drain(&self) -> Drained {
        let sh = &*self.shared;
        // Clear the wake flag first: a push after this point either lands in this drain or sets
        // the flag again and notifies.
        sh.armed.store(false, Ordering::SeqCst);
        let spare = std::mem::take(&mut *sh.spare.lock().unwrap_or_else(PoisonError::into_inner));
        let mut remaining = sh.ring.len();
        let taken: Taken = loop {
            let mut t = sh.lock();
            let n = sh.fold_into(&mut t, remaining.min(DRAIN_CHUNK));
            remaining = if n == 0 {
                0
            } else {
                remaining.saturating_sub(n)
            };
            if remaining == 0 {
                let taken = t.take(spare);
                sh.held_hint.store(t.held(), Ordering::Relaxed);
                break taken;
            }
        };
        let (drained, meta) = taken.finish();
        *sh.spare.lock().unwrap_or_else(PoisonError::into_inner) = meta;
        if !sh.ring.is_empty() {
            sh.arm();
        }
        sh.settle();
        sh.update_over();
        let mut drained = drained;
        drained.garbage.shed =
            std::mem::take(&mut *sh.parked.lock().unwrap_or_else(PoisonError::into_inner));
        drained
    }

    /// Verify that every cap counter equals the real size of its structure (tests).
    pub fn check_invariants(&self) -> Result<(), String> {
        let sh = &*self.shared;
        let mut t = sh.lock();
        sh.fold_all(&mut t);
        sh.settle();
        t.check()
    }

    /// Resolves when something was pushed into an empty ingress.
    pub async fn notified(&self) {
        self.shared.wake.notified().await;
    }

    /// Budget units this ingress holds: its pending slots plus the queued text deltas, each
    /// charged once (SPEC AU-1/AU-4). At quiescence the sum over all ingresses of a budget is
    /// exactly [`SlotBudget::used`].
    pub fn charged_units(&self) -> usize {
        let sh = &*self.shared;
        let mut t = sh.lock();
        sh.fold_all(&mut t);
        sh.settle();
        t.held()
    }

    /// Pending slots (queued events not yet delivered, after coalescing).
    pub fn pending(&self) -> usize {
        let sh = &*self.shared;
        let mut t = sh.lock();
        sh.fold_all(&mut t);
        sh.settle();
        t.live()
    }
}

impl Drop for IngressReceiver {
    fn drop(&mut self) {
        let sh = &*self.shared;
        sh.closed.store(true, Ordering::SeqCst);
        {
            let mut t = sh.lock();
            sh.discard_ring();
            t.teardown();
            sh.held_hint.store(0, Ordering::Relaxed);
        }
        if let Some(b) = &sh.budget {
            b.over_remove(sh);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        clippy::uninlined_format_args
    )]
    use super::*;
    use gorca_model::{
        AppId, CaretOrigin, ConnectorId, LocalId, NodeEvent, Politeness, PropMask, TextDelta,
        WindowKind,
    };
    use std::sync::Arc;

    fn key(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 0,
            },
            local: LocalId(n),
        }
    }

    fn cfg(max_keys: usize) -> IngressConfig {
        IngressConfig {
            max_keys,
            ..IngressConfig::default()
        }
    }

    fn ann(i: u32) -> NodeEvent {
        NodeEvent::Announcement {
            node: None,
            text: Arc::from(format!("a{i}")),
            politeness: Politeness::Polite,
        }
    }

    fn ins(offset: u32) -> NodeEvent {
        NodeEvent::TextChanged {
            node: key(1),
            delta: TextDelta::Insert {
                offset,
                len: 1,
                text: None,
            },
        }
    }

    fn del(offset: u32) -> NodeEvent {
        NodeEvent::TextChanged {
            node: key(1),
            delta: TextDelta::Delete {
                offset,
                len: 1,
                text: None,
            },
        }
    }

    #[test]
    fn focus_is_last_writer_wins_and_keeps_position() {
        let (tx, rx) = ingress(IngressConfig::default());
        tx.push(NodeEvent::SelectionChanged { node: key(9) });
        tx.push(NodeEvent::FocusChanged { node: key(1) });
        tx.push(NodeEvent::FocusChanged { node: key(2) });
        tx.push(NodeEvent::FocusChanged { node: key(3) });
        let d = rx.drain();
        assert_eq!(d.events.len(), 2);
        assert!(matches!(d.events[0], NodeEvent::SelectionChanged { .. }));
        assert!(matches!(d.events[1], NodeEvent::FocusChanged { node } if node == key(3)));
        assert_eq!(d.stats.focus_sequence, 3);
        assert_eq!(rx.pending(), 0);
    }

    #[test]
    fn property_masks_are_ored() {
        let (tx, rx) = ingress(IngressConfig::default());
        for mask in [PropMask::NAME, PropMask::STATES, PropMask::NAME] {
            tx.push(NodeEvent::PropertiesChanged { node: key(1), mask });
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), 1);
        assert!(matches!(
            d.events[0],
            NodeEvent::PropertiesChanged { mask, .. } if mask == (PropMask::NAME | PropMask::STATES)
        ));
    }

    #[test]
    fn removed_supersedes_every_pending_slot_of_the_node() {
        let (tx, rx) = ingress(IngressConfig::default());
        tx.push(NodeEvent::PropertiesChanged {
            node: key(1),
            mask: PropMask::NAME,
        });
        tx.push(NodeEvent::SelectionChanged { node: key(1) });
        tx.push(NodeEvent::SelectionChanged { node: key(2) });
        tx.push(NodeEvent::NodeRemoved { node: key(1) });
        let d = rx.drain();
        assert_eq!(d.events.len(), 2);
        assert!(matches!(d.events[0], NodeEvent::SelectionChanged { node } if node == key(2)));
        assert!(matches!(d.events[1], NodeEvent::NodeRemoved { node } if node == key(1)));
    }

    #[test]
    fn delete_then_insert_collapses_to_replaced() {
        let (tx, rx) = ingress(IngressConfig::default());
        tx.push(del(0));
        tx.push(ins(0));
        tx.push(ins(5));
        let d = rx.drain();
        assert_eq!(d.events.len(), 1);
        assert!(matches!(
            d.events[0],
            NodeEvent::TextChanged {
                delta: TextDelta::Replaced { .. },
                ..
            }
        ));
    }

    #[test]
    fn more_than_64_deltas_collapse_to_replaced() {
        let (tx, rx) = ingress(IngressConfig::default());
        for i in 0..64 {
            tx.push(ins(i));
        }
        assert_eq!(rx.pending(), 1);
        let d = rx.drain();
        assert_eq!(d.events.len(), 64);
        for i in 0..65 {
            tx.push(ins(i));
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), 1);
    }

    #[test]
    fn announcements_are_never_coalesced_but_capped() {
        let (tx, rx) = ingress(IngressConfig {
            announcement_cap: 3,
            ..IngressConfig::default()
        });
        for i in 0..5 {
            tx.push(ann(i));
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), 3);
        assert_eq!(d.stats.announcements_dropped, 2);
        // The cap is per drain.
        tx.push(ann(9));
        assert_eq!(rx.drain().events.len(), 1);
    }

    #[test]
    fn overload_floods_and_keeps_only_the_essentials() {
        let (tx, rx) = ingress(cfg(4));
        tx.push(NodeEvent::FocusChanged { node: key(100) });
        for n in 0..4 {
            tx.push(NodeEvent::PropertiesChanged {
                node: key(n),
                mask: PropMask::NAME,
            });
        }
        // The fifth distinct key trips the bound.
        tx.push(NodeEvent::PropertiesChanged {
            node: key(50),
            mask: PropMask::NAME,
        });
        tx.push(NodeEvent::CaretMoved {
            node: key(100),
            offset: 3,
            origin: CaretOrigin::Unknown,
        });
        tx.push(NodeEvent::CaretMoved {
            node: key(7),
            offset: 3,
            origin: CaretOrigin::Unknown,
        });
        tx.push(NodeEvent::NodeRemoved { node: key(3) });
        let d = rx.drain();
        assert!(d.stats.flooded);
        assert_eq!(
            d.events.len(),
            3,
            "focus, focused caret, removed: {:?}",
            d.events
        );
        let dirty = d.dirty.unwrap();
        for n in [0, 1, 2, 50, 7] {
            assert!(dirty.contains(key(n)), "node {n} must be dirty");
        }
        // Back to normal after the drain.
        tx.push(NodeEvent::SelectionChanged { node: key(1) });
        let d = rx.drain();
        assert!(!d.stats.flooded && d.dirty.is_none() && d.events.len() == 1);
    }

    #[test]
    fn memory_is_bounded_under_a_storm_of_distinct_keys() {
        let (tx, rx) = ingress(cfg(64));
        for n in 0..100_000u64 {
            tx.push(NodeEvent::ChildrenChanged { node: key(n) });
        }
        assert!(rx.pending() <= 64);
    }

    #[test]
    fn a_ring_full_of_one_key_folds_without_loss() {
        let (tx, rx) = ingress(IngressConfig::default());
        for i in 0..(RING_CAP as u32 * 5 + 17) {
            tx.push(NodeEvent::CaretMoved {
                node: key(1),
                offset: i,
                origin: CaretOrigin::Unknown,
            });
        }
        let d = rx.drain();
        assert_eq!(d.events.len(), 1);
        assert_eq!(d.stats.pushed, u64::from(RING_CAP as u32 * 5 + 17));
        assert!(matches!(
            d.events[0],
            NodeEvent::CaretMoved { offset, .. } if offset == RING_CAP as u32 * 5 + 16
        ));
    }

    #[test]
    fn four_producers_and_a_consumer_lose_nothing() {
        let (tx, rx) = ingress(IngressConfig::default());
        let per = 20_000u32;
        let handles: Vec<_> = (0..4u64)
            .map(|p| {
                let tx = tx.clone();
                std::thread::spawn(move || {
                    for i in 0..per {
                        tx.push(NodeEvent::CaretMoved {
                            node: key(p),
                            offset: i,
                            origin: CaretOrigin::Unknown,
                        });
                    }
                })
            })
            .collect();
        let mut pushed = 0u64;
        let mut last = [0u32; 4];
        while handles.iter().any(|h| !h.is_finished()) {
            let d = rx.drain();
            pushed += d.stats.pushed;
            for e in d.events {
                if let NodeEvent::CaretMoved { node, offset, .. } = e {
                    let slot = &mut last[node.local.0 as usize];
                    assert!(offset >= *slot, "per-producer order violated");
                    *slot = offset;
                }
            }
        }
        for h in handles {
            h.join().unwrap();
        }
        let d = rx.drain();
        pushed += d.stats.pushed;
        assert_eq!(pushed, 4 * u64::from(per));
        for e in d.events {
            if let NodeEvent::CaretMoved { node, offset, .. } = e {
                assert_eq!(offset, per - 1, "node {:?} final offset", node);
            }
        }
    }

    #[test]
    fn concurrent_pushes_never_overshoot_the_global_budget() {
        let limit = 5_000;
        let budget = SlotBudget::new(limit);
        let apps = 6u32;
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (budget, stop) = (Arc::clone(&budget), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut worst = 0;
                while !stop.load(Ordering::Relaxed) {
                    worst = worst.max(budget.used());
                }
                worst
            })
        };
        let handles: Vec<_> = (0..apps)
            .map(|a| {
                let budget = Arc::clone(&budget);
                std::thread::spawn(move || {
                    let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
                    let app = AppId {
                        connector: ConnectorId(0),
                        serial: a,
                    };
                    for n in 0..40_000u64 {
                        tx.push(NodeEvent::WindowEvent {
                            node: NodeKey {
                                app,
                                local: LocalId(n),
                            },
                            kind: WindowKind::Create,
                        });
                        if n % 97 == 0 {
                            tx.push(NodeEvent::FocusChanged {
                                node: NodeKey {
                                    app,
                                    local: LocalId(n),
                                },
                            });
                        }
                        if n % 5_000 == 4_999 {
                            drop(rx.drain());
                        }
                    }
                    rx.check_invariants().unwrap();
                    drop(rx.drain());
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        let worst = watcher.join().unwrap();
        assert!(
            worst <= limit + apps as usize * (RESERVED_SLOTS + 1),
            "budget reached {worst}, limit {limit}: concurrent charges overshot"
        );
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn dropping_the_receiver_returns_the_budget() {
        let budget = SlotBudget::new(10_000);
        let (tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..500 {
            tx.push(NodeEvent::WindowEvent {
                node: key(n),
                kind: WindowKind::Create,
            });
        }
        assert_eq!(rx.pending(), 500);
        assert_eq!(budget.used(), 500);
        drop(rx);
        assert_eq!(budget.used(), 0);
        tx.push(NodeEvent::FocusChanged { node: key(1) });
        assert_eq!(budget.used(), 0);
    }

    /// F-4: with the reaper backed up (or gone), the preempting push does not free the victim's
    /// table; the victim's next drain carries it.
    #[test]
    fn a_backed_up_reaper_hands_the_garbage_to_the_victim() {
        let budget = SlotBudget::build(240, false);
        let (vtx, vrx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..250 {
            vtx.push(NodeEvent::WindowEvent {
                node: key(n),
                kind: WindowKind::Create,
            });
        }
        assert_eq!(vrx.pending(), 250);
        let (ftx, frx) = ingress_with_budget(IngressConfig::default(), &budget);
        for n in 0..200 {
            ftx.push(NodeEvent::WindowEvent {
                node: key(10_000 + n),
                kind: WindowKind::Create,
            });
        }
        assert!(budget.reaper_fallbacks() >= 1, "nobody was preempted");
        let d = vrx.drain();
        assert!(
            !d.garbage.shed.is_empty(),
            "the victim must carry its table"
        );
        drop(frx.drain());
    }

    /// R3-3: whatever the order in which apps go away (receiver first, sender first, a neighbour
    /// mid-removal), the over-reserve set ends empty and never keeps a dead entry.
    #[test]
    fn the_over_set_is_empty_after_any_drop_order() {
        let budget = SlotBudget::build(1_000_000, false);
        let mut apps: Vec<Option<(IngressSender, IngressReceiver)>> = (0..300u64)
            .map(|a| {
                let c = ingress_with_budget(IngressConfig::default(), &budget);
                for n in 0..60 {
                    c.0.push(NodeEvent::WindowEvent {
                        node: key(a * 1_000 + n),
                        kind: WindowKind::Create,
                    });
                }
                c.1.pending();
                Some(c)
            })
            .collect();
        assert_eq!(budget.over().apps.len(), 300);
        // A fixed pseudo-random order, receivers and senders separately.
        let mut x = 12345u64;
        let mut order: Vec<usize> = (0..300).collect();
        for i in (1..order.len()).rev() {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            order.swap(i, (x >> 33) as usize % (i + 1));
        }
        let mut senders = Vec::new();
        for (k, &i) in order.iter().enumerate() {
            if let Some((tx, rx)) = apps[i].take() {
                if k % 2 == 0 {
                    drop(rx);
                    senders.push(tx);
                } else {
                    drop(tx);
                    drop(rx);
                }
            }
        }
        drop(senders);
        let o = budget.over();
        assert!(o.apps.is_empty(), "{} dead entries", o.apps.len());
        assert_eq!(budget.over_len.load(Ordering::Relaxed), 0);
        drop(o);
        assert_eq!(budget.used(), 0);
    }

    /// A stalled victim preempted again and again while the reaper is backed up carries at most
    /// `MAX_PARKED` tables; the rest are freed in place and counted.
    #[test]
    fn parked_tables_are_bounded() {
        let budget = SlotBudget::build(1_000, false);
        let (_tx, rx) = ingress_with_budget(IngressConfig::default(), &budget);
        for _ in 0..10 {
            rx.shared.park(Shed::default());
        }
        assert_eq!(rx.shared.parked.lock().unwrap().len(), MAX_PARKED);
        assert_eq!(budget.inline_frees(), 10 - MAX_PARKED as u64);
        assert_eq!(rx.drain().garbage.shed.len(), MAX_PARKED);
    }

    #[tokio::test]
    async fn receiver_is_woken_by_the_first_push() {
        let (tx, rx) = ingress(IngressConfig::default());
        let waiter = tokio::spawn(async move {
            rx.notified().await;
            rx.drain().events.len()
        });
        tokio::task::yield_now().await;
        tx.push(NodeEvent::FocusChanged { node: key(1) });
        assert_eq!(waiter.await.unwrap(), 1);
    }
}
