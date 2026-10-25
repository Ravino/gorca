// SPDX-License-Identifier: GPL-3.0-or-later
//! Connector-neutral object model.
//!
//! Responsibility (ADR-0001 section 4, section 11): `NodeKey`, `AppId`, `Role`, `StateSet`,
//! `NodeSnapshot`, `NodeEvent`, `TextDelta`, `Utterance`, `Segment`, `Epoch`, `TextUnit`.
//!
//! > "no tokio, no I/O."  (section 11)
//! > "The core never names AT-SPI, D-Bus, zbus, AccessKit, CDP or Wayland types."  (section 1)
//!
//! Allowed dependencies: `bitflags`, `smallvec` and the standard library only. This crate must
//! never depend, even transitively, on `tokio`, `atspi*`, `zbus*`, `accesskit*`, `wayland-*`,
//! `wasmtime*` or libei bindings (section 11, rules 1 and 4). `cargo xtask deps` enforces this.

pub mod doc;
pub mod event;
pub mod fact;
pub mod geometry;
pub mod ids;
pub mod node;
pub mod redact;
pub mod role;
pub mod sem;
pub mod speech;
#[cfg(feature = "testing")]
pub mod testing;
pub mod text;

pub use doc::{
    DocAction, DocDelta, DocHint, DocId, DocNodeId, DocRange, SelSpan, SelectionSpec, SrcNode,
    SrcNodeId, TextAnchor, TextEdit,
};
pub use event::{
    AppInfo, AppLifecycle, CaretOrigin, ConnectorKind, DocKind, LazyText, NodeEvent, Politeness,
    RawEvent, RawMsg, SemTables, TextDelta, WebEngine, WindowKind,
};
pub use fact::{Fact, Subject};
pub use geometry::{Point, Rect};
pub use ids::{AppId, ConnectorId, Epoch, LocalId, NodeKey, ReqId, UttId};
pub use node::{
    AttrKey, AttrMap, ChildList, Freshness, FreshnessEvent, Ifaces, Lookup, NodeSem, NodeSnapshot,
    NodeView, PropMask, RelationKind, Relations, SnapshotBatch, StateSet, Value,
};
pub use redact::{RedactKey, is_secret_node};
pub use role::Role;
pub use sem::{SemEntry, SemRole, Semantics};
#[cfg(feature = "seg-origin")]
pub use speech::{MsgId, OriginId};
pub use speech::{Prio, Segment, SegmentKind, Utterance};
pub use text::{
    EditQuirks, MAX_SELECTION_TEXT, ScriptPosition, TextFormat, TextGranularity, TextLine,
    TextLines, TextQuery, TextSelections, TextUnit, TextUnitAt, TextWindowFill, WordLanding,
    WordLevel,
};
