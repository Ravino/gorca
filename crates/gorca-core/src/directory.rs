//! Lock-free directory of per-app mirrors for the Reader (PC-1).

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use gorca_mirror::{MirrorHandle, MirrorRead};
use gorca_model::{AppId, Lookup, NodeKey, NodeSnapshot};

/// Maps an `AppId` to its mirror read handle. Readers load the current map without a lock;
/// the supervisor replaces it (copy-on-write) when an app appears or goes away, which is rare.
#[derive(Debug, Default)]
pub struct MirrorDirectory {
    apps: ArcSwap<HashMap<AppId, MirrorHandle>>,
}

impl MirrorDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, app: AppId, handle: &MirrorHandle) {
        self.apps.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.insert(app, handle.clone());
            next
        });
    }

    pub fn remove(&self, app: AppId) {
        self.apps.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.remove(&app);
            next
        });
    }

    /// Wait-free. An unknown app is `Gone`: its keys must never resolve to something else (I-3).
    pub fn lookup(&self, k: NodeKey) -> Lookup<Arc<NodeSnapshot>> {
        match self.apps.load().get(&k.app) {
            Some(m) => m.get(k),
            None => Lookup::Gone,
        }
    }

    pub fn len(&self) -> usize {
        self.apps.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.apps.load().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_mirror::mirror;
    use gorca_model::{
        ChildList, ConnectorId, Freshness, Ifaces, LocalId, Role, SnapshotBatch, StateSet,
    };
    use std::time::Instant;

    fn app(n: u32) -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: n,
        }
    }

    fn snap(a: u32, n: u64) -> Arc<NodeSnapshot> {
        Arc::new(NodeSnapshot {
            key: NodeKey {
                app: app(a),
                local: LocalId(n),
            },
            role: Role::Button,
            states: StateSet::empty(),
            name: Some(Arc::from("ok")),
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    #[test]
    fn lookup_routes_by_app_and_unknown_apps_are_gone() {
        let dir = MirrorDirectory::new();
        let (mut w, h) = mirror();
        w.apply(SnapshotBatch {
            nodes: vec![snap(1, 5)],
        });
        dir.insert(app(1), &h);
        let k = NodeKey {
            app: app(1),
            local: LocalId(5),
        };
        assert!(dir.lookup(k).hit().is_some());
        assert_eq!(
            dir.lookup(NodeKey {
                app: app(1),
                local: LocalId(6)
            }),
            Lookup::Miss
        );
        assert_eq!(
            dir.lookup(NodeKey {
                app: app(2),
                local: LocalId(5)
            }),
            Lookup::Gone
        );
        dir.remove(app(1));
        assert_eq!(dir.lookup(k), Lookup::Gone);
        assert!(dir.is_empty());
    }
}
