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
use gorca_model::{
    AppId, ChildList, Freshness, FreshnessEvent, Lookup, NodeKey, NodeSnapshot, NodeView, PropMask,
    SnapshotBatch, Value,
};

use crate::limits::{
    GLOBAL_MIRROR_BYTES, MAX_MIRROR_BYTES, MAX_MIRROR_NODES, MAX_RECENT_INVALIDATIONS,
    MIN_PROTECTED_NODES, SWEEP_BUDGET, TOMBSTONE_SLACK, TOMBSTONE_TTL,
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
    let mut b = std::mem::size_of::<NodeSnapshot>() + INDEX_OVERHEAD;
    b += n.name.as_deref().map_or(0, str::len);
    b += n.description.as_deref().map_or(0, str::len);
    if let Some(Value::Text(t)) = &n.value {
        b += t.len();
    }
    if let ChildList::Known(c) = &n.children {
        b += c.len() * std::mem::size_of::<NodeKey>();
    }
    for (_, v) in &n.attrs {
        b += v.len() + std::mem::size_of::<(gorca_model::AttrKey, Arc<str>)>();
    }
    b += n.relations.len() * std::mem::size_of::<NodeKey>();
    b as u64
}

/// Entry overhead of the hash index, charged per node.
const INDEX_OVERHEAD: usize = 48;

fn is_partial(n: &NodeSnapshot) -> bool {
    !n.have.contains(PropMask::PRESENT)
}

fn is_stale(n: &NodeSnapshot) -> bool {
    matches!(n.fresh, Freshness::Stale { .. })
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
}

