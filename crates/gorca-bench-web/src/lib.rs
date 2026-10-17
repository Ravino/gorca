// SPDX-License-Identifier: GPL-3.0-or-later
//! B-6 harness skeleton (ADR-0003 section 16): corpus loader, page server with a command
//! channel, private Firefox, the two arms behind one trait, and the scenarios that produce
//! ADR-0001 M-9 / M-13 / M-15 and ADR-0003 WM-1 / WM-4.
//!
//! Benchmark tooling: numeric casts between sample counts, nanoseconds and floats are pervasive
//! and intentional, so the pedantic cast lints are relaxed for the whole crate.
#![allow(
    missing_debug_implementations,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::doc_markdown
)]

pub mod arm;
pub mod corpus;
pub mod discover;
pub mod firefox;
pub mod oracle;
pub mod report;
pub mod research;
pub mod scenarios;
pub mod server;
