//! The five candidate stores behind one trait set. All use the same cheap integer hasher so the
//! comparison is about the concurrency scheme, not SipHash. Values are `Arc<Node>` (MS-1: a hit
//! hands out a shared snapshot; per-node consistency only, MS-2).

use std::collections::HashMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use arc_swap::ArcSwap;

pub type Key = u64;
/// Parent of a root.
pub const NONE: Key = u64::MAX;

/// A mirrored accessible object, sized like the planned `NodeSnapshot` (name, role, states,
/// parent, children).
#[derive(Debug)]
pub struct Node {
    pub parent: Key,
    pub states: u64,
    pub version: u32,
    pub role: u16,
    pub name: Box<str>,
    pub children: Box<[Key]>,
}

pub fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Mix;
#[derive(Clone, Copy, Debug, Default)]
pub struct MixHasher(u64);

impl BuildHasher for Mix {
    type Hasher = MixHasher;
    fn build_hasher(&self) -> MixHasher {
        MixHasher(0)
    }
}

impl Hasher for MixHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = splitmix(self.0 ^ u64::from(*b));
        }
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = splitmix(i);
    }
}

/// Read side, one per reader thread; a session spans one command (a few lookups).
pub trait Session {
    fn get(&self, k: Key) -> Option<Arc<Node>>;
}

pub trait Reader {
    type Sess<'s>: Session
    where
        Self: 's;
    fn session(&mut self) -> Self::Sess<'_>;
}

/// Write side, single writer (the app actor). `publish` makes buffered writes visible; it is a
/// no-op for stores that publish on every call.
pub trait Writer {
    fn put(&mut self, k: Key, v: Arc<Node>);
    fn remove(&mut self, k: Key);
    fn publish(&mut self);
}

pub trait Store: Sync + Sized {
    const NAME: &'static str;
    type R<'a>: Reader
    where
        Self: 'a;
    type W<'a>: Writer
    where
        Self: 'a;
    fn new() -> Self;
    fn reader(&self) -> Self::R<'_>;
    fn writer(&self) -> Self::W<'_>;
}

/// Reader and writer for stores whose operations take `&self` and need no per-thread state.
pub struct Shared<'a, S>(&'a S);

pub trait Direct: Sync {
    fn d_get(&self, k: Key) -> Option<Arc<Node>>;
    fn d_put(&self, k: Key, v: Arc<Node>);
    fn d_remove(&self, k: Key);
}

impl<S: Direct> Session for Shared<'_, S> {
    fn get(&self, k: Key) -> Option<Arc<Node>> {
        self.0.d_get(k)
    }
}
impl<S: Direct> Reader for Shared<'_, S> {
    type Sess<'s>
        = Shared<'s, S>
    where
        Self: 's;
    fn session(&mut self) -> Self::Sess<'_> {
        Shared(self.0)
    }
}
impl<S: Direct> Writer for Shared<'_, S> {
    fn put(&mut self, k: Key, v: Arc<Node>) {
        self.0.d_put(k, v);
    }
    fn remove(&mut self, k: Key) {
        self.0.d_remove(k);
    }
    fn publish(&mut self) {}
}

macro_rules! direct_store {
    ($t:ty, $name:expr) => {
        impl Store for $t {
            const NAME: &'static str = $name;
            type R<'a> = Shared<'a, $t>;
            type W<'a> = Shared<'a, $t>;
            fn new() -> Self {
                <$t>::make()
            }
            fn reader(&self) -> Self::R<'_> {
                Shared(self)
            }
            fn writer(&self) -> Self::W<'_> {
                Shared(self)
            }
        }
    };
}

// ---------------------------------------------------------------- dashmap (baseline)

pub struct DashStore(dashmap::DashMap<Key, Arc<Node>, Mix>);
impl DashStore {
    fn make() -> Self {
        Self(dashmap::DashMap::with_hasher(Mix))
    }
}
impl Direct for DashStore {
    fn d_get(&self, k: Key) -> Option<Arc<Node>> {
        self.0.get(&k).map(|r| Arc::clone(r.value()))
    }
    fn d_put(&self, k: Key, v: Arc<Node>) {
        self.0.insert(k, v);
    }
    fn d_remove(&self, k: Key) {
        self.0.remove(&k);
    }
}
direct_store!(DashStore, "dashmap");

