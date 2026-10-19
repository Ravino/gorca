// SPDX-License-Identifier: GPL-3.0-or-later
//! The fact-source contract (ADR-0024 section 2.6): `SourceDescriptor`, `SourceGrant`,
//! `StableIdentity`, pure `admit()`, the per-(source, scope) state machine and `SourceEpoch`.
//!
//! Pure and transport-neutral: no tokio, no IO, no clock (timestamps are passed in). Depends
//! on `gorca-model` only; the runtime types (`FactPort`, router) arrive with M23-0.
//!
//! Ordered model types (`BindQuality`, `ConfClass`, `TrustTier`) have no `Ord`; this crate
//! uses their explicit helpers (`at_least`, `weaker`, `is_stronger_than`) everywhere.

mod admit;
mod descriptor;
mod epoch;
mod grant;
mod state;

pub use admit::{
    Admission, Admitted, BindInfo, MAX_PAGE_STRING, PrivacyClass, RejectReason, Rejection, Scope,
    ScopeLookup, ScopeView, Severity, Standing, admit,
};
pub use descriptor::{AnchorKinds, InputNeeds, PeerProof, SourceDescriptor, StableIdentity};
pub use epoch::{EpochBook, MaskKind, MaskMessage, SourceEpoch};
pub use grant::{
    EvidenceMask, GROUP_CLASSES, GroupBindClass, PrivacyGrant, SourceGrant, TokenBucket,
    class_allows,
};
pub use state::{
    Notice, Remembered, ScopedEffect, SourceBook, SourceEffect, SourceEvent, SourceState, Strikes,
    step,
};
