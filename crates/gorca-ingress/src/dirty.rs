//! Per-node dirty bits used when an app is `Flooded` (R-3).

use std::collections::HashSet;

use gorca_model::NodeKey;

/// Overflow bound (PC-7): beyond it the set degrades to "everything is dirty".
pub const MAX_DIRTY_NODES: usize = 65_536;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DirtySet {
    all: bool,
    nodes: HashSet<NodeKey>,
}

impl DirtySet {
    pub fn mark(&mut self, node: NodeKey) {
        if self.all {
            return;
        }
        if self.nodes.len() >= MAX_DIRTY_NODES && !self.nodes.contains(&node) {
            self.mark_all();
        } else {
            self.nodes.insert(node);
        }
    }

    /// Everything must be resynced (set overflow or drain restart).
    pub fn mark_all(&mut self) {
        self.all = true;
        self.nodes.clear();
    }

    pub fn is_all(&self) -> bool {
        self.all
    }

    pub fn is_empty(&self) -> bool {
        !self.all && self.nodes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn contains(&self, node: NodeKey) -> bool {
        self.all || self.nodes.contains(&node)
    }

    pub fn iter(&self) -> impl Iterator<Item = NodeKey> + '_ {
        self.nodes.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId};

    fn key(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 0,
            },
            local: LocalId(n),
        }
    }

    #[test]
    fn marks_dedupe() {
        let mut d = DirtySet::default();
        d.mark(key(1));
        d.mark(key(1));
        assert_eq!(d.len(), 1);
        assert!(d.contains(key(1)) && !d.contains(key(2)));
    }

    #[test]
    fn overflow_degrades_to_all_dirty() {
        let mut d = DirtySet::default();
        for n in 0..=MAX_DIRTY_NODES as u64 {
            d.mark(key(n));
        }
        assert!(d.is_all());
        assert!(d.contains(key(u64::MAX)));
        assert_eq!(d.len(), 0);
    }
}
