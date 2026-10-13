//! Normalised events (ADR-0001 section 4.4). Connectors map raw events into this closed set and
//! the coalescer works on it.

use std::sync::Arc;

use crate::ids::NodeKey;

/// Raw message bytes still owned by the drain (F-4, R-6). Opaque to the core.
pub type RawMsg = Arc<[u8]>;

/// Payload that is still inside the raw message bytes and is decoded only if a consumer asks.
#[derive(Clone, Debug)]
pub struct LazyText {
    pub msg: RawMsg,
    pub field: u8,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CaretOrigin {
    Unknown,
    /// Chromium `text-caret-moved(0)` on a document (WEB 2.4).
    ScrollToAnchor,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Politeness {
    Polite,
    Assertive,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WindowKind {
    Activate,
    Deactivate,
    Create,
    Destroy,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DocKind {
    LoadStart,
    LoadComplete,
    Reload,
    LoadStopped,
}

/// Text offsets are Unicode scalar value offsets (TU-1).
#[derive(Clone, Debug)]
pub enum TextDelta {
    Insert {
        offset: u32,
        len: u32,
        text: Option<LazyText>,
    },
    Delete {
        offset: u32,
        len: u32,
        text: Option<LazyText>,
    },
    /// Coalesced: a delete+insert pair or more than 64 deltas within one drain batch.
    /// Consumers re-read the text.
    Replaced { text: Option<LazyText> },
}

#[derive(Clone, Debug)]
pub enum NodeEvent {
    FocusChanged {
        node: NodeKey,
    },
    ActiveDescendantChanged {
        container: NodeKey,
        child: Option<NodeKey>,
    },
    PropertiesChanged {
        node: NodeKey,
        mask: crate::node::PropMask,
    },
    /// Coarse: subtree structure is dirty.
    ChildrenChanged {
        node: NodeKey,
    },
    TextChanged {
        node: NodeKey,
        delta: TextDelta,
    },
    CaretMoved {
        node: NodeKey,
        offset: u32,
        origin: CaretOrigin,
    },
    SelectionChanged {
        node: NodeKey,
    },
    Announcement {
        node: Option<NodeKey>,
        text: Arc<str>,
        politeness: Politeness,
    },
    WindowEvent {
        node: NodeKey,
        kind: WindowKind,
    },
    DocumentEvent {
        node: NodeKey,
        kind: DocKind,
    },
    NodeRemoved {
        node: NodeKey,
    },
}

/// A `NodeEvent` whose payload may still be lazy (R-6).
pub type RawEvent = NodeEvent;

impl NodeEvent {
    /// The node an event is about, if any. For `ActiveDescendantChanged` this is the container.
    pub fn node(&self) -> Option<NodeKey> {
        match self {
            NodeEvent::FocusChanged { node }
            | NodeEvent::PropertiesChanged { node, .. }
            | NodeEvent::ChildrenChanged { node }
            | NodeEvent::TextChanged { node, .. }
            | NodeEvent::CaretMoved { node, .. }
            | NodeEvent::SelectionChanged { node }
            | NodeEvent::WindowEvent { node, .. }
            | NodeEvent::DocumentEvent { node, .. }
            | NodeEvent::NodeRemoved { node } => Some(*node),
            NodeEvent::ActiveDescendantChanged { container, .. } => Some(*container),
            NodeEvent::Announcement { node, .. } => *node,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ConnectorKind {
    Atspi,
    /// Reserved for the deferred Newton connector (R5).
    Newton,
    Fake,
}

/// Extensions match on these; the core never branches on toolkit or app name.
#[derive(Clone, Debug)]
pub struct AppInfo {
    pub toolkit: Arc<str>,
    pub app_name: Arc<str>,
    pub pid: Option<u32>,
    pub connector_kind: ConnectorKind,
    /// Set by the connector only when the registration matched the protected Shell identity
    /// (same process group and start time as the verified compositor shell process). Never
    /// derived from `app_name` or `toolkit`, which the application reports about itself.
    pub verified_shell: bool,
    /// The connector verified that a bulk `CacheDump` returns the whole tree of this app (B-1
    /// 2.2). True only for a verified gnome-shell today; computed from verified identity, never
    /// from self-reported names. A policy may use `CacheDump` only when this holds.
    pub dump_complete: bool,
}

#[derive(Clone, Debug)]
pub enum AppLifecycle {
    Appeared(AppInfo),
    Ready,
    Gone,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{AppId, ConnectorId, LocalId};

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
    fn node_accessor_covers_container_and_optional_nodes() {
        let ev = NodeEvent::ActiveDescendantChanged {
            container: key(1),
            child: Some(key(2)),
        };
        assert_eq!(ev.node(), Some(key(1)));
        let ev = NodeEvent::Announcement {
            node: None,
            text: Arc::from("hi"),
            politeness: Politeness::Polite,
        };
        assert_eq!(ev.node(), None);
        assert_eq!(
            NodeEvent::FocusChanged { node: key(3) }.node(),
            Some(key(3))
        );
    }
}
