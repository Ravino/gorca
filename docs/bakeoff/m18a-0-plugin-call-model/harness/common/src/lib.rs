// SPDX-License-Identifier: MIT OR Apache-2.0
//! `m18a0-common`: everything the two M18a-0 arms share (SPEC 2.2, 2.3). The arms differ only in
//! the `WLaneRunner` they implement; candidates must not fork this crate.
#![allow(
    clippy::needless_range_loop,
    clippy::unusual_byte_groupings,
    clippy::should_implement_trait,
    clippy::if_same_then_else,
    clippy::chunks_exact_to_as_chunks,
    clippy::manual_is_multiple_of,
    clippy::disallowed_methods,
    clippy::too_many_arguments,
    clippy::type_complexity
)] // throwaway prototype harness: style lints only

pub mod budget;
pub mod conformance;
pub mod corpora;
pub mod fixture;
pub mod host;
pub mod memstat;
pub mod model;
pub mod presenter;
pub mod runner;
pub mod scenario;
pub mod slot;
pub mod summary;
pub mod timing;
pub mod wake;
pub mod writer;
