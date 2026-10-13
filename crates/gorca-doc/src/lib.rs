//! Document mirror: keyed forest, flat text, bookmarks (ADR-0001 section 9.4).
//!
//! Responsibility (section 11): "document mirror: keyed forest, flat text, indexes, diff,
//! live-region policy, bookmarks. Pure data + rayon; no tokio, no I/O."
//!
//! > "The document mirror owns the model. A source only produces deltas and executes requests."
//!
//! This is the Phase 0 core: identity mapping (`SrcNodeId` to `DocNodeId`, IR-1), applying
//! `DocDelta`s, the ordered child diff (SL-4) and snapshot publication. TBD (not scaffolded):
//! the structural-navigation indexes (data structure goes to math-researcher, WEB 4.9), the
//! live-region classifier (W-3), the parallel build on rayon, bookmark relocation (IR-7).
//!
//! Allowed dependencies: `gorca-model` only for now; `rayon` and `arc-swap` may be added. It must
//! never depend, even transitively, on `tokio` (section 11, rule 4) or any transport crate.

mod mirror;
pub mod web;

pub use mirror::{
    ApplyOutcome, Bookmark, DocCursor, DocNode, DocSnapshot, DocumentMirror, SNAPSHOT_EVERY_MS,
    SNAPSHOT_EVERY_NODES,
};
