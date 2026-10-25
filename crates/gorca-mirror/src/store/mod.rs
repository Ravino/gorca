// SPDX-License-Identifier: GPL-3.0-or-later
//! The store: `scc::HashIndex<NodeKey, Arc<ArcSwap<Entry>>>` per app (decision MS-3), one writer
//! (the app actor), any number of lock-free readers.
//!
//! The value is an `ArcSwap` cell, not the entry: `HashIndex` updates an existing key by
//! inserting the new value and then marking the old one removed, and a concurrent reader can miss
//! the key in between (measured: 16 % of reads of a key under continuous update). A cell is
//! swapped in place, the index structure is touched only when a key appears or disappears.
//!
//! - **MS-1** Reads never block and never allocate on a hit (one `Arc` clone).
//! - **MS-2** Per-node consistency only: a node is published whole.
//! - A snapshot is immutable once published; an invalidation publishes a new snapshot whose
//!   `fresh` is `Stale { mask }`. The app-wide stale epoch (`invalidate_all`) is O(1): a node
//!   confirmed before it reads as `Stale(ALL)`.
//! - **Late-reply rules (B-1 5.4).** Every write takes a sequence number from a counter that a
//!   request records as `issued_at`. A reply issued before a tombstone never resurrects the key
//!   (LR-1); a reply issued before an invalidation is stored `Stale` (LR-2); a successful reply
//!   issued after a tombstone is proof of life and revives the key (LR-3).

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Instant;

use arc_swap::ArcSwap;
use gorca_model::sem::{SemEntry, SemText, SrcRecord};
use gorca_model::{
    AppId, ChildList, Freshness, FreshnessEvent, Lookup, NodeKey, NodeSem, NodeSnapshot, NodeView,
    PropMask, SnapshotBatch, Value,
};

use crate::limits::{
    GLOBAL_MIRROR_BYTES, MAX_MIRROR_BYTES, MAX_MIRROR_NODES, MAX_RECENT_INVALIDATIONS,
    SWEEP_BUDGET, TOMBSTONE_SLACK, TOMBSTONE_TTL,
};

/// Read side. Implementations must be wait-free on a hit.
pub trait MirrorRead: Send + Sync + std::fmt::Debug {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>>;
    /// `get` plus the app-wide stale epoch folded into `stale` (5.2). Never blocks, no
    /// allocation on a hit.
    fn view(&self, k: NodeKey) -> Lookup<NodeView>;
    /// Bumped once per applied batch or mutation.
    fn revision(&self) -> u64;
    /// The write sequence (the value a request records as `issued_at`).
    fn seq(&self) -> u64;
    /// Take a fresh sequence number: the point at which a request is issued or a trigger (a focus
    /// change) happens. Every request and every trigger gets its own, so "issued after" is a
    /// strict comparison (B-1 5.3, 5.4).
    fn bump_seq(&self) -> u64;
    /// Counters, exact (MA-1).
    fn stats(&self) -> MirrorStats;
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MirrorStats {
    pub live: u64,
    pub tombstones: u64,
    /// Live nodes whose own snapshot is marked `Stale` (the app-wide epoch is not counted).
    pub stale: u64,
    pub bytes: u64,
    /// Live nodes that do not carry `PropMask::PRESENT`.
    pub partial: u64,
    pub evictions: u64,
    pub revivals: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RemovalCause {
    Event,
    Defunct,
    UnknownObject,
    CacheSignal,
    WindowDestroyed,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ApplyOrigin {
    /// The reply to a request issued at write sequence `issued_at`.
    Reply { issued_at: u64 },
    /// A connector push: confirmed now; never revives a tombstone.
    Push,
    /// A `Cache.GetItems` dump (same rules as a reply, per node).
    CacheDump { issued_at: u64 },
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplyOutcome {
    pub applied: u32,
    /// Stored, but stays `Stale` because an invalidation is newer than the request (LR-2).
    pub kept_stale: u32,
    /// Dropped: tombstoned after the request was issued (LR-1), or a push on a tombstone.
    pub dropped_tombstoned: u32,
    /// Dropped: the key belongs to another app (MA-3).
    pub dropped_foreign: u32,
    /// Dropped: the stored content is newer than this reply.
    pub superseded: u32,
    pub revived: u32,
    pub evicted: u32,
    /// Turned into a tombstone because the node carried `defunct` (5.1).
    pub defunct: u32,
    /// Not stored: the all-apps byte cap had no room and this origin may not evict.
    pub dropped_cap: u32,
    pub truncated: bool,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct SweepStats {
    pub tombstones_expired: u32,
    pub evicted: u32,
    pub queue_compacted: u32,
}

/// A counter disagrees with the structure it counts (MA-1).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MirrorInvariantError {
    pub what: &'static str,
    pub counter: u64,
    pub actual: u64,
}

impl std::fmt::Display for MirrorInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mirror counter `{}` is {} but the structure has {}",
            self.what, self.counter, self.actual
        )
    }
}

impl std::error::Error for MirrorInvariantError {}

// ------------------------------------------------------------------------- hashing

/// Cheap integer hasher for `NodeKey` (the SipHash default would dominate a 150 ns read).
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyBuild;

#[derive(Clone, Copy, Debug, Default)]
pub struct KeyHasher(u64);

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

impl BuildHasher for KeyBuild {
    type Hasher = KeyHasher;
    fn build_hasher(&self) -> KeyHasher {
        KeyHasher(0)
    }
}

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = splitmix(self.0 ^ u64::from(*b));
        }
    }
    fn write_u16(&mut self, i: u16) {
        self.0 = splitmix(self.0 ^ u64::from(i));
    }
    fn write_u32(&mut self, i: u32) {
        self.0 = splitmix(self.0 ^ u64::from(i));
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = splitmix(self.0 ^ i);
    }
}

// ------------------------------------------------------------------------- accounting

