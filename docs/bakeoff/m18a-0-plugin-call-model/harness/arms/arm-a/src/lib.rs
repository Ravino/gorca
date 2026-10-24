// SPDX-License-Identifier: MIT OR Apache-2.0
//! Arm A (fallback, reference): `derive` runs synchronously inside the writer turn.
//!
//! PLACEHOLDER owned by the arm-a candidate. The harness links `factory()`; replace the body and
//! keep the signature. Until then every run of `--arm a` is recorded as a failed run (NOT
//! MEASURED), which is what an unimplemented arm must be.

use m18a0_common::host::PluginRuntime;
use m18a0_common::runner::WLaneRunner;

/// Builds the runner from the fixture's warmed plugin runtimes (plugin id = `PluginRuntime::plugin`).
pub fn factory(_runtimes: Vec<PluginRuntime>) -> anyhow::Result<Box<dyn WLaneRunner>> {
    Err(anyhow::anyhow!(
        "arm A is not implemented yet (placeholder crate m18a0-arm-a)"
    ))
}
