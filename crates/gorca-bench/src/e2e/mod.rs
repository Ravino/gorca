// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end latency harness for Phase 1 (keypress / focus event -> speech start).
//!
//! ```text
//!  fakeapp.py / fake_mutter.py          reader under test                 mock speech server
//!  (private a11y + session bus)   -->   stub reader or gorca-daemon  -->  (voicego binary | SSIP)
//!   t_emit (CLOCK_MONOTONIC)             drain -> ingress -> lane           t_speak, t_cancel
//! ```
//!
//! Every timestamp is `CLOCK_MONOTONIC` nanoseconds ([`mono::now_ns`]), comparable across the
//! Python stimulus processes and this process. The reader is a child process behind a tiny seam
//! ([`runner::ReaderSpec`]): the in-tree [`stub`] reader today, `gorca-daemon` once it lands;
//! both are configured by the same environment variables ([`env`]).
//!
//! Benchmark tooling: casts between sample counts, nanoseconds and floats are pervasive and
//! intentional, threads here are dedicated OS threads that may sleep, and a few purely
//! stylistic pedantic lints are relaxed (same policy as `gorca-bench-b2`).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::disallowed_methods,
    clippy::too_many_arguments,
    clippy::needless_pass_by_value,
    clippy::unreadable_literal,
    clippy::items_after_statements,
    clippy::manual_let_else,
    clippy::single_match_else,
    clippy::match_same_arms,
    clippy::struct_excessive_bools,
    clippy::type_complexity
)]

pub mod account;
pub mod apps;
pub mod host;
pub mod m1;
pub mod m3;
pub mod m4;
pub mod m4b;
pub mod m6;
pub mod mock;
pub mod mono;
pub mod runner;
pub mod scenarios;
pub mod stub;
pub mod web;
pub mod webtext;

/// Environment variables that configure a reader (stub or daemon). The daemon is expected to
/// honour the same ones.
pub mod env {
    /// Address of the (private) a11y bus. Already set by `tests/harness/run-isolated.sh`.
    pub const A11Y_BUS: &str = "GORCA_A11Y_BUS_ADDRESS";
    /// Address of the (private) session bus (keyboard monitor). Standard variable.
    pub const SESSION_BUS: &str = "DBUS_SESSION_BUS_ADDRESS";
    /// `voicego` (binary framing) or `ssip` (text framing).
    pub const SPEECH_BACKEND: &str = "GORCA_SPEECH_BACKEND";
    /// Filesystem path of the unix socket of the speech server.
    pub const SPEECH_SOCKET: &str = "GORCA_SPEECH_SOCKET";
    /// Stub only: `1` = every non-empty event batch also makes the app actor call `Get Name` on
    /// the source app with a deadline (exercises hung peers; speech does not wait for it).
    pub const STUB_CALLS: &str = "GORCA_STUB_CALLS";
}
