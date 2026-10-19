// SPDX-License-Identifier: GPL-3.0-or-later
//! Name <-> code tables of every registry, for the oracle test and for the generated copy in
//! `gorca-ext-api` (which cannot depend on this crate). Codes are append-only.

use super::codes::{BindQuality, ConfClass, EvidenceKind, Group, ProvCode, SourceClass, TrustTier};
use super::role::SemRole;
use super::states::{
    Avail, Check, Current, Expand, Invalid, NameFrom, Orientation, Popup, PosSrc, Press, Relevant,
    Select, SemFlags, Sort,
};

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

macro_rules! rows_of {
    ($($(#[$m:meta])* $fn_name:ident => $ty:ty),+ $(,)?) => {
        $(
            $(#[$m])*
            pub fn $fn_name() -> Vec<Row> {
                <$ty>::ALL.iter().map(|v| (v.name(), u32::from(v.code()))).collect()
            }
        )+
    };
}

rows_of! {
    /// `aria-checked` states.
    check => Check,
    /// `aria-pressed` states.
    press => Press,
    /// `aria-expanded` states.
    expand => Expand,
    /// `aria-selected` states.
    select => Select,
    /// `aria-current` values.
    current => Current,
    /// `aria-invalid` values.
    invalid => Invalid,
    /// `aria-haspopup` values.
    popup => Popup,
    /// Availability values.
    avail => Avail,
    /// Name sources.
    name_from => NameFrom,
    /// Orientation values.
    orientation => Orientation,
    /// Sort values.
    sort => Sort,
    /// Set-position sources.
    pos_src => PosSrc,
}

/// `SemFlags` bit values (the name is the flag name, the code is the bit mask).
pub fn sem_flags() -> Vec<Row> {
    SemFlags::all()
        .iter_names()
        .map(|(n, f)| (n, f.bits()))
        .collect()
}

/// `Relevant` bit values.
pub fn relevant() -> Vec<Row> {
    Relevant::all()
        .iter_names()
        .map(|(n, f)| (n, u32::from(f.bits())))
        .collect()
}
