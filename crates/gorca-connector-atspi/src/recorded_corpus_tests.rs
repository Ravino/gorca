#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The recorded A2 corpus as a test rig (M22a-C1b, ADR-0022 section 2.11): every recorded node
//! goes through the connector's decoder (the snapshot fields + the source record), ready for the
//! app actor's real fusion writer. The golden tests of the presenters replay these pages.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use gorca_model::{AppId, ConnectorId, LocalId, NodeKey, NodeSnapshot, PropMask};

use crate::decode::{RawNode, keep_attrs, keep_sem_attrs, snapshot};

pub(crate) fn root() -> PathBuf {
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
pub(crate) fn page(engine: &str, name: &str) -> Option<Vec<Arc<NodeSnapshot>>> {
    page_with(engine, name, &|_| {})
}

/// [`page`] with a hook that may edit each raw node before it is decoded (the cut-over gate's
/// state-change corpus flips states this way).
pub(crate) fn page_with(
    engine: &str,
    name: &str,
    edit: &dyn Fn(&mut RawNode),
) -> Option<Vec<Arc<NodeSnapshot>>> {
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
        let mut raw = RawNode {
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
        raw.value = recorded_value(&v, raw.role);
        if raw.value.is_some() {
            raw.have |= PropMask::VALUE;
        }
        edit(&mut raw);
        out.push(snapshot(key(i), &raw, Instant::now()));
    }
    Some(out)
}

/// The `Value` interface reading the connector takes on a focus fetch (`value_for` in
/// `fetch/strategy.rs`): slider, progress bar and spin button only, as `CurrentValue`, `MinimumValue`
/// and `MaximumValue`. The recorder stored the same reading under `value`.
fn recorded_value(v: &serde_json::Value, raw_role: u32) -> Option<crate::decode::RawValue> {
    use gorca_model::Role;
    if !matches!(
        crate::decode::role_from_raw(raw_role),
        Role::Slider | Role::ProgressBar | Role::SpinButton
    ) {
        return None;
    }
    let o = v.get("value")?.as_object()?;
    let num = |k: &str| o.get(k).and_then(serde_json::Value::as_f64);
    Some(crate::decode::RawValue::Number {
        current: num("CurrentValue")?,
        min: num("MinimumValue").unwrap_or(f64::NAN),
        max: num("MaximumValue").unwrap_or(f64::NAN),
    })
}

/// What a recorded node says about itself beyond the decoded snapshot: the replay of the cut-over
/// gate matches the nodes of two recordings by it and checks model facts against it.
#[derive(Clone, Debug)]
pub(crate) struct NodeMeta {
    /// `bus_name|path`: stable inside one recording session.
    pub path_key: String,
    /// The engine-exported HTML id or the GTK fixture id.
    pub fixture_id: Option<String>,
}

/// Per-node metadata of a recorded page, aligned with [`page`] (same lines, same order), and the
/// ids (`i`) of the nodes the recorder could not read (no role: the engine's calls failed).
pub(crate) fn page_meta(engine: &str, name: &str) -> Option<(Vec<NodeMeta>, Vec<u64>)> {
    let text = std::fs::read_to_string(root().join(engine).join(format!("{name}.jsonl"))).ok()?;
    let mut metas = Vec::new();
    let mut unreadable = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("kind").and_then(|k| k.as_str()) == Some("header") {
            continue;
        }
        let i = v.get("i").and_then(serde_json::Value::as_u64).unwrap_or(0);
        if v.get("role").and_then(serde_json::Value::as_u64).is_none() {
            unreadable.push(i);
            continue;
        }
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("");
        metas.push(NodeMeta {
            path_key: format!("{}|{}", s("bus_name"), s("path")),
            fixture_id: v
                .get("fixture_id")
                .and_then(|x| x.as_str())
                .map(str::to_owned),
        });
    }
    Some((metas, unreadable))
}

pub(crate) fn pages_of(engine: &str) -> Vec<String> {
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
