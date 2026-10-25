#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The recorded A2 corpus (Firefox and Chromium pages, including the M8 pages) as a test rig:
//! every recorded node goes through the source's real normalisation and engine tables, the fusion
//! stage and the translator, and the documents the actor builds from them are what the browse
//! and live-region tests read.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gorca_doc_source_api::web_attrs;
use gorca_doc_source_atspi::norm::{KeyNames, fuse_setup, record_interned};
use gorca_doc_source_atspi::wire::AttrInterner;
use gorca_doc_source_atspi::{WebQuirks, roles};
use gorca_model::{AppInfo, ConnectorKind, DocDelta, Role, SrcNode, SrcNodeId, WebEngine};

use super::fuse::{FuseStage, SemLookup};
use super::translate::Translator;

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
pub(crate) struct Built {
    pub(crate) nodes: Vec<SrcNode>,
    pub(crate) stage: FuseStage,
    batch: Vec<DocDelta>,
}

pub(crate) fn build(
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
            synth_role: gorca_model::testing::sem_role(role),
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
        .filter(|v| v.data.is_some_and(|m| m.sem.is_some()))
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
        synth_role: gorca_model::testing::sem_role(role),
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

/// Build `control` (nameless, role `sem`) with `kids`, fuse it, and give back the one-line label
/// the presenter takes from the entry.
fn synthetic(sem: gorca_model::sem::SemRole, legacy_role: Role, kids: Vec<Kid>) -> Option<String> {
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
    let setup = fuse_setup(&info(WebEngine::Gecko, "Firefox"))?;
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
        .find(|id| doc.node(*id).and_then(|v| v.data).and_then(|m| m.src) == Some(SrcNodeId(2)))
        .expect("the control is in the mirror");
    let _ = (doc, id);
    let entry = stage.sem(SrcNodeId(2)).expect("fused");
    entry
        .name
        .as_deref()
        .and_then(gorca_present::one_line_label)
}

type Case = (&'static str, SemRole, Role, Vec<Kid>, Option<&'static str>);

#[test]
fn a_nameless_control_is_named_from_its_content() {
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
        let new = synthetic(sem, role, kids);
        assert_eq!(new.as_deref(), want, "{what}: the fused entry");
    }
}

#[test]
fn a_long_content_name_is_cut_to_the_label_cap() {
    use gorca_model::sem::SemRole as S;
    let new = synthetic(
        S::Button,
        Role::Button,
        vec![Kid::Run(
            "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor \
             incididunt ut labore et dolore magna aliqua Ut enim ad minim veniam quis nostrud \
             exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat",
        )],
    );
    let new = new.unwrap();
    assert!(new.chars().count() <= 160);
    assert!(new.starts_with("Lorem ipsum dolor sit amet"));
}

/// S3 looks through nameless containers (accname 2F): text inside a wrapper names the control.
#[test]
fn a_nameless_wrapper_does_not_hide_its_text_from_the_content_name() {
    use gorca_model::sem::SemRole as S;
    let new = synthetic(
        S::Button,
        Role::Button,
        vec![Kid::Wrap(vec![Kid::Run("Save")])],
    );
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
