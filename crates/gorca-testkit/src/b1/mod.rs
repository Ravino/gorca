//! B-1 harness (bake-off `docs/bakeoff/b1-mirror-fill/SPEC.md`), test-engineer share.
//!
//! | Module | SPEC item |
//! |---|---|
//! | [`proto`], [`service`], [`world`] | H-2: model schema, control commands, truth log, seeded service-time model, the pure state of a fake app |
//! | [`appctl`] | H-2/H-3: drive the `b1-fakeapp` binary |
//! | [`sut`], [`sim`], [`verdict`], [`ic`] | H-8: invalidation conformance IC-1..IC-20 against any reader |
//! | [`audit`], [`items`] | H-8: audit tests MA-1..MA-10 against thin adapter traits |
//! | [`walk`] | H-3: scripted walk W-1 |
//!
//! Nothing here depends on a transport crate or on the PW-1..PW-12 types: the reader under test,
//! the mirror and the policy enter through the adapter traits of [`sut`] and [`audit`].

// Test-support code: seeded pseudo-random generators, byte mutators and counters use casts and
// arithmetic whose range is bounded by construction.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::unreadable_literal,
    clippy::manual_is_multiple_of,
    clippy::range_plus_one,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::struct_excessive_bools,
    clippy::unnecessary_wraps,
    clippy::match_same_arms,
    clippy::semicolon_if_nothing_returned,
    clippy::nonminimal_bool,
    clippy::single_match_else,
    clippy::map_unwrap_or,
    clippy::redundant_closure_for_method_calls
)]

pub mod appctl;
pub mod audit;
pub mod ic;
pub mod items;
pub mod proto;
pub mod real;
mod selftest;
pub mod service;
pub mod sim;
pub mod sut;
pub mod verdict;
pub mod walk;
pub mod world;
