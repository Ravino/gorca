//! `AtspiDocSource`: the mandatory AT-SPI document source under the web mirror (ADR-0003 6.2).
//!
//! * [`p2p`]: per-app pool of peer-to-peer connections (fallback: the bus connection).
//! * [`fetch`]: block fetch of a document with `Collection.GetTree` plus pipelined role, state
//!   and text passes; output is the neutral `gorca_doc_source_api::TreeBlock`.
//! * [`identity`]: SPA identity variant C (ADR-0003 section 5), as a pure re-keying layer.
//! * [`events`]: AT-SPI events in, dirty parents and refetch plans out; applying a refetch.
//! * [`source`]: the `DocSource` implementation tying the above to the doc actor.
//!
//! This crate talks raw D-Bus (zbus) and never links the `atspi` crate or libatspi: a libatspi
//! client makes the browser return an empty P2P address (web source-layer report 2.1).

pub mod bus;
pub mod events;
pub mod factory;
pub mod fetch;
pub mod identity;
pub mod p2p;
pub mod roles;
pub mod runs;
pub mod source;
pub mod warm;
mod wake;
pub mod wire;

pub use events::{AtspiEvent, EventKind, SettlePolicy};
pub use factory::{AtspiDocSourceFactory, PathResolver};
pub use fetch::{
    FetchError, FetchOpts, FetchTimings, FetchedDoc, PathTable, Split, StatePolicy, TextPolicy,
    fetch_document, fetch_forest,
};
pub use identity::{Burst, IdentityMap, MatchStats};
pub use p2p::{AppConn, ConnectOpts};
pub use source::{AtspiDocSource, SourceConfig, SourceMetrics, UpdateRecord};
pub use wire::{AttrInterner, PathKey};
