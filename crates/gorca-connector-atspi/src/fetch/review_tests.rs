// SPDX-License-Identifier: GPL-3.0-or-later
//! The review operations (bounds, visible lines, actions, collection) against the fake
//! application on an isolated bus. Skips (and passes) when the harness prerequisites are missing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use gorca_connector_api::{ActionReq, BackendError, BackendReply, MatchRule};
use gorca_dbus::drain::local_id;
use gorca_model::{AppId, ConnectorId, NodeKey, PropMask, Rect, Role, StateSet, TextQuery};
use gorca_rt::{Clock, Deadline};
use gorca_testkit::harness::{FakeApp, Session};

use super::{CallCx, Caller, FetchCounters, Resolved, review, strategy};
use crate::paths::PathTable;

const BASE: &str = "/org/a11y/atspi/accessible";

/// The test tree, pre-order ids: 1 frame, 2 label, 3 entry, 4 notes, 5 status bar, 6 its label,
/// 7 button.
const TREE: &str = r#""cmd":"build","coord":"window","tree":{"name":"Win","role":"frame","extents":[0,0,400,300],"children":[
{"name":"Name:","role":"label","extents":[10,10,60,20]},
{"name":"name","role":"entry","editable":true,"focusable":true,"text":"bob","extents":[80,10,100,20]},
{"name":"notes","role":"text","text":"alpha\nbeta\ngamma","extents":[10,50,200,48]},
{"name":"status","role":"status bar","extents":[0,280,400,20],"children":[
  {"name":"Ready","role":"label","extents":[4,282,50,16]}]},
{"name":"OK","role":"push button","focusable":true,"actions":["click"],"extents":[10,200,40,20]},
{"name":"bogus","role":"label","extents":[0,0,0,0]},
{"name":"stuck","role":"label","hang":true,"extents":[1,1,5,5]}]}"#;

struct Rig {
    cx: CallCx,
    app: FakeApp,
    _session: Session,
}

fn dl() -> Deadline {
    Deadline::after(&Clock::System, Duration::from_secs(5))
}

fn short() -> Deadline {
    Deadline::after(&Clock::System, Duration::from_millis(400))
}

fn path(i: u64) -> String {
    format!("{BASE}/{i}")
}

async fn rig(coord: &str) -> Option<Rig> {
    if let Some(why) = Session::unavailable_reason() {
        eprintln!("SKIPPED (harness unavailable): {why}");
        return None;
    }
    let session = Session::start().expect("isolated session");
    let mut app = FakeApp::start(&session, &["--scenario", "review"]).expect("fake app");
    let tree = TREE.replace("\"coord\":\"window\"", &format!("\"coord\":\"{coord}\""));
    app.cmd(&tree.replace('\n', "")).expect("build");
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
        app,
        _session: session,
    })
}

fn resolved(cx: &CallCx, i: u64) -> Resolved {
    let p = path(i);
    let key = NodeKey {
        app: cx.app,
        local: local_id(&p),
    };
    cx.paths.insert(key, &p);
    Resolved {
        key,
        path: Arc::from(p),
    }
}

fn run<F: std::future::Future<Output = ()>>(f: F) {
    gorca_testkit::harness::run_bounded(2, std::time::Duration::from_secs(300), f);
}

async fn fetched(r: &Rig, i: u64) -> Arc<gorca_model::NodeSnapshot> {
    let n = resolved(&r.cx, i);
    let raw = strategy::fetch(&r.cx, &n, PropMask::PRESENT | PropMask::PARENT, dl())
        .await
        .unwrap();
    crate::decode::snapshot(n.key, &raw, std::time::Instant::now())
}

