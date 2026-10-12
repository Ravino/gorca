//! Reverse mapping `NodeKey` -> object path, bounded **per application**.
//!
//! The drain hashes the object path into the `LocalId` (`gorca_dbus::drain::local_id`), which is
//! what keeps the hot path allocation-free but loses the path. Focus events, the only ones the
//! connector must call back on, record it here (`Router::focus_seen`) before they are pushed.
//!
//! Isolation (ADR-0001 6.5, ADR-0004 per-app reserve): every application has its own bucket with
//! its own quota. An application that mints object paths without end can only evict its own
//! entries; it can neither flush nor starve another application's. The number of buckets is
//! bounded too (one per live application, dropped by [`PathTable::forget_app`]), and so is the
//! length of one path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use gorca_model::{AppId, LocalId, NodeKey};

/// Entries one application may keep. Past it, that application's bucket is cleared and refilled
/// by its next focus events; no other application is touched.
pub const MAX_PATHS_PER_APP: usize = 1024;
/// Longest object path kept (D-Bus allows 2^32; an application that uses more is lying).
pub const MAX_PATH_BYTES: usize = 512;
/// Applications with a bucket. Far above any desktop; a flood of fake apps cannot grow it.
pub const MAX_APPS: usize = 4096;

type Bucket = HashMap<LocalId, Arc<str>>;

#[derive(Debug, Default)]
pub struct PathTable {
    map: Mutex<HashMap<AppId, Bucket>>,
}

impl PathTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `path` for `node`. Allocates only when the node is new. Refuses oversized paths
    /// and new applications beyond [`MAX_APPS`].
    pub fn insert(&self, node: NodeKey, path: &str) {
        if path.len() > MAX_PATH_BYTES {
            return;
        }
        let mut m = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if !m.contains_key(&node.app) && m.len() >= MAX_APPS {
            return;
        }
        let bucket = m.entry(node.app).or_default();
        if bucket.get(&node.local).is_some_and(|p| &**p == path) {
            return;
        }
        if bucket.len() >= MAX_PATHS_PER_APP && !bucket.contains_key(&node.local) {
            bucket.clear();
        }
        bucket.insert(node.local, Arc::from(path));
    }

    pub fn get(&self, node: NodeKey) -> Option<Arc<str>> {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&node.app)
            .and_then(|b| b.get(&node.local))
            .cloned()
    }

    /// Forget everything of an application that left the bus.
    pub fn forget_app(&self, app: AppId) {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&app);
    }

    pub fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(HashMap::len)
            .sum()
    }

    pub fn app_len(&self, app: AppId) -> usize {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&app)
            .map_or(0, HashMap::len)
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
        t.insert(key(1, 1), "/a/1");
        t.insert(key(2, 1), "/a/9");
        assert_eq!(t.get(key(1, 1)).as_deref(), Some("/a/1"));
        t.forget_app(app(1));
        assert!(t.get(key(1, 1)).is_none());
        assert_eq!(t.get(key(2, 1)).as_deref(), Some("/a/9"));
    }

    #[test]
    fn one_app_is_bounded_by_its_own_quota() {
        let t = PathTable::new();
        for n in 0..(MAX_PATHS_PER_APP as u64 * 5) {
            t.insert(key(1, n), "/x");
        }
        assert!(t.app_len(app(1)) <= MAX_PATHS_PER_APP);
        assert!(t.get(key(1, MAX_PATHS_PER_APP as u64 * 5 - 1)).is_some());
    }

    /// Regression (security review): a hostile app minting paths must not evict, flush or
    /// starve the entries of another application.
    #[test]
    fn a_hostile_app_cannot_touch_another_apps_entries() {
        let t = PathTable::new();
        t.insert(key(2, 7), "/victim/focus");
        for n in 0..(MAX_PATHS_PER_APP as u64 * 50) {
            t.insert(key(1, n), "/attacker/path/with/some/length");
        }
        assert_eq!(t.get(key(2, 7)).as_deref(), Some("/victim/focus"));
        assert_eq!(t.app_len(app(2)), 1);
        assert!(t.len() <= MAX_PATHS_PER_APP + 1);
        // The victim can still record new focus nodes.
        t.insert(key(2, 8), "/victim/other");
        assert_eq!(t.get(key(2, 8)).as_deref(), Some("/victim/other"));
    }

    #[test]
    fn the_number_of_apps_is_bounded_and_existing_apps_keep_working() {
        let t = PathTable::new();
        let max = u32::try_from(MAX_APPS).unwrap();
        for a in 0..(max + 100) {
            t.insert(key(a, 1), "/p");
        }
        assert!(t.get(key(max + 50, 1)).is_none());
        t.insert(key(0, 2), "/q");
        assert_eq!(t.get(key(0, 2)).as_deref(), Some("/q"));
    }

    #[test]
    fn absurd_paths_are_refused() {
        let t = PathTable::new();
        t.insert(key(1, 1), &"p".repeat(MAX_PATH_BYTES + 1));
        assert!(t.is_empty());
        t.insert(key(1, 1), &"p".repeat(MAX_PATH_BYTES));
        assert_eq!(t.len(), 1);
    }
}
