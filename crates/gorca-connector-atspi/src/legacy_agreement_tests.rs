#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! Legacy agreement on the recorded A2 corpus (M22a-C1b, ADR-0022 section 2.11): every recorded
//! node goes through the connector's decoder (legacy fields + source record), the app actor's
//! real fusion writer and the agreement check. The number of disagreements outside the documented
//! deviations must be zero; the deviations themselves are listed (and counted) here.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gorca_model::{AppId, ConnectorId, LocalId, NodeKey, NodeSnapshot, PropMask, WebEngine};

use crate::decode::{RawNode, keep_attrs, keep_sem_attrs, snapshot};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sem")
}

fn key(i: u64) -> NodeKey {
    NodeKey {
        app: AppId {
            connector: ConnectorId(1),
            serial: 1,
        },
        local: LocalId(i),
    }
}

/// The recorded pages of one engine as snapshots (document order, parents first), the way the
/// connector decodes a full fetch: role, states, name, description, attributes.
fn page(engine: &str, name: &str) -> Option<Vec<Arc<NodeSnapshot>>> {
    let text = std::fs::read_to_string(root().join(engine).join(format!("{name}.jsonl"))).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("kind").and_then(|k| k.as_str()) == Some("header") {
            continue;
        }
        let Some(role) = v.get("role").and_then(serde_json::Value::as_u64) else {
            continue; // an unreadable node
        };
        let words: Vec<u32> = v
            .get("state_u32")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .map(|w| u32::try_from(w.as_u64().unwrap_or(0)).unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default();
        let pairs: Vec<(String, String)> = v
            .get("attrs")
            .and_then(|a| a.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
        let i = v.get("i").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let parent = v
            .get("parent")
            .and_then(serde_json::Value::as_i64)
            .and_then(|p| u64::try_from(p).ok())
            .map(key);
        let borrowed = || pairs.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        let raw = RawNode {
            role: u32::try_from(role).unwrap_or(0),
            state_words: words,
            name: Some(s("name")),
            description: Some(s("desc")),
            parent,
            attrs: keep_attrs(borrowed()),
            sem_attrs: keep_sem_attrs(borrowed()),
            have: PropMask::ROLE
                | PropMask::STATES
                | PropMask::NAME
                | PropMask::DESCRIPTION
                | PropMask::ATTRS
                | PropMask::PARENT,
            ..RawNode::default()
        };
        out.push(snapshot(key(i), &raw, Instant::now()));
    }
    Some(out)
}

const ENGINES: [(&str, WebEngine, &str); 3] = [
    ("firefox", WebEngine::Gecko, ""),
    ("chromium", WebEngine::Blink, ""),
    ("gtk4", WebEngine::None, "GTK"),
];

fn pages_of(engine: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root().join(engine))
        .map(|d| {
            d.filter_map(Result::ok)
                .filter_map(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|n| n.strip_suffix(".jsonl"))
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// `(checked, deviated, mismatches)` over the whole corpus, with the mismatches grouped.
fn run_corpus() -> (u64, BTreeMap<String, u64>, BTreeMap<String, Vec<String>>) {
    let mut checked = 0;
    let mut deviated: BTreeMap<String, u64> = BTreeMap::new();
    let mut bad: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (engine, web, toolkit) in ENGINES {
        let tables = crate::sem_profile::tables_for(toolkit, "app", web);
        for name in pages_of(engine) {
            let Some(snaps) = page(engine, &name) else {
                continue;
            };
            let fused = gorca_core::fuse_snapshots(tables, &snaps);
            for (orig, got) in snaps.iter().zip(&fused) {
                let (Some(rec), Some(entry)) = (orig.sem.rec.as_ref(), got.sem.entry.as_ref())
                else {
                    bad.entry(format!("{engine}/{name}: node without entry"))
                        .or_default()
                        .push(format!("{:?}", orig.key));
                    continue;
                };
                let rep = gorca_core::check_agreement(got, rec, entry);
                checked += rep.checked;
                for (g, id) in rep.deviated {
                    *deviated.entry(format!("{id} ({})", g.name())).or_default() += 1;
                }
                for m in rep.mismatches {
                    bad.entry(format!(
                        "{engine}: {} legacy={} fused={}",
                        m.group.name(),
                        m.legacy.split(' ').next().unwrap_or(""),
                        m.fused.split(' ').next().unwrap_or("")
                    ))
                    .or_default()
                    .push(format!(
                        "{name}#{} role={:?} raw={}",
                        orig.key.local.0, orig.role, rec.raw_role
                    ));
                }
            }
        }
    }
    (checked, deviated, bad)
}

#[test]
fn the_corpus_agrees_with_the_legacy_normaliser_outside_the_documented_deviations() {
    let (checked, deviated, bad) = run_corpus();
    assert!(checked > 5000, "the corpus was not read ({checked} checks)");
    if !bad.is_empty() {
        let msg: Vec<String> = bad
            .iter()
            .map(|(k, v)| format!("{k}: {} nodes, e.g. {:?}", v.len(), &v[..v.len().min(3)]))
            .collect();
        let msg = msg.join("\n");
        panic!("{} kinds of disagreement:\n{msg}", bad.len());
    }
    // A deviation row that no recorded node needs any more is dead weight: delete it.
    for d in gorca_core::DEVIATIONS {
        assert!(
            deviated.keys().any(|k| k.starts_with(d.id)),
            "deviation {} has no witness in the corpus ({})",
            d.id,
            d.reason
        );
    }
    eprintln!("legacy agreement: {checked} checks, deviations {deviated:?}");
}