/// Deterministic size of a snapshot for the byte counters: a function of the snapshot only, so
/// the counter can be recomputed from the structure (MA-1).
pub fn node_bytes(n: &NodeSnapshot) -> u64 {
    const HDR: usize = 2 * std::mem::size_of::<usize>();
    let mut b = std::mem::size_of::<NodeSnapshot>() + INDEX_OVERHEAD;
    b += n.name().as_deref().map_or(0, str::len);
    b += n.description().as_deref().map_or(0, str::len);
    if let Some(Value::Text(t)) = &n.value() {
        b += t.len();
    }
    if let ChildList::Known(c) = &n.children() {
        b += c.len() * std::mem::size_of::<NodeKey>();
    }
    b += n.relations().len() * std::mem::size_of::<NodeKey>();
    // The dual-carried semantics (M22a-C1b), real heap: each `Arc` header (two counters), the
    // value, its name unless it is the snapshot's own allocation, and every string of its text.
    let own_name = |name: &Option<Arc<str>>| match (name, &n.name()) {
        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => 0,
        (Some(a), _) => HDR + a.len(),
        (None, _) => 0,
    };
    if let Some(r) = &n.sem().rec {
        b += HDR
            + std::mem::size_of::<SrcRecord>()
            + own_name(&r.name)
            + r.sem.text.as_deref().map_or(0, SemText::heap_bytes);
    }
    if let Some(e) = &n.sem().entry {
        b += HDR
            + std::mem::size_of::<SemEntry>()
            + own_name(&e.name)
            + e.sem.text.as_deref().map_or(0, SemText::heap_bytes);
    }
    b as u64
}

/// Entry overhead of the hash index, charged per node.
const INDEX_OVERHEAD: usize = 48;

fn is_partial(n: &NodeSnapshot) -> bool {
    !n.have().contains(PropMask::PRESENT)
}

fn is_stale(n: &NodeSnapshot) -> bool {
    matches!(n.fresh(), Freshness::Stale { .. })
}

// ------------------------------------------------------------------------- global budget

/// One app's footprint, shared with the global budget.
#[derive(Debug, Default)]
pub struct AppUsage {
    bytes: AtomicU64,
    live: AtomicU64,
    /// The focused app and the Shell: preempted last, and never below `MIN_PROTECTED_NODES`.
    protected: AtomicBool,
    /// Bytes this app was asked to shed by a writer of another app that hit the global cap.
    shed: AtomicU64,
}

impl AppUsage {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }
    pub fn live(&self) -> u64 {
        self.live.load(Ordering::Relaxed)
    }
    /// The focused app or the Shell: never asked to shed for another app.
    pub fn protected(&self) -> bool {
        self.protected.load(Ordering::Relaxed)
    }
}

/// The all-apps byte cap (`GLOBAL_MIRROR_BYTES`), enforced exactly: every byte a mirror holds is
/// reserved here first, so the total never exceeds the cap, not even for a moment.
#[derive(Debug)]
pub struct MirrorBudget {
    cap: u64,
    total: AtomicU64,
    apps: Mutex<Vec<Weak<AppUsage>>>,
}

impl MirrorBudget {
    /// The all-apps cap of the section 7 table.
    pub fn global() -> Arc<Self> {
        Self::new(GLOBAL_MIRROR_BYTES)
    }

    pub fn new(cap: u64) -> Arc<Self> {
        Arc::new(Self {
            cap,
            total: AtomicU64::new(0),
            apps: Mutex::new(Vec::new()),
        })
    }

    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    pub fn cap(&self) -> u64 {
        self.cap
    }

    fn register(&self, usage: &Arc<AppUsage>) {
        let mut apps = self.apps.lock().unwrap_or_else(PoisonError::into_inner);
        apps.retain(|w| w.strong_count() > 0);
        apps.push(Arc::downgrade(usage));
    }

    /// Reserve `n` bytes if they fit under the cap.
    fn try_reserve(&self, n: u64) -> bool {
        let mut cur = self.total.load(Ordering::Relaxed);
        loop {
            let Some(next) = cur.checked_add(n).filter(|t| *t <= self.cap) else {
                return false;
            };
            match self
                .total
                .compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(seen) => cur = seen,
            }
        }
    }

    fn release(&self, n: u64) {
        let _ = self
            .total
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |t| {
                Some(t.saturating_sub(n))
            });
    }

    /// `who` needs `need` bytes and the cap is full: ask the largest holder that is neither the
    /// focused app nor the Shell (`protected`) to shed them. The request is a target, not an
    /// increment: asking again does not add up. The victim sheds on its next apply or sweep,
    /// oldest and least recently read nodes first.
    fn request_shed(&self, who: &Arc<AppUsage>, need: u64) {
        let apps = self.apps.lock().unwrap_or_else(PoisonError::into_inner);
        let mut best: Option<Arc<AppUsage>> = None;
        for w in apps.iter() {
            let Some(u) = w.upgrade() else { continue };
            if Arc::ptr_eq(&u, who) || u.protected.load(Ordering::Relaxed) || u.live() == 0 {
                continue;
            }
            if best.as_ref().is_none_or(|b| u.bytes() > b.bytes()) {
                best = Some(u);
            }
        }
        if let Some(v) = best {
            v.shed.fetch_max(need, Ordering::Relaxed);
        }
    }
}

// ------------------------------------------------------------------------- shared state

#[derive(Clone, Debug)]
enum Entry {
    Live {
        snap: Arc<NodeSnapshot>,
        /// The write sequence of the latest invalidation of this node (LR-2).
        inv_seq: u64,
        inv_mask: PropMask,
    },
    Tombstone {
        seq: u64,
        #[allow(dead_code)]
        why: RemovalCause,
    },
}

/// One key's cell: the entry is swapped in place by the writer and loaded by any reader;
/// `touched` is the write sequence at the latest read (the least-recently-used signal).
#[derive(Debug)]
struct Cell {
    entry: ArcSwap<Entry>,
    touched: AtomicU64,
}

type CellRef = Arc<Cell>;

