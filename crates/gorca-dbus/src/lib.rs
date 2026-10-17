// SPDX-License-Identifier: GPL-3.0-or-later
//! Deadline call helper, drain primitives, P2P pool and `SIOCINQ` sampling.
//!
//! Responsibility (ADR-0001 sections 5.3 and 11): "deadline call helper, event-connection drain
//! (B-2), P2P pool, match-rule/RegisterEvent management, `SIOCINQ` sampling".
//!
//! > "D-1 Drains never wait downstream ... D-2 Lag detection: the drain samples its own socket
//! > backlog with `ioctl(SIOCINQ)` every batch ... Backlog over a watermark (default 4 MiB)
//! > triggers a global `Shed` signal."
//! > "D-3 Replies never share a socket with the flood."
//! > "D-4 Deadline helper: one `call(deadline, ...)` helper wraps `tokio::time::timeout`.
//! > Direct zbus proxy calls are banned with `clippy::disallowed_methods`."
//! > "D-6 Own connections: `max_queued` >= 4096 (zbus default is 64)."
//!
//! Phase 0 contains [`with_deadline`], [`call_method`] (the single allowed zbus call site),
//! [`socket_backlog`] (the only `unsafe` here, in [`socket`]) and the [`LagDetector`]. TBD: the
//! drain loop itself (bake-off B-2 decides zbus `MessageStream` vs raw framing), the P2P pool
//! (D-7), `RegisterEvent` and match-rule management (D-5; do not use atspi-rs
//! `remove_match_rule`, it adds a rule).
//!
//! Allowed dependencies: `gorca-rt`, `zbus`, `libc`, `serde`, `thiserror`. This is one of the
//! five crates that may depend on `zbus` (section 11, rule 2).

mod call;
pub mod drain;
mod lag;
pub mod socket;

pub use call::{DbusError, call_method, call_method_to, with_deadline};
pub use lag::{LagDetector, ShedEvent};
pub use socket::{set_recv_buffer, socket_backlog};

/// `max_queued` for our own connections (D-6). zbus defaults to 64, which backpressures into the
/// broker and gets us disconnected.
pub const MAX_QUEUED: usize = 4096;
/// A drain reads at least this much per `recv` (D-1).
pub const DRAIN_RECV_BYTES: usize = 64 * 1024;
/// Default `SIOCINQ` watermark that triggers `Shed` (D-2).
pub const SHED_WATERMARK_BYTES: usize = 4 * 1024 * 1024;
