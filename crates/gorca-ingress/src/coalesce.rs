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

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, Weak};

use crossbeam_queue::ArrayQueue;
use gorca_model::{NodeKey, RawEvent};
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
const HELP_CHUNK: usize = 512;
/// Budget units a producer takes from the shared counter at once for its ring (and the slack
/// kept when units are given back), so a push usually touches only per-app atomics: one
/// compare-and-swap on the global counter per push made four producers wait on each other.
const RING_CREDIT_BLOCK: usize = 16;
/// Records folded between two returns of ring charges.
const FOLD_GROUP: usize = 16;
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
}

/// Process-wide bound on pending slots across many ingresses (hostile many-app floods).
///
/// Shared by every ingress created with [`ingress_with_budget`]. Each queued slot, each queued
/// text delta beyond the first of a slot and each record still in the lock-free ring is charged
/// once, and returned when it is drained, superseded, turned into a dirty mark, or when the
/// ingress is dropped (AU-1): the sum over all ingresses of their pending units is exactly
/// [`SlotBudget::used`], ring included.
///
/// Fairness (AU-3): every app owns a reserved minimum of [`IngressConfig::reserved_slots`]
/// (`R`) units, ring records included, that never compete for the pool. Above `R` a unit comes
/// from the pool through a compare-and-swap that never exceeds `limit` (AU-7); an app holding
/// less than 64 units that finds the pool exhausted takes the slots of a larger holder, of any
/// size above `R`, back: the victim is flooded and marked dirty-all (never a silent loss) and its
/// buffers are freed on a reaper thread (AU-6). Hence `used() <= limit + apps * (R + 1)`, the
/// `+ 1` being the newest `Focus` slot.
#[derive(Debug)]
pub struct SlotBudget {
    used: AtomicUsize,
    limit: usize,
    registry: Mutex<Vec<Weak<Shared>>>,
    /// Lazily started thread that frees the buffers of preempted tables; it ends when the budget
    /// is dropped (the sender is its only owner-side handle).
    reaper: OnceLock<Option<SyncSender<Reapable>>>,
}

type Reapable = Box<dyn Send + 'static>;

/// Default global pending-slot budget (about 25 MB of slots).
pub const GLOBAL_SLOT_BUDGET: usize = 262_144;

/// Garbage handed to the reaper before the hand-over falls back to freeing in place.
const REAPER_QUEUE: usize = 256;

