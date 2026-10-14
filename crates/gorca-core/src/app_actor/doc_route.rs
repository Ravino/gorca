//! Routing of a browser application's events to the documents that own the nodes (browse-mode
//! design JN-3, EF-1). One call from the app actor's pump; no shared lines with the Reader
//! routing.
//!
//! The router is the only place the app actor knows about documents. It is O(1) per event: one
//! wait-free lookup in the join, one `try_send`. A full queue drops the event (EF-2); the
//! source notices the gap through its own settle and the document rebuilds on the next
//! overflow, so a flooding page degrades only its own document.

use std::sync::Arc;
use std::time::Instant;

use gorca_doc_source_api::DocEvent;
use gorca_model::{AppId, AppInfo, AppLifecycle, DocKind, NodeEvent};

use crate::doc_actor::{DocDirectory, WebJoin};
use crate::doc_registry::{HubMsg, HubTx};

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
}

impl DocRouter {
    pub fn new(app: AppId, link: WebLink) -> Self {
        Self {
            app,
            link,
            seq: 0,
            dropped: 0,
            routed: 0,
        }
    }

    /// Lifecycle messages go to the registry (warm-up and document creation).
    pub fn lifecycle(&self, l: &AppLifecycle) {
        let msg = match l {
            AppLifecycle::Appeared(info) => HubMsg::Appeared {
                app: self.app,
                info: Box::new(AppInfo::clone(info)),
            },
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
                    let _ = self.link.hub.try_send(HubMsg::Loaded { node: *node });
                }
                NodeEvent::ChildrenChanged { node }
                | NodeEvent::NodeRemoved { node }
                | NodeEvent::PropertiesChanged { node, .. }
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

    fn send(&mut self, ev: &NodeEvent, node: gorca_model::NodeKey, now: Instant) {
        // The node's document; an unknown node is most often a new child of a known parent, so
        // it goes to the application's document (JN-3).
        let handle = match self.link.join.get(node) {
            Some((doc, _)) => self.link.docs.get(doc),
            None => self.link.docs.of_app(self.app),
        };
        let Some(h) = handle else { return };
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
