// SPDX-License-Identifier: GPL-3.0-or-later
//! Normalised events (ADR-0001 section 4.4). Connectors map raw events into this closed set and
//! the coalescer works on it.

use std::sync::Arc;

use crate::ids::NodeKey;

/// Raw message bytes still owned by the drain (F-4, R-6). Opaque to the core.
pub type RawMsg = Arc<[u8]>;

/// Payload that is still inside the raw message bytes and is decoded only if a consumer asks.
#[derive(Clone)]
pub struct LazyText {
    pub msg: RawMsg,
    pub field: u8,
}

impl std::fmt::Debug for LazyText {
    // The bytes may be a typed password: only their number is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyText")
            .field("bytes", &self.msg.len())
            .field("field", &self.field)
            .finish()
    }
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

/// The web engine behind an application's pages (`AppInfo::web_engine`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum WebEngine {
    /// Not a web engine (or one gorca does not know).
    #[default]
    None,
    /// Gecko (Firefox, Thunderbird).
    Gecko,
    /// Blink (Chrome, Chromium, Edge, Brave, Electron).
    Blink,
}

/// The data tables of one source profile (quirk rows, source priority, statically inexpressible
/// groups), as text the core parses with `gorca-fuse` (M22a-C1b). The connector picks the profile
/// from its own facts and hands the tables over inside [`AppInfo`], so the core never names a
/// connector crate or a toolkit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SemTables {
    /// Profile name, for logs and `explain`.
    pub name: &'static str,
    /// `[[rule]]` document (`QuirkSet::parse`).
    pub quirks: &'static str,
    /// `[pi]` document (`parse_pi`).
    pub pi: &'static str,
    /// `[[row]]` document (`parse_inexpressible`).
    pub inexpressible: &'static str,
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
    /// The unit of this application's text offsets (TQ-1). Native text tracking (ADR-0007) is
    /// for applications that count scalars; UTF-16 offsets wait for M3b.
    pub text_unit: crate::TextUnit,
    /// Text-event quirk (0: none): this application may name an insert at another offset than
    /// where the text really went (Gecko). The core then trusts a typed key's own insert for
    /// where the caret goes (it moves by the insert's length whatever offset is named) and
    /// accepts any other insert reported up to this many units after the caret. Data from the
    /// connector's per-engine quirk table (ADR-0008), never derived in the core from a name.
    pub text_insert_slack: u32,
    /// Facts the shared editing presenter needs about this application's toolkit (word landing).
    /// Connector quirk table data (ADR-0008).
    pub edit_quirks: crate::EditQuirks,
    /// Connector quirk (0: none): `document:load-complete` that arrives within this many
    /// milliseconds of the application's appearance announces the creation of the tree (the
    /// application built it for the wake-up call), not a page load that happened while the
    /// reader was running. Chromium's tree appears after the first read; its documents then
    /// report `load-complete` although their pages were loaded long ago.
    pub page_events_settle_ms: u32,
    /// The web engine that renders this application's pages, from the connector's toolkit
    /// classification (`ToolkitName`, then the application name as the fallback): Chrome,
    /// Chromium, Edge, Brave and Electron apps are all `Blink`. Selects the web document source
    /// and its quirks; never derived from a name outside the connector.
    pub web_engine: WebEngine,
    /// The application's root node, when the connector knows it. A fill policy that starts at
    /// the root (an eager dump, a window prefetch) gets it with `FillInput::Appeared`.
    pub root: Option<crate::NodeKey>,
    /// The semantic source profile tables of this application (M22a-C1b), chosen by the
    /// connector from its own facts. `None` for a connector without one: the core then fuses
    /// with the default profile and no quirk rows.
    pub sem_tables: Option<SemTables>,
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
