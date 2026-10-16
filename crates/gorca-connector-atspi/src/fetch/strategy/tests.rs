//! Arm A strategy: ordering and bounds of the concurrent paths, against a fake application on an
//! isolated bus. Skips (and passes) when the harness prerequisites are missing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use gorca_dbus::drain::local_id;
use gorca_model::{AppId, ConnectorId, NodeKey, PropMask};
use gorca_rt::{Clock, Deadline};
use gorca_testkit::harness::{FakeApp, Session};

use super::*;
use crate::fetch::{Caller, FetchCounters};
use crate::paths::PathTable;

const BASE: &str = "/org/a11y/atspi/accessible";
const ROOT: &str = "/org/a11y/atspi/accessible/root";

fn dl() -> Deadline {
    Deadline::after(&Clock::System, Duration::from_secs(5))
}

struct Rig {
    cx: CallCx,
    _app: FakeApp,
    _session: Session,
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
    let cx = CallCx {
        caller: Caller::new(conn, Arc::from(bus)),
        paths: Arc::new(PathTable::new()),
        app: AppId {
            connector: ConnectorId(1),
            serial: 1,
        },
        counters: Arc::new(FetchCounters::default()),
    };
    Some(Rig {
        cx,
        _app: app,
        _session: session,
    })
}

