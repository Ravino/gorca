#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The CI gate of the dual-carry period on the recorded A2 corpus (Firefox and Chromium pages,
//! including the M8 pages): every recorded node goes through the source's real normalisation and
//! engine tables, the fusion stage and the translator's legacy flag derivation, and the two
//! views must agree outside [`super::agree::DEVIATIONS`].

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gorca_doc_source_api::web_attrs;
use gorca_doc_source_atspi::norm::{KeyNames, fuse_setup, record_interned};
use gorca_doc_source_atspi::wire::AttrInterner;
use gorca_doc_source_atspi::{WebQuirks, roles};
use gorca_model::{AppInfo, ConnectorKind, DocDelta, SrcNode, SrcNodeId, WebEngine};

use super::agree::{Aspect, compare, deviation};
use super::fuse::{FuseStage, SemLookup};
use super::live::props_of;
use super::translate::{Translator, derive_flags};

struct Rec {
    role: u32,
    states: u64,
    attrs: Vec<(String, String)>,
    name: String,
    desc: String,
    parent: Option<usize>,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sem")
}

fn load(engine: &str, page: &str) -> Vec<Rec> {
    let path = root().join(engine).join(format!("{page}.jsonl"));
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("kind").and_then(|k| k.as_str()) == Some("header") {
            continue;
        }
        let Some(role) = v.get("role").and_then(serde_json::Value::as_u64) else {
            continue;
        };
        let words: Vec<u64> = v
            .get("state_u32")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().map(|w| w.as_u64().unwrap_or(0)).collect())
            .unwrap_or_default();
        let lo = words.first().copied().unwrap_or(0);
        let hi = words.get(1).copied().unwrap_or(0);
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_owned();
        out.push(Rec {
            role: u32::try_from(role).unwrap_or(0),
            states: lo | (hi << 32),
            attrs: v
                .get("attrs")
                .and_then(|a| a.as_object())
                .map(|o| {
                    o.iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_owned()))
                        .collect()
                })
                .unwrap_or_default(),
            name: s("name"),
            desc: s("desc"),
            parent: v
                .get("parent")
                .and_then(serde_json::Value::as_u64)
                .and_then(|p| usize::try_from(p).ok()),
        });
    }
    out
}

