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

pub mod appctl;
pub mod proto;
pub mod service;
pub mod world;
pub mod sim;
pub mod sut;
pub mod ic;
pub mod verdict;
mod selftest;
