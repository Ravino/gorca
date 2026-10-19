// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration profiles (ADR-0016): triggers, files, the compiled set, the published snapshot,
//! the resolvers and edit planning. Pure: no tokio, no I/O. The daemon's config task reads and
//! writes files and publishes the snapshot; the Reader is the only writer of the snapshot.

pub mod compile;
pub mod file;
pub mod naming;
pub mod trigger;

pub use compile::{ProfileId, ProfileLoader, ProfileSet};
pub use file::{
    ExtSchema, ExtType, KeyRef, Origin, ParsedProfile, ParsedRows, Profile, ProfileSource,
    RowOrigin, VOLUME_FLOOR, admit_core, admit_ext, matrix_allows, parse_rows, parse_user_profile,
};
pub use trigger::{
    AppFacts, AppMatcher, ContextSet, FocusContext, FocusFacts, N_CONTEXT, SiteFacts, Specificity,
    Trigger, TriggerKind, WindowFacts, WindowKind,
};