// ---------------------------------------------------------------- scc::HashMap

pub struct SccMapStore(scc::HashMap<Key, Arc<Node>, Mix>);
impl SccMapStore {
    fn make() -> Self {
        Self(scc::HashMap::with_hasher(Mix))
    }
}
impl Direct for SccMapStore {
    fn d_get(&self, k: Key) -> Option<Arc<Node>> {
        self.0.read_sync(&k, |_, v| Arc::clone(v))
    }
    fn d_put(&self, k: Key, v: Arc<Node>) {
        let _ = self.0.upsert_sync(k, v);
    }
    fn d_remove(&self, k: Key) {
        let _ = self.0.remove_sync(&k);
    }
}
direct_store!(SccMapStore, "scc-map");

// ---------------------------------------------------------------- scc::HashIndex

pub struct SccIndexStore(scc::HashIndex<Key, Arc<Node>, Mix>);
impl SccIndexStore {
    fn make() -> Self {
        Self(scc::HashIndex::with_hasher(Mix))
    }
}
impl Direct for SccIndexStore {
    fn d_get(&self, k: Key) -> Option<Arc<Node>> {
        self.0.peek_with(&k, |_, v| Arc::clone(v))
    }
    fn d_put(&self, k: Key, v: Arc<Node>) {
        match self.0.entry_sync(k) {
            scc::hash_index::Entry::Occupied(mut o) => {
                o.update(v);
            }
            scc::hash_index::Entry::Vacant(e) => {
                e.insert_entry(v);
            }
        }
    }
    fn d_remove(&self, k: Key) {
        let _ = self.0.remove_sync(&k);
    }
}
direct_store!(SccIndexStore, "scc-index");

// ---------------------------------------------------------------- papaya

pub struct PapayaStore(papaya::HashMap<Key, Arc<Node>, Mix>);
impl PapayaStore {
    fn make() -> Self {
        Self(papaya::HashMap::builder().hasher(Mix).build())
    }
}
impl Direct for PapayaStore {
    fn d_get(&self, k: Key) -> Option<Arc<Node>> {
        self.0.pin().get(&k).cloned()
    }
    fn d_put(&self, k: Key, v: Arc<Node>) {
        self.0.pin().insert(k, v);
    }
    fn d_remove(&self, k: Key) {
        self.0.pin().remove(&k);
    }
}
direct_store!(PapayaStore, "papaya");

/// Papaya with one pin held for a whole command, as a reader doing a parent/child walk would.
pub struct PapayaPinStore(papaya::HashMap<Key, Arc<Node>, Mix>);
pub struct PapayaSess<'a>(papaya::HashMapRef<'a, Key, Arc<Node>, Mix, papaya::LocalGuard<'a>>);
impl Session for PapayaSess<'_> {
    fn get(&self, k: Key) -> Option<Arc<Node>> {
        self.0.get(&k).cloned()
    }
}
pub struct PapayaReader<'a>(&'a PapayaPinStore);
impl Reader for PapayaReader<'_> {
    type Sess<'s>
        = PapayaSess<'s>
    where
        Self: 's;
    fn session(&mut self) -> Self::Sess<'_> {
        PapayaSess(self.0.0.pin())
    }
}
pub struct PapayaWriter<'a>(&'a PapayaPinStore);
impl Writer for PapayaWriter<'_> {
    fn put(&mut self, k: Key, v: Arc<Node>) {
        self.0.0.pin().insert(k, v);
    }
    fn remove(&mut self, k: Key) {
        self.0.0.pin().remove(&k);
    }
    fn publish(&mut self) {}
}
impl Store for PapayaPinStore {
    const NAME: &'static str = "papaya-pin";
    type R<'a> = PapayaReader<'a>;
    type W<'a> = PapayaWriter<'a>;
    fn new() -> Self {
        Self(papaya::HashMap::builder().hasher(Mix).build())
    }
    fn reader(&self) -> Self::R<'_> {
        PapayaReader(self)
    }
    fn writer(&self) -> Self::W<'_> {
        PapayaWriter(self)
    }
}

// ---------------------------------------------------------------- sharded ArcSwap

