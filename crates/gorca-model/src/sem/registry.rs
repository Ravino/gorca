// SPDX-License-Identifier: GPL-3.0-or-later
//! Name <-> code tables of every registry, for the oracle test and for the generated copy in
//! `gorca-ext-api` (which cannot depend on this crate). Codes are append-only.

use super::codes::{BindQuality, ConfClass, EvidenceKind, Group, ProvCode, SourceClass, TrustTier};
use super::role::SemRole;

/// One registry row: registry name and code.
pub type Row = (&'static str, u32);

/// Fused semantic groups (code = slot).
pub fn groups() -> Vec<Row> {
    Group::ALL
        .iter()
        .map(|g| (g.name(), u32::from(g.code())))
        .collect()
}

/// Evidence kinds (bit 15 set).
pub fn evidence() -> Vec<Row> {
    EvidenceKind::ALL
        .iter()
        .map(|k| (k.name(), u32::from(k.code())))
        .collect()
}

/// Provenance codes.
pub fn prov_codes() -> Vec<Row> {
    ProvCode::ALL
        .iter()
        .map(|p| (p.name(), u32::from(p.code())))
        .collect()
}

/// Confidence classes.
pub fn conf_classes() -> Vec<Row> {
    ConfClass::ALL
        .iter()
        .map(|c| (c.name(), u32::from(c.code())))
        .collect()
}

/// Binding qualities.
pub fn bind_qualities() -> Vec<Row> {
    BindQuality::ALL
        .iter()
        .map(|b| (b.name(), u32::from(b.code())))
        .collect()
}

/// Source classes.
pub fn source_classes() -> Vec<Row> {
    SourceClass::ALL
        .iter()
        .map(|c| (c.name(), u32::from(c.code())))
        .collect()
}

/// Trust tiers.
pub fn trust_tiers() -> Vec<Row> {
    TrustTier::ALL
        .iter()
        .map(|t| (t.name(), u32::from(t.code())))
        .collect()
}

/// Semantic roles.
pub fn roles() -> Vec<Row> {
    SemRole::ALL
        .iter()
        .map(|r| (r.name(), u32::from(r.code())))
        .collect()
}
