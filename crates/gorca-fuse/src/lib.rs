// SPDX-License-Identifier: GPL-3.0-or-later
//! The fusion point (ADR-0022 section 2.2, ADR-0024 section 2.2): a pure function of registers,
//! static tables and an epoch snapshot, evaluated in strata S-1, S0..S4 and R.
//!
//! M22a-B1a: inline primary registers ([`register`]), strata S-1, S0 (with static `inexpressible`
//! candidacy), S1, S2, S3, the S4 fill rule and R ([`strata`], [`repair`]), fill rules and
//! `ProvTag`/hedge/taint encoding into [`gorca_model::sem::SemEntry`]. The evidence side table,
//! S4 ranking, taint plumbing from facts, `explain` (B1b) and incremental maintenance (B2) build
//! on these seams.
//!
//! Dependency rules (`cargo xtask deps`): tokio-free and transport-neutral; depends on
//! `gorca-model` only.

pub mod profile;
pub mod register;
pub mod repair;
pub mod strata;
pub mod value;

pub use profile::{PiRow, Profile, RoleSet, Rules};
pub use register::{MAX_PRIMARIES, NodeRegs, PrimarySlot, Reg, SrcRegs, UserOp, UserReg, slot_of};
pub use repair::{Eff, invariant_violations, repair as repair_eff};
pub use strata::{
    ChildView, FactCand, Fused, Layer, Neighbours, NoPatches, NoQuirks, NodeCands, PatchHit,
    PatchSource, Quirks, Record, S0Node, S0Slot, Scratch, default_slot, fuse_node, is_alive, s0,
};
pub use value::{Info, Seq, Slot, Val, Write, hedge_cap};