/// The index is split into shards that grow independently: a `HashIndex` doubles its table in
/// one go (measured: 2.8 ms at 53 000 entries), which would stall an `apply` far beyond the 250 us
/// budget of MA-6; with 64 shards the biggest single resize is 1/64 of that.
const SHARDS: usize = 64;

struct Index {
    shards: Box<[scc::HashIndex<NodeKey, CellRef, KeyBuild>]>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("shards", &self.shards.len())
            .finish()
    }
}

impl Index {
    fn new() -> Self {
        Self {
            shards: (0..SHARDS)
                .map(|_| scc::HashIndex::with_hasher(KeyBuild))
                .collect(),
        }
    }

    fn shard(&self, k: &NodeKey) -> &scc::HashIndex<NodeKey, CellRef, KeyBuild> {
        let h = splitmix(KeyBuild.hash_one(k));
        // The top bits pick the shard; the inner table uses the low ones.
        &self.shards[(h >> 58) as usize % SHARDS]
    }

    fn peek_with<R, F: FnOnce(&NodeKey, &CellRef) -> R>(&self, k: &NodeKey, f: F) -> Option<R> {
        self.shard(k).peek_with(k, f)
    }

    fn insert_sync(&self, k: NodeKey, v: CellRef) -> Result<(), (NodeKey, CellRef)> {
        self.shard(&k).insert_sync(k, v)
    }

    fn remove_sync(&self, k: &NodeKey) -> bool {
        self.shard(k).remove_sync(k)
    }

    fn iter_sync<F: FnMut(&NodeKey, &CellRef) -> bool>(&self, mut f: F) {
        for sh in &self.shards {
            if !sh.iter_sync(&mut f) {
                break;
            }
        }
    }

    fn clear_sync(&self) {
        for sh in &self.shards {
            sh.clear_sync();
        }
    }
}

#[derive(Debug)]
struct Shared {
    index: Index,
    app: Option<AppId>,
    revision: AtomicU64,
    seq: AtomicU64,
    stale_epoch: AtomicU64,
    /// On-read TTL in milliseconds (0 = never ages).
    ttl_ms: AtomicU64,
    retired: AtomicBool,
    live: AtomicU64,
    tombstones: AtomicU64,
    stale: AtomicU64,
    bytes: AtomicU64,
    partial: AtomicU64,
    evictions: AtomicU64,
    revivals: AtomicU64,
}

/// Default on-read TTL (B-1 2.2: GTK4, browser chrome and unknown apps 30 s; GTK3 and the
/// Shell 60 s via [`MirrorWriter::set_ttl`]).
pub const DEFAULT_TTL: std::time::Duration = std::time::Duration::from_secs(30);

fn millis(d: std::time::Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

fn is_aged(snap: &NodeSnapshot, ttl_ms: u64) -> bool {
    match snap.fresh() {
        Freshness::Fresh { at } if ttl_ms > 0 => millis(at.elapsed()) >= ttl_ms,
        Freshness::Fresh { .. } | Freshness::Stale { .. } | Freshness::Defunct => false,
    }
}

fn stale_of(snap: &NodeSnapshot, epoch: u64) -> PropMask {
    if snap.confirmed_seq() < epoch {
        PropMask::ALL
    } else {
        snap.fresh().stale_mask()
    }
}

impl Shared {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>> {
        if self.retired.load(Ordering::Acquire) {
            return Lookup::Gone;
        }
        match self.index.peek_with(&k, |_, c| match &**c.entry.load() {
            Entry::Live { snap, .. } => Lookup::Hit(Arc::clone(snap)),
            Entry::Tombstone { .. } => Lookup::Gone,
        }) {
            Some(l) => l,
            None => Lookup::Miss,
        }
    }

    fn view(&self, k: NodeKey) -> Lookup<NodeView> {
        if self.retired.load(Ordering::Acquire) {
            return Lookup::Gone;
        }
        let epoch = self.stale_epoch.load(Ordering::Acquire);
        let ttl_ms = self.ttl_ms.load(Ordering::Relaxed);
        let now_seq = self.seq.load(Ordering::Relaxed);
        match self.index.peek_with(&k, |_, c| match &**c.entry.load() {
            Entry::Live { snap, .. } => {
                c.touched.store(now_seq, Ordering::Relaxed);
                Lookup::Hit(NodeView {
                    snap: Arc::clone(snap),
                    stale: stale_of(snap, epoch),
                    aged: is_aged(snap, ttl_ms),
                })
            }
            Entry::Tombstone { .. } => Lookup::Gone,
        }) {
            Some(l) => l,
            None => Lookup::Miss,
        }
    }

    fn stats(&self) -> MirrorStats {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        MirrorStats {
            live: g(&self.live),
            tombstones: g(&self.tombstones),
            stale: g(&self.stale),
            bytes: g(&self.bytes),
            partial: g(&self.partial),
            evictions: g(&self.evictions),
            revivals: g(&self.revivals),
        }
    }
}

/// Cloneable read handle.
#[derive(Clone, Debug)]
pub struct MirrorHandle {
    shared: Arc<Shared>,
}

impl MirrorRead for MirrorHandle {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>> {
        self.shared.get(k)
    }
    fn view(&self, k: NodeKey) -> Lookup<NodeView> {
        self.shared.view(k)
    }
    fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }
    fn seq(&self) -> u64 {
        self.shared.seq.load(Ordering::Acquire)
    }
    fn bump_seq(&self) -> u64 {
        self.shared.seq.fetch_add(1, Ordering::AcqRel) + 1
    }
    fn stats(&self) -> MirrorStats {
        self.shared.stats()
    }
}

// ------------------------------------------------------------------------- writer

#[derive(Debug)]
struct Tomb {
    key: NodeKey,
    at: Instant,
    seq: u64,
}

