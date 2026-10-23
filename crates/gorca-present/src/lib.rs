// SPDX-License-Identifier: GPL-3.0-or-later
//! `gorca-present`: the semantic presentation pipeline (ADR-0022 section 2.5, milestone M22b1).
//!
//! `SemEntry -> compose -> PresIr -> (hooks) -> render -> tokens`. It reads only the fused
//! semantic model (`gorca-model::sem`), never a snapshot or a connector type.
//!
//! * [`ir`] - the `PresIr` records (draft v0, frozen at the end of M22b1 slice 2).
//! * [`compose`] - the rule table (slice 1: `native.focus`).
//! * [`render`] - the only producer of text.
//! * [`diff`], [`deviations`] - the shadow diff against the legacy presenter and its allowed
//!   differences (`deviations.toml`).
//! * [`shadow`] - the presenter actor that runs next to the legacy presenter and only counts.
//!
//! Everything but [`shadow`] is pure and synchronous.

pub mod compose;
pub mod deviations;
pub mod diff;
/// The IR records live in `gorca-present-ir` (no runtime), re-exported here.
pub use gorca_present_ir as ir;
pub mod render;
pub mod roles;
pub mod shadow;

pub use compose::{ComposeCtx, ComposeError, ReportFlags, compose};
pub use deviations::{Deviations, DeviationsError};
pub use diff::{DiffCtx, DiffOutcome, LegacyToken, diff};
pub use ir::{
    Arg, ArgVal, Content, ExtKindError, ExtKinds, ExtNs, ExtRef, Field, FieldFlags, FieldKind,
    FlatSpan, IrFlags, LangId, LangTags, Origin, PRES_IR_VERSION, PresIr, Reason, Redacted, RowId,
};
pub use render::{RenderOpts, Token, TokenText, render};
pub use shadow::{
    FocusSample, SHADOW_QUEUE, ShadowActor, ShadowHandle, ShadowSnapshot, SkipWhy, channel,
    channel_with,
};
