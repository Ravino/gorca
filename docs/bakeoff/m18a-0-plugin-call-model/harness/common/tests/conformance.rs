// SPDX-License-Identifier: MIT OR Apache-2.0
//! The reference runner passes the conformance checks every `WLaneRunner` must pass.

use m18a0_common::host::PluginRuntime;
use m18a0_common::runner::{RefInlineRunner, WLaneRunner};

fn make(r: Vec<PluginRuntime>) -> anyhow::Result<Box<dyn WLaneRunner>> {
    Ok(Box::new(RefInlineRunner::new(r)))
}

#[test]
fn reference_runner_conforms() {
    let res = m18a0_common::conformance::run(make).expect("run");
    let bad: Vec<_> = res.iter().filter(|r| !r.1).collect();
    assert!(bad.is_empty(), "{bad:?}");
}
