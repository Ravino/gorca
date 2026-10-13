//! Reverse mapping `NodeKey` -> object path, bounded **per owning process** and globally.
//!
//! The drain hashes the object path into the `LocalId` (`gorca_dbus::drain::local_id`), which is
//! what keeps the hot path allocation-free but loses the path. Focus events, the only ones the
//! connector must call back on, record it here (`Router::focus_seen`) before they are pushed.
//!
//! Isolation (ADR-0001 6.5, ADR-0004 per-app reserve and AU-3 preemption):
//!
//! - Quotas belong to an **owner**: the process id the bus reports for a connection
//!   (`GetConnectionCredentials`). One process with a thousand bus names has one quota, not a
//!   thousand. A connection whose credentials are unknown gets its own small bucket.
//! - A global **byte** cap bounds the whole table. When it is exceeded the largest bucket is
//!   evicted (preemption of the biggest holder), never a newcomer: a new application always
//!   gets room, and a hostile process only ever loses its own entries.
//! - One path is at most [`MAX_PATH_BYTES`] long.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use gorca_model::{AppId, LocalId, NodeKey};

/// Entries one owner (process) may keep. Past it, that owner's bucket is cleared and refilled by
/// its next focus events; no other owner is touched.
pub const MAX_PATHS_PER_OWNER: usize = 1024;
/// Entries of a connection with unknown credentials.
pub const MAX_PATHS_UNKNOWN_OWNER: usize = 64;
/// Longest object path kept. Real AT-SPI paths are under 64 bytes.
pub const MAX_PATH_BYTES: usize = 256;
/// Bytes the whole table may hold, counting a fixed per-entry overhead.
pub const GLOBAL_BYTE_CAP: usize = 16 * 1024 * 1024;
/// Accounting overhead of one entry (map slot, `Arc` header, key).
pub const ENTRY_OVERHEAD: usize = 64;

/// Quota holder: a process id, or a per-connection stand-in when the bus cannot say.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Owner {
    Pid(u32),
    /// Credentials unknown: the application is its own owner, with a small quota.
    Unknown(AppId),
}

impl Owner {
    fn quota(self) -> usize {
        match self {
            Owner::Pid(_) => MAX_PATHS_PER_OWNER,
            Owner::Unknown(_) => MAX_PATHS_UNKNOWN_OWNER,
        }
    }
}

#[derive(Debug, Default)]
struct Bucket {
    map: HashMap<(AppId, LocalId), Arc<str>>,
    bytes: usize,
}

fn cost(path: &str) -> usize {
    path.len() + ENTRY_OVERHEAD
}

#[derive(Debug, Default)]
struct Inner {
    owners: HashMap<Owner, Bucket>,
    app_owner: HashMap<AppId, Owner>,
    total_bytes: usize,
}

impl Inner {
    fn clear_bucket(&mut self, owner: Owner) {
        if let Some(b) = self.owners.get_mut(&owner) {
            self.total_bytes = self.total_bytes.saturating_sub(b.bytes);
            b.map.clear();
            b.bytes = 0;
        }
    }

    /// Evict the largest bucket until the table fits its cap again.
    fn enforce_cap(&mut self, cap: usize) {
        while self.total_bytes > cap {
            let Some((&owner, _)) = self.owners.iter().max_by_key(|(_, b)| b.bytes) else {
                break;
            };
            let before = self.total_bytes;
            self.clear_bucket(owner);
            if self.total_bytes == before {
                break;
            }
        }
    }
}

#[derive(Debug)]
pub struct PathTable {
    inner: Mutex<Inner>,
    cap: usize,
}

impl Default for PathTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PathTable {
    pub fn new() -> Self {
        Self::with_byte_cap(GLOBAL_BYTE_CAP)
    }

