// SPDX-License-Identifier: GPL-3.0-or-later
//! Routing of a browser application's events to the documents that own the nodes (browse-mode
//! design JN-3, EF-1). One call from the app actor's pump; no shared lines with the Reader
//! routing.
//!
//! The router is the only place the app actor knows about documents. It is O(1) per event: one
//! wait-free lookup in the join, one `try_send`. A full queue drops the event (EF-2); the
//! source notices the gap through its own settle and the document rebuilds on the next
//! overflow, so a flooding page degrades only its own document.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc_source_api::DocEvent;
use gorca_model::{AppId, AppInfo, AppLifecycle, DocKind, NodeEvent};

use crate::doc_actor::repair::RepairReason;
use crate::doc_actor::{DocDirectory, WebJoin};
use crate::doc_registry::{HubMsg, HubTx};
use gorca_model::PropMask;

/// What an app actor needs to route web events.
#[derive(Clone, Debug)]
pub struct WebLink {
    pub hub: HubTx,
    pub join: Arc<WebJoin>,
    pub docs: Arc<DocDirectory>,
}

#[derive(Debug)]
pub(super) struct DocRouter {
    app: AppId,
    link: WebLink,
    seq: u64,
    /// Events dropped because a document's queue was full (EF-2).
    pub dropped: u64,
    pub routed: u64,
    /// `load-complete` events before this are the tree's creation (see `lifecycle`).
    settle_until: std::sync::OnceLock<Instant>,
}

impl DocRouter {
    pub fn new(app: AppId, link: WebLink) -> Self {
        Self {
            app,
            link,
            seq: 0,
            dropped: 0,
            routed: 0,
            settle_until: std::sync::OnceLock::new(),
        }
    }

    /// Lifecycle messages go to the registry (warm-up and document creation).
    pub fn lifecycle(&self, l: &AppLifecycle) {
        let msg = match l {
            AppLifecycle::Appeared(info) => {
                // Connector quirk: `load-complete` right after the application appeared is the
                // creation of the tree, not a page load (`AppInfo::page_events_settle_ms`).
                if info.page_events_settle_ms > 0 {
                    let _ = self.settle_until.set(
                        Instant::now()
                            + Duration::from_millis(u64::from(info.page_events_settle_ms)),
                    );
                }
                HubMsg::Appeared {
                    app: self.app,
                    info: Box::new(AppInfo::clone(info)),
                }
            }
            AppLifecycle::Ready => HubMsg::Ready { app: self.app },
            AppLifecycle::Gone => HubMsg::Gone { app: self.app },
        };
        let _ = self.link.hub.try_send(msg);
    }

    /// Route one drained batch.
    pub fn route(&mut self, events: &[NodeEvent], now: Instant) {
        for ev in events {
            match ev {
                NodeEvent::DocumentEvent {
                    node,
                    kind: DocKind::LoadComplete | DocKind::Reload,
                } => {
                    // A document that already has a mirror is stale after its load ended
                    // (Chromium sends no structural event for it): repair it whatever the
                    // settle window says. The window only keeps a fresh tree's creation from
                    // being taken for a page load.
                    self.repair_doc_of(*node, RepairReason::LoadComplete);
                    if self.settle_until.get().is_some_and(|t| now < *t) {
                        continue;
                    }
                    let _ = self.link.hub.try_send(HubMsg::Loaded { node: *node });
                }
                NodeEvent::PropertiesChanged { node, mask } => {
                    if mask.contains(PropMask::STATES) {
                        self.root_state(*node);
                    }
                    self.send(ev, *node, now);
                }
                NodeEvent::ChildrenChanged { node }
                | NodeEvent::NodeRemoved { node }
                | NodeEvent::TextChanged { node, .. } => {
                    self.send(ev, *node, now);
                }
                NodeEvent::FocusChanged { .. }
                | NodeEvent::ActiveDescendantChanged { .. }
                | NodeEvent::CaretMoved { .. }
                | NodeEvent::SelectionChanged { .. }
                | NodeEvent::Announcement { .. }
                | NodeEvent::WindowEvent { .. }
                | NodeEvent::DocumentEvent { .. } => {}
            }
        }
    }

    /// Ask the document that owns `node` (a node of its mirror) for a repair.
    fn repair_doc_of(&self, node: gorca_model::NodeKey, why: RepairReason) {
        if let Some((doc, _)) = self.link.join.get(node)
            && let Some(h) = self.link.docs.get(doc)
        {
            h.request_repair(why);
        }
    }

    /// The states of a document root changed (busy to idle is one of them): the document may
    /// have finished loading. The document decides whether it is young enough to care.
    fn root_state(&self, node: gorca_model::NodeKey) {
        if let Some((doc, _)) = self.link.join.get(node)
            && let Some(h) = self.link.docs.get(doc)
            && h.root == node
        {
            h.request_repair(RepairReason::RootState);
        }
    }

    fn send(&mut self, ev: &NodeEvent, node: gorca_model::NodeKey, now: Instant) {
        // The node's document; an unknown node is most often a new child of a known parent, so
        // it goes to the application's document (JN-3).
        let (handle, known) = match self.link.join.get(node) {
            Some((doc, _)) => (self.link.docs.get(doc), true),
            None => (self.link.docs.of_app(self.app), false),
        };
        let Some(h) = handle else { return };
        if !known && !matches!(ev, NodeEvent::NodeRemoved { .. }) {
            // Counted, never acted on here: the document looks again after a settle period.
            h.orphans.note(node);
        }
        self.seq += 1;
        let de = DocEvent {
            ev: ev.clone(),
            seq: self.seq,
            at: now,
        };
        if h.events.try_send(de).is_ok() {
            self.routed += 1;
        } else {
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::doc_actor::{DocCtl, DocHandle};
    use gorca_doc::web::channel;
    use gorca_model::{ConnectorId, DocId, DocNodeId, LocalId, NodeKey};
    use gorca_rt::mpsc;

    fn app() -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: 1,
        }
    }

