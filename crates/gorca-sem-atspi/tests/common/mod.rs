// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared loader for the A2 corpus tests: recorded raw nodes and the `expect.toml` helpers.
#![allow(dead_code)]

use std::path::PathBuf;

use gorca_model::PropMask;
use gorca_sem_atspi::{RawNode, SrcRecord, StateBits, normalise};
use toml_edit::{Item, Table};

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sem")
}

/// One recorded accessible.
pub struct Rec {
    pub role: u32,
    pub states: StateBits,
    pub attrs: Vec<(String, String)>,
    pub name: String,
    pub desc: String,
    pub fixture_id: Option<String>,
}

pub fn load(engine: &str, page: &str) -> Option<Vec<Rec>> {
    let path = root().join(engine).join(format!("{page}.jsonl"));
    let text = std::fs::read_to_string(path).ok()?;
    let mut out = Vec::new();
    for line in text.lines() {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        if v.get("kind").and_then(|k| k.as_str()) == Some("header") {
            continue;
        }
        let Some(role) = v.get("role").and_then(serde_json::Value::as_u64) else {
            continue; // an unreadable node (Firefox date input)
        };
        let words = v
            .get("state_u32")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .map(|w| w.as_u64().unwrap_or(0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let lo = u32::try_from(words.first().copied().unwrap_or(0)).unwrap_or(0);
        let hi = u32::try_from(words.get(1).copied().unwrap_or(0)).unwrap_or(0);
        let attrs = v
            .get("attrs")
            .and_then(|a| a.as_object())
            .map(|o| {
                o.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
        out.push(Rec {
            role: u32::try_from(role).unwrap_or(0),
            states: StateBits::from_words(lo, hi),
            attrs,
            name: s("name"),
            desc: s("desc"),
            fixture_id: v
                .get("fixture_id")
                .and_then(|f| f.as_str())
                .map(str::to_owned),
        });
    }
    Some(out)
}

pub fn find<'a>(recs: &'a [Rec], engine: &str, fixture: &str) -> Option<&'a Rec> {
    if engine == "gtk4" {
        let marker = format!("fx:{fixture}");
        recs.iter().find(|r| r.desc == marker)
    } else {
        recs.iter()
            .find(|r| r.fixture_id.as_deref() == Some(fixture))
    }
}

pub fn run(engine: &str, r: &Rec) -> SrcRecord {
    let attrs: Vec<(&str, &str)> = r
        .attrs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    // GTK4 fixtures carry the fixture id in the description; the real description is empty.
    let desc = if engine == "gtk4" || r.desc.is_empty() {
        None
    } else {
        Some(r.desc.as_str())
    };
    normalise(&RawNode {
        fetched: PropMask::ROLE
            | PropMask::STATES
            | PropMask::ATTRS
            | PropMask::NAME
            | PropMask::DESCRIPTION,
        role: r.role,
        states: r.states,
        attrs: &attrs,
        name: Some(r.name.as_str()),
        description: desc,
    })
}

pub fn str_of<'a>(t: &'a Table, k: &str) -> Option<&'a str> {
    t.get(k).and_then(Item::as_str)
}

pub fn status_ok(t: &Table) -> bool {
    t.get("status")
        .and_then(Item::as_array)
        .is_some_and(|a| a.len() == 1 && a.iter().all(|s| s.as_str() == Some("ok")))
}

pub fn status_text(t: &Table) -> String {
    t.get("status")
        .and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("+")
        })
        .unwrap_or_default()
}
