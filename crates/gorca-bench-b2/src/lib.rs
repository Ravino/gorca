//! Bake-off B-2 harness (event socket drain + ingress coalescer).
//!
//! The harness compiles against the public API of `gorca-dbus::drain` and `gorca-ingress`, so
//! the same code measures the reference arm and every candidate: a candidate's worktree
//! replaces the internals of those two crates and keeps the API. See
//! `docs/bakeoff/b2-ingress/SPEC.md` for the contract and `docs/bakeoff/b2-ingress/harness/README.md`
//! for how to run it.
//!
//! This is benchmark tooling: numeric casts between sample counts, nanoseconds and floats are
//! pervasive and intentional, so the pedantic cast lints are relaxed for the whole crate.
//! Every thread here is a dedicated OS thread that sleeps on purpose (pacing, polling), never
//! an async task, so `std::thread::sleep` is allowed crate-wide.
//! Purely stylistic pedantic lints (builder `must_use`, `let...else` shape, ...) are relaxed too.
#![allow(
    clippy::return_self_not_must_use,
    clippy::float_cmp,
    clippy::type_complexity,
    clippy::unused_self,
    clippy::items_after_statements,
    clippy::if_not_else,
    clippy::manual_midpoint,
    clippy::manual_div_ceil,
    clippy::needless_late_init,
    clippy::semicolon_if_nothing_returned,
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::match_same_arms,
    clippy::too_many_lines,
    clippy::disallowed_methods,
    clippy::unreadable_literal,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::struct_excessive_bools,
    clippy::needless_pass_by_value,
    clippy::too_many_arguments
)]

pub mod bus;
pub mod clock;
pub mod fakeapp;
pub mod feeder;
pub mod limits;
pub mod mockbus;
pub mod model;
pub mod procfs;
pub mod report;
pub mod rig;
pub mod runner;
pub mod scenarios;
pub mod stats;
pub mod sys;
pub mod tap;
pub mod wire;
pub mod workload;
