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
use gorca_model::{AppInfo, ConnectorKind, DocDelta, Role, SrcNode, SrcNodeId, WebEngine};

use super::agree::{Aspect, compare, deviation};
use super::fuse::{FuseStage, SemLookup};
use super::live::props_of;
use super::translate::{Translator, derive_flags};

struct Rec {
    /// The first 400 scalars of the Text interface (`GetText`), when the node has one.
    text: Option<String>,
    role: u32,
    states: u64,
    attrs: Vec<(String, String)>,
    name: String,
    desc: String,
    parent: Option<usize>,
    /// The index of the target of the `error-message` relation.
    error_target: Option<usize>,
    /// The error message of an invalid control, read the way the document source reads it
    /// (`extras::error_text`): the text of the relation's target, else its name.
    error_text: Option<String>,
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
            text: v.get("text").and_then(|t| t.as_str()).map(str::to_owned),
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
            error_target: v
                .get("relations")
                .and_then(|r| r.as_array())
                .and_then(|rels| {
                    rels.iter()
                        .find(|r| r.get("name").and_then(|n| n.as_str()) == Some("error-message"))
                })
                .and_then(|r| r.get("targets").and_then(|t| t.as_array()))
                .and_then(|t| t.first())
                .and_then(serde_json::Value::as_u64)
                .and_then(|t| usize::try_from(t).ok()),
            error_text: None,
        });
    }
    // The document source reads the message of an invalid control (state `invalid-entry`) of a
    // role that takes one from the relation's target.
    let invalid = 1u64 << roles::state_bit::INVALID_ENTRY;
    let found: Vec<Option<String>> = out
        .iter()
        .map(|r| {
            if r.states & invalid == 0
                || !gorca_doc_source_atspi::extras::takes_error_text(roles::map_role(r.role))
            {
                return None;
            }
            let t = out.get(r.error_target?)?;
            let raw = t
                .text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(t.name.trim());
            let text = raw.replace('\u{FFFC}', "").replace('\n', " ");
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_owned())
        })
        .collect();
    for (r, f) in out.iter_mut().zip(found) {
        r.error_text = f;
    }
    out
}

pub(crate) fn pages(engine: &str) -> Vec<String> {
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
        exe: None,
        text_unit: gorca_model::TextUnit::Scalar,
        text_insert_slack: 0,
        edit_quirks: gorca_model::EditQuirks::default(),
        page_events_settle_ms: 0,
        web_engine: engine,
        root: None,
        sem_tables: None,
    }
}

/// One recorded page, normalised and fused the way the document actor does it.
struct Built {
    recs: Vec<Rec>,
    nodes: Vec<SrcNode>,
    stage: FuseStage,
    batch: Vec<DocDelta>,
}