impl SlotBudget {
    /// A pool of `limit` units on top of the per-app reserves.
    pub fn new(limit: usize) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            limit,
            registry: Mutex::new(Vec::new()),
            reaper: OnceLock::new(),
        })
    }

    /// Slots currently charged to the budget (the sum over all ingresses of their pending units:
    /// table slots, queued text deltas and records in the ring).
    pub fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    /// The configured limit.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Take up to `n` units in one atomic operation each: `reserved` of them without looking at
    /// the pool, the rest from the pool without exceeding the limit. Returns how many.
    pub(crate) fn charge_up_to(&self, n: usize, reserved: usize) -> usize {
        let free = n.min(reserved);
        if free > 0 {
            self.used.fetch_add(free, Ordering::AcqRel);
        }
        let want = n - free;
        if want == 0 {
            return free;
        }
        let mut cur = self.used.load(Ordering::Relaxed);
        loop {
            let take = want.min(self.limit.saturating_sub(cur));
            if take == 0 {
                return free;
            }
            match self.used.compare_exchange_weak(
                cur,
                cur + take,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return free + take,
                Err(now) => cur = now,
            }
        }
    }

    /// Charge a unit of the reserve to an app whose reserved units in use are `own < reserve`.
    pub(crate) fn admit(&self, own: usize, reserve: usize) -> bool {
        if own < reserve {
            self.used.fetch_add(1, Ordering::AcqRel);
            true
        } else {
            false
        }
    }

    /// Free `g` on the reaper thread when it is large; in place otherwise or when the reaper
    /// is unavailable or busy. Nothing is ever retained.
    pub(crate) fn dispose(&self, g: Shed) {
        if !g.is_large() {
            return;
        }
        let tx = self.reaper.get_or_init(|| {
            let (tx, rx): (SyncSender<Reapable>, Receiver<Reapable>) = sync_channel(REAPER_QUEUE);
            std::thread::Builder::new()
                .name("gorca-ingress-reap".to_owned())
                .spawn(move || while rx.recv().map(drop).is_ok() {})
                .ok()
                .map(|_| tx)
        });
        if let Some(tx) = tx {
            // A full or closed channel hands the garbage back inside the error: dropped here.
            drop(tx.try_send(Box::new(g)));
        }
    }

    /// Take one unit if the pool is not exhausted. Never exceeds the limit, whatever the
    /// number of concurrent callers.
    pub(crate) fn try_charge(&self) -> bool {
        let mut cur = self.used.load(Ordering::Relaxed);
        loop {
            if cur >= self.limit {
                return false;
            }
            match self
                .used
                .compare_exchange_weak(cur, cur + 1, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(now) => cur = now,
            }
        }
    }

    /// Charge the focus slot of an app that holds no other unit and finds the pool exhausted.
    /// The overshoot is at most one unit per app and only when `limit` is below the app count.
    pub(crate) fn force_charge(&self) {
        self.used.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn release(&self, n: usize) {
        let prev = self.used.fetch_sub(n, Ordering::AcqRel);
        debug_assert!(prev >= n, "budget underflow: used {prev}, releasing {n}");
    }

    fn registry(&self) -> MutexGuard<'_, Vec<Weak<Shared>>> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn register(&self, sh: &Arc<Shared>) {
        let mut r = self.registry();
        r.retain(|w| w.strong_count() > 0);
        r.push(Arc::downgrade(sh));
    }

    fn unregister(&self, id: usize) {
        self.registry()
            .retain(|w| w.upgrade().is_some_and(|s| s.id != id));
    }

    /// Take the slots of the largest holder (other than `me`) back, if it holds more than
    /// `floor` units and more than the reserve. Only `try_lock` is used on the victim, so two
    /// preempting apps cannot deadlock; `tries` bounds how long a busy victim is waited for (a
    /// victim's critical sections are short and never wait on anything). The victim's buffers go
    /// to the reaper, so this costs a registry scan and a little bookkeeping whatever the
    /// victim's size (AU-6).
    pub(crate) fn preempt(&self, me: usize, floor: usize, tries: u32) -> bool {
        for attempt in 0..tries.max(1) {
            if self.used() < self.limit {
                return true;
            }
            let victim = {
                let r = self.registry();
                r.iter()
                    .filter_map(Weak::upgrade)
                    .filter(|s| s.id != me)
                    .max_by_key(|s| s.weight())
            };
            let Some(v) = victim else { return false };
            if v.weight() <= floor {
                return false;
            }
            let shed = match v.table.try_lock() {
                Ok(mut g) => Some(v.shed(&mut g)),
                Err(TryLockError::Poisoned(p)) => Some(v.shed(&mut p.into_inner())),
                Err(TryLockError::WouldBlock) => None,
            };
            if let Some((freed, garbage)) = shed {
                self.dispose(garbage);
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
    /// Units taken from the budget on behalf of the ring (AU-1, AU-3): at least the number of
    /// records in the ring (give or take the producers racing for the last unit), at most a
    /// couple of blocks more; equal to it whenever the ingress is observed (`settle`).
    ring_credit: Padded<AtomicUsize>,
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
    /// Lock-free and safe to race: only the thread whose compare-and-swap wins releases units.
    fn trim_credit(&self, slack: usize) {
        let Some(b) = &self.budget else { return };
        let queued = self.ring.len();
        let credit = self.ring_credit.0.load(Ordering::Acquire);
        if credit > queued + 2 * slack {
            let keep = queued + slack;
            if self
                .ring_credit
                .0
                .compare_exchange(credit, keep, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                b.release(credit - keep);
            }
        }
    }

    /// Make `used()` exact for this ingress: credit equals the records in the ring.
    fn settle(&self) {
        self.trim_credit(0);
    }

    /// Make sure there is credit for one more record in the ring, taking a block from the budget
    /// when it is used up. False when the pool refuses even one unit.
    fn cover(&self, b: &SlotBudget) -> bool {
        let credit = self.ring_credit.0.load(Ordering::Acquire);
        if self.ring.len() < credit {
            return true;
        }
        let held = self.held_hint.load(Ordering::Relaxed);
        let room = self.cfg.reserved_slots.saturating_sub(credit + held);
        let got = b.charge_up_to(RING_CREDIT_BLOCK, room);
        if got == 0 {
            return false;
        }
        self.ring_credit.0.fetch_add(got, Ordering::AcqRel);
        true
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
        let ring = self.discard_ring();
        let (freed, garbage) = t.shed();
        self.held_hint.store(t.held(), Ordering::Relaxed);
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
        t.ring_units = self.ring_credit.0.load(Ordering::Relaxed);
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
            // A full pool needs the units of the records just taken at once.
            if self.budget.as_ref().is_some_and(|b| b.used() >= b.limit()) {
                self.trim_credit(0);
            }
            t.ring_units = self.ring_credit.0.load(Ordering::Relaxed);
            n += group.len();
            for ev in group {
                t.push(&self.cfg, ev);
            }
        }
        // O(n) check, sampled and limited to small tables so debug builds keep bounded pushes.
        debug_assert!(
            n == 0 || t.live() > 2048 || !t.pushed_total().is_multiple_of(509) || t.check().is_ok(),
            "{:?}",
            t.check()
        );
        self.trim_credit(RING_CREDIT_BLOCK);
        self.held_hint.store(t.held(), Ordering::Relaxed);
        n
    }

    /// The pool refused the ring charge: apply `ev` to the table right away, behind the records
    /// already queued (a producer in this state is a flood; it pays with the lock).
    fn push_direct(&self, ev: RawEvent) {
        let mut t = self.lock();
        self.fold_all(&mut t);
        if !self.closed.load(Ordering::Relaxed) {
            t.ring_units = self.ring_credit.0.load(Ordering::Relaxed);
            t.push(&self.cfg, ev);
            self.held_hint.store(t.held(), Ordering::Relaxed);
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
    let shared = Arc::new(Shared {
        id,
        cfg,
        ring: ArrayQueue::new(RING_CAP),
        armed: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        held_hint: AtomicUsize::new(0),
        ring_credit: Padded(AtomicUsize::new(0)),
        wake: Notify::new(),
        table: Mutex::new(Table::new(id, budget.clone())),
        spare: Mutex::new(Vec::new()),
        budget,
    });
    if let Some(b) = &shared.budget {
        b.register(&shared);
    }
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
        let left = self.ring_credit.0.load(Ordering::Relaxed);
        if let Some(b) = &self.budget {
            b.release(left);
        }
    }
}

impl IngressSender {
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
        loop {
            match sh.ring.push(ev) {
                Ok(()) => break,
                Err(back) => {
                    ev = back;
                    // Whoever holds the lock is folding, which frees ring slots one record at a
                    // time: wait for a slot, not for the end of its whole critical section.
                    let guard = match sh.table.try_lock() {
                        Ok(g) => Some(g),
                        Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
                        Err(TryLockError::WouldBlock) => None,
                    };
                    match guard {
                        Some(mut t) => {
                            sh.fold_into(&mut t, HELP_CHUNK);
                        }
                        None => std::hint::spin_loop(),
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
            b.unregister(sh.id);
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
