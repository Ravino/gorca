// SPDX-License-Identifier: GPL-3.0-or-later
//! The SourceLayer boundary under the web document mirror (ADR-0001 section 9.2).
//!
//! Responsibility (section 11): `DocSource`, `DocSourceCtx`, `SourceCaps`, `DocDelta`,
//! `DocRequest`, `JoinPort`, `DocNodeId`.
//!
//! > "The document mirror owns the model. A source only produces deltas and executes requests."
//!
//! Source obligations (conformance suite in `gorca-testkit`):
//! - **SL-1** `sink.push` never blocks. Overflow marks the document dirty; the doc actor runs a
//!   `Subtree` resync.
//! - **SL-2** Each request is answered or dropped by its deadline plus 5 ms.
//! - **SL-3** All source data is untrusted. A crash loop detaches the source (IR-7).
//! - **SL-4** `Children` deltas are complete ordered lists.
//! - **SL-5** A source never emits focus or caret; those come from the app's connector.
//!
//! Which source feeds which browser is decided by bake-off B-6. TBD-S1 (CDP), TBD-S2 (Firefox
//! WebExtension), TBD-S3 (join keys) and TBD-S4 (hybrid split) are not scaffolded here; the
//! `SourceKind` variants and the `JoinPort` shape are the hooks they plug into.
//!
//! Allowed dependencies (section 11): `gorca-model` and the channel types re-exported from
//! `gorca-rt`. The pure-data delta types are defined in `gorca-model` (see
//! `gorca_model::doc` for why) and re-exported here.

mod caps;
mod ctx;
mod factory;
mod join;
pub mod limits;
mod request;
mod web;

pub use caps::{ActionCaps, JoinKeyCaps, SourceCaps, SourceKind};
pub use ctx::{DeltaReceiver, DeltaSink, DocEvent, DocSource, DocSourceCtx, delta_channel};
pub use factory::{DocSourceFactory, WarmOutcome, WarmReport};
pub use gorca_model::{
    DocAction, DocDelta, DocHint, DocId, DocNodeId, SrcNode, SrcNodeId, TextEdit, TextUnit,
};
pub use join::{JoinEntry, JoinPort, JoinQuality, JoinReceiver, join_channel};
pub use request::{Activity, DocRequest, RepairReport, Reply, SourceError};
pub use web::{Completeness, TreeBlock, is_dynamic, takes_error_text, web_attrs};