/// Write side. Not `Clone`: owned by exactly one app actor.
#[derive(Debug)]
pub struct MirrorWriter {
    shared: Arc<Shared>,
    tomb_q: VecDeque<Tomb>,
    /// Eviction order: insertion order with a second chance for refreshed nodes.
    fifo: VecDeque<(NodeKey, u64)>,
    recent_inval: HashMap<NodeKey, (u64, PropMask), KeyBuild>,
    /// Each key of `recent_inval` once, oldest first.
    recent_order: VecDeque<NodeKey>,
    /// Requests issued before this sequence may have missed an invalidation of an unknown key.
    inval_floor: u64,
    focus: Option<NodeKey>,
    /// The key an `apply` is writing: never evicted to make room for itself.
    pinned: Option<NodeKey>,
    /// Bytes this `apply` could not reserve; asked of the other apps once, at its end.
    unmet: u64,
    hints: Vec<NodeKey>,
    max_nodes: usize,
    max_bytes: u64,
    usage: Arc<AppUsage>,
    budget: Option<Arc<MirrorBudget>>,
}

/// Create a mirror: the single writer and a cloneable read handle. The app is not checked.
pub fn mirror() -> (MirrorWriter, MirrorHandle) {
    build(None, None)
}

/// A mirror that belongs to `app`: a node of another app is dropped and counted (MA-3), and the
/// footprint is charged to `budget`.
pub fn mirror_for(app: AppId, budget: Option<Arc<MirrorBudget>>) -> (MirrorWriter, MirrorHandle) {
    build(Some(app), budget)
}

fn build(app: Option<AppId>, budget: Option<Arc<MirrorBudget>>) -> (MirrorWriter, MirrorHandle) {
    let shared = Arc::new(Shared {
        index: Index::new(),
        app,
        revision: AtomicU64::new(0),
        seq: AtomicU64::new(0),
        stale_epoch: AtomicU64::new(0),
        ttl_ms: AtomicU64::new(millis(DEFAULT_TTL)),
        retired: AtomicBool::new(false),
        live: AtomicU64::new(0),
        tombstones: AtomicU64::new(0),
        stale: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        partial: AtomicU64::new(0),
        evictions: AtomicU64::new(0),
        revivals: AtomicU64::new(0),
    });
    let usage = Arc::new(AppUsage::default());
    if let Some(b) = &budget {
        b.register(&usage);
    }
    let writer = MirrorWriter {
        shared: Arc::clone(&shared),
        tomb_q: VecDeque::new(),
        fifo: VecDeque::new(),
        recent_inval: HashMap::with_hasher(KeyBuild),
        recent_order: VecDeque::new(),
        inval_floor: 0,
        focus: None,
        pinned: None,
        unmet: 0,
        hints: Vec::new(),
        max_nodes: MAX_MIRROR_NODES,
        max_bytes: MAX_MIRROR_BYTES,
        usage,
        budget,
    };
    (writer, MirrorHandle { shared })
}

/// The unpublished replacement for `old` once `new` arrives: every field only the old snapshot
/// has survives a partial reply when nothing marked it stale; the rest comes from the new
/// content. The Reader's fetches ask for the spoken fields only, and must not make the node
/// forget its parent (P9-3 ancestors, focus protection), children or description.
fn merge(old: &NodeSnapshot, new: &mut NodeSnapshot, old_stale: PropMask) {
    const EXTRA: [PropMask; 7] = [
        PropMask::VALUE,
        PropMask::RELATIONS,
        PropMask::ATTRS,
        PropMask::BOUNDS,
        PropMask::PARENT,
        PropMask::CHILDREN,
        PropMask::DESCRIPTION,
    ];
    for f in EXTRA {
        if old.have().contains(f) && !new.have().contains(f) && !old_stale.intersects(f) {
            match f {
                PropMask::VALUE => new.set_value(old.value().clone()),
                PropMask::RELATIONS => new.set_relations(old.relations().clone()),
                // The attributes live in the source record, which a reply carries for itself.
                PropMask::ATTRS => {}
                PropMask::BOUNDS => new.set_bounds(old.bounds()),
                PropMask::PARENT => new.set_parent(old.parent()),
                PropMask::CHILDREN => new.set_children(old.children().clone()),
                _ => new.set_description(old.description().clone()),
            }
            new.set_have(new.have() | f);
        }
    }
}

impl MirrorWriter {
    /// Monotonic write sequence; every request records `issued_at = seq()` (5.4).
    pub fn seq(&self) -> u64 {
        self.shared.seq.load(Ordering::Acquire)
    }