    fn nk(n: u64) -> NodeKey {
        NodeKey {
            app: app(),
            local: LocalId(n),
        }
    }

    struct Rig {
        router: DocRouter,
        join: Arc<WebJoin>,
        ctl: mpsc::Receiver<DocCtl>,
        handle: Arc<DocHandle>,
        _hub: mpsc::Receiver<HubMsg>,
    }

    /// One document (id 1, root key 11) with the nodes 11 and 12 joined.
    fn rig() -> Rig {
        let (hub, hub_rx) = crate::doc_registry::hub_channel();
        let join = Arc::new(WebJoin::new());
        let docs = Arc::new(DocDirectory::new());
        let (_writer, read) = channel(1);
        let (ctl_tx, ctl) = mpsc::channel(16);
        let (ev_tx, _ev) = mpsc::channel(16);
        docs.insert(DocHandle {
            doc: DocId(1),
            app: app(),
            root: nk(11),
            read,
            ctl: ctl_tx,
            events: ev_tx,
            orphans: Arc::default(),
            repair_flag: Arc::default(),
        });
        join.set(nk(11), (DocId(1), DocNodeId::from_parts(0, 0)));
        join.set(nk(12), (DocId(1), DocNodeId::from_parts(1, 0)));
        let handle = docs.get(DocId(1)).unwrap();
        let router = DocRouter::new(
            app(),
            WebLink {
                hub,
                join: Arc::clone(&join),
                docs,
            },
        );
        Rig {
            router,
            join,
            ctl,
            handle,
            _hub: hub_rx,
        }
    }

    #[test]
    fn a_load_complete_in_the_settle_window_still_repairs_a_mirrored_document() {
        let mut r = rig();
        let now = Instant::now();
        // The window that keeps a fresh tree's creation from being taken for a page load.
        r.router
            .settle_until
            .set(now + Duration::from_secs(3))
            .unwrap();
        r.router.route(
            &[NodeEvent::DocumentEvent {
                node: nk(11),
                kind: DocKind::LoadComplete,
            }],
            now,
        );
        assert!(matches!(r.ctl.try_recv(), Ok(DocCtl::RepairPending)));
        assert_eq!(
            r.handle
                .repair_flag
                .load(std::sync::atomic::Ordering::Relaxed),
            RepairReason::LoadComplete.code()
        );
    }

    #[test]
    fn a_state_change_of_the_root_asks_for_a_young_document_check_but_not_a_child() {
        let mut r = rig();
        let now = Instant::now();
        let states = |n| NodeEvent::PropertiesChanged {
            node: nk(n),
            mask: PropMask::STATES,
        };
        r.router.route(&[states(12)], now);
        assert!(
            r.ctl.try_recv().is_err(),
            "a child's state is no load signal"
        );
        r.router.route(&[states(11)], now);
        assert!(matches!(r.ctl.try_recv(), Ok(DocCtl::RepairPending)));
        assert_eq!(
            r.handle
                .repair_flag
                .load(std::sync::atomic::Ordering::Relaxed),
            RepairReason::RootState.code()
        );
    }

    #[test]
    fn a_storm_of_root_state_events_takes_one_control_slot_and_cannot_drop_a_click() {
        let mut r = rig();
        let now = Instant::now();
        let st = NodeEvent::PropertiesChanged {
            node: nk(11),
            mask: PropMask::STATES,
        };
        for _ in 0..64 {
            r.router.route(std::slice::from_ref(&st), now);
        }
        r.router.route(
            &[NodeEvent::DocumentEvent {
                node: nk(11),
                kind: DocKind::LoadComplete,
            }],
            now,
        );
        let click = DocCtl::Act {
            node: DocNodeId::from_parts(1, 0),
            act: gorca_model::DocAction::Click,
        };
        assert!(r.handle.ctl.try_send(click).is_ok(), "the click fits");
        let mut pending = 0;
        while let Ok(m) = r.ctl.try_recv() {
            if matches!(m, DocCtl::RepairPending) {
                pending += 1;
            }
        }
        assert_eq!(pending, 1, "one control message for the whole storm");
        // The strongest reason won.
        assert_eq!(
            r.handle
                .repair_flag
                .load(std::sync::atomic::Ordering::Relaxed),
            RepairReason::LoadComplete.code()
        );
    }

    #[test]
    fn events_on_unknown_nodes_are_counted_as_orphans_but_removals_are_not() {
        let mut r = rig();
        let now = Instant::now();
        r.router
            .route(&[NodeEvent::NodeRemoved { node: nk(99) }], now);
        assert!(!r.handle.orphans.pending());
        r.router.route(
            &[
                NodeEvent::ChildrenChanged { node: nk(12) },
                NodeEvent::TextChanged {
                    node: nk(98),
                    delta: gorca_model::TextDelta::Replaced { text: None },
                },
            ],
            now,
        );
        assert!(r.handle.orphans.pending());
        assert_eq!(r.handle.orphans.take(), vec![nk(98)]);
        assert!(r.join.get(nk(12)).is_some());
    }
}