    pub fn with_byte_cap(cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            cap,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Assign `app` to `owner`. Call once, when the application is published.
    pub fn bind_app(&self, app: AppId, owner: Owner) {
        let mut g = self.lock();
        g.app_owner.insert(app, owner);
        g.owners.entry(owner).or_default();
    }

    /// Record `path` for `node`. Allocates only when the node is new. Oversized paths are
    /// refused. Never refuses an application: if the table is full, the largest holder pays.
    pub fn insert(&self, node: NodeKey, path: &str) {
        if path.len() > MAX_PATH_BYTES {
            return;
        }
        let mut g = self.lock();
        let owner = if let Some(o) = g.app_owner.get(&node.app) {
            *o
        } else {
            let o = Owner::Unknown(node.app);
            g.app_owner.insert(node.app, o);
            o
        };
        let key = (node.app, node.local);
        let bucket = g.owners.entry(owner).or_default();
        if bucket.map.get(&key).is_some_and(|p| &**p == path) {
            return;
        }
        let replacing = bucket.map.contains_key(&key);
        if !replacing && bucket.map.len() >= owner.quota() {
            g.clear_bucket(owner);
        }
        let bucket = g.owners.entry(owner).or_default();
        let old = bucket.map.insert(key, Arc::from(path));
        let freed = old.map_or(0, |p| cost(&p));
        bucket.bytes = bucket.bytes.saturating_sub(freed) + cost(path);
        g.total_bytes = g.total_bytes.saturating_sub(freed) + cost(path);
        let cap = self.cap;
        g.enforce_cap(cap);
    }

    pub fn get(&self, node: NodeKey) -> Option<Arc<str>> {
        let g = self.lock();
        let owner = g.app_owner.get(&node.app)?;
        g.owners
            .get(owner)?
            .map
            .get(&(node.app, node.local))
            .cloned()
    }

    /// Forget everything of an application that left the bus.
    pub fn forget_app(&self, app: AppId) {
        let mut g = self.lock();
        let Some(owner) = g.app_owner.remove(&app) else {
            return;
        };
        let mut freed = 0;
        let mut empty = false;
        if let Some(b) = g.owners.get_mut(&owner) {
            b.map.retain(|(a, _), p| {
                let keep = *a != app;
                if !keep {
                    freed += cost(p);
                }
                keep
            });
            b.bytes = b.bytes.saturating_sub(freed);
            empty = b.map.is_empty();
        }
        g.total_bytes = g.total_bytes.saturating_sub(freed);
        if empty && !g.app_owner.values().any(|o| *o == owner) {
            g.owners.remove(&owner);
        }
    }

    /// Entries held in total.
    pub fn len(&self) -> usize {
        self.lock().owners.values().map(|b| b.map.len()).sum()
    }

    /// Entries held by the owner of `app`.
    pub fn owner_len(&self, app: AppId) -> usize {
        let g = self.lock();
        g.app_owner
            .get(&app)
            .and_then(|o| g.owners.get(o))
            .map_or(0, |b| b.map.len())
    }

    pub fn bytes(&self) -> usize {
        self.lock().total_bytes
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::ConnectorId;

    fn app(n: u32) -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: n,
        }
    }

    fn key(a: u32, n: u64) -> NodeKey {
        NodeKey {
            app: app(a),
            local: LocalId(n),
        }
    }

    #[test]
    fn insert_get_and_forget() {
        let t = PathTable::new();
        t.bind_app(app(1), Owner::Pid(10));
        t.bind_app(app(2), Owner::Pid(20));
        t.insert(key(1, 1), "/a/1");
        t.insert(key(2, 1), "/a/9");
        assert_eq!(t.get(key(1, 1)).as_deref(), Some("/a/1"));
        t.forget_app(app(1));
        assert!(t.get(key(1, 1)).is_none());
        assert_eq!(t.get(key(2, 1)).as_deref(), Some("/a/9"));
        assert_eq!(t.bytes(), cost("/a/9"));
    }

