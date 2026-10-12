//! Runtime primitives shared by every gorca crate.
//!
//! Responsibility (ADR-0001 section 11): "lanes (thread spawn, naming, rtkit priority),
//! `EpochCell`, `Deadline`, `BudgetClass`, virtual `Clock`, bounded-channel re-exports,
//! supervisor primitives, watchdog heartbeat."
//!
//! Principles (section 3.1): PC-4 hot lanes are dedicated threads; PC-7 every queue is bounded.
//! Only bounded channels are re-exported from here: `mpsc::channel`, never `unbounded_channel`
//! (callers must not use the latter; clippy cannot forbid it per crate, review does).
//!
//! Allowed dependencies: `gorca-model`, `tokio`, `tokio-util`, `futures-core`, `thiserror`,
//! `tracing`, `libc`. No `zbus`, `atspi*` or other transport types. `unsafe` is allowed only in
//! [`process`] (prctl), each block with a `// SAFETY:` comment (section 11, rule 6).
#![deny(clippy::wildcard_enum_match_arm)]

pub mod clock;
pub mod deadline;
pub mod epoch;
pub mod focused;
pub mod guard;
pub mod lane;
pub mod process;
pub mod supervisor;
pub mod watchdog;

pub use clock::{Clock, VirtualClock};
pub use deadline::{BudgetClass, Deadline, DeadlineExceeded, with_deadline};
pub use epoch::EpochCell;
pub use focused::FocusedApp;
pub use futures_core::future::BoxFuture;
pub use lane::{Lane, LaneExit, LaneHandle, LanePriority, spawn_lane};
pub use supervisor::{
    RestartDecision, RestartPolicy, RestartTracker, TaskExit, TaskReport, TaskSet,
};
pub use tokio::sync::{Notify, mpsc, oneshot, watch};
pub use tokio_util::sync::CancellationToken;
pub use watchdog::{Heartbeat, LaneVerdict};
