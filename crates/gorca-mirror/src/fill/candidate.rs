//! The bake-off hook (B-1 SPEC 10): `gorca_mirror::fill::Candidate`.
//!
//! After the B-1 decision (2026-10-14) the candidate *is* the adopted product policy,
//! [`FocusPrefetch`] with its default configuration. The harness keeps selecting it with
//! `--bench-fill-policy=candidate`; R0 stays [`super::NullPolicy`] (`--bench-fill-policy=r0`).
//! The harness needs only: `Candidate: FillPolicy + Default + Send + 'static`.

use super::FocusPrefetch;

/// The adopted fill policy under its bake-off name.
pub type Candidate = FocusPrefetch;