    #[test]
    fn one_owner_is_bounded_by_its_own_quota() {
        let t = PathTable::new();
        t.bind_app(app(1), Owner::Pid(10));
        for n in 0..(MAX_PATHS_PER_OWNER as u64 * 5) {
            t.insert(key(1, n), "/x");
        }
        assert!(t.owner_len(app(1)) <= MAX_PATHS_PER_OWNER);
        assert!(t.get(key(1, MAX_PATHS_PER_OWNER as u64 * 5 - 1)).is_some());
    }

    /// Regression (review F5): the quota belongs to the process, not to the bus name. One
    /// process with thousands of names (the probe: 4096 names x 1024 paths, +2.4 GiB) stays
    /// inside one owner's quota, and an application that starts afterwards is still served.
    #[test]
    fn many_bus_names_of_one_process_share_one_quota() {
        let t = PathTable::new();
        t.insert(key(100_000, 7), "/victim/focus"); // unbound: small unknown-owner bucket
        t.bind_app(app(100_001), Owner::Pid(2));
        for name in 0..4096u32 {
            t.bind_app(app(name), Owner::Pid(666));
            for n in 0..1024u64 {
                t.insert(key(name, n), "/attacker/path/with/some/length");
            }
        }
        assert!(
            t.len() <= MAX_PATHS_PER_OWNER + MAX_PATHS_UNKNOWN_OWNER + 1,
            "{} entries",
            t.len()
        );
        assert!(t.bytes() < 4 * 1024 * 1024, "{} bytes", t.bytes());
        // A new application (new process) is not locked out.
        t.insert(key(100_001, 1), "/new/app/focus");
        assert_eq!(t.get(key(100_001, 1)).as_deref(), Some("/new/app/focus"));
        assert_eq!(t.get(key(100_000, 7)).as_deref(), Some("/victim/focus"));
    }

    /// The global byte cap preempts the largest holder; the small victim keeps its entries and
    /// the newcomer always fits.
    #[test]
    fn the_global_cap_evicts_the_largest_owner_not_the_newcomer() {
        let t = PathTable::with_byte_cap(200 * 1024);
        t.bind_app(app(1), Owner::Pid(1));
        t.insert(key(1, 1), "/victim/focus");
        for pid in 100..140u32 {
            t.bind_app(app(pid), Owner::Pid(pid));
            for n in 0..1024u64 {
                t.insert(key(pid, n), "/hostile/path/that/is/not/short/at/all");
            }
        }
        assert!(t.bytes() <= 200 * 1024, "{} bytes", t.bytes());
        assert_eq!(t.get(key(1, 1)).as_deref(), Some("/victim/focus"));
        t.bind_app(app(500), Owner::Pid(500));
        t.insert(key(500, 1), "/late/app");
        assert_eq!(t.get(key(500, 1)).as_deref(), Some("/late/app"));
        assert!(t.bytes() <= 200 * 1024);
    }

    #[test]
    fn unknown_credentials_get_a_small_private_bucket() {
        let t = PathTable::new();
        for n in 0..(MAX_PATHS_UNKNOWN_OWNER as u64 * 10) {
            t.insert(key(9, n), "/x");
        }
        assert!(t.owner_len(app(9)) <= MAX_PATHS_UNKNOWN_OWNER);
    }

    #[test]
    fn absurd_paths_are_refused() {
        let t = PathTable::new();
        t.insert(key(1, 1), &"p".repeat(MAX_PATH_BYTES + 1));
        assert!(t.is_empty());
        t.insert(key(1, 1), &"p".repeat(MAX_PATH_BYTES));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn forgetting_the_last_app_of_an_owner_frees_everything() {
        let t = PathTable::new();
        t.bind_app(app(1), Owner::Pid(5));
        t.bind_app(app(2), Owner::Pid(5));
        t.insert(key(1, 1), "/a");
        t.insert(key(2, 1), "/b");
        t.forget_app(app(1));
        assert_eq!(t.get(key(2, 1)).as_deref(), Some("/b"));
        t.forget_app(app(2));
        assert_eq!((t.len(), t.bytes()), (0, 0));
    }
}