fn build(
    engine: WebEngine,
    app: &str,
    legacy: WebQuirks,
    page: &str,
    dir: &str,
    synth_text: bool,
    shuffle: u64,
) -> Result<Built, String> {
    let mut recs = load(dir, page);
    if synth_text {
        recs = web_document_of(recs);
    }
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
            if let Some(t) = &r.error_text {
                v.push((web_attrs::ERROR_TEXT, Arc::from(t.as_str())));
            }
            v
        })
        .collect();
    let names = KeyNames::of(&interner);
    let texts = if synth_text {
        recorded_texts(&recs)
    } else {
        vec![None; recs.len()]
    };
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
            // The document source reads states of the stateful roles only (`StatePolicy::Stateful`).
            roles::is_stateful(r.role).then_some(r.states),
            raw_name.as_deref(),
            &attrs,
            &alias_rows[i],
        );
        nodes.push(SrcNode {
            id: SrcNodeId(i as u64 + 1),
            role,
            states: legacy.states(role, roles::map_web_states(r.states)),
            name: legacy.name(role, authored, raw_name),
            text: texts[i].clone(),
            attrs: attrs.into_iter().collect(),
            local: None,
            rec: Some(rec),
        });
    }
    let Some(setup) = fuse_setup(&info(engine, app)) else {
        return Err("no fusion tables".to_owned());
    };
    let mut stage = FuseStage::new(setup);
    let mut kids: BTreeMap<usize, Vec<SrcNodeId>> = BTreeMap::new();
    for (i, r) in recs.iter().enumerate() {
        if let Some(p) = r.parent {
            kids.entry(p).or_default().push(SrcNodeId(i as u64 + 1));
        }
    }
    // A non-zero seed hands the nodes to the fusion stage in a shuffled order: the fused values
    // are a function of the final state, so nothing downstream may change.
    let mut arrival = nodes.clone();
    let mut x = shuffle;
    if x != 0 {
        for i in (1..arrival.len()).rev() {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            let j = usize::try_from(z % (i as u64 + 1)).unwrap_or(0);
            arrival.swap(i, j);
        }
    }
    let mut batch = vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(arrival),
    ];
    for (p, k) in kids {
        batch.push(DocDelta::Children {
            parent: SrcNodeId(p as u64 + 1),
            children: k.into_iter().collect(),
        });
    }
    if synth_text {
        // The document source rewrites hypertext into text runs between its fetch and the actor
        // (Gecko: split at the embedded-object marks; Blink: its static children repeat the
        // parent's text and are dropped): do the same, so the document has the shape it has live.
        let mut layer = if engine == WebEngine::Blink {
            gorca_doc_source_atspi::runs::RunLayer::dropping_static_children()
        } else {
            gorca_doc_source_atspi::runs::RunLayer::new()
        };
        batch = layer.process(batch);
    }
    stage.apply(&batch);
    // The engine fuses under a time budget; the page is complete before anything is compared.
    while stage.pending() > 0 {
        stage.step();
    }
    Ok(Built {
        recs,
        nodes,
        stage,
        batch,
    })
}

/// The recorded Text interface of every node, as the document source would read it (`GetText`).
/// A node without one has no text of its own.
fn recorded_texts(recs: &[Rec]) -> Vec<Option<Arc<str>>> {
    recs.iter()
        .map(|r| {
            r.text
                .as_deref()
                .filter(|t| !t.is_empty())
                .map(Arc::<str>::from)
        })
        .collect()
}

/// Keep the subtree of the first web document of a recording (an Electron or Telegram recording
/// starts at the application, with the window chrome around the page), re-indexed from 0. A page
/// whose first node is already the document is returned as it is.
fn web_document_of(recs: Vec<Rec>) -> Vec<Rec> {
    let Some(root) = recs
        .iter()
        .position(|r| roles::map_role(r.role) == Role::DocumentWeb)
    else {
        return recs;
    };
    if root == 0 {
        return recs;
    }
    let mut keep = vec![false; recs.len()];
    keep[root] = true;
    for (i, r) in recs.iter().enumerate().skip(root + 1) {
        if r.parent.is_some_and(|p| keep[p]) {
            keep[i] = true;
        }
    }
    let mut new_index = vec![usize::MAX; recs.len()];
    let mut next = 0;
    for (i, k) in keep.iter().enumerate() {
        if *k {
            new_index[i] = next;
            next += 1;
        }
    }
    recs.into_iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(i, mut r)| {
            r.parent = if i == root {
                None
            } else {
                r.parent.map(|p| new_index[p])
            };
            r
        })
        .collect()
}

/// The document of one recorded page, built the way the document actor builds it: the real
/// normalisation, the fusion stage (every node carries its entry) and the translator's flags.
/// `None` for a page with no node or no fusion tables.
pub(crate) fn recorded_doc(
    engine: WebEngine,
    app: &str,
    legacy: WebQuirks,
    page: &str,
    dir: &str,
    shuffle: u64,
) -> Option<gorca_doc::web::WebDoc> {
    let Built { stage, batch, .. } = build(engine, app, legacy, page, dir, true, shuffle).ok()?;
    let (mut w, _read) = gorca_doc::web::channel(1);
    let mut t = Translator::new();
    t.apply_fused(&mut w, batch, &stage, &[]).ok()?;
    Some(w.doc().clone())
}