#[test]
fn bounds_are_window_relative_in_both_toolkit_flavours() {
    for coord in ["window", "global"] {
        run(async {
            let Some(r) = rig(coord).await else { return };
            let mut snaps = [fetched(&r, 2).await, fetched(&r, 3).await];
            assert!(snaps[0].bounds.is_none() && !snaps[0].have.contains(PropMask::BOUNDS));
            review::add_bounds(&r.cx, &mut snaps, dl()).await;
            assert_eq!(
                snaps[0].bounds,
                Some(Rect {
                    x: 10,
                    y: 10,
                    w: 60,
                    h: 20
                }),
                "{coord}"
            );
            assert_eq!(snaps[1].bounds.map(|b| b.x), Some(80));
            assert!(snaps[0].have.contains(PropMask::BOUNDS | PropMask::PRESENT));
        });
    }
}

#[test]
fn a_bogus_rectangle_is_none_but_the_node_is_marked_answered() {
    run(async {
        let Some(r) = rig("window").await else { return };
        let mut snaps = [fetched(&r, 8).await];
        review::add_bounds(&r.cx, &mut snaps, dl()).await;
        assert!(snaps[0].bounds.is_none());
        assert!(snaps[0].have.contains(PropMask::BOUNDS), "do not ask again");
    });
}

#[test]
fn a_hung_node_costs_only_its_own_bounds() {
    run(async {
        let Some(r) = rig("window").await else { return };
        let mut snaps = [fetched(&r, 2).await, {
            // The hung node is fetched by hand: its own role/state calls would hang too.
            let n = resolved(&r.cx, 9);
            let mut s = (*fetched(&r, 2).await).clone();
            s.key = n.key;
            Arc::new(s)
        }];
        let t0 = std::time::Instant::now();
        review::add_bounds(&r.cx, &mut snaps, short()).await;
        assert!(
            t0.elapsed() < Duration::from_secs(2),
            "bounded by the deadline"
        );
        assert!(snaps[0].bounds.is_some());
        assert!(snaps[1].bounds.is_none() && !snaps[1].have.contains(PropMask::BOUNDS));
    });
}

fn lines_of(reply: BackendReply) -> gorca_model::TextLines {
    match reply {
        BackendReply::Lines(l) => l,
        other => panic!("not lines: {other:?}"),
    }
}

fn visible(node: NodeKey, clip: Rect) -> TextQuery {
    TextQuery::VisibleLines {
        node,
        clip,
        max_lines: 50,
        max_bytes: 4096,
    }
}

#[test]
fn visible_lines_follow_the_text_and_the_clip() {
    run(async {
        let Some(mut r) = rig("window").await else {
            return;
        };
        let notes = resolved(&r.cx, 4);
        let all = Rect {
            x: 0,
            y: 0,
            w: 400,
            h: 300,
        };
        let l = lines_of(
            review::text(&r.cx, &visible(notes.key, all), dl())
                .await
                .unwrap(),
        );
        let texts: Vec<&str> = l.lines.iter().map(|x| &*x.text).collect();
        assert_eq!(texts, ["alpha", "beta", "gamma"]);
        assert_eq!(l.lines[0].range, 0..6);
        assert_eq!(l.lines[2].range, 11..16);
        assert_eq!(l.lines[1].rect.map(|x| x.y), Some(66));
        assert!(!l.truncated);
        // A clip that cuts the third line off.
        let clip = Rect {
            x: 0,
            y: 60,
            w: 400,
            h: 20,
        };
        let l = lines_of(
            review::text(&r.cx, &visible(notes.key, clip), dl())
                .await
                .unwrap(),
        );
        let texts: Vec<&str> = l.lines.iter().map(|x| &*x.text).collect();
        assert_eq!(texts, ["alpha", "beta"]);
        // Per-member counts are readable for zero-IPC assertions.
        let calls = r.app.cmd("\"cmd\":\"calls\",\"reset\":true").unwrap();
        assert!(calls.contains("Text.GetStringAtOffset"), "{calls}");
    });
}

