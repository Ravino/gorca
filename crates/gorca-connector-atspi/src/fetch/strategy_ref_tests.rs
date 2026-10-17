//! The naive reference strategy against a fake application on an isolated bus (no real desktop).
//! Skips (and passes) when the harness prerequisites are missing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use gorca_connector_api::BackendError;
use gorca_dbus::drain::local_id;
use gorca_model::{AppId, ConnectorId, NodeKey, PropMask};
use gorca_rt::{BudgetClass, Clock, Deadline};
use gorca_testkit::harness::{FakeApp, Session};

use super::{CallCx, Caller, FetchCounters, RawBatch, Resolved, strategy};
use crate::paths::PathTable;

const BASE: &str = "/org/a11y/atspi/accessible";
const ROOT: &str = "/org/a11y/atspi/accessible/root";

struct Rig {
    cx: CallCx,
    app: FakeApp,
    _session: Session,
}

fn dl() -> Deadline {
    Deadline::after(&Clock::System, Duration::from_secs(5))
}

fn node_path(i: u64) -> String {
    if i == 0 {
        ROOT.to_owned()
    } else {
        format!("{BASE}/{i}")
    }
}

async fn rig(args: &[&str]) -> Option<Rig> {
    if let Some(why) = Session::unavailable_reason() {
        eprintln!("SKIPPED (harness unavailable): {why}");
        return None;
    }
    let session = Session::start().expect("isolated session");
    let app = FakeApp::start(&session, args).expect("fake app");
    let bus = app.bus_name().expect("bus name").to_owned();
    let conn = crate::session::connect(session.a11y_address(), dl())
        .await
        .expect("connect");
    let appid = AppId {
        connector: ConnectorId(1),
        serial: 1,
    };
    let paths = Arc::new(PathTable::new());
    let cx = CallCx {
        caller: Caller::new(conn, Arc::from(bus)),
        paths,
        app: appid,
        counters: Arc::new(FetchCounters::default()),
    };
    Some(Rig {
        cx,
        app,
        _session: session,
    })
}

fn resolved(cx: &CallCx, i: u64) -> Resolved {
    let path = node_path(i);
    let key = NodeKey {
        app: cx.app,
        local: local_id(&path),
    };
    cx.paths.insert(key, &path);
    Resolved {
        key,
        path: Arc::from(path),
    }
}

fn names(b: &RawBatch) -> Vec<String> {
    b.nodes
        .iter()
        .map(|(_, r)| r.name.clone().unwrap_or_default())
        .collect()
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    gorca_testkit::harness::run_bounded(2, std::time::Duration::from_secs(300), f);
}

const TREE: &[&str] = &["--scenario", "tree", "--nodes", "40", "--fanout", "3"];

#[test]
fn fetch_reports_what_was_asked_in_have() {
    run(async {
        let Some(r) = rig(TREE).await else { return };
        let n = resolved(&r.cx, 5);
        let want =
            PropMask::PRESENT | PropMask::PARENT | PropMask::CHILDREN | PropMask::DESCRIPTION;
        let raw = strategy::fetch(&r.cx, &n, want, dl()).await.unwrap();
        assert_eq!(raw.name.as_deref(), Some("node-5"));
        assert!(
            raw.have
                .contains(PropMask::ROLE | PropMask::STATES | PropMask::NAME)
        );
        assert!(raw.have.contains(PropMask::PARENT | PropMask::CHILDREN));
        assert_eq!(
            raw.parent,
            Some(NodeKey {
                app: r.cx.app,
                local: local_id(&node_path(1))
            }),
            "node 5's parent is node 1 (fanout 3)"
        );
        assert_eq!(
            raw.child_count,
            Some(3),
            "node 5 has children 16, 17 and 18"
        );
        // A narrower want carries less, and says so.
        let raw = strategy::fetch(&r.cx, &n, PropMask::NAME, dl())
            .await
            .unwrap();
        assert!(!raw.have.contains(PropMask::PARENT));
        assert!(raw.parent.is_none());
    });
}

