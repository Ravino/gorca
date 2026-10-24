// SPDX-License-Identifier: MIT OR Apache-2.0
//! Arm registry. `noop` and `ref` belong to the harness; `a` and `bprime` are the candidate crates
//! (placeholders until the candidates replace their `lib.rs`).

use m18a0_common::host::PluginRuntime;
use m18a0_common::runner::{NoopRunner, RefInlineRunner, WLaneRunner};

pub type Built = anyhow::Result<Box<dyn WLaneRunner>>;
pub type Factory = fn(Vec<PluginRuntime>) -> Built;

fn noop(_r: Vec<PluginRuntime>) -> Built {
    Ok(Box::new(NoopRunner))
}

fn reference(r: Vec<PluginRuntime>) -> Built {
    Ok(Box::new(RefInlineRunner::new(r)))
}

pub fn get(arm: &str) -> Option<Factory> {
    match arm {
        "noop" => Some(noop),
        "ref" => Some(reference),
        "a" => Some(m18a0_arm_a::factory),
        "bprime" => Some(m18a0_arm_bprime::factory),
        _ => None,
    }
}

pub const ARMS: [&str; 4] = ["noop", "ref", "a", "bprime"];

/// The two real candidates judged by the gates; `noop` and `ref` only prove the harness.
pub fn is_candidate(arm: &str) -> bool {
    arm == "a" || arm == "bprime"
}
