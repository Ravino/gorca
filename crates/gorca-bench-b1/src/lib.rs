// SPDX-License-Identifier: GPL-3.0-or-later
//! Bake-off B-1 harness (native mirror fill strategy), perf-engineer share (SPEC section 11:
//! H-4 verdicts, H-5 message accounting, H-6 runner, H-7 step bench, H-9 R0 definition, H-12 host).
//!
//! The crate has no dependency on the mirror, the core or any connector: it consumes **files**
//! (the truth log, the presentation log and a counters file, see [`rundir`]) that the walk driver
//! (H-3) and the daemon (PW-10) write, so the same code judges every arm. The walk driver and the
//! fake apps live elsewhere (`walk.rs`, `tests/harness/b1/`) and plug in through the run-directory
//! contract; nothing here spawns a daemon.
//!
//! Benchmark tooling: casts between counts, nanoseconds and floats are intentional, and threads
//! here are dedicated OS threads that may sleep.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::disallowed_methods,
    clippy::float_cmp,
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::match_same_arms,
    clippy::struct_excessive_bools,
    clippy::type_complexity,
    clippy::return_self_not_must_use
)]

pub mod host;
pub mod limits;
pub mod msgs;
pub mod rundir;
pub mod runner;
pub mod step;
pub mod verdict;
