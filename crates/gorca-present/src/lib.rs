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
//!
//! Everything here is pure and synchronous (no runtime).

pub mod browse;
pub mod change;
pub mod compose;
pub mod desktop;
pub mod deviations;
pub mod diff;
pub mod edit;
/// The IR records live in `gorca-present-ir` (no runtime), re-exported here.
pub use gorca_present_ir as ir;
pub mod render;
pub mod roles;
mod web;
pub mod zone;

pub use change::{changed_groups, compose_change};
pub use compose::{
    ComposeCtx, ComposeError, LABEL_CAP, ReportFlags, compose, is_secret_entry, is_secret_role,
    name_is_blank, one_line_label,
};
pub use desktop::{compose_notice, compose_window, notice_has_words};
pub use deviations::{Deviations, DeviationsError};
pub use diff::{DiffCtx, DiffOutcome, LegacyToken, diff, diff_report};
pub use edit::{IrEdit, apply as apply_edits, fields_from_tokens, kind_of_token};
pub use gorca_i18n::Lang;
pub use ir::{
    Arg, ArgVal, Content, ExtKindError, ExtKinds, ExtNs, ExtRef, Field, FieldFlags, FieldKind,
    FlatSpan, IrFlags, LangId, LangTags, Origin, PRES_IR_VERSION, PresIr, Reason, Redacted, RowId,
};
pub use render::{RenderOpts, Token, TokenText, render, render_kinds};
pub use zone::{plain_text_role, review_role_word, role_word_of};