/// The all-apps byte cap (`GLOBAL_MIRROR_BYTES`).
#[derive(Debug)]
pub struct MirrorBudget {
    cap: u64,
    total: AtomicU64,
    apps: Mutex<Vec<Weak<AppUsage>>>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Pressure {
    /// The caller is the largest eligible holder: it sheds itself.
    Own,
    /// Another app was asked to shed.
    Preempted,
    Nothing,
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

    /// Over the cap: the largest non-protected holder sheds the excess; protected holders only
    /// when nothing else is left, and never below their floor.
    fn pressure(&self, who: &Arc<AppUsage>) -> Pressure {
        let excess = self.total().saturating_sub(self.cap);
        if excess == 0 {
            return Pressure::Nothing;
        }
        let apps = self.apps.lock().unwrap_or_else(PoisonError::into_inner);
        let mut best: Option<Arc<AppUsage>> = None;
        for pass_protected in [false, true] {
            for w in apps.iter() {
                let Some(u) = w.upgrade() else { continue };
                if u.protected.load(Ordering::Relaxed) != pass_protected {
                    continue;
                }
                let floor = if pass_protected {
                    MIN_PROTECTED_NODES as u64
                } else {
                    0
                };
                if u.live() <= floor {
                    continue;
                }
                if best.as_ref().is_none_or(|b| u.bytes() > b.bytes()) {
                    best = Some(u);
                }
            }
            if best.is_some() {
                break;
            }
        }
        match best {
            Some(v) if Arc::ptr_eq(&v, who) => Pressure::Own,
            Some(v) => {
                v.shed.fetch_add(excess, Ordering::Relaxed);
                Pressure::Preempted
            }
            None => Pressure::Nothing,
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

/// One key's cell: swapped in place by the writer, loaded by any reader.
type Cell = Arc<ArcSwap<Entry>>;

#[derive(Debug)]
struct Shared {
    index: scc::HashIndex<NodeKey, Cell, KeyBuild>,
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
    match snap.fresh {
        Freshness::Fresh { at } if ttl_ms > 0 => millis(at.elapsed()) >= ttl_ms,
        Freshness::Fresh { .. } | Freshness::Stale { .. } | Freshness::Defunct => false,
    }
}

fn stale_of(snap: &NodeSnapshot, epoch: u64) -> PropMask {
    if snap.confirmed_seq < epoch {
        PropMask::ALL
    } else {
        snap.fresh.stale_mask()
    }
}

impl Shared {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>> {
        if self.retired.load(Ordering::Acquire) {
            return Lookup::Gone;
        }
        match self.index.peek_with(&k, |_, c| match &**c.load() {
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
        match self.index.peek_with(&k, |_, c| match &**c.load() {
            Entry::Live { snap, .. } => Lookup::Hit(NodeView {
                snap: Arc::clone(snap),
                stale: stale_of(snap, epoch),
                aged: is_aged(snap, ttl_ms),
            }),
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
    recent_order: VecDeque<(NodeKey, u64)>,
    /// Requests issued before this sequence may have missed an invalidation of an unknown key.
    inval_floor: u64,
    focus: Option<NodeKey>,
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
        index: scc::HashIndex::with_hasher(KeyBuild),
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
        hints: Vec::new(),
        max_nodes: MAX_MIRROR_NODES,
        max_bytes: MAX_MIRROR_BYTES,
        usage,
        budget,
    };
    (writer, MirrorHandle { shared })
}

/// The unpublished replacement for `old` once `new` arrives: fields only the old snapshot has
/// (value, relations, attributes, bounds) survive a partial reply when nothing marked them
/// stale; everything else comes from the new content.
fn merge(old: &NodeSnapshot, new: &mut NodeSnapshot, old_stale: PropMask) {
    const EXTRA: [PropMask; 4] = [
        PropMask::VALUE,
        PropMask::RELATIONS,
        PropMask::ATTRS,
        PropMask::BOUNDS,
    ];
    for f in EXTRA {
        if old.have.contains(f) && !new.have.contains(f) && !old_stale.intersects(f) {
            match f {
                PropMask::VALUE => new.value.clone_from(&old.value),
                PropMask::RELATIONS => new.relations.clone_from(&old.relations),
                PropMask::ATTRS => new.attrs.clone_from(&old.attrs),
                _ => new.bounds = old.bounds,
            }
            new.have |= f;
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
            .peek_with(&k, |_, c| Entry::clone(&c.load()))
    }

    fn put(&self, k: NodeKey, e: Entry) {
        let e = Arc::new(e);
        let swapped = self
            .shared
            .index
            .peek_with(&k, |_, c| c.store(Arc::clone(&e)));
        if swapped.is_none() {
            let _ = self.shared.index.insert_sync(k, Arc::new(ArcSwap::new(e)));
        }
    }

    fn charge(&self, n: &NodeSnapshot) {
        let s = &self.shared;
        let b = node_bytes(n);
        s.live.fetch_add(1, Ordering::Relaxed);
        s.bytes.fetch_add(b, Ordering::Relaxed);
        self.usage.live.fetch_add(1, Ordering::Relaxed);
        self.usage.bytes.fetch_add(b, Ordering::Relaxed);
        if let Some(bu) = &self.budget {
            bu.total.fetch_add(b, Ordering::Relaxed);
        }
        if is_stale(n) {
            s.stale.fetch_add(1, Ordering::Relaxed);
        }
        if is_partial(n) {
            s.partial.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn refund(&self, n: &NodeSnapshot) {
        let s = &self.shared;
        let b = node_bytes(n);
        s.live.fetch_sub(1, Ordering::Relaxed);
        s.bytes.fetch_sub(b, Ordering::Relaxed);
        self.usage.live.fetch_sub(1, Ordering::Relaxed);
        self.usage.bytes.fetch_sub(b, Ordering::Relaxed);
        if let Some(bu) = &self.budget {
            bu.total.fetch_sub(b, Ordering::Relaxed);
        }
        if is_stale(n) {
            s.stale.fetch_sub(1, Ordering::Relaxed);
        }
        if is_partial(n) {
            s.partial.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Apply a reply or a push. Per node: dropped if tombstoned at a seq > `issued_at`; kept
    /// `Stale` if invalidated at a seq > `issued_at`; otherwise `Fresh`. Never revives a key
    /// whose tombstone is newer than the request (MA-4).
    pub fn apply(&mut self, batch: SnapshotBatch, origin: ApplyOrigin) -> ApplyOutcome {
        let mut out = ApplyOutcome {
            truncated: batch.truncated,
            ..ApplyOutcome::default()
        };
        let (issued_at, allow_revive) = match origin {
            ApplyOrigin::Reply { issued_at } | ApplyOrigin::CacheDump { issued_at } => {
                (issued_at, true)
            }
            ApplyOrigin::Push => (self.seq(), false),
        };
        let now = Instant::now();
        for snap in batch.nodes {
            self.apply_one(snap, issued_at, allow_revive, now, &mut out);
        }
        self.enforce_bounds(&mut out);
        self.bump_rev();
        self.debug_check();
        out
    }

    fn apply_one(
        &mut self,
        snap: Arc<NodeSnapshot>,
        issued_at: u64,
        allow_revive: bool,
        now: Instant,
        out: &mut ApplyOutcome,
    ) {
        let k = snap.key;
        if !self.owns(k) {
            out.dropped_foreign += 1;
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
            && o.confirmed_seq > issued_at
        {
            // The stored content was confirmed by a newer request than this reply.
            out.superseded += 1;
            return;
        }
        let is_new = old.is_none();
        let mut node = Arc::try_unwrap(snap).unwrap_or_else(|a| (*a).clone());
        node.confirmed_seq = issued_at;
        let redirtied = inv_seq > issued_at;
        if let Some(o) = &old {
            let old_stale = stale_of(o, self.shared.stale_epoch.load(Ordering::Acquire));
            merge(o, &mut node, old_stale);
        }
        node.fresh = if redirtied {
            out.kept_stale += 1;
            Freshness::Stale {
                since: now,
                mask: inv_mask,
            }
        } else {
            Freshness::Fresh { at: now }
        };
        let node = Arc::new(node);
        match old {
            Some(o) => self.refund(&o),
            None => {
                if !revived {
                    self.make_room(out);
                }
            }
        }
        if revived {
            self.shared.tombstones.fetch_sub(1, Ordering::Relaxed);
            self.shared.revivals.fetch_add(1, Ordering::Relaxed);
            out.revived += 1;
        }
        if is_new {
            self.fifo.push_back((k, issued_at));
        }
        self.charge(&node);
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
        let mut cur = self.focus;
        while let Some(k) = cur {
            if keys.len() >= 65 || keys.contains(&k) {
                break;
            }
            keys.push(k);
            cur = self
                .shared
                .index
                .peek_with(&k, |_, c| match &**c.load() {
                    Entry::Live { snap, .. } => snap.parent,
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
            let live_seq = self.shared.index.peek_with(&k, |_, c| match &**c.load() {
                Entry::Live { snap, .. } => Some(snap.confirmed_seq),
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
        if let Some(b) = self.budget.clone()
            && b.total() > b.cap()
        {
            match b.pressure(&self.usage) {
                Pressure::Own => {
                    let floor = self.own_floor();
                    while b.total() > b.cap() && self.shared.live.load(Ordering::Relaxed) > floor {
                        if !self.evict_one() {
                            break;
                        }
                        out.evicted += 1;
                    }
                }
                Pressure::Preempted | Pressure::Nothing => {}
            }
        }
        let shed = self.usage.shed.swap(0, Ordering::Relaxed);
        if shed > 0 {
            out.evicted += self.shed_bytes(shed);
        }
    }

    fn own_floor(&self) -> u64 {
        if self.usage.protected.load(Ordering::Relaxed) {
            MIN_PROTECTED_NODES as u64
        } else {
            0
        }
    }

    /// Evict until `target` bytes were freed (another app hit the global cap), at most
    /// `SWEEP_BUDGET` nodes per call.
    fn shed_bytes(&mut self, target: u64) -> u32 {
        let start = self.shared.bytes.load(Ordering::Relaxed);
        let floor = self.own_floor();
        let mut n = 0u32;
        while (n as usize) < SWEEP_BUDGET
            && start.saturating_sub(self.shared.bytes.load(Ordering::Relaxed)) < target
            && self.shared.live.load(Ordering::Relaxed) > floor
        {
            if !self.evict_one() {
                break;
            }
            n += 1;
        }
        n
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
                    matches!(snap.fresh, Freshness::Stale { mask: m, .. } if m.contains(mask));
                let next = if already {
                    Arc::clone(&snap)
                } else {
                    let mut n = (*snap).clone();
                    n.fresh = n.fresh.step(FreshnessEvent::Invalidate { at, mask });
                    let n = Arc::new(n);
                    self.refund(&snap);
                    self.charge(&n);
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
            Some(Entry::Tombstone { .. }) => false,
            None => {
                self.remember_invalidation(k, seq, mask);
                false
            }
        };
        self.bump_rev();
        self.debug_check();
        marked
    }

    fn remember_invalidation(&mut self, k: NodeKey, seq: u64, mask: PropMask) {
        while self.recent_inval.len() >= MAX_RECENT_INVALIDATIONS {
            let Some((old, s)) = self.recent_order.pop_front() else {
                // Order queue lost track: fall back to the floor for everything.
                self.inval_floor = seq;
                self.recent_inval.clear();
                break;
            };
            if self
                .recent_inval
                .get(&old)
                .is_some_and(|&(cur, _)| cur == s)
            {
                self.recent_inval.remove(&old);
                self.inval_floor = self.inval_floor.max(s);
            }
        }
        let m = self
            .recent_inval
            .get(&k)
            .map_or(mask, |&(_, prev)| prev | mask);
        self.recent_inval.insert(k, (seq, m));
        self.recent_order.push_back((k, seq));
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
        self.recent_inval.remove(&k);
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
                |_, c| matches!(&**c.load(), Entry::Tombstone { seq, .. } if *seq == t.seq),
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
            if let Entry::Live { snap, .. } = &**c.load() {
                bytes += node_bytes(snap);
            }
            true
        });
        self.shared.index.clear_sync();
        if let Some(b) = &self.budget {
            b.total.fetch_sub(bytes, Ordering::Relaxed);
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

    /// Counters equal real sizes (MA-1). O(n).
    pub fn check_invariants(&self) -> Result<(), MirrorInvariantError> {
        let (mut live, mut tomb, mut stale, mut bytes, mut partial) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        self.shared.index.iter_sync(|_, c| {
            match &**c.load() {
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

#[cfg(test)]
mod tests;