fn pages(engine: &str) -> Vec<String> {
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

fn info(engine: WebEngine, name: &str) -> AppInfo {
    AppInfo {
        toolkit: Arc::from(""),
        app_name: Arc::from(name),
        pid: None,
        connector_kind: ConnectorKind::Atspi,
        verified_shell: false,
        dump_complete: false,
        text_unit: gorca_model::TextUnit::Scalar,
        text_insert_slack: 0,
        edit_quirks: gorca_model::EditQuirks::default(),
        page_events_settle_ms: 0,
        web_engine: engine,
        root: None,
    }
}

/// Compare one recorded page: the nodes compared and the disagreements found.
fn run_page(
    engine: WebEngine,
    app: &str,
    legacy: WebQuirks,
    page: &str,
    dir: &str,
) -> (usize, Vec<(Aspect, String)>) {
    let recs = load(dir, page);
    let interner = AttrInterner::default();
    let alias_rows: Vec<Vec<(&'static str, Arc<str>)>> = recs
        .iter()
        .map(|r| {
            r.attrs
                .iter()
                .filter_map(|(k, v)| {
                    gorca_doc_source_atspi::wire::alias_name(k)
                        .map(|n| (n, Arc::<str>::from(v.as_str())))
                })
                .collect()
        })
        .collect();
    let attr_rows: Vec<Vec<(gorca_model::AttrKey, Arc<str>)>> = recs
        .iter()
        .map(|r| {
            let mut v: Vec<_> = r
                .attrs
                .iter()
                .map(|(k, val)| (interner.key(k), Arc::<str>::from(val.as_str())))
                .collect();
            if !r.desc.is_empty() {
                v.push((web_attrs::DESCRIPTION, Arc::from(r.desc.as_str())));
            }
            v
        })
        .collect();
    let names = KeyNames::of(&interner);
    let mut nodes: Vec<SrcNode> = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        let role = roles::map_role(r.role);
        let attrs = attr_rows[i].clone();
        let authored = attrs
            .iter()
            .any(|(k, v)| *k == web_attrs::EXPLICIT_NAME && &**v == "true");
        let raw_name = (!r.name.is_empty()).then(|| Arc::<str>::from(r.name.as_str()));
        let rec = record_interned(
            &names,
            r.role,
            Some(r.states),
            raw_name.as_deref(),
            &attrs,
            &alias_rows[i],
        );
        nodes.push(SrcNode {
            id: SrcNodeId(i as u64 + 1),
            role,
            states: legacy.states(role, roles::map_states(r.states)),
            name: legacy.name(role, authored, raw_name),
            text: None,
            attrs: attrs.into_iter().collect(),
            local: None,
            rec: Some(rec),
        });
    }
    let Some(setup) = fuse_setup(&info(engine, app)) else {
        return (0, vec![(Aspect::Role, "no fusion tables".to_owned())]);
    };
    let mut stage = FuseStage::new(setup);
    let mut kids: BTreeMap<usize, Vec<SrcNodeId>> = BTreeMap::new();
    for (i, r) in recs.iter().enumerate() {
        if let Some(p) = r.parent {
            kids.entry(p).or_default().push(SrcNodeId(i as u64 + 1));
        }
    }
    let mut batch = vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(nodes.clone()),
    ];
    for (p, k) in kids {
        batch.push(DocDelta::Children {
            parent: SrcNodeId(p as u64 + 1),
            children: k.into_iter().collect(),
        });
    }
    stage.apply(&batch);
    let mut bad_live: Vec<(Aspect, String)> = Vec::new();
    // The same batch through the translator: every node is checked against its entry while the
    // mirror is built (a debug build asserts), then the tracker's region parse is checked.
    {
        let (mut w, _read) = gorca_doc::web::channel(1);
        let mut t = Translator::new();
        // The translator checks every node (a debug assertion): the first one stops the pass,
        // the per-node report below says which ones.
        let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.apply_fused(&mut w, batch, &stage, &[]).is_ok()
        }));
        if !matches!(applied, Ok(true)) {
            eprintln!("{page}: the translator pass stopped at a disagreement or an error");
        }
        let doc = w.doc();
        for id in doc.preorder() {
            if let Some(v) = doc.node(id) {
                // A debug build asserts on a disagreement: catch it to say where it was.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| props_of(&v)));
                if let Err(e) = r {
                    let msg = e
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default();
                    bad_live.push((
                        Aspect::LiveProps,
                        format!(
                            "{page} node {:?} {}: {msg}",
                            v.meta.and_then(|m| m.src),
                            v.name.unwrap_or("")
                        ),
                    ));
                }
            }
        }
    }
    let mut bad = bad_live;
    for n in &nodes {
        let Some(e) = stage.sem(n.id) else { continue };
        let (flags, level) = derive_flags(n, false);
        for a in compare(n, flags, level, &e) {
            bad.push((
                a,
                format!(
                    "{page} #{}: {:?} legacy name {:?} fused {:?} role {}",
                    n.id.0 - 1,
                    n.role,
                    n.name
                        .as_deref()
                        .map(|s| s.chars().take(30).collect::<String>()),
                    e.name
                        .as_deref()
                        .map(|s| s.chars().take(30).collect::<String>()),
                    e.sem.role.name()
                ),
            ));
        }
    }
    (nodes.len(), bad)
}

#[test]
fn the_legacy_and_fused_views_agree_on_the_recorded_pages() {
    let mut total = 0usize;
    let mut report: Vec<String> = Vec::new();
    let mut by_aspect: BTreeMap<&'static str, usize> = BTreeMap::new();
    for (dir, engine, app, legacy) in [
        ("firefox", WebEngine::Gecko, "Firefox", WebQuirks::GECKO),
        ("chromium", WebEngine::Blink, "Chromium", WebQuirks::BLINK),
    ] {
        for page in pages(dir) {
            let (n, bad) = run_page(engine, app, legacy, &page, dir);
            total += n;
            for (a, line) in bad {
                if deviation(a) {
                    continue;
                }
                *by_aspect.entry(a.label()).or_default() += 1;
                report.push(format!("[{dir}] {} {line}", a.label()));
            }
        }
    }
    assert!(total > 500, "the corpus was not found or is tiny: {total}");
    eprintln!("legacy agreement: {total} nodes, disagreements by aspect {by_aspect:?}");
    for l in report.iter().take(80) {
        eprintln!("  {l}");
    }
    assert!(report.is_empty(), "{} disagreements", report.len());
}
