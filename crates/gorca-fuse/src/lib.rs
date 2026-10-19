// SPDX-License-Identifier: GPL-3.0-or-later
//! The fusion point (ADR-0022 section 2.2, ADR-0024 section 2.2): a pure function of registers,
//! static tables and an epoch snapshot, evaluated in strata S-1, S0..S4 and R.
//!
//! * B1a: inline primary registers ([`register`]), strata S-1, S0 (with static `inexpressible`
//!   candidacy), S1, S2, S3 and R ([`strata`], [`repair`]), `ProvTag`/hedge/taint encoding.
//! * B1b: the evidence side table with binding, generation, epoch, `vis_rev`, TTL and floor
//!   validity ([`side`]), S4 ranking and fill ([`s4`]), taint and hedge ([`taint`]) and `explain`
//!   ([`explain`]). Non-primary candidates reach the strata only through [`side::NodeCands`].
//!
//! * B5: the S-1 quirk engine, the `PI`/`inexpressible` loaders and source-profile selection
//!   ([`quirk`]); the AT-SPI tables are data in `gorca-sem-atspi`.
//!
//! Dependency rules (`cargo xtask deps`): tokio-free and transport-neutral; depends on
//! `gorca-model` only.

pub mod epoch;
pub mod explain;
pub mod ivm;
pub mod profile;
pub mod quirk;
pub mod register;
pub mod repair;
pub mod s4;
mod sched;
pub mod side;
pub mod strata;
pub mod taint;
pub mod value;

pub use epoch::{Epochs, silences};
pub use explain::{Conflict, Explain, NoTrace, ObsCand, PatchTrail, Trace, Tracer, TrailEntry};
pub use ivm::{
    ApplyReport, Budget, Input, Ivm, IvmError, IvmStats, Published, RELATION_FANOUT_CAP, RunReport,
};
pub use profile::{Limits, PiRow, Profile, RoleSet, Rules};
pub use quirk::{
    AppFacts, PiData, ProfileSelector, QuirkAction, QuirkError, QuirkRule, QuirkSet,
    parse_inexpressible, parse_pi,
};
pub use register::{
    MAX_PRIMARIES, NodeRegs, PrimarySlot, Reg, RegRef, SrcRegs, UserOp, UserReg, slot_of,
};
pub use repair::{Eff, invariant_violations, repair as repair_eff};
pub use side::{
    AnchorId, AnchorIndex, Bound, EpochSnapshot, FactCand, FactIn, FactLayer, FactTrail, FuseCtx,
    Grant, GrantClass, NodeCands, NodeIdx, Payload, Ranked, Resolved, SideError, SideTable,
    SourceInfo, SourceState, Status, TrailStatus, grant_class,
};
pub use strata::{
    ChildView, Fused, Layer, Neighbours, NoPatches, NoQuirks, PatchHit, PatchSource, Quirks,
    Record, S0Node, S0Slot, S0View, Scratch, SelectorReads, default_slot, fuse_node,
    fuse_node_with, is_alive, rebuild_slot, s0, s0_with,
};
pub use value::{Info, MAX_TEXT_BYTES, Seq, Slot, Val, Write, hedge_cap, name_extends, rare_write};