#[test]
fn an_editable_answers_one_line_at_its_own_extents() {
    run(async {
        let Some(r) = rig("window").await else { return };
        let name = resolved(&r.cx, 3);
        let l = lines_of(
            review::text(
                &r.cx,
                &visible(
                    name.key,
                    Rect {
                        x: 0,
                        y: 0,
                        w: 400,
                        h: 300,
                    },
                ),
                dl(),
            )
            .await
            .unwrap(),
        );
        assert_eq!(l.lines.len(), 1);
        assert_eq!(&*l.lines[0].text, "bob");
        assert_eq!(
            l.lines[0].rect.map(|x| (x.x, x.y, x.w, x.h)),
            Some((80, 10, 100, 20))
        );
    });
}

#[test]
fn get_text_is_clamped_and_the_caret_is_read() {
    run(async {
        let Some(r) = rig("window").await else { return };
        let notes = resolved(&r.cx, 4);
        let t = review::text(
            &r.cx,
            &TextQuery::GetText {
                node: notes.key,
                range: 6..10_000,
            },
            dl(),
        )
        .await
        .unwrap();
        assert!(matches!(t, BackendReply::Text(s) if &*s == "beta\ngamma"));
        let c = review::text(&r.cx, &TextQuery::CaretOffset { node: notes.key }, dl())
            .await
            .unwrap();
        assert!(matches!(c, BackendReply::CaretOffset(0)));
        let a = review::text(
            &r.cx,
            &TextQuery::Attributes {
                node: notes.key,
                range: 0..1,
            },
            dl(),
        )
        .await;
        assert!(matches!(a, Err(BackendError::NotSupported)));
    });
}

#[test]
fn the_status_bar_is_found_by_role_and_showing() {
    run(async {
        let Some(r) = rig("window").await else { return };
        let frame = resolved(&r.cx, 1);
        let rule = MatchRule {
            root: frame.key,
            roles: vec![Role::StatusBar],
            states_all: StateSet::SHOWING | StateSet::VISIBLE,
            states_none: StateSet::empty(),
            max_results: 1,
        };
        let m = review::collection(&r.cx, &rule, dl()).await.unwrap();
        let BackendReply::Matches(keys) = m else {
            panic!("not matches")
        };
        assert_eq!(
            keys,
            vec![NodeKey {
                app: r.cx.app,
                local: local_id(&path(5))
            }]
        );
        // No such role: empty, not an error.
        let none = MatchRule {
            roles: vec![Role::Terminal],
            ..rule
        };
        let BackendReply::Matches(keys) = review::collection(&r.cx, &none, dl()).await.unwrap()
        else {
            panic!("not matches")
        };
        assert!(keys.is_empty());
    });
}

#[test]
fn actions_report_success_or_not_supported() {
    run(async {
        let Some(mut r) = rig("window").await else {
            return;
        };
        let ok = resolved(&r.cx, 7);
        let label = resolved(&r.cx, 2);
        let done = review::action(&r.cx, &ActionReq::DoDefault { node: ok.key }, dl()).await;
        assert!(matches!(done, Ok(BackendReply::ActionDone)), "{done:?}");
        let none = review::action(&r.cx, &ActionReq::DoDefault { node: label.key }, dl()).await;
        assert!(matches!(none, Err(BackendError::NotSupported)), "{none:?}");
        let f = review::action(&r.cx, &ActionReq::Focus { node: ok.key }, dl()).await;
        assert!(matches!(f, Ok(BackendReply::ActionDone)), "{f:?}");
        let f = review::action(&r.cx, &ActionReq::Focus { node: label.key }, dl()).await;
        assert!(matches!(f, Err(BackendError::NotSupported)), "{f:?}");
        let s = review::action(&r.cx, &ActionReq::Select { node: label.key }, dl()).await;
        assert!(matches!(s, Ok(BackendReply::ActionDone)), "{s:?}");
        let s = review::action(&r.cx, &ActionReq::ScrollTo { node: label.key }, dl()).await;
        assert!(matches!(s, Err(BackendError::NotSupported)));
        let calls = r.app.cmd("\"cmd\":\"calls\"").unwrap();
        for m in [
            "Action.DoAction",
            "Component.GrabFocus",
            "Selection.SelectChild",
        ] {
            assert!(calls.contains(m), "{m} in {calls}");
        }
    });
}
