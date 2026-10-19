// SPDX-License-Identifier: GPL-3.0-or-later
//! The per-source data the document actor needs to fuse a source's records (M22a-C2).
//!
//! The normaliser and its tables live in the source crate (H-7, `xtask deps` rule 10); the core
//! never reads them. The source hands over the already parsed result: the trust table and
//! applicability rules of its profile and the S-1 quirk rows of its engine.

use gorca_fuse::{Profile, QuirkSet};

/// Tables of one source profile, ready for `gorca_fuse::Ivm::new`.
#[derive(Clone, Debug)]
pub struct FuseSetup {
    /// Profile name as the selector reported it (logs and `explain`).
    pub name: std::sync::Arc<str>,
    /// Trust table `PI`, applicability rules, inexpressible groups.
    pub profile: Profile,
    /// S-1 quirk rows (empty for an engine without rows).
    pub quirks: QuirkSet,
}
