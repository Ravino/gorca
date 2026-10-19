// SPDX-License-Identifier: GPL-3.0-or-later
//! The typed semantic model (ADR-0022, ADR-0024 section 2.12): value types, code registries and
//! the x32 entry layout. No tokio, no connector types, no provenance inside `Semantics`.

mod coded;
pub mod codes;
pub mod entry;
pub mod registry;
pub mod role;
pub mod src_record;
pub mod states;

pub use codes::{
    BindQuality, ConfClass, EVIDENCE_BIT, EvidenceKind, GROUP_CAP, GROUP_COUNT, Group, GroupClass,
    GroupCode, GroupMask, HIGH_MILLI, PROBABLE_MILLI, ProvCode, ProvTag, SlotBits2, SourceClass,
    TrustTier,
};
pub use entry::{SemEntry, Semantics};
pub use role::SemRole;
pub use src_record::{Notes, RoleSource, SrcRecord, TagName};
pub use states::{
    Avail, Check, Current, Expand, GridPos, Invalid, Live, NameFrom, Orientation, Popup, PosSrc,
    Press, Relevant, Select, SemFlags, SemText, SetPos, Sort,
};