#[test]
fn child_snapshots_returns_the_range_with_parent_links_and_records_paths() {
    run(async {
        let Some(r) = rig(TREE).await else { return };
        let root = resolved(&r.cx, 0);
        let b = strategy::child_snapshots(&r.cx, &root, 0..10, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(names(&b), ["node-1", "node-2", "node-3"]);
        assert!(b.nodes.iter().all(|(_, n)| n.parent == Some(root.key)));
        for (k, _) in &b.nodes {
            assert!(r.cx.paths.get(*k).is_some(), "paths come from replies");
        }
        let b = strategy::child_snapshots(&r.cx, &root, 1..2, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(names(&b), ["node-2"]);
    });
}

#[test]
fn ancestry_climbs_to_the_root_and_truncates_at_the_depth() {
    run(async {
        let Some(r) = rig(TREE).await else { return };
        let leaf = resolved(&r.cx, 13);
        let b = strategy::ancestry(&r.cx, &leaf, 10, PropMask::PRESENT, dl())
            .await
            .unwrap();
        // 13 -> 4 -> 1 -> 0 (the application root).
        assert_eq!(b.nodes.len(), 4, "{:?}", names(&b));
        assert!(!b.truncated);
        assert_eq!(b.nodes[0].1.name.as_deref(), Some("node-13"));
        assert_eq!(b.nodes[1].1.name.as_deref(), Some("node-4"));
        let b = strategy::ancestry(&r.cx, &leaf, 1, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(b.nodes.len(), 2);
        assert!(b.truncated);
    });
}

#[test]
fn subtree_is_breadth_first_and_bounded() {
    run(async {
        let Some(r) = rig(TREE).await else { return };
        let root = resolved(&r.cx, 0);
        let b = strategy::subtree(&r.cx, &root, 1000, dl()).await.unwrap();
        assert_eq!(b.nodes.len(), 40);
        assert!(!b.truncated);
        let b = strategy::subtree(&r.cx, &root, 7, dl()).await.unwrap();
        assert_eq!(b.nodes.len(), 7);
        assert!(b.truncated);
        // Known child lists were recorded for expanded nodes.
        assert!(b.nodes[0].1.children.is_some());
    });
}

#[test]
fn cache_dump_decodes_the_whole_app_and_records_paths() {
    run(async {
        let Some(r) = rig(TREE).await else { return };
        let b = strategy::cache_dump(&r.cx, 65_536, dl()).await.unwrap();
        assert_eq!(b.nodes.len(), 40);
        assert!(!b.truncated);
        let leaf = b
            .nodes
            .iter()
            .find(|(_, n)| n.name.as_deref() == Some("node-13"))
            .unwrap();
        assert_eq!(leaf.1.have, PropMask::CACHE_ITEM);
        assert!(leaf.1.parent.is_some());
        assert_eq!(
            r.cx.paths.get(leaf.0).as_deref(),
            Some(node_path(13).as_str())
        );
        let b = strategy::cache_dump(&r.cx, 5, dl()).await.unwrap();
        assert_eq!(b.nodes.len(), 5);
        assert!(b.truncated);
    });
}

/// Hostile refs (foreign bus names, null, self, dangling) never reach the mirror: the walk
/// goes on and the bad ones are dropped.
#[test]
fn hostile_refs_are_dropped_and_counted() {
    run(async {
        let Some(r) = rig(&[
            "--scenario",
            "tree",
            "--nodes",
            "40",
            "--fanout",
            "3",
            "--malformed",
            "bad_refs",
        ])
        .await
        else {
            return;
        };
        let root = resolved(&r.cx, 0);
        let b = strategy::subtree(&r.cx, &root, 1000, dl()).await.unwrap();
        for (k, _) in &b.nodes {
            assert_eq!(k.app, r.cx.app, "never a key of another app");
        }
        assert!(b.nodes.len() <= 40);
        assert!(
            r.cx.counters.snapshot().foreign_refs > 0,
            "some of the corrupted refs named another bus name"
        );
    });
}

#[test]
fn a_dead_node_is_gone_and_an_unknown_key_is_unresolved() {
    run(async {
        let Some(r) = rig(TREE).await else { return };
        let ghost = resolved(&r.cx, 9999);
        let e = strategy::fetch(&r.cx, &ghost, PropMask::PRESENT, dl()).await;
        assert!(
            matches!(e, Err(BackendError::Gone | BackendError::Protocol(_))),
            "{e:?}"
        );
        // The path table, not the app, decides `Unresolved`.
        let key = NodeKey {
            app: r.cx.app,
            local: local_id("/never/seen"),
        };
        assert!(super::resolve(&r.cx, key).is_err());
        assert!(matches!(
            super::resolve(&r.cx, key),
            Err(BackendError::Unresolved)
        ));
        let _ = BudgetClass::Interactive;
    });
}

/// F12: strategy code (which an arm replaces) must not be able to reach the raw connection or
/// `call_method`, or it could bypass the shadow-mode allowlist.
#[test]
fn strategy_sources_cannot_reach_the_raw_connection() {
    fn sources(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() && name.starts_with("strategy") {
                sources(&p, out);
            } else if p.is_file()
                && name.starts_with("strategy")
                && p.extension().is_some_and(|x| x == "rs")
                && let Ok(t) = std::fs::read_to_string(&p)
            {
                out.push((name, t));
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/fetch");
    let mut found = Vec::new();
    sources(&root, &mut found);
    // strategy.rs itself, and any strategy/ directory or strategy_*.rs an arm adds. The tests
    // of this reference strategy are excluded: they build a connection to talk to a fake app.
    found.retain(|(n, _)| !n.contains("_tests"));
    assert!(!found.is_empty(), "no strategy sources found");
    for (name, text) in &found {
        for banned in ["call_method", ".conn", "zbus::Connection", "gorca_dbus::"] {
            assert!(
                !text.contains(banned),
                "{name} names `{banned}`: strategies may only use the operations of `Caller`"
            );
        }
    }
}

fn id_of(reply: &str) -> u64 {
    // The reply carries the request id too; the node id is the one inside "path".
    let path = reply.find("/accessible/").expect("path in reply");
    let tail = &reply[path + "/accessible/".len()..];
    let end = tail.find('"').expect("closing quote");
    tail[..end].parse().expect("numeric node id")
}

const LABEL: u32 = 29;
const BUTTON: u32 = 43;

fn add(r: &mut Rig, parent: u64, role: u32, name: &str, extra: &str) -> u64 {
    let body = format!(
        "\"cmd\":\"add_child\",\"parent\":{parent},\"role\":{role},\"name\":\"{name}\"{extra}"
    );
    id_of(&r.app.cmd(&body).expect("add_child"))
}

async fn name_of(r: &Rig, id: u64) -> Option<String> {
    let n = resolved(&r.cx, id);
    strategy::fetch(&r.cx, &n, PropMask::NAME, dl())
        .await
        .unwrap()
        .name
}

/// An unnamed button takes the name of its `labelled-by` target.
#[test]
fn an_unnamed_node_is_named_by_its_labelled_by_target() {
    run(async {
        let Some(mut r) = rig(TREE).await else { return };
        let label = add(&mut r, 1, LABEL, "Files", "");
        let b = add(&mut r, 1, BUTTON, "", &format!(",\"labelled_by\":{label}"));
        assert_eq!(name_of(&r, b).await.as_deref(), Some("Files"));
    });
}

/// With no relation, the names of the label children are joined (bounded depth and count).
#[test]
fn an_unnamed_node_is_named_by_its_label_children() {
    run(async {
        let Some(mut r) = rig(TREE).await else { return };
        let b = add(&mut r, 1, BUTTON, "", "");
        add(&mut r, b, LABEL, "Open", "");
        add(&mut r, b, LABEL, "Files", "");
        assert_eq!(name_of(&r, b).await.as_deref(), Some("Open Files"));
    });
}

/// A nameless child with only text is read through the Text interface; a node with nothing
/// to say stays unnamed (no invented name).
#[test]
fn text_children_name_a_node_and_an_empty_one_stays_empty() {
    run(async {
        let Some(mut r) = rig(TREE).await else { return };
        let b = add(&mut r, 1, BUTTON, "", "");
        add(&mut r, b, LABEL, "", ",\"text\":\"Terminal\"");
        assert_eq!(name_of(&r, b).await.as_deref(), Some("Terminal"));
        let empty = add(&mut r, 1, BUTTON, "", "");
        assert!(name_of(&r, empty).await.is_none_or(|n| n.is_empty()));
    });
}

/// The descendant search is bounded: a wide unnamed subtree is cut at the node budget.
#[test]
fn the_descendant_label_search_is_bounded() {
    run(async {
        let Some(mut r) = rig(TREE).await else { return };
        let b = add(&mut r, 1, BUTTON, "", "");
        for i in 0..20 {
            add(&mut r, b, LABEL, &format!("w{i}"), "");
        }
        let name = name_of(&r, b).await.unwrap_or_default();
        assert_eq!(
            name.split(' ').count(),
            super::strategy::LABEL_SEARCH_NODES as usize
        );
    });
}

const PASSWORD: u32 = 40;
const ENTRY: u32 = 79;

/// A password field inside a nameless button, or behind its `labelled-by`, never becomes the
/// button's name (neither its name nor its text is read); a plain label beside it still does.
#[test]
fn a_password_field_never_leaks_into_a_name() {
    run(async {
        let Some(mut r) = rig(TREE).await else { return };
        // Descendant chain: button > [password(text), label].
        let b = add(&mut r, 1, BUTTON, "", "");
        add(&mut r, b, PASSWORD, "", ",\"text\":\"hunter2\"");
        assert!(name_of(&r, b).await.is_none_or(|n| n.is_empty()));
        add(&mut r, b, LABEL, "Sign in", "");
        let name = name_of(&r, b).await.unwrap_or_default();
        assert_eq!(name, "Sign in");
        assert!(!name.contains("hunter2"));
        // Deeper: button > panel > password.
        let b2 = add(&mut r, 1, BUTTON, "", "");
        let panel = add(&mut r, b2, 39, "", "");
        add(&mut r, panel, PASSWORD, "", ",\"text\":\"hunter2\"");
        assert!(name_of(&r, b2).await.is_none_or(|n| n.is_empty()));
        // Relation: labelled-by a password field (named or not).
        let pw = add(&mut r, 1, PASSWORD, "Secret", ",\"text\":\"hunter2\"");
        let b3 = add(&mut r, 1, BUTTON, "", &format!(",\"labelled_by\":{pw}"));
        assert!(name_of(&r, b3).await.is_none_or(|n| n.is_empty()));
        // The password field itself gets no fallback name.
        let pw2 = add(&mut r, 1, PASSWORD, "", ",\"text\":\"hunter2\"");
        add(&mut r, pw2, LABEL, "leak", "");
        assert!(name_of(&r, pw2).await.is_none_or(|n| n.is_empty()));
    });
}

/// Text is lent only by label-like, non-editable leaves: an entry's contents are never a name.
#[test]
fn an_editable_entry_lends_no_text_to_a_name() {
    run(async {
        let Some(mut r) = rig(TREE).await else { return };
        let b = add(&mut r, 1, BUTTON, "", "");
        add(&mut r, b, ENTRY, "", ",\"text\":\"private note\"");
        assert!(name_of(&r, b).await.is_none_or(|n| n.is_empty()));
    });
}
