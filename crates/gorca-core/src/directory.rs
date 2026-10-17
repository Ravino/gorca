// SPDX-License-Identifier: GPL-3.0-or-later
//! Lock-free directory of per-app mirrors for the Reader (PC-1).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arc_swap::ArcSwap;
use gorca_mirror::{MirrorHandle, MirrorRead};
use gorca_model::{AppId, EditQuirks, Lookup, NodeKey, NodeSnapshot, NodeView, TextUnit};

/// Who is behind an application connection, for budgets that a misbehaving local app must not
/// escape by reconnecting, taking a new bus name or opening more connections: the process id the
/// connector read from the bus credentials. A pid-less connection (credentials unavailable)
/// shares one owner with every other. Budgets are token buckets that refill within a second and
/// stay in the table until idle, so a reconnect carries the spent budget over, and a reused pid
/// at worst meets a bucket that has refilled.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Owner {
    Pid(u32),
    Unknown,
}

/// Maps an `AppId` to its mirror read handle. Readers load the current map without a lock;
/// the supervisor replaces it (copy-on-write) when an app appears or goes away, which is rare.
#[derive(Debug, Default)]
pub struct MirrorDirectory {
    apps: ArcSwap<HashMap<AppId, MirrorHandle>>,
    /// Applications whose text offsets the native text window cannot follow (UTF-8 byte
    /// offsets, as the connector reported): the native text layer leaves them alone.
    /// Gecko's UTF-16 offsets are followed as they are (`TextUnit::window_follows`).
    unfollowed_text: ArcSwap<HashSet<AppId>>,
    /// Per-app insert-offset slack (connector quirk data); apps without the quirk are absent.
    insert_slack: ArcSwap<HashMap<AppId, u32>>,
    /// Per-app editing-presenter facts (connector quirk data); apps without a row are absent.
    edit_quirks: ArcSwap<HashMap<AppId, EditQuirks>>,
    /// Per-app end of the "tree creation" window (connector quirk `page_events_settle_ms`):
    /// a document that appears before it was built for the reader's wake-up call, not loaded by
    /// the user's navigation.
    settle_until: ArcSwap<HashMap<AppId, std::time::Instant>>,
    /// Process id of each application connection, as the connector reported it (M4b budgets).
    pids: ArcSwap<HashMap<AppId, u32>>,
    /// Applications whose presentation goes through the installed `AppPolicy`.
    policy: ArcSwap<HashSet<AppId>>,
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

    /// Record the unit the app counts text in. Unknown apps count as scalars.
    pub fn set_text_unit(&self, app: AppId, unit: TextUnit) {
        let follows = unit.window_follows();
        self.unfollowed_text.rcu(|cur| {
            let mut next = HashSet::clone(cur);
            if follows {
                next.remove(&app);
            } else {
                next.insert(app);
            }
            next
        });
    }

