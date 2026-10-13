//! Per-app lock-free snapshot store (ADR-0001 section 5.2, PC-1).
//!
//! Responsibility (section 11): "`MirrorRead` / `MirrorWriter`, per-app lock-free snapshot store
//! (MB-1)". One writer (the owning app actor) publishes immutable `Arc<NodeSnapshot>`s; any
//! thread reads without a lock and without a message.
//!
//! - **MS-1** Reads never block and never allocate on a hit.
//! - **MS-2** Per-node consistency only.
//! - **MS-3** The implementation candidate is decided by microbenchmark MB-1. The one here is a
//!   sharded map of `ArcSwap`s with copy-on-write shards: wait-free reads, writer cost
//!   proportional to the shard size per touched shard. It is a placeholder for MB-1, not a
//!   decision.
//!
//! Allowed dependencies: `gorca-model`, `arc-swap`. Never a transport crate.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use gorca_model::{Lookup, NodeKey, NodeSnapshot, SnapshotBatch};

/// Read side. Implementations must be wait-free on a hit.
pub trait MirrorRead: Send + Sync {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>>;
    /// Bumped once per applied batch.
    fn revision(&self) -> u64;
}

const SHARDS: usize = 16;

#[derive(Clone, Debug)]
enum Entry {
    Live(Arc<NodeSnapshot>),
    /// Removed; resolves to `Lookup::Gone` until evicted.
    Tombstone,
}

type Shard = HashMap<NodeKey, Entry>;

#[derive(Debug)]
struct Shared {
    shards: [ArcSwap<Shard>; SHARDS],
    revision: AtomicU64,
}

impl Shared {
    fn shard_of(k: NodeKey) -> usize {
        // Fibonacci hashing of the connector-local id; app serial mixed in.
        let h = (k.local.0 ^ (u64::from(k.app.serial) << 32)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // Intentional truncation: the top bits select one of 16 shards.
        usize::try_from(h >> 60).unwrap_or(0) % SHARDS
    }
}

/// Cloneable read handle.
#[derive(Clone, Debug)]
pub struct MirrorHandle {
    shared: Arc<Shared>,
}

impl MirrorRead for MirrorHandle {
    fn get(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>> {
        let shard = self.shared.shards[Shared::shard_of(k)].load();
        match shard.get(&k) {
            Some(Entry::Live(n)) => Lookup::Hit(Arc::clone(n)),
            Some(Entry::Tombstone) => Lookup::Gone,
            None => Lookup::Miss,
        }
    }

    fn revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }
}

/// Write side. Not `Clone`: owned by exactly one app actor.
#[derive(Debug)]
pub struct MirrorWriter {
    shared: Arc<Shared>,
}

/// Create a mirror: the single writer and a cloneable read handle.
pub fn mirror() -> (MirrorWriter, MirrorHandle) {
    let shared = Arc::new(Shared {
        shards: std::array::from_fn(|_| ArcSwap::from_pointee(Shard::new())),
        revision: AtomicU64::new(0),
    });
    (
        MirrorWriter {
            shared: Arc::clone(&shared),
        },
        MirrorHandle { shared },
    )
}

impl MirrorWriter {
    /// Publish a batch; each node becomes visible atomically (MS-2). A node whose key is
    /// tombstoned stays tombstoned: a key is never revived (I-1).
    pub fn apply(&mut self, batch: SnapshotBatch) {
        let mut per_shard: [Vec<Arc<NodeSnapshot>>; SHARDS] = std::array::from_fn(|_| Vec::new());
        for n in batch.nodes {
            per_shard[Shared::shard_of(n.key)].push(n);
        }
        for (i, nodes) in per_shard.into_iter().enumerate() {
            if nodes.is_empty() {
                continue;
            }
            let mut next = Shard::clone(&self.shared.shards[i].load());
            for n in nodes {
                if matches!(next.get(&n.key), Some(Entry::Tombstone)) {
                    continue;
                }
                next.insert(n.key, Entry::Live(n));
            }
            self.shared.shards[i].store(Arc::new(next));
        }
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
    }

    pub fn tombstone(&mut self, k: NodeKey) {
        let i = Shared::shard_of(k);
        let mut next = Shard::clone(&self.shared.shards[i].load());
        next.insert(k, Entry::Tombstone);
        self.shared.shards[i].store(Arc::new(next));
        self.shared.revision.fetch_add(1, Ordering::AcqRel);
    }

    /// TBD: tombstone eviction after N seconds or when the app is `Gone` (section 4.4).
    pub fn reader(&self) -> MirrorHandle {
        MirrorHandle {
            shared: Arc::clone(&self.shared),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ChildList, ConnectorId, Freshness, Ifaces, LocalId, Role, StateSet};
    use std::time::Instant;

    fn key(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            local: LocalId(n),
        }
    }

    fn node(n: u64, name: &str) -> Arc<NodeSnapshot> {
        Arc::new(NodeSnapshot {
            key: key(n),
            role: Role::Button,
            states: StateSet::empty(),
            name: Some(Arc::from(name)),
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            have: gorca_model::PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    #[test]
    fn miss_hit_gone() {
        let (mut w, r) = mirror();
        assert_eq!(r.get(key(1)), Lookup::Miss);
        w.apply(SnapshotBatch {
            nodes: vec![node(1, "ok")],
            ..Default::default()
        });
        let hit = r.get(key(1)).hit().unwrap();
        assert_eq!(hit.name.as_deref(), Some("ok"));
        w.tombstone(key(1));
        assert_eq!(r.get(key(1)), Lookup::Gone);
    }

    #[test]
    fn a_tombstoned_key_is_never_revived() {
        let (mut w, r) = mirror();
        w.tombstone(key(1));
        w.apply(SnapshotBatch {
            nodes: vec![node(1, "late reply")],
            ..Default::default()
        });
        assert_eq!(r.get(key(1)), Lookup::Gone);
    }

    #[test]
    fn revision_bumps_per_batch() {
        let (mut w, r) = mirror();
        assert_eq!(r.revision(), 0);
        w.apply(SnapshotBatch {
            nodes: (0..100).map(|n| node(n, "x")).collect(),
            ..Default::default()
        });
        assert_eq!(r.revision(), 1);
        for n in 0..100 {
            assert!(r.get(key(n)).hit().is_some());
        }
    }

    #[test]
    fn readers_see_whole_nodes_while_the_writer_runs() {
        let (mut w, r) = mirror();
        w.apply(SnapshotBatch {
            nodes: vec![node(1, "v0")],
            ..Default::default()
        });
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let r = r.clone();
                std::thread::spawn(move || {
                    for _ in 0..20_000 {
                        let n = r.get(key(1)).hit().unwrap();
                        let name = n.name.as_deref().unwrap();
                        assert!(name.starts_with('v'));
                    }
                })
            })
            .collect();
        for i in 1..2000 {
            w.apply(SnapshotBatch {
                nodes: vec![node(1, &format!("v{i}"))],
                ..Default::default()
            });
        }
        for h in readers {
            h.join().unwrap();
        }
    }
}