/// 64 shards of `RwLock<HashMap<Key, ArcSwap<Node>>>`. A property change swaps the `ArcSwap`
/// under the shard's read lock (readers are never excluded); only insert and remove take the
/// shard's write lock.
const SHARDS: usize = 64;
pub struct ArcSwapStore(Box<[RwLock<HashMap<Key, ArcSwap<Node>, Mix>>]>);

fn shard_of(k: Key) -> usize {
    // Independent of the inner hash so the inner tables keep full entropy.
    (splitmix(k ^ 0x5851_F42D_4C95_7F2D) >> 58) as usize % SHARDS
}

impl ArcSwapStore {
    fn make() -> Self {
        Self(
            (0..SHARDS)
                .map(|_| RwLock::new(HashMap::with_hasher(Mix)))
                .collect(),
        )
    }
}
impl Direct for ArcSwapStore {
    fn d_get(&self, k: Key) -> Option<Arc<Node>> {
        let g = self.0[shard_of(k)]
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.get(&k).map(ArcSwap::load_full)
    }
    fn d_put(&self, k: Key, v: Arc<Node>) {
        let shard = &self.0[shard_of(k)];
        {
            let g = shard
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(slot) = g.get(&k) {
                slot.store(v);
                return;
            }
        }
        let mut g = shard
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match g.get(&k) {
            Some(slot) => slot.store(v),
            None => {
                g.insert(k, ArcSwap::from(v));
            }
        }
    }
    fn d_remove(&self, k: Key) {
        let mut g = self.0[shard_of(k)]
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        g.remove(&k);
    }
}
direct_store!(ArcSwapStore, "arcswap-sharded");

// ---------------------------------------------------------------- left-right

#[derive(Clone, Default)]
pub struct LrMap(HashMap<Key, Arc<Node>, Mix>);

pub enum LrOp {
    Put(Key, Arc<Node>),
    Remove(Key),
}

impl left_right::Absorb<LrOp> for LrMap {
    fn absorb_first(&mut self, op: &mut LrOp, _other: &Self) {
        match op {
            LrOp::Put(k, v) => {
                self.0.insert(*k, Arc::clone(v));
            }
            LrOp::Remove(k) => {
                self.0.remove(k);
            }
        }
    }
    fn absorb_second(&mut self, op: LrOp, _other: &Self) {
        match op {
            LrOp::Put(k, v) => {
                self.0.insert(k, v);
            }
            LrOp::Remove(k) => {
                self.0.remove(&k);
            }
        }
    }
    fn sync_with(&mut self, first: &Self) {
        self.0.clone_from(&first.0);
    }
}

pub struct LrStore {
    w: Mutex<left_right::WriteHandle<LrMap, LrOp>>,
    r: left_right::ReadHandleFactory<LrMap>,
}

pub struct LrReader(left_right::ReadHandle<LrMap>);
pub struct LrSess<'a>(Option<left_right::ReadGuard<'a, LrMap>>);
impl Session for LrSess<'_> {
    fn get(&self, k: Key) -> Option<Arc<Node>> {
        self.0.as_ref().and_then(|m| m.0.get(&k).cloned())
    }
}
impl Reader for LrReader {
    type Sess<'s>
        = LrSess<'s>
    where
        Self: 's;
    fn session(&mut self) -> Self::Sess<'_> {
        LrSess(self.0.enter())
    }
}
pub struct LrWriter<'a>(MutexGuard<'a, left_right::WriteHandle<LrMap, LrOp>>);
impl Writer for LrWriter<'_> {
    fn put(&mut self, k: Key, v: Arc<Node>) {
        self.0.append(LrOp::Put(k, v));
    }
    fn remove(&mut self, k: Key) {
        self.0.append(LrOp::Remove(k));
    }
    fn publish(&mut self) {
        self.0.publish();
    }
}
impl Store for LrStore {
    const NAME: &'static str = "left-right";
    type R<'a> = LrReader;
    type W<'a> = LrWriter<'a>;
    fn new() -> Self {
        let (w, r) = left_right::new_from_empty::<LrMap, LrOp>(LrMap::default());
        Self {
            w: Mutex::new(w),
            r: r.factory(),
        }
    }
    fn reader(&self) -> Self::R<'_> {
        LrReader(self.r.handle())
    }
    fn writer(&self) -> Self::W<'_> {
        LrWriter(
            self.w
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}