    /// Record the app's editing-presenter facts (connector quirk table, ADR-0008).
    pub fn set_edit_quirks(&self, app: AppId, q: EditQuirks) {
        self.edit_quirks.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            if q == EditQuirks::default() {
                next.remove(&app);
            } else {
                next.insert(app, q);
            }
            next
        });
    }

    /// The app's editing-presenter facts: the defaults unless its connector declared a row.
    pub fn edit_quirks(&self, app: AppId) -> EditQuirks {
        self.edit_quirks
            .load()
            .get(&app)
            .copied()
            .unwrap_or_default()
    }

    /// Record the app's insert-offset slack (connector quirk `gecko-insert-offset`).
    pub fn set_text_insert_slack(&self, app: AppId, slack: u32) {
        self.insert_slack.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            if slack == 0 {
                next.remove(&app);
            } else {
                next.insert(app, slack);
            }
            next
        });
    }

    /// Open the app's tree-creation window for `ms` milliseconds from now (0: no window).
    pub fn set_page_events_settle(&self, app: AppId, ms: u32) {
        if ms == 0 {
            return;
        }
        let until = std::time::Instant::now() + std::time::Duration::from_millis(u64::from(ms));
        self.settle_until.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.entry(app).or_insert(until);
            next
        });
    }

    /// Is `app` still inside its tree-creation window at `now`?
    pub fn is_settling(&self, app: AppId, now: std::time::Instant) -> bool {
        self.settle_until.load().get(&app).is_some_and(|t| now < *t)
    }

    /// The app's insert-offset slack: 0 unless its connector declared the quirk.
    pub fn text_insert_slack(&self, app: AppId) -> u32 {
        self.insert_slack.load().get(&app).copied().unwrap_or(0)
    }

    /// May the native text layer track this app's text fields? True for scalars (GTK, St) and
    /// for Gecko's UTF-16 offsets; false for byte offsets.
    pub fn text_window_follows(&self, app: AppId) -> bool {
        !self.unfollowed_text.load().contains(&app)
    }

    /// Record the process behind `app` (`None` when the connector could not tell).
    pub fn set_pid(&self, app: AppId, pid: Option<u32>) {
        self.pids.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            match pid {
                Some(p) => next.insert(app, p),
                None => next.remove(&app),
            };
            next
        });
    }

    /// Mark whether `app` is presented through the installed policy.
    pub fn set_policy(&self, app: AppId, on: bool) {
        self.policy.rcu(|cur| {
            let mut next = HashSet::clone(cur);
            if on {
                next.insert(app);
            } else {
                next.remove(&app);
            }
            next
        });
    }

    pub fn has_policy(&self, app: AppId) -> bool {
        self.policy.load().contains(&app)
    }

    /// The owner whose budgets `app` spends.
    pub fn owner(&self, app: AppId) -> Owner {
        self.pids
            .load()
            .get(&app)
            .map_or(Owner::Unknown, |p| Owner::Pid(*p))
    }

    pub fn remove(&self, app: AppId) {
        self.set_pid(app, None);
        self.set_policy(app, false);
        if !self.text_window_follows(app) {
            self.set_text_unit(app, TextUnit::Scalar);
        }
        self.set_text_insert_slack(app, 0);
        self.set_edit_quirks(app, EditQuirks::default());
        self.settle_until.rcu(|cur| {
            let mut next = HashMap::clone(cur);
            next.remove(&app);
            next
        });
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

    /// Like [`MirrorDirectory::lookup`] with the stale mask and aging applied (B-1 5.2).
    pub fn view(&self, k: NodeKey) -> Lookup<NodeView> {
        match self.apps.load().get(&k.app) {
            Some(m) => m.view(k),
            None => Lookup::Gone,
        }
    }

    /// The app's mirror read handle (one directory lookup, then lock-free reads).
    pub fn handle(&self, app: AppId) -> Option<MirrorHandle> {
        self.apps.load().get(&app).cloned()
    }

    /// The app's mirror write sequence (what a request issued now records as `issued_at`); 0 for
    /// an unknown app.
    pub fn seq(&self, app: AppId) -> u64 {
        self.apps.load().get(&app).map_or(0, MirrorRead::seq)
    }

    /// A fresh sequence point of the app's mirror: for a trigger (a focus change) or a request
    /// about to be issued. 0 for an unknown app.
    pub fn bump_seq(&self, app: AppId) -> u64 {
        self.apps.load().get(&app).map_or(0, MirrorRead::bump_seq)
    }

    /// Counters of an app's mirror; `None` once the app is gone and its mirror released.
    pub fn stats(&self, app: AppId) -> Option<gorca_mirror::MirrorStats> {
        self.apps.load().get(&app).map(MirrorRead::stats)
    }

    pub fn len(&self) -> usize {
        self.apps.load().len()
    }

    pub fn is_empty(&self) -> bool {
        self.apps.load().is_empty()
    }
}

impl crate::review::NavSource for MirrorDirectory {
    fn view(&self, k: NodeKey) -> Lookup<NodeView> {
        MirrorDirectory::view(self, k)
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
            have: gorca_model::PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    #[test]
    fn lookup_routes_by_app_and_unknown_apps_are_gone() {
        let dir = MirrorDirectory::new();
        let (mut w, h) = mirror();
        w.apply(
            SnapshotBatch {
                nodes: vec![snap(1, 5)],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
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

    #[test]
    fn owners_are_processes_stable_across_reconnects_and_never_per_connection() {
        let dir = MirrorDirectory::new();
        dir.set_pid(app(1), Some(100));
        dir.set_pid(app(2), Some(100));
        dir.set_pid(app(3), Some(200));
        assert_eq!(
            dir.owner(app(1)),
            dir.owner(app(2)),
            "two names, one process"
        );
        assert_ne!(dir.owner(app(1)), dir.owner(app(3)));
        // The process disconnects and comes back under a new connection: the same owner, so the
        // budget it had spent is still its own.
        let before = dir.owner(app(1));
        dir.remove(app(1));
        dir.remove(app(2));
        dir.set_pid(app(4), Some(100));
        assert_eq!(dir.owner(app(4)), before);
        // No credentials: one shared owner, so a new connection is no fresh budget.
        dir.set_pid(app(5), None);
        dir.set_pid(app(6), None);
        assert_eq!(dir.owner(app(5)), Owner::Unknown);
        assert_eq!(dir.owner(app(5)), dir.owner(app(6)));
    }
}