/// Compare one recorded page: the nodes compared and the disagreements found.
fn run_page(
    engine: WebEngine,
    app: &str,
    legacy: WebQuirks,
    page: &str,
    dir: &str,
) -> (usize, Vec<(Aspect, String)>) {
    let Built {
        nodes,
        stage,
        batch,
        ..
    } = match build(engine, app, legacy, page, dir, false, 0) {
        Ok(b) => b,
        Err(e) => return (0, vec![(Aspect::Role, e)]),
    };
    let mut bad_live: Vec<(Aspect, String)> = Vec::new();
    // The same batch through the translator: every node is checked against its entry while the
    // mirror is built (a debug build asserts), then the tracker's region parse is checked.
    {
        let (mut w, _read) = gorca_doc::web::channel(1);
        let mut t = Translator::new();
        // The translator checks every node (a debug assertion): the first one stops the pass,
        // the per-node report below says which ones.
        let applied = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.apply_fused(&mut w, batch.clone(), &stage, &[]).is_ok()
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
fn a_recorded_page_builds_a_document_with_text_and_entries() {
    let doc = recorded_doc(
        WebEngine::Gecko,
        "Firefox",
        WebQuirks::GECKO,
        "m8-forms",
        "firefox",
        0,
    )
    .expect("document");
    assert!(doc.text_len() > 20, "{}", doc.text_len());
    let with_sem = doc
        .preorder()
        .filter_map(|id| doc.node(id))
        .filter(|v| v.meta.is_some_and(|m| m.sem.is_some()))
        .count();
    assert!(with_sem > 20, "{with_sem}");
    for k in 0..doc.line_count().min(40) {
        let Some(a) = doc.line_start(k) else { continue };
        let (s, e) = doc.line_range(a);
        let items = crate::browse::present::describe(&doc, s, e);
        eprintln!(
            "L{k}: {:?}",
            items
                .iter()
                .map(|i| i.text().to_owned())
                .collect::<Vec<_>>()
        );
    }
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

// ---- C3: the fused entry replaces `subtree_label` (X1) -----------------------------------------

/// The retired `subtree_label` of `browse/commands.rs`, kept here as the oracle of the parity
/// test: the visible text of a node's subtree as a one-line label (whitespace collapsed, icon
/// glyphs dropped, capped), or `None` when it holds none.
fn legacy_subtree_label(d: &gorca_doc::web::WebDoc, id: gorca_model::DocNodeId) -> Option<String> {
    const CAP: u32 = 160;
    let (a, b) = d.subtree_text_range(id)?;
    if a >= b {
        return None;
    }
    let mut raw = String::new();
    d.read_text(a, b.min(a.saturating_add(CAP)), &mut raw);
    let raw: String = raw
        .chars()
        .filter(|c| {
            !gorca_model::text::is_icon_glyph(*c) && !gorca_model::text::is_object_marker(*c)
        })
        .collect();
    let label = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!crate::present::name_is_blank(Some(&label))).then_some(label)
}

/// The CSS list markers the engine puts in front of a list item's name.
const MARKERS: [&str; 10] = ["• ", "◦ ", "▪ ", "■ ", "□ ", "– ", "● ", "○ ", "‣ ", "⁃ "];

#[derive(Default, Debug)]
struct Parity {
    /// Controls the legacy overlay would have named from the page text.
    legacy_fills: usize,
    /// ... of which the entry gives the same name.
    same: usize,
    /// ... of which the entry's name differs only by what the text normalisation does.
    same_after_normalising: usize,
    different: Vec<String>,
    /// The legacy overlay found text, the entry no name.
    lost: Vec<String>,
    /// The entry names a control the legacy overlay left nameless.
    gained: usize,
    /// List and tree items whose engine name carries a marker.
    marked: usize,
    marker_stripped: usize,
}

fn collapse(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !gorca_model::text::is_icon_glyph(*c) && !gorca_model::text::is_object_marker(*c)
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parity_of_page(
    engine: WebEngine,
    app: &str,
    legacy: WebQuirks,
    page: &str,
    dir: &str,
    out: &mut Parity,
) {
    let Ok(Built {
        recs,
        nodes,
        stage,
        batch,
    }) = build(engine, app, legacy, page, dir, false, 0)
    else {
        return;
    };
    let (mut w, _read) = gorca_doc::web::channel(1);
    let mut t = Translator::new();
    if t.apply_fused(&mut w, batch, &stage, &[]).is_err() {
        return;
    }
    let doc = w.doc();
    let mut by_src: BTreeMap<u64, gorca_model::DocNodeId> = BTreeMap::new();
    for id in doc.preorder() {
        if let Some(src) = doc.node(id).and_then(|v| v.meta).and_then(|m| m.src) {
            by_src.insert(src.0, id);
        }
    }
    for (i, r) in recs.iter().enumerate() {
        let Some(id) = by_src.get(&(i as u64 + 1)).copied() else {
            continue;
        };
        let role = roles::map_role(r.role);
        let node = &nodes[i];
        let Some(entry) = stage.sem(node.id) else {
            continue;
        };
        let snap_name = (!r.name.is_empty()).then(|| Arc::<str>::from(r.name.as_str()));
        let snap = fake_snapshot(role, snap_name.clone());
        let _ = &snap;
        let fused = entry.name.clone();
        // The list marker.
        if matches!(role, Role::ListItem | Role::TreeItem)
            && let Some(n) = snap_name.as_deref()
            && MARKERS.iter().any(|m| n.starts_with(m))
        {
            out.marked += 1;
            if fused
                .as_deref()
                .is_some_and(|f| !f.starts_with(['•', '◦', '▪', '■', '□', '–', '●', '○', '‣', '⁃']))
            {
                out.marker_stripped += 1;
            }
        }
        let blank = crate::present::name_is_blank(snap_name.as_deref());
        if !(blank && crate::present::needs_name(role) && role != Role::ComboBox) {
            continue;
        }
        let old = legacy_subtree_label(doc, id);
        let new = entry
            .name
            .as_deref()
            .filter(|n| !crate::present::name_is_blank(Some(n)))
            .map(str::to_owned);
        match (old, new) {
            (None, None) => {}
            (None, Some(_)) => out.gained += 1,
            (Some(o), None) => out.lost.push(format!("{page} #{i} {role:?}: legacy {o:?}")),
            (Some(o), Some(n)) => {
                out.legacy_fills += 1;
                if o == n {
                    out.same += 1;
                } else if collapse(&o) == collapse(&n) {
                    out.same_after_normalising += 1;
                } else {
                    out.different
                        .push(format!("{page} #{i} {role:?}: legacy {o:?} fused {n:?}"));
                }
            }
        }
    }
}

fn fake_snapshot(role: Role, name: Option<Arc<str>>) -> gorca_model::NodeSnapshot {
    use gorca_model::{
        AppId, ChildList, ConnectorId, Freshness, Ifaces, LocalId, NodeKey, NodeSem, PropMask,
    };
    gorca_model::NodeSnapshot {
        key: NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(1),
        },
        role,
        states: gorca_model::StateSet::empty(),
        name,
        description: None,
        value: None,
        parent: None,
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: gorca_model::AttrMap::default(),
        relations: gorca_model::Relations::default(),
        bounds: None,
        have: PropMask::PRESENT,
        confirmed_seq: 0,
        fresh: Freshness::Fresh {
            at: std::time::Instant::now(),
        },
        sem: NodeSem::NONE,
    }
}

#[test]
fn the_fused_entry_names_the_controls_subtree_label_used_to_name() {
    let mut p = Parity::default();
    for (dir, engine, app, legacy) in [
        ("firefox", WebEngine::Gecko, "Firefox", WebQuirks::GECKO),
        ("chromium", WebEngine::Blink, "Chromium", WebQuirks::BLINK),
    ] {
        for page in pages(dir) {
            parity_of_page(engine, app, legacy, &page, dir, &mut p);
        }
    }
    eprintln!(
        "overlay parity: legacy fills {} same {} same-after-normalising {} different {} lost {} gained {}; markers {} stripped {}",
        p.legacy_fills,
        p.same,
        p.same_after_normalising,
        p.different.len(),
        p.lost.len(),
        p.gained,
        p.marked,
        p.marker_stripped
    );
    for l in p.different.iter().chain(p.lost.iter()).take(60) {
        eprintln!("  {l}");
    }
    assert!(p.marked > 0, "the corpus has marked list items");
    assert_eq!(
        p.marked, p.marker_stripped,
        "every marker is stripped by the entry"
    );
    assert!(p.lost.is_empty(), "{:?}", p.lost);
    assert!(p.different.is_empty(), "{:?}", p.different);
}

// ---- synthetic controls: the cases the recorded pages never hit -----------------------------------
//
// A recorded page has a name for every control the engine names from content, so the corpus above
// says only that the quirk rows strip what the legacy overlay stripped. These small trees are the
// controls the engine left nameless: the retired `subtree_label` against the fused entry.

enum Kid {
    /// A text run (a synthetic node, the source's inline text).
    Run(&'static str),
    /// A real static-text leaf: the text is both its name and its text.
    Leaf(&'static str),
    /// A nameless generic wrapper around more content.
    Wrap(Vec<Kid>),
    /// A node with a role, the name the engine gave it (and where that came from), and content.
    Node(
        SemRole,
        Role,
        Option<&'static str>,
        gorca_model::sem::NameFrom,
        Vec<Kid>,
    ),
}

use gorca_model::sem::{Group, GroupMask, SemRole, SrcRecord};

fn syn_rec(role: SemRole, name: Option<&str>) -> Arc<SrcRecord> {
    let mut r = SrcRecord::default();
    r.sem.role = role;
    r.name = name.map(Arc::from);
    r.known = GroupMask::EMPTY;
    r.known.set(Group::Role, true);
    r.known.set(Group::Name, true);
    Arc::new(r)
}
fn syn_node(id: u64, role: Role, rec: Option<Arc<SrcRecord>>, text: Option<&str>) -> SrcNode {
    SrcNode {
        id: SrcNodeId(id),
        role,
        states: gorca_model::StateSet::VISIBLE
            | gorca_model::StateSet::SHOWING
            | gorca_model::StateSet::ENABLED,
        name: None,
        text: text.map(Arc::from),
        attrs: gorca_model::AttrMap::default(),
        local: None,
        rec,
    }
}
fn syn_add(
    parent: u64,
    kids: Vec<Kid>,
    next: &mut u64,
    nodes: &mut Vec<SrcNode>,
    lists: &mut Vec<(u64, Vec<u64>)>,
) {
    let mut ids = Vec::new();
    for k in kids {
        let id = *next;
        *next += 1;
        ids.push(id);
        match k {
            Kid::Run(t) => nodes.push(syn_node(id, Role::StaticText, None, Some(t))),
            Kid::Leaf(t) => nodes.push(syn_node(
                id,
                Role::StaticText,
                Some(syn_rec(SemRole::StaticText, Some(t))),
                Some(t),
            )),
            Kid::Wrap(inner) => {
                nodes.push(syn_node(
                    id,
                    Role::Panel,
                    Some(syn_rec(SemRole::Generic, None)),
                    None,
                ));
                syn_add(id, inner, next, nodes, lists);
            }
            Kid::Node(sem, legacy, name, from, inner) => {
                let mut r = (*syn_rec(sem, name)).clone();
                r.sem.name_from = from;
                r.known.set(Group::NameFrom, true);
                nodes.push(syn_node(id, legacy, Some(Arc::new(r)), None));
                syn_add(id, inner, next, nodes, lists);
            }
        }
    }
    lists.push((parent, ids));
}

/// Build `control` (nameless, role `sem`) with `kids`, fuse it, and give back the legacy label and
/// what the overlay would take from the entry.
fn synthetic(
    sem: gorca_model::sem::SemRole,
    legacy_role: Role,
    kids: Vec<Kid>,
) -> (Option<String>, Option<String>) {
    let mut next = 3u64;
    let mut nodes: Vec<SrcNode> = Vec::new();
    let mut lists: Vec<(u64, Vec<u64>)> = Vec::new();
    nodes.push(syn_node(
        1,
        Role::DocumentWeb,
        Some(syn_rec(SemRole::DocumentWeb, None)),
        None,
    ));
    nodes.push(syn_node(2, legacy_role, Some(syn_rec(sem, None)), None));
    lists.push((1, vec![2]));
    syn_add(2, kids, &mut next, &mut nodes, &mut lists);
    let Some(setup) = fuse_setup(&info(WebEngine::Gecko, "Firefox")) else {
        return (None, None);
    };
    let mut stage = FuseStage::new(setup);
    let mut batch = vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(nodes),
    ];
    for (p, k) in lists {
        batch.push(DocDelta::Children {
            parent: SrcNodeId(p),
            children: k.into_iter().map(SrcNodeId).collect(),
        });
    }
    batch.push(DocDelta::Settled { batch: 1 });
    stage.apply(&batch);
    while stage.pending() > 0 {
        stage.step();
    }
    let (mut w, _read) = gorca_doc::web::channel(1);
    let mut t = Translator::new();
    assert!(t.apply_fused(&mut w, batch, &stage, &[]).is_ok());
    let doc = w.doc();
    let id = doc
        .preorder()
        .find(|id| doc.node(*id).and_then(|v| v.meta).and_then(|m| m.src) == Some(SrcNodeId(2)))
        .expect("the control is in the mirror");
    let old = legacy_subtree_label(doc, id);
    let entry = stage.sem(SrcNodeId(2)).expect("fused");
    let _ = legacy_role;
    let new = entry
        .name
        .as_deref()
        .and_then(gorca_present::one_line_label);
    (old, new)
}

type Case = (&'static str, SemRole, Role, Vec<Kid>, Option<&'static str>);

#[test]
fn a_nameless_control_is_named_from_its_content_as_subtree_label_did() {
    use gorca_model::sem::SemRole as S;
    let cases: Vec<Case> = vec![
        (
            "one run",
            S::Button,
            Role::Button,
            vec![Kid::Run("Save")],
            Some("Save"),
        ),
        (
            "runs and blanks",
            S::Link,
            Role::Link,
            vec![Kid::Run("  Save "), Kid::Run(" "), Kid::Run("all  ")],
            Some("Save all"),
        ),
        (
            "an icon glyph and a word",
            S::Button,
            Role::Button,
            vec![Kid::Run("\u{e900}"), Kid::Run("Mute")],
            Some("Mute"),
        ),
        (
            "an icon glyph alone",
            S::Button,
            Role::Button,
            vec![Kid::Run("\u{e900}")],
            None,
        ),
        (
            "static text leaves",
            S::MenuItem,
            Role::MenuItem,
            vec![Kid::Leaf("Open "), Kid::Leaf("recent")],
            Some("Open recent"),
        ),
        (
            "a tab",
            S::Tab,
            Role::Tab,
            vec![Kid::Run("Inbox")],
            Some("Inbox"),
        ),
        ("no content", S::Button, Role::Button, vec![], None),
    ];
    for (what, sem, role, kids, want) in cases {
        let (old, new) = synthetic(sem, role, kids);
        assert_eq!(old.as_deref(), want, "{what}: the retired label");
        assert_eq!(new.as_deref(), want, "{what}: the fused entry");
    }
}

#[test]
fn a_long_content_name_is_cut_like_the_old_label() {
    use gorca_model::sem::SemRole as S;
    let (old, new) = synthetic(
        S::Button,
        Role::Button,
        vec![Kid::Run(
            "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
             incididunt ut labore et dolore magna aliqua Ut enim ad minim veniam quis nostrud \
             exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat",
        )],
    );
    let (old, new) = (old.unwrap(), new.unwrap());
    assert!(new.chars().count() <= 160);
    // The old label cut the raw text at 160 characters, the new one cuts the cleaned label there.
    assert!(new.starts_with(old.trim_end()) || old.starts_with(new.trim_end()));
}

/// S3 looks through nameless containers (accname 2F): text inside a wrapper names the control.
#[test]
fn a_nameless_wrapper_does_not_hide_its_text_from_the_content_name() {
    use gorca_model::sem::SemRole as S;
    let (old, new) = synthetic(
        S::Button,
        Role::Button,
        vec![Kid::Wrap(vec![Kid::Run("Save")])],
    );
    assert_eq!(old.as_deref(), Some("Save"));
    assert_eq!(new.as_deref(), Some("Save"));
}

/// The Signal message row of `tests/web/chr0/pages/signal-like.html` (study G1,
/// docs/reports/signal-orca-vs-gorca.md): Blink names the focused row `module-message` from the
/// one button it sees, "React to message"; the content row, the author, the text and the time sit
/// in nameless containers. The fused entry names the row from all of it.
#[test]
fn the_signal_message_row_is_named_from_author_text_and_time_not_from_its_button_alone() {
    use gorca_model::sem::NameFrom;
    let row = || {
        Kid::Node(
            SemRole::Row,
            Role::Row,
            Some("React to message"),
            NameFrom::Content,
            vec![Kid::Wrap(vec![
                Kid::Node(
                    SemRole::Row,
                    Role::Row,
                    None,
                    NameFrom::None,
                    vec![
                        Kid::Wrap(vec![Kid::Leaf("Contact 0")]),
                        Kid::Wrap(vec![
                            Kid::Wrap(vec![Kid::Leaf("Message number 398 body text")]),
                            Kid::Wrap(vec![Kid::Leaf("10:38")]),
                        ]),
                    ],
                ),
                Kid::Node(
                    SemRole::Button,
                    Role::Button,
                    Some("React to message"),
                    NameFrom::Author,
                    vec![],
                ),
            ])],
        )
    };
    let mut next = 3u64;
    let mut nodes = vec![syn_node(
        1,
        Role::DocumentWeb,
        Some(syn_rec(SemRole::DocumentWeb, None)),
        None,
    )];
    let mut lists = vec![(1u64, vec![2u64])];
    // The row is node 2; its content follows.
    let mut r = (*syn_rec(SemRole::Row, Some("React to message"))).clone();
    r.sem.name_from = NameFrom::Content;
    r.known.set(Group::NameFrom, true);
    nodes.push(syn_node(2, Role::Row, Some(Arc::new(r)), None));
    let Kid::Node(_, _, _, _, inner) = row() else {
        panic!("a node")
    };
    syn_add(2, inner, &mut next, &mut nodes, &mut lists);
    let setup = fuse_setup(&info(WebEngine::Blink, "Chromium")).expect("blink tables");
    let mut stage = FuseStage::new(setup);
    let mut batch = vec![
        DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        },
        DocDelta::Upsert(nodes),
    ];
    for (p, k) in lists {
        batch.push(DocDelta::Children {
            parent: SrcNodeId(p),
            children: k.into_iter().map(SrcNodeId).collect(),
        });
    }
    stage.apply(&batch);
    while stage.pending() > 0 {
        stage.step();
    }
    let e = stage.sem(SrcNodeId(2)).expect("the row is fused");
    let name = e.name.as_deref().unwrap_or("");
    assert_eq!(
        name,
        "Contact 0 Message number 398 body text 10:38 React to message"
    );
    assert_eq!(
        e.prov.get(Group::Name),
        Some(gorca_model::sem::ProvCode::Derived)
    );
    // What the Reader speaks: the engine's shallow name gives way to the derived one.
    let spoken = e.name.clone();
    assert_eq!(
        spoken.as_deref(),
        Some("Contact 0 Message number 398 body text 10:38 React to message")
    );
    // The authored name of the button is read as it is.
    assert_eq!(
        stage.sem(SrcNodeId(2)).map(|e| e.sem.role),
        Some(SemRole::Row)
    );
}
