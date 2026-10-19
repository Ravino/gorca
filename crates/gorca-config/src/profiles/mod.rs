// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration profiles (ADR-0016): triggers, files, the compiled set, the published snapshot,
//! the resolvers and edit planning. Pure: no tokio, no I/O. The daemon's config task reads and
//! writes files and publishes the snapshot; the Reader is the only writer of the snapshot.

pub mod build;
pub mod compile;
pub mod edit;
pub mod file;
pub mod naming;
pub mod resolve;
pub mod snapshot;
pub mod trigger;

pub use build::{BuiltLayers, build_layers, initial_snapshot, product_row_diags, product_rows};
pub use compile::{AppCandidates, ProfileId, ProfileLoader, ProfileSet};
pub use edit::{
    DiskView, Edit, EditApplied, EditPlan, EditRefusal, FileId, SaveGlobalReport, SavePlan,
    SaveRefusal, Target, TargetWhy, WriteStep,
};
pub use file::{
    ExtSchema, ExtType, KeyRef, Origin, ParsedProfile, ParsedRows, Profile, ProfileSource,
    RowOrigin, VOLUME_FLOOR, admit_core, admit_ext, matrix_allows, parse_rows, parse_user_profile,
};
pub use resolve::{
    AppScoped, DocFacts, DocScoped, Effective, EffectiveCache, ExtEntry, LayerRef, ReportFlags,
};
pub use snapshot::{
    AppKey, BaseLayers, Deltas, EditTargetKind, EditTargets, Interner, LayerDelta, LruMap,
    MatchSetId, Pin, PinId, PinSel, Pins, ProfileCell, ProfileKey, ProfileMode, ProfileSnapshot,
    UserBase,
};
pub use trigger::{
    AppFacts, AppMatcher, ContextSet, FocusContext, FocusFacts, N_CONTEXT, SiteFacts, Specificity,
    Trigger, TriggerKind, WindowFacts, WindowKind,
};

#[cfg(test)]
mod tests;
