// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Signal private-session fixture of ADR-0026 section 5 (P-2 + 9 dummies), the PX-4
//! adversaries, kappa calibration, and the glue that turns a name list into a writer and runtimes.

use crate::budget::BudgetCfg;
use crate::host::{calibrate_kappa, EngineCfg, HostEngine, LoadedPlugin, PluginRuntime};
use crate::model::*;
use crate::writer::{PluginSpec, StubWriter, WriterCfg};
use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::sync::Arc;

pub const BENIGN: [&str; 10] = [
    "signal-p2",
    "dummy-1",
    "dummy-2",
    "dummy-3",
    "dummy-4",
    "dummy-5",
    "dummy-6",
    "dummy-7",
    "dummy-8",
    "dummy-9",
];

/// Adversary ids as the scenario files name them -> guest name(s).
pub fn adversary_guests(id: &str) -> Vec<&'static str> {
    match id {
        "a1" => vec!["a1-page-stride"],
        "a2" => vec!["a2-pointer-chase"],
        "a3" => vec!["a3-huge-string", "a3b-huge-assert", "a3c-huge-attr"],
        "a4" => vec!["a4-huge-return"],
        "a5" => vec!["a5-cold-load"],
        // 10 plugins each spending the full interactive grant: nine spinners on top of the fixture.
        "a6" => vec!["a6-full-grant"; 9],
        "a13" => vec!["a13-subtree-loop"],
        "a7" => vec!["a7-patch-match"],
        "a8" => vec!["a8-refresh-each"],
        "a12" => vec!["a12-self"],
        _ => vec![],
    }
}

/// Watch masks over role codes. Dummies pick from this table with the scenario seed.
const MASKS: [u64; 6] = [
    1 << ROLE_ROW,
    1 << ROLE_LABEL,
    (1 << ROLE_ROW) | (1 << ROLE_LABEL),
    (1 << ROLE_BUTTON) | (1 << ROLE_ROW),
    (1 << ROLE_LIST) | (1 << ROLE_WINDOW) | (1 << ROLE_ROW),
    (1 << ROLE_LABEL) | (1 << ROLE_BUTTON),
];

pub fn watch_mask(name: &str, seed: u64, index: usize) -> u64 {
    if name == "signal-p2" {
        return 1 << ROLE_ROW;
    }
    if let Some(k) = name
        .strip_prefix("dummy-")
        .and_then(|k| k.parse::<usize>().ok())
    {
        return MASKS[(k + seed as usize) % MASKS.len()];
    }
    let _ = index;
    (1 << ROLE_ROW) | (1 << ROLE_LABEL)
}

pub fn guests_dir() -> PathBuf {
    if let Ok(d) = std::env::var("M18A0_GUESTS") {
        return PathBuf::from(d);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/guests")
}

pub fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("M18A0_CACHE") {
        return PathBuf::from(d);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/cwasm")
}

/// Plugin names of a scenario: `fixture` = "benign" | "p2" | "none" | explicit list via adv.
pub fn plugin_names(fixture: &str, adv: &[String]) -> Vec<String> {
    let mut v: Vec<String> = match fixture {
        "benign" => BENIGN.iter().map(|s| s.to_string()).collect(),
        "p2" => vec!["signal-p2".to_string()],
        _ => Vec::new(),
    };
    for a in adv {
        for g in adversary_guests(a) {
            v.push(g.to_string());
        }
    }
    v
}

pub struct Fixture {
    pub he: Arc<HostEngine>,
    pub names: Vec<String>,
    pub loaded: Vec<Arc<LoadedPlugin>>,
    pub specs: Vec<PluginSpec>,
    pub kappa: f64,
    pub kappa_rounds: Vec<f64>,
    pub seed: u64,
}

impl Fixture {
    pub fn build(names: &[String], seed: u64, kappa_override: Option<f64>) -> Result<Fixture> {
        let he = HostEngine::new(&EngineCfg {
            cache_dir: cache_dir(),
            pooling: true,
        })
        .map_err(|e| anyhow!("{e}"))?;
        let gd = guests_dir();
        if !gd.join("SHA256SUMS").exists() {
            return Err(anyhow!(
                "guests not built: run harness/guests/build.sh (looked in {gd:?})"
            ));
        }
        let (kappa, rounds) = match kappa_override {
            Some(k) => (k, vec![]),
            None => {
                let mut r = calibrate_kappa(&he, &gd.join("a6-full-grant.wasm"), 9)
                    .map_err(|e| anyhow!("{e}"))?;
                r.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                (r[r.len() / 2], r)
            }
        };
        let mut loaded = Vec::new();
        let mut specs = Vec::new();
        for (i, n) in names.iter().enumerate() {
            let lp = he
                .load(n, &gd.join(format!("{n}.wasm")))
                .map_err(|e| anyhow!("load {n}: {e}"))?;
            loaded.push(Arc::new(lp));
            let adversary = n.starts_with('a') && n != "a5-cold-load";
            specs.push(PluginSpec {
                name: if names.iter().filter(|x| *x == n).count() > 1 {
                    format!("{n}#{i}")
                } else {
                    n.clone()
                },
                tier: if n == "signal-p2" {
                    1
                } else if adversary {
                    3
                } else {
                    2
                },
                watch_roles: watch_mask(n, seed, i),
                adversary,
            });
        }
        Ok(Fixture {
            he,
            names: names.to_vec(),
            loaded,
            specs,
            kappa,
            kappa_rounds: rounds,
            seed,
        })
    }

    /// Fresh, warmed runtimes (instantiate + attach off the writer turn), plugin id = index.
    pub fn runtimes(&self) -> Result<Vec<PluginRuntime>> {
        let mut v = Vec::new();
        for (i, lp) in self.loaded.iter().enumerate() {
            let mut rt = PluginRuntime::new(i as PluginId, self.he.clone(), lp.clone());
            rt.warm().map_err(|e| anyhow!("warm {}: {e}", lp.name))?;
            v.push(rt);
        }
        Ok(v)
    }

    pub fn writer_cfg(&self, chain: usize, record_cap: usize) -> WriterCfg {
        let mut c = WriterCfg::spec(self.kappa);
        c.chain = chain;
        c.record_cap = record_cap;
        c
    }

    pub fn writer(&self, nodes: usize, chain: usize, record_cap: usize) -> StubWriter {
        let table = NodeTable::build(nodes, self.seed);
        StubWriter::new(
            table,
            self.specs.clone(),
            self.writer_cfg(chain, record_cap),
        )
    }

    /// A writer without any plugin (the plugins-off leg).
    pub fn writer_off(&self, nodes: usize, chain: usize, record_cap: usize) -> StubWriter {
        let table = NodeTable::build(nodes, self.seed);
        StubWriter::new(table, Vec::new(), self.writer_cfg(chain, record_cap))
    }

    pub fn budget(&self) -> BudgetCfg {
        BudgetCfg::spec(self.kappa)
    }

    /// The row node ids of a table, for focus streams.
    pub fn row_ids(table: &NodeTable) -> Vec<NodeId> {
        table
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.role == ROLE_ROW)
            .map(|(i, _)| i as NodeId)
            .collect()
    }

    pub fn guest_hashes(&self) -> Vec<(String, String)> {
        self.loaded
            .iter()
            .map(|l| (l.name.clone(), l.sha256.clone()))
            .collect()
    }
}