    fn bump_seq(&mut self) -> u64 {
        self.shared.seq.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// A fresh sequence number for a request about to be issued (see [`MirrorRead::bump_seq`]).
    pub fn next_seq(&mut self) -> u64 {
        self.bump_seq()
    }

    fn bump_rev(&self) {
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub fn reader(&self) -> MirrorHandle {
        MirrorHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Footprint handle (global budget, tests).
    pub fn usage(&self) -> Arc<AppUsage> {
        Arc::clone(&self.usage)
    }

    /// The on-read TTL of this app (zero disables aging).
    pub fn set_ttl(&mut self, ttl: std::time::Duration) {
        self.shared.ttl_ms.store(millis(ttl), Ordering::Relaxed);
    }

    /// The app's own caps; tests shrink them.
    pub fn set_caps(&mut self, max_nodes: usize, max_bytes: u64) {
        self.max_nodes = max_nodes;
        self.max_bytes = max_bytes;
    }

    /// The Reader's focus: never evicted, nor its ancestry.
    pub fn set_focus(&mut self, focus: Option<NodeKey>) {
        self.focus = focus;
    }

    /// The focused app and the Shell are preempted last (global cap).
    pub fn set_protected(&mut self, protected: bool) {
        self.usage.protected.store(protected, Ordering::Relaxed);
    }

    /// Is `k` the focus or on its ancestry (never evicted)?
    pub fn is_protected(&self, k: NodeKey) -> bool {
        self.protected_keys().contains(&k)
    }

    /// A policy's `Evict` hint: first in line when the caps force an eviction.
    pub fn hint_evict(&mut self, k: NodeKey) {
        if self.hints.len() < 256 {
            self.hints.push(k);
        }
    }

    fn owns(&self, k: NodeKey) -> bool {
        self.shared.app.is_none_or(|a| a == k.app)
    }

    fn peek(&self, k: NodeKey) -> Option<Entry> {
        self.shared
            .index
            .peek_with(&k, |_, c| Entry::clone(&c.entry.load()))
    }

    fn put(&self, k: NodeKey, e: Entry) {
        let e = Arc::new(e);
        let swapped = self
            .shared
            .index
            .peek_with(&k, |_, c| c.entry.store(Arc::clone(&e)));
        if swapped.is_none() {
            let _ = self.shared.index.insert_sync(
                k,
                Arc::new(Cell {
                    entry: ArcSwap::new(e),
                    touched: AtomicU64::new(self.seq()),
                }),
            );
        }
    }

    /// Counters only: the all-apps budget is reserved and released separately.
    fn counters_add(&self, n: &NodeSnapshot) {
        let s = &self.shared;
        let b = node_bytes(n);
        s.live.fetch_add(1, Ordering::Relaxed);
        s.bytes.fetch_add(b, Ordering::Relaxed);
        self.usage.live.fetch_add(1, Ordering::Relaxed);
        self.usage.bytes.fetch_add(b, Ordering::Relaxed);
        if is_stale(n) {
            s.stale.fetch_add(1, Ordering::Relaxed);
        }
        if is_partial(n) {
            s.partial.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn counters_sub(&self, n: &NodeSnapshot) {
        let s = &self.shared;
        let b = node_bytes(n);
        s.live.fetch_sub(1, Ordering::Relaxed);
        s.bytes.fetch_sub(b, Ordering::Relaxed);
        self.usage.live.fetch_sub(1, Ordering::Relaxed);
        self.usage.bytes.fetch_sub(b, Ordering::Relaxed);
        if is_stale(n) {
            s.stale.fetch_sub(1, Ordering::Relaxed);
        }
        if is_partial(n) {
            s.partial.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// A node leaves the store: counters and its share of the all-apps budget.
    fn refund(&self, n: &NodeSnapshot) {
        self.counters_sub(n);
        if let Some(bu) = &self.budget {
            bu.release(node_bytes(n));
        }
    }

    /// Reserve `delta` bytes of the all-apps cap for a write. When the cap is full, the largest
    /// unprotected other app is asked to shed; a writer allowed to (`may_evict`: the reply to a
    /// request the user is waiting on) makes room by evicting its own oldest and least recently
    /// read nodes. `false` means the write must be dropped (the node degrades to a miss).
    fn reserve(&mut self, delta: u64, may_evict: bool) -> bool {
        let Some(b) = self.budget.clone() else {
            return true;
        };
        if delta == 0 || b.try_reserve(delta) {
            return true;
        }
        self.unmet += delta;
        while may_evict && self.evict_one() {
            if b.try_reserve(delta) {
                return true;
            }
        }
        false
    }

    /// Apply a reply or a push. Per node: dropped if tombstoned at a seq > `issued_at`; kept
    /// `Stale` if invalidated at a seq > `issued_at`; otherwise `Fresh`. Never revives a key
    /// whose tombstone is newer than the request (MA-4).
    pub fn apply(&mut self, batch: SnapshotBatch, origin: ApplyOrigin) -> ApplyOutcome {
        let mut out = ApplyOutcome {
            truncated: batch.truncated,
            ..ApplyOutcome::default()
        };
        let (issued_at, allow_revive, may_evict) = match origin {
            ApplyOrigin::Reply { issued_at } => (issued_at, true, true),
            ApplyOrigin::CacheDump { issued_at } => (issued_at, true, false),
            ApplyOrigin::Push => (self.seq(), false, false),
        };
        let now = Instant::now();
        for snap in batch.nodes {
            self.apply_one(snap, issued_at, (allow_revive, may_evict), now, &mut out);
        }
        self.enforce_bounds(&mut out);
        if self.unmet > 0 {
            if let Some(b) = &self.budget {
                // A floor keeps a drip of small writes from asking for one node at a time.
                b.request_shed(&self.usage, self.unmet.max(b.cap() / 64));
            }
            self.unmet = 0;
        }
        self.bump_rev();
        self.debug_check();
        out
    }

    fn apply_one(
        &mut self,
        snap: Arc<NodeSnapshot>,
        issued_at: u64,
        (allow_revive, may_evict): (bool, bool),
        now: Instant,
        out: &mut ApplyOutcome,
    ) {
        let k = snap.key();
        if !self.owns(k) {
            out.dropped_foreign += 1;
            return;
        }
        if snap.struct_state().is_defunct() {
            // 5.1: a reply that says `defunct` is a removal, not a node.
            self.tombstone(k, now, RemovalCause::Defunct);
            out.defunct += 1;
            return;
        }
        let mut revived = false;
        let (old, mut inv_seq, mut inv_mask) = match self.peek(k) {
            Some(Entry::Tombstone { seq, .. }) => {
                if !allow_revive || seq > issued_at {
                    out.dropped_tombstoned += 1;
                    return;
                }
                revived = true;
                (None, 0, PropMask::empty())
            }
            Some(Entry::Live {
                snap,
                inv_seq,
                inv_mask,
            }) => (Some(snap), inv_seq, inv_mask),
            None => (None, 0, PropMask::empty()),
        };
        if old.is_none() {
            if let Some(&(s, m)) = self.recent_inval.get(&k) {
                inv_seq = inv_seq.max(s);
                inv_mask |= m;
            } else if issued_at < self.inval_floor {
                inv_seq = self.inval_floor;
                inv_mask = PropMask::ALL;
            }
        }
        if let Some(o) = &old
            && o.confirmed_seq() > issued_at
        {
            // The stored content was confirmed by a newer request than this reply.
            out.superseded += 1;
            return;
        }
        let is_new = old.is_none();
        let mut node = Arc::try_unwrap(snap).unwrap_or_else(|a| (*a).clone());
        node.set_confirmed_seq(issued_at);
        let redirtied = inv_seq > issued_at;
        if let Some(o) = &old {
            let old_stale = stale_of(o, self.shared.stale_epoch.load(Ordering::Acquire));
            merge(o, &mut node, old_stale);
        }
        node.set_fresh(if redirtied {
            out.kept_stale += 1;
            Freshness::Stale {
                since: now,
                mask: inv_mask,
            }
        } else {
            Freshness::Fresh { at: now }
        });
        let node = Arc::new(node);
        // Room under the node cap, revivals included.
        if is_new {
            self.pinned = Some(k);
            self.make_room(out);
        }
        // Room under the all-apps byte cap.
        let new_bytes = node_bytes(&node);
        let old_bytes = old.as_ref().map_or(0, |o| node_bytes(o));
        self.pinned = Some(k);
        let fits = if new_bytes > old_bytes {
            self.reserve(new_bytes - old_bytes, may_evict)
        } else {
            if let Some(b) = &self.budget {
                b.release(old_bytes - new_bytes);
            }
            true
        };
        self.pinned = None;
        if !fits {
            out.dropped_cap += 1;
            return;
        }
        if let Some(o) = &old {
            self.counters_sub(o);
        }
        if revived {
            self.shared.tombstones.fetch_sub(1, Ordering::Relaxed);
            self.shared.revivals.fetch_add(1, Ordering::Relaxed);
            out.revived += 1;
        }
        if is_new {
            self.fifo.push_back((k, issued_at));
        }
        self.counters_add(&node);
        self.put(
            k,
            Entry::Live {
                snap: node,
                inv_seq,
                inv_mask,
            },
        );
        out.applied += 1;
    }

    fn make_room(&mut self, out: &mut ApplyOutcome) {
        while self.shared.live.load(Ordering::Relaxed) >= self.max_nodes as u64 {
            if !self.evict_one() {
                break;
            }
            out.evicted += 1;
        }
    }

    fn protected_keys(&self) -> Vec<NodeKey> {
        let mut keys = Vec::new();
        if let Some(p) = self.pinned {
            keys.push(p);
        }
        let mut cur = self.focus;
        while let Some(k) = cur {
            if keys.len() >= 65 || keys.contains(&k) {
                break;
            }
            keys.push(k);
            cur = self
                .shared
                .index
                .peek_with(&k, |_, c| match &**c.entry.load() {
                    Entry::Live { snap, .. } => snap.parent(),
                    Entry::Tombstone { .. } => None,
                })
                .flatten();
        }
        keys
    }

    /// Evict one live node that is neither the focus nor its ancestry: the policy's hints
    /// first, else the oldest by confirmation.
    fn evict_one(&mut self) -> bool {
        let protected = self.protected_keys();
        while let Some(h) = self.hints.pop() {
            if !protected.contains(&h) && self.evict(h) {
                return true;
            }
        }
        let mut budget = self.fifo.len().saturating_mul(2).saturating_add(4);
        while budget > 0 {
            budget -= 1;
            let Some((k, qseq)) = self.fifo.pop_front() else {
                return false;
            };
            // Recency: the later of the last confirmation and the last read (LRU).
            let live_seq = self
                .shared
                .index
                .peek_with(&k, |_, c| match &**c.entry.load() {
                    Entry::Live { snap, .. } => {
                        Some(snap.confirmed_seq().max(c.touched.load(Ordering::Relaxed)))
                    }
                    Entry::Tombstone { .. } => None,
                });
            match live_seq.flatten() {
                Some(seq) if seq > qseq => self.fifo.push_back((k, seq)),
                Some(_) if protected.contains(&k) => self.fifo.push_back((k, qseq)),
                Some(_) if self.evict(k) => return true,
                // Already gone from the store, or not evictable: nothing to do.
                Some(_) | None => {}
            }
        }
        false
    }

    fn enforce_bounds(&mut self, out: &mut ApplyOutcome) {
        while self.shared.bytes.load(Ordering::Relaxed) > self.max_bytes {
            if !self.evict_one() {
                break;
            }
            out.evicted += 1;
        }
        // Another app hit the all-apps cap and asked this one to shed (never the focused app
        // or the Shell: they are not asked).
        let want = self.usage.shed.swap(0, Ordering::Relaxed);
        if want > 0 && !self.usage.protected.load(Ordering::Relaxed) {
            let (n, freed) = self.shed_bytes(want);
            out.evicted += n;
            if freed < want && self.shared.live.load(Ordering::Relaxed) > 0 {
                // More to do than one slice allows: ask again for the rest.
                self.usage.shed.fetch_max(want - freed, Ordering::Relaxed);
            }
        }
    }

    /// Evict until `target` bytes were freed, at most `SWEEP_BUDGET` nodes per call. Returns
    /// the nodes evicted and the bytes freed.
    fn shed_bytes(&mut self, target: u64) -> (u32, u64) {
        let start = self.shared.bytes.load(Ordering::Relaxed);
        let mut n = 0u32;
        while (n as usize) < SWEEP_BUDGET
            && start.saturating_sub(self.shared.bytes.load(Ordering::Relaxed)) < target
        {
            if !self.evict_one() {
                break;
            }
            n += 1;
        }
        (
            n,
            start.saturating_sub(self.shared.bytes.load(Ordering::Relaxed)),
        )
    }

    /// Attach the fused entry made from `rec` to the live snapshot of `k` that carries exactly
    /// that record (pointer identity: an entry fused from a record that a newer reply has since
    /// replaced must not be attached to the newer content). The snapshot's record is dropped (the
    /// entry is the fused form of it). Returns whether the entry was attached; `false` for a node
    /// that is gone, replaced, or when the all-apps budget has no room for the difference.
    pub fn attach_sem(&mut self, k: NodeKey, rec: &Arc<SrcRecord>, entry: Arc<SemEntry>) -> bool {
        self.swap_sem(k, entry, |sem| {
            sem.rec.as_ref().is_some_and(|r| Arc::ptr_eq(r, rec))
        })
    }

    /// Replace the entry of a snapshot that already carries one with a re-fused `entry` (a
    /// parent whose content name changed with its children, a node the engine marked stale after
    /// a property event). Does nothing, and returns `false`, when the node is gone, carries no
    /// entry yet (its record is still waiting to be fused: [`Self::attach_sem`] owns that case)
    /// or already carries an equal one.
    pub fn refresh_sem(&mut self, k: NodeKey, entry: Arc<SemEntry>) -> bool {
        let newer = entry.clone();
        self.swap_sem(k, entry, move |sem| {
            sem.entry.as_ref().is_some_and(|e| **e != *newer)
        })
    }

    fn swap_sem(
        &mut self,
        k: NodeKey,
        entry: Arc<SemEntry>,
        ok: impl Fn(&NodeSem) -> bool,
    ) -> bool {
        if !self.owns(k) {
            return false;
        }
        let Some(Entry::Live {
            snap,
            inv_seq,
            inv_mask,
        }) = self.peek(k)
        else {
            return false;
        };
        if !ok(snap.sem()) {
            return false;
        }
        let mut n = (*snap).clone();
        n.set_sem(NodeSem {
            rec: None,
            entry: Some(entry),
        });
        let (old_bytes, new_bytes) = (node_bytes(&snap), node_bytes(&n));
        if new_bytes > old_bytes {
            self.pinned = Some(k);
            let ok = self.reserve(new_bytes - old_bytes, false);
            self.pinned = None;
            if !ok {
                return false;
            }
        } else if let Some(b) = &self.budget {
            b.release(old_bytes - new_bytes);
        }
        let n = Arc::new(n);
        self.counters_sub(&snap);
        self.counters_add(&n);
        self.put(
            k,
            Entry::Live {
                snap: n,
                inv_seq,
                inv_mask,
            },
        );
        // No revision bump: the content a reader sees (legacy fields, freshness) is unchanged, so
        // a review model built at the current revision stays valid.
        self.debug_check();
        true
    }

    /// Mark fields of `k` untrustworthy. Returns whether a live node was marked.
    pub fn invalidate(&mut self, k: NodeKey, mask: PropMask, at: Instant) -> bool {
        if !self.owns(k) || mask.is_empty() {
            return false;
        }
        let seq = self.bump_seq();
        let marked = match self.peek(k) {
            Some(Entry::Live {
                snap,
                inv_seq: _,
                inv_mask,
            }) => {
                let already =
                    matches!(snap.fresh(), Freshness::Stale { mask: m, .. } if m.contains(mask));
                let next = if already {
                    Arc::clone(&snap)
                } else {
                    let mut n = (*snap).clone();
                    n.set_fresh(n.fresh().step(FreshnessEvent::Invalidate { at, mask }));
                    let n = Arc::new(n);
                    // Same size: only the stale counter moves.
                    self.counters_sub(&snap);
                    self.counters_add(&n);
                    n
                };
                self.put(
                    k,
                    Entry::Live {
                        snap: next,
                        inv_seq: seq,
                        inv_mask: inv_mask | mask,
                    },
                );
                true
            }
            // LR-2 on the proof-of-life path: a removed key may be revived by a reply issued
            // before this event, which must then stay stale.
            Some(Entry::Tombstone { .. }) | None => {
                self.remember_invalidation(k, seq, mask);
                false
            }
        };
        self.bump_rev();
        self.debug_check();
        marked
    }

    /// Invalidations of keys the mirror does not hold (or holds as tombstones) are remembered, so
    /// a request that was in flight for them cannot store pre-event content as fresh (LR-2).
    /// At most `MAX_RECENT_INVALIDATIONS` keys, each once; when full, the oldest is forgotten and
    /// the floor rises, so everything issued before it is conservatively treated as invalidated.
    fn remember_invalidation(&mut self, k: NodeKey, seq: u64, mask: PropMask) {
        if let Some(e) = self.recent_inval.get_mut(&k) {
            e.0 = seq;
            e.1 |= mask;
            return;
        }
        while self.recent_inval.len() >= MAX_RECENT_INVALIDATIONS {
            let Some(old) = self.recent_order.pop_front() else {
                self.inval_floor = seq;
                self.recent_inval.clear();
                break;
            };
            if let Some((s, _)) = self.recent_inval.remove(&old) {
                self.inval_floor = self.inval_floor.max(s);
            }
        }
        self.recent_inval.insert(k, (seq, mask));
        self.recent_order.push_back(k);
    }

    /// O(1): every node confirmed before now is `Stale(ALL)` (flood resync, dirty-all).
    pub fn invalidate_all(&mut self, _at: Instant) {
        let seq = self.bump_seq();
        self.shared.stale_epoch.store(seq, Ordering::Release);
        self.recent_inval.clear();
        self.recent_order.clear();
        self.bump_rev();
    }

    /// Remove `k` for good (until proof of life): it reads `Gone`.
    pub fn tombstone(&mut self, k: NodeKey, at: Instant, why: RemovalCause) {
        if !self.owns(k) {
            return;
        }
        let seq = self.bump_seq();
        match self.peek(k) {
            Some(Entry::Live { snap, .. }) => {
                self.refund(&snap);
                self.shared.tombstones.fetch_add(1, Ordering::Relaxed);
            }
            Some(Entry::Tombstone { .. }) => {}
            None => {
                self.shared.tombstones.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.put(k, Entry::Tombstone { seq, why });
        self.tomb_q.push_back(Tomb { key: k, at, seq });
        self.cap_tombstones();
        self.bump_rev();
        self.debug_check();
    }

    fn tombstone_cap(&self) -> u64 {
        2 * self.shared.live.load(Ordering::Relaxed) + TOMBSTONE_SLACK as u64
    }

    fn cap_tombstones(&mut self) {
        let cap = self.tombstone_cap();
        while self.shared.tombstones.load(Ordering::Relaxed) > cap
            || self.tomb_q.len() as u64 > 2 * cap
        {
            let Some(t) = self.tomb_q.pop_front() else {
                break;
            };
            self.expire(&t);
        }
    }

    /// Drop the tombstone `t` refers to, unless the key has been revived or re-tombstoned.
    fn expire(&mut self, t: &Tomb) -> bool {
        let current = self
            .shared
            .index
            .peek_with(
                &t.key,
                |_, c| matches!(&**c.entry.load(), Entry::Tombstone { seq, .. } if *seq == t.seq),
            )
            .unwrap_or(false);
        if current {
            let _ = self.shared.index.remove_sync(&t.key);
            self.shared.tombstones.fetch_sub(1, Ordering::Relaxed);
        }
        current
    }

    /// Forget a live node (bounds). The key reads `Miss`, never `Gone`.
    pub fn evict(&mut self, k: NodeKey) -> bool {
        let Some(Entry::Live { snap, .. }) = self.peek(k) else {
            return false;
        };
        self.refund(&snap);
        let _ = self.shared.index.remove_sync(&k);
        self.shared.evictions.fetch_add(1, Ordering::Relaxed);
        self.bump_rev();
        true
    }

    /// Bounded work per call (at most `SWEEP_BUDGET` entries): tombstone TTL, queue
    /// compaction, shedding for the global cap.
    pub fn sweep(&mut self, now: Instant) -> SweepStats {
        let mut st = SweepStats::default();
        let mut budget = SWEEP_BUDGET;
        while budget > 0 {
            let Some(front) = self.tomb_q.front() else {
                break;
            };
            if now.saturating_duration_since(front.at) < TOMBSTONE_TTL {
                break;
            }
            let Some(t) = self.tomb_q.pop_front() else {
                break;
            };
            if self.expire(&t) {
                st.tombstones_expired += 1;
            }
            budget -= 1;
        }
        let live = usize::try_from(self.shared.live.load(Ordering::Relaxed)).unwrap_or(usize::MAX);
        if self.fifo.len() > 2 * live + TOMBSTONE_SLACK {
            for _ in 0..budget {
                let Some((k, seq)) = self.fifo.pop_front() else {
                    break;
                };
                let alive = matches!(self.peek(k), Some(Entry::Live { .. }));
                if alive {
                    self.fifo.push_back((k, seq));
                } else {
                    st.queue_compacted += 1;
                }
            }
        }
        let mut out = ApplyOutcome::default();
        self.enforce_bounds(&mut out);
        st.evicted = out.evicted;
        if st != SweepStats::default() {
            self.bump_rev();
        }
        self.debug_check();
        st
    }

    /// App gone: drop everything; readers get `Gone` for every key of the app afterwards.
    pub fn retire_all(&mut self) {
        self.shared.retired.store(true, Ordering::Release);
        let mut bytes = 0u64;
        self.shared.index.iter_sync(|_, c| {
            if let Entry::Live { snap, .. } = &**c.entry.load() {
                bytes += node_bytes(snap);
            }
            true
        });
        self.shared.index.clear_sync();
        if let Some(b) = &self.budget {
            b.release(bytes);
        }
        let s = &self.shared;
        for c in [&s.live, &s.tombstones, &s.stale, &s.bytes, &s.partial] {
            c.store(0, Ordering::Relaxed);
        }
        self.usage.live.store(0, Ordering::Relaxed);
        self.usage.bytes.store(0, Ordering::Relaxed);
        self.tomb_q = VecDeque::new();
        self.fifo = VecDeque::new();
        self.recent_inval = HashMap::with_hasher(KeyBuild);
        self.recent_order = VecDeque::new();
        self.hints = Vec::new();
        self.bump_rev();
    }

    /// How many invalidation records of unknown keys are held (bounded by
    /// `MAX_RECENT_INVALIDATIONS`).
    pub fn remembered_invalidations(&self) -> usize {
        self.recent_order.len()
    }

    /// Counters equal real sizes (MA-1). O(n).
    pub fn check_invariants(&self) -> Result<(), MirrorInvariantError> {
        let (mut live, mut tomb, mut stale, mut bytes, mut partial) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        self.shared.index.iter_sync(|_, c| {
            match &**c.entry.load() {
                Entry::Live { snap, .. } => {
                    live += 1;
                    bytes += node_bytes(snap);
                    stale += u64::from(is_stale(snap));
                    partial += u64::from(is_partial(snap));
                }
                Entry::Tombstone { .. } => tomb += 1,
            }
            true
        });
        let st = self.shared.stats();
        for (what, counter, actual) in [
            ("live", st.live, live),
            ("tombstones", st.tombstones, tomb),
            ("stale", st.stale, stale),
            ("bytes", st.bytes, bytes),
            ("partial", st.partial, partial),
            ("usage.live", self.usage.live(), live),
            ("usage.bytes", self.usage.bytes(), bytes),
        ] {
            if counter != actual {
                return Err(MirrorInvariantError {
                    what,
                    counter,
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Debug builds verify the counters after every mutation of a small mirror.
    #[inline]
    #[cfg_attr(not(debug_assertions), allow(clippy::unused_self))]
    fn debug_check(&self) {
        #[cfg(debug_assertions)]
        if self.shared.live.load(Ordering::Relaxed) + self.shared.tombstones.load(Ordering::Relaxed)
            <= 512
        {
            debug_assert!(
                self.check_invariants().is_ok(),
                "{:?}",
                self.check_invariants()
            );
        }
    }
}

impl Drop for MirrorWriter {
    /// A writer that goes away without `retire_all` (a cancelled actor) still gives its share of
    /// the all-apps budget back.
    fn drop(&mut self) {
        if let Some(b) = &self.budget {
            b.release(self.usage.bytes());
        }
        self.usage.bytes.store(0, Ordering::Relaxed);
        self.usage.live.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
