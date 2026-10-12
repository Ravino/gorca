//! Reverse mapping `NodeKey` -> object path.
//!
//! The drain hashes the object path into the `LocalId` (`gorca_dbus::drain::local_id`), which is
//! what keeps the hot path allocation-free but loses the path. Focus events, the only ones the
//! connector must call back on, record it here (`Router::focus_seen`) before they are pushed.
//! The table is bounded: past the cap it is cleared, and the next focus event of a node records
//! it again, so a hostile application cannot grow memory by minting object paths.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use gorca_model::{AppId, NodeKey};

/// Entries before the table is cleared.
pub const MAX_PATHS: usize = 16_384;
/// Longest object path kept (D-Bus allows 2^32; an application that uses more is lying).
pub const MAX_PATH_BYTES: usize = 1024;

#[derive(Debug, Default)]
pub struct PathTable {
    map: Mutex<HashMap<NodeKey, Arc<str>>>,
}

impl PathTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record `path` for `node`. Allocates only when the node is new.
    pub fn insert(&self, node: NodeKey, path: &str) {
        if path.len() > MAX_PATH_BYTES {
            return;
        }
        let mut m = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if m.get(&node).is_some_and(|p| &**p == path) {
            return;
        }
        if m.len() >= MAX_PATHS {
            m.clear();
        }
        m.insert(node, Arc::from(path));
    }

    pub fn get(&self, node: NodeKey) -> Option<Arc<str>> {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&node)
            .cloned()
    }

    /// Forget everything of an application that left the bus.
    pub fn forget_app(&self, app: AppId) {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|k, _| k.app != app);
    }

    pub fn len(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{ConnectorId, LocalId};

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
    fn the_table_is_bounded() {
        let t = PathTable::new();
        for n in 0..(MAX_PATHS as u64 * 3) {
            t.insert(key(1, n), "/x");
        }
        assert!(t.len() <= MAX_PATHS);
        assert!(t.get(key(1, MAX_PATHS as u64 * 3 - 1)).is_some());
    }

    #[test]
    fn absurd_paths_are_refused() {
        let t = PathTable::new();
        t.insert(key(1, 1), &"p".repeat(MAX_PATH_BYTES + 1));
        assert!(t.is_empty());
    }
}