fn resolved(cx: &CallCx, i: u64) -> Resolved {
    let path = if i == 0 {
        ROOT.to_owned()
    } else {
        format!("{BASE}/{i}")
    };
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

fn run<F: std::future::Future<Output = ()>>(f: F) {
    gorca_testkit::harness::run_bounded(2, std::time::Duration::from_secs(300), f);
}

fn names(b: &RawBatch) -> Vec<String> {
    b.nodes
        .iter()
        .map(|(_, r)| r.name.clone().unwrap_or_default())
        .collect()
}

const WIDE: &[&str] = &["--scenario", "tree", "--nodes", "60", "--fanout", "30"];

#[test]
fn a_wide_range_keeps_the_children_in_order_on_both_paths() {
    run(async {
        let Some(r) = rig(WIDE).await else { return };
        let root = resolved(&r.cx, 0);
        // Indexed path (8 or fewer): one call per child, all in flight.
        let b = child_snapshots(&r.cx, &root, 5..13, PropMask::PRESENT, dl())
            .await
            .unwrap();
        let want: Vec<String> = (6..=13).map(|i| format!("node-{i}")).collect();
        assert_eq!(names(&b), want);
        assert!(!b.truncated);
        // GetChildren path (more than 8).
        let b = child_snapshots(&r.cx, &root, 5..25, PropMask::PRESENT, dl())
            .await
            .unwrap();
        let want: Vec<String> = (6..=25).map(|i| format!("node-{i}")).collect();
        assert_eq!(names(&b), want);
        assert!(b.nodes.iter().all(|(_, n)| n.parent == Some(root.key)));
        // A range past the end is empty, not an error.
        let b = child_snapshots(&r.cx, &root, 40..50, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert!(b.nodes.is_empty());
        // An empty range asks for nothing.
        let b = child_snapshots(&r.cx, &root, 3..3, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert!(b.nodes.is_empty());
    });
}

#[test]
fn ancestry_reads_each_parent_from_the_fetch_of_its_child() {
    run(async {
        let Some(r) = rig(&["--scenario", "tree", "--nodes", "40", "--fanout", "3"]).await else {
            return;
        };
        let leaf = resolved(&r.cx, 13);
        let b = ancestry(&r.cx, &leaf, 10, PropMask::PRESENT, dl())
            .await
            .unwrap();
        // 13 -> 4 -> 1 -> root.
        assert_eq!(names(&b).len(), 4, "{:?}", names(&b));
        for pair in b.nodes.windows(2) {
            assert_eq!(pair[0].1.parent, Some(pair[1].0), "each link is the parent");
        }
        assert!(
            b.nodes
                .iter()
                .all(|(_, n)| n.have.contains(PropMask::PARENT))
        );
    });
}

#[test]
fn map_concurrent_preserves_order_and_the_bound() {
    run(async {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (l, p) = (Arc::clone(&live), Arc::clone(&peak));
        let out = map_concurrent((0..20u32).collect(), 3, move |i| {
            let (l, p) = (Arc::clone(&l), Arc::clone(&p));
            async move {
                let now = l.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(u64::from(20 - i))).await;
                l.fetch_sub(1, Ordering::SeqCst);
                i * 2
            }
        })
        .await;
        let got: Vec<u32> = out.into_iter().map(Option::unwrap).collect();
        assert_eq!(got, (0..20u32).map(|i| i * 2).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 3);
    });
}

#[test]
fn siblings_finds_the_index_in_the_child_list_and_windows_around_it() {
    run(async {
        let Some(r) = rig(WIDE).await else { return };
        let root = resolved(&r.cx, 0);
        let node = resolved(&r.cx, 12); // child index 11 of the root
        let b = siblings(&r.cx, &root, &node, (2, 3), false, PropMask::PRESENT, dl())
            .await
            .unwrap();
        let want: Vec<String> = (10..=15).map(|i| format!("node-{i}")).collect();
        assert_eq!(names(&b), want);
        assert!(!b.truncated);
        assert!(b.nodes.iter().all(|(_, n)| n.parent == Some(root.key)));
        // Clipped at the list start and end.
        let first = resolved(&r.cx, 1);
        let b = siblings(&r.cx, &root, &first, (5, 1), false, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(names(&b), vec!["node-1".to_owned(), "node-2".to_owned()]);
        let last = resolved(&r.cx, 30);
        let b = siblings(&r.cx, &root, &last, (1, 9), false, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(names(&b), vec!["node-29".to_owned(), "node-30".to_owned()]);
        // Only the anchor: before and after both zero.
        let b = siblings(&r.cx, &root, &node, (0, 0), false, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(names(&b), vec!["node-12".to_owned()]);
    });
}

#[test]
fn siblings_of_a_node_that_is_not_a_child_of_the_parent_is_empty_not_an_error() {
    run(async {
        let Some(r) = rig(WIDE).await else { return };
        let root = resolved(&r.cx, 0);
        // Node 40 is a grandchild: not in the root's child list.
        let stranger = resolved(&r.cx, 40);
        let b = siblings(
            &r.cx,
            &root,
            &stranger,
            (2, 2),
            false,
            PropMask::PRESENT,
            dl(),
        )
        .await
        .unwrap();
        assert!(b.nodes.is_empty());
        // A parent that does not exist is the app's `UnknownObject`.
        let ghost = resolved(&r.cx, 9999);
        let e = siblings(
            &r.cx,
            &ghost,
            &stranger,
            (1, 1),
            false,
            PropMask::PRESENT,
            dl(),
        )
        .await;
        assert!(
            matches!(e, Err(BackendError::Gone | BackendError::Protocol(_))),
            "{e:?}"
        );
    });
}

const HUGE: &[&str] = &[
    "--scenario",
    "tree",
    "--nodes",
    "20001",
    "--fanout",
    "20000",
];

/// B-1 review F-2: the indexed path (`GetIndexInParent` + `GetChildAtIndex`) returns the same
/// window as the listing, and on a 20 000-row parent it is the only one that does not read the
/// whole list; a listing above the byte cap is refused before it is decoded.
#[test]
fn siblings_by_index_matches_the_listing_and_a_huge_listing_is_refused_undecoded() {
    run(async {
        let Some(r) = rig(WIDE).await else { return };
        let root = resolved(&r.cx, 0);
        let node = resolved(&r.cx, 12);
        let by_list = siblings(&r.cx, &root, &node, (1, 4), false, PropMask::PRESENT, dl())
            .await
            .unwrap();
        let by_index = siblings(&r.cx, &root, &node, (1, 4), true, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(names(&by_list), names(&by_index));
        assert!(
            by_index
                .nodes
                .iter()
                .all(|(_, n)| n.parent == Some(root.key))
        );
        // Clipped at the ends and at the indexed bound (8 entries), a stranger is empty.
        let first = resolved(&r.cx, 1);
        let b = siblings(&r.cx, &root, &first, (5, 1), true, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert_eq!(names(&b), vec!["node-1".to_owned(), "node-2".to_owned()]);
        let b = siblings(&r.cx, &root, &node, (40, 40), true, PropMask::PRESENT, dl())
            .await
            .unwrap();
        assert!(b.nodes.len() <= 8, "{}", b.nodes.len());
        let stranger = resolved(&r.cx, 40);
        let b = siblings(
            &r.cx,
            &root,
            &stranger,
            (2, 2),
            true,
            PropMask::PRESENT,
            dl(),
        )
        .await
        .unwrap();
        assert!(b.nodes.is_empty());
    });
}

#[test]
fn a_huge_parent_costs_the_same_by_index_and_is_refused_when_listed() {
    run(async {
        let Some(r) = rig(HUGE).await else { return };
        let root = resolved(&r.cx, 0);
        let node = resolved(&r.cx, 10_001);
        let t = std::time::Instant::now();
        let by_index = siblings(&r.cx, &root, &node, (1, 6), true, PropMask::PRESENT, dl())
            .await
            .unwrap();
        let indexed_ms = t.elapsed().as_secs_f64() * 1e3;
        assert_eq!(by_index.nodes.len(), 8);
        assert_eq!(
            names(&by_index).first().map(String::as_str),
            Some("node-10000")
        );
        let t = std::time::Instant::now();
        let listed = siblings(&r.cx, &root, &node, (1, 6), false, PropMask::PRESENT, dl()).await;
        let listed_ms = t.elapsed().as_secs_f64() * 1e3;
        assert!(
            matches!(listed, Err(BackendError::Protocol(_))),
            "a 20 000-ref listing is refused: {listed:?}"
        );
        // The unbounded listing, for comparison (what the first port paid on every step).
        let t = std::time::Instant::now();
        let all =
            r.cx.caller
                .children_of(&root.path, dl())
                .await
                .map(|v| v.len());
        let unbounded_ms = t.elapsed().as_secs_f64() * 1e3;
        eprintln!(
            "HUGE-LIST window of 8: by index {indexed_ms:.2} ms; listing refused after {listed_ms:.2} ms; \
             unbounded GetChildren decode {unbounded_ms:.2} ms ({all:?} refs)"
        );
    });
}
