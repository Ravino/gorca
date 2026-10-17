// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-app lock-free snapshot store (ADR-0001 section 5.2, PC-1, decision MS-3), the fill policy
//! trait and the shared fill driver (B-1).
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `gorca-ingress`. Never a transport crate.

pub mod driver;
pub mod fill;
pub mod limits;
mod store;

pub use limits::FillLimits;
pub use store::{
    AppUsage, ApplyOrigin, ApplyOutcome, DEFAULT_TTL, KeyBuild, MirrorBudget, MirrorHandle,
    MirrorInvariantError, MirrorRead, MirrorStats, MirrorWriter, RemovalCause, SweepStats, mirror,
    mirror_for, node_bytes,
};
