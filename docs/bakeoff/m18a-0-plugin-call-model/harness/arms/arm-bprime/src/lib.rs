// SPDX-License-Identifier: MIT OR Apache-2.0
//! Arm B' (design target): one actor per plugin on a dedicated pool, bounded futex wait.
//!
//! PLACEHOLDER owned by the arm-bprime candidate. The harness links `factory()`; replace the body
//! and keep the signature. `m18a0_common::wake::{futex_wait_abs, futex_wake, TimerFd}` and
//! `m18a0_common::timing::set_timerslack` are provided as shared building blocks.

use m18a0_common::host::PluginRuntime;
use m18a0_common::runner::WLaneRunner;

/// Builds the runner from the fixture's warmed plugin runtimes (plugin id = `PluginRuntime::plugin`).
pub fn factory(_runtimes: Vec<PluginRuntime>) -> anyhow::Result<Box<dyn WLaneRunner>> {
    Err(anyhow::anyhow!(
        "arm B' is not implemented yet (placeholder crate m18a0-arm-bprime)"
    ))
}
