// SPDX-License-Identifier: GPL-3.0-or-later
//! Document mirror: keyed forest, flat text, bookmarks (ADR-0001 section 9.4).
//!
//! Responsibility (section 11): "document mirror: keyed forest, flat text, indexes, diff,
//! live-region policy, bookmarks. Pure data + rayon; no tokio, no I/O."
//!
//! > "The document mirror owns the model. A source only produces deltas and executes requests."
//!
//! The model lives in [`web`]. The scaffold `DocumentMirror` is gone (ADR-0003 AM-2); the
//! delta translator that feeds [`web::DocWriter`] is M7a work.
//!
//! Allowed dependencies: `gorca-model` only for now; `rayon` and `arc-swap` may be added. It must
//! never depend, even transitively, on `tokio` (section 11, rule 4) or any transport crate.

pub mod web;
