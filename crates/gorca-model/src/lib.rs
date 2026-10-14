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
pub mod geometry;
pub mod ids;
pub mod node;
pub mod redact;
pub mod role;
pub mod speech;
pub mod text;

pub use doc::{
    DocAction, DocDelta, DocHint, DocId, DocNodeId, SrcNode, SrcNodeId, TextAnchor, TextEdit,
};
pub use event::{
    AppInfo, AppLifecycle, CaretOrigin, ConnectorKind, DocKind, LazyText, NodeEvent, Politeness,
    RawEvent, RawMsg, TextDelta, WindowKind,
};
pub use geometry::{Point, Rect};
pub use ids::{AppId, ConnectorId, Epoch, LocalId, NodeKey, ReqId, UttId};
pub use node::{
    AttrKey, AttrMap, ChildList, Freshness, FreshnessEvent, Ifaces, Lookup, NodeSnapshot, NodeView,
    PropMask, RelationKind, Relations, SnapshotBatch, StateSet, Value,
};
pub use redact::RedactKey;
pub use role::Role;
pub use speech::{Prio, Segment, SegmentKind, Utterance};
pub use text::{TextLine, TextLines, TextQuery, TextUnit};
