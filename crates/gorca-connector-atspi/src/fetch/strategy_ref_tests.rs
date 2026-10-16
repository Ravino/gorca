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
    _app: FakeApp,
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
        _app: app,
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
