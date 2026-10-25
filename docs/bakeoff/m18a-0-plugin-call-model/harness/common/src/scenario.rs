// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scenario files (TOML): one file per scenario, a seed makes every stream reproducible.

use serde::{Deserialize, Serialize};

fn d_seed() -> u64 {
    1
}
fn d_samples() -> u64 {
    200_000
}
fn d_warm() -> u64 {
    5_000
}
fn d_interval() -> u64 {
    400
}
fn d_nodes() -> usize {
    4096
}
fn d_chain() -> usize {
    8
}
fn d_fixture() -> String {
    "benign".to_string()
}

/// One stage of a multi-stage scenario (`adv-each-added`): the adversaries of the stage are added
/// on top of the scenario's fixture; every stage is its own result directory.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stage {
    pub name: String,
    #[serde(default)]
    pub adv: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    /// focus | keypress | k1k2 | flap | storm | memory | wq1 | wq2 | wq3 | wq6-wake | wq6-rows |
    /// mq2 | pf6 | pf1 | a5 | smoke | selftest
    pub kind: String,
    #[serde(default)]
    pub desc: String,
    #[serde(default = "d_seed")]
    pub seed: u64,
    #[serde(default = "d_fixture")]
    pub fixture: String,
    #[serde(default)]
    pub adv: Vec<String>,
    #[serde(default, rename = "stage")]
    pub stages: Vec<Stage>,
    #[serde(default = "d_samples")]
    pub samples: u64,
    #[serde(default = "d_warm")]
    pub warmup: u64,
    /// Open-loop spacing of focus changes / keypresses.
    #[serde(default = "d_interval")]
    pub interval_us: u64,
    #[serde(default = "d_nodes")]
    pub nodes: usize,
    /// Focus chain size (nodes per focus change).
    #[serde(default = "d_chain")]
    pub chain: usize,
    /// keypress: seconds per leg and number of on/off pairs.
    #[serde(default)]
    pub leg_s: f64,
    #[serde(default)]
    pub pairs: u32,
    /// storm: events per second and duration.
    #[serde(default)]
    pub rate: f64,
    #[serde(default)]
    pub duration_s: f64,
    /// storm: fraction of nodes watched by the storm plugins.
    #[serde(default)]
    pub watch_fraction: f64,
    /// flap / k1k2: the `N` ms cap on interactive waits.
    #[serde(default)]
    pub flap_ms: Option<u64>,
    /// memory: plugin counts to load.
    #[serde(default)]
    pub counts: Vec<u32>,
    /// mq2 / pf6 / pf1: number of runs.
    #[serde(default)]
    pub runs: u64,
    /// Diagnostic variants (labelled in meta.json): pin writer to a CPU, SCHED_FIFO.
    #[serde(default)]
    pub pin_writer: Option<usize>,
    #[serde(default)]
    pub timerslack_ns: Option<u64>,
    /// Per-scenario run watchdog in seconds (spec clarification C-3). `--watchdog-s` overrides it; when
    /// absent, `default_watchdog_s` applies.
    #[serde(default)]
    pub watchdog_s: Option<u64>,
}

impl Scenario {
    /// Watchdog for this scenario: the explicit `watchdog_s`, else for `keypress` 1.5 x (2 legs x pairs x
    /// leg_s) + 120 s of setup and calibration headroom (1,020 s at 5 x 60 s), else 600 s.
    pub fn default_watchdog_s(&self) -> u64 {
        if let Some(w) = self.watchdog_s {
            return w;
        }
        if self.kind == "keypress" {
            let legs = 2.0 * self.pairs.max(1) as f64 * self.leg_s;
            return ((legs * 1.5) as u64 + 120).max(600);
        }
        600
    }

    pub fn load(path: &std::path::Path) -> anyhow::Result<Scenario> {
        let s = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&s)?)
    }

    /// Apply smoke-style overrides.
    pub fn with_overrides(
        mut self,
        samples: Option<u64>,
        warmup: Option<u64>,
        leg_s: Option<f64>,
        duration_s: Option<f64>,
    ) -> Scenario {
        if let Some(s) = samples {
            self.samples = s;
        }
        if let Some(w) = warmup {
            self.warmup = w;
        }
        if let Some(l) = leg_s {
            self.leg_s = l;
        }
        if let Some(d) = duration_s {
            self.duration_s = d;
        }
        self
    }

    /// Expand stages into resolved single-stage scenarios (name suffix = stage name).
    pub fn expand(&self) -> Vec<Scenario> {
        if self.stages.is_empty() {
            return vec![self.clone()];
        }
        self.stages
            .iter()
            .map(|st| {
                let mut s = self.clone();
                s.name = format!("{}-{}", self.name, st.name);
                s.adv = st.adv.clone();
                s.stages.clear();
                s
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_expands() {
        let s: Scenario = toml::from_str(
            r#"
name = "adv-each-added"
kind = "focus"
[[stage]]
name = "a1"
adv = ["a1"]
[[stage]]
name = "a1-a2"
adv = ["a1", "a2"]
"#,
        )
        .expect("parse");
        let v = s.expand();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].adv, ["a1", "a2"]);
        assert_eq!(v[0].samples, 200_000);
    }
}
