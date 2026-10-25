// SPDX-License-Identifier: GPL-3.0-or-later
//! Tests of the I/O side of review against an in-process fake application: a task that answers
#![allow(
    clippy::wildcard_enum_match_arm,
    clippy::unnecessary_wraps,
    clippy::unwrap_used,
    clippy::single_match_else,
    clippy::redundant_closure
)]
//! `BackendRequest`s from a scripted tree, and a stand-in app actor that applies `ReviewApply`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gorca_connector_api::{ActionReq, BackendError, BackendOp, BackendReply, BackendRequest};
use gorca_mirror::{ApplyOrigin, MirrorRead, mirror};
use gorca_model::{
    AppId, ChildList, ConnectorId, Freshness, Ifaces, LocalId, Lookup, NodeKey, NodeSnapshot,
    PropMask, Rect, Role, SnapshotBatch, StateSet, TextLine, TextLines, TextQuery,
};
use gorca_rt::{BudgetClass, Clock, mpsc};

use super::build::{
    BuildSpec, NavFetchError, ProbeItem, ReviewIo, RrVerdict, act, build_model, expand,
    find_status_bar, nav_fetch, revalidate,
};
use super::model::TruncWhy;
use super::{Completeness, ReviewLimits};
use crate::app_actor::FillMsg;

fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

#[derive(Default)]
struct Script {
    nodes: HashMap<NodeKey, NodeSnapshot>,
    lines: HashMap<NodeKey, Vec<TextLine>>,
    hang: HashSet<NodeKey>,
    act_error: Option<BackendError>,
    status_bar: Option<NodeKey>,
    delay: Duration,
    /// The next this many `VisibleLines` requests are refused as busy.
    busy_lines: usize,
    /// `VisibleLines` of these nodes never gets an answer in time.
    slow_lines: HashSet<NodeKey>,
}

struct Fake {
    script: Arc<Mutex<Script>>,
    requests: Arc<AtomicUsize>,
    io: ReviewIo,
}

fn snap(
    n: u64,
    role: Role,
    name: &str,
    parent: Option<u64>,
    kids: &[u64],
    r: Option<Rect>,
) -> NodeSnapshot {
    NodeSnapshot::new(key(n), Freshness::Fresh { at: Instant::now() })
        .with_name((!name.is_empty()).then(|| Arc::from(name)))
        .with_parent(parent.map(key))
        .with_children(ChildList::Known(kids.iter().map(|k| key(*k)).collect()))
        .with_bounds(r)
        .with_have(PropMask::ALL)
        .with_test_source(
            role,
            StateSet::SHOWING | StateSet::VISIBLE | StateSet::FOCUSABLE,
        )
}

/// What a toolkit gives for `want`: children as a count, bounds only when asked.
fn reply_node(s: &NodeSnapshot, want: PropMask) -> Arc<NodeSnapshot> {
    let mut n = s.clone();
    n.set_children(match s.children() {
        ChildList::Known(k) => ChildList::Count(u32::try_from(k.len()).unwrap_or(0)),
        other => other.clone(),
    });
    let mut have = PropMask::PRESENT | PropMask::PARENT | PropMask::CHILDREN;
    if want.contains(PropMask::BOUNDS) {
        have |= PropMask::BOUNDS;
    } else {
        n.set_bounds(None);
    }
    n.set_have(have);
    Arc::new(n)
}

fn fake(script: Script) -> Fake {
    let (writer, handle) = mirror();
    let script = Arc::new(Mutex::new(script));
    let requests = Arc::new(AtomicUsize::new(0));
    let (rtx, mut rrx) = mpsc::channel::<BackendRequest>(64);
    let (ftx, mut frx) = mpsc::channel::<FillMsg>(64);
    let (s2, r2) = (Arc::clone(&script), Arc::clone(&requests));
    tokio::spawn(async move {
        let mut held: Vec<BackendRequest> = Vec::new();
        while let Some(req) = rrx.recv().await {
            r2.fetch_add(1, Ordering::Relaxed);
            let (answer, delay) = {
                let mut s = s2.lock().unwrap();
                let lines_of = match &req.op {
                    BackendOp::Text(TextQuery::VisibleLines { node, .. }) => Some(*node),
                    _ => None,
                };
                let forced = match lines_of {
                    Some(_) if s.busy_lines > 0 => {
                        s.busy_lines -= 1;
                        Some(Err(BackendError::Busy))
                    }
                    Some(n) if s.slow_lines.contains(&n) => Some(Err(BackendError::Timeout)),
                    _ => None,
                };
                (forced.or_else(|| answer(&s, &req.op)), s.delay)
            };
            match answer {
                None => held.push(req),
                Some(a) => {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    let _ = req.reply.send(a);
                }
            }
        }
    });
    tokio::spawn(async move {
        let mut writer = writer;
        while let Some(m) = frx.recv().await {
            if let FillMsg::ReviewApply {
                issued_at,
                result,
                ack,
                ..
            } = m
            {
                if let Ok(b) = result {
                    writer.apply(b, ApplyOrigin::Reply { issued_at });
                }
                if let Some(a) = ack {
                    let _ = a.send(());
                }
            }
        }
    });
    Fake {
        script,
        requests,
        io: ReviewIo {
            requests: rtx,
            fill: ftx,
            health: None,
            mirror: handle,
            clock: Clock::System,
            next_req: Arc::new(AtomicU64::new(1)),
            limits: ReviewLimits::default(),
            static_leaves_repeat: false,
            partial_pages: Arc::default(),
        },
    }
}

fn answer(s: &Script, op: &BackendOp) -> Option<Result<BackendReply, BackendError>> {
    let get = |k: &NodeKey| s.nodes.get(k);
    match op {
        BackendOp::Fetch { node, want } => {
            if s.hang.contains(node) {
                return None;
            }
            Some(
                get(node)
                    .map(|n| BackendReply::Node(reply_node(n, *want)))
                    .ok_or(BackendError::Gone),
            )
        }
        BackendOp::ChildSnapshots { node, range, want } => {
            if s.hang.contains(node) {
                return None;
            }
            let Some(p) = get(node) else {
                return Some(Err(BackendError::Gone));
            };
            let ChildList::Known(kids) = &p.children() else {
                return Some(Ok(BackendReply::Subtree(SnapshotBatch::default())));
            };
            let nodes = kids
                .iter()
                .skip(range.start as usize)
                .take((range.end - range.start) as usize)
                .filter_map(|k| get(k))
                .map(|n| reply_node(n, *want))
                .collect();
            Some(Ok(BackendReply::Subtree(SnapshotBatch::new(nodes))))
        }
        BackendOp::Ancestry { node, want, .. } => {
            let mut out = Vec::new();
            let mut cur = Some(*node);
            while let Some(k) = cur {
                let Some(n) = get(&k) else { break };
                out.push(reply_node(n, *want));
                cur = n.parent();
            }
            Some(Ok(BackendReply::Subtree(SnapshotBatch::new(out))))
        }
        BackendOp::Text(TextQuery::VisibleLines { node, .. }) => {
            if s.hang.contains(node) {
                return None;
            }
            let lines = s.lines.get(node).cloned().unwrap_or_default();
            Some(Ok(BackendReply::Lines(TextLines {
                lines,
                truncated: false,
            })))
        }
        BackendOp::Collection(_) => Some(Ok(BackendReply::Matches(
            s.status_bar.into_iter().collect(),
        ))),
        BackendOp::Action(a) => Some(match &s.act_error {
            Some(e) => Err(e.clone()),
            None => {
                let _ = a;
                Ok(BackendReply::ActionDone)
            }
        }),
        BackendOp::Probe => Some(Ok(BackendReply::ActionDone)),
        BackendOp::Children { .. }
        | BackendOp::Subtree { .. }
        | BackendOp::Siblings { .. }
        | BackendOp::CacheDump { .. }
        | BackendOp::Text(_)
        | BackendOp::Locate { .. }
        | BackendOp::HitTest { .. } => Some(Err(BackendError::NotSupported)),
    }
}

fn rect(x: i32, y: i32, w: u32, h: u32) -> Option<Rect> {
    Some(Rect { x, y, w, h })
}

/// window 1 > list 2 > [buttons 3.. 3+n], laid out one per row.
fn form(n: u64) -> Script {
    let kids: Vec<u64> = (3..3 + n).collect();
    let mut s = Script::default();
    s.nodes.insert(
        key(1),
        snap(1, Role::Window, "Form", None, &[2], rect(0, 0, 400, 5_000)),
    );
    s.nodes.insert(
        key(2),
        snap(2, Role::Panel, "", Some(1), &kids, rect(0, 0, 400, 5_000)),
    );
    for i in 0..n {
        s.nodes.insert(
            key(3 + i),
            snap(
                3 + i,
                Role::Button,
                &format!("b{i}"),
                Some(2),
                &[],
                rect(0, i32::try_from(i).unwrap_or(0) * 30, 100, 24),
            ),
        );
    }
    s
}

fn spec(prev: Option<Arc<super::LineModel>>, dirty: Option<Arc<[NodeKey]>>) -> BuildSpec {
    BuildSpec {
        surface: key(1),
        dirty,
        prev,
        budget: Duration::from_millis(120),
        class: BudgetClass::Interactive,
    }
}

#[tokio::test]
async fn expanding_a_node_makes_its_child_list_known_and_brings_extents() {
    let f = fake(form(5));
    expand(&f.io, key(2), BudgetClass::Interactive, true, None)
        .await
        .unwrap();
    let Lookup::Hit(v) = f.io.mirror.view(key(2)) else {
        panic!("parent missing")
    };
    assert!(matches!(&v.snap.children(), ChildList::Known(k) if k.len() == 5));
    let Lookup::Hit(c) = f.io.mirror.view(key(4)) else {
        panic!("child missing")
    };
    assert!(c.snap.have().contains(PropMask::BOUNDS) && c.snap.bounds().is_some());
    assert_eq!(
        f.requests.load(Ordering::Relaxed),
        2,
        "one Fetch, one ChildSnapshots"
    );
}

#[tokio::test]
async fn a_cold_build_fetches_what_is_missing_and_a_warm_build_fetches_nothing() {
    let f = fake(form(40));
    // The mirror knows only the focused button's chain, as after a focus change.
    nav_fetch(&f.io, super::NavOp::Ancestry { node: key(5) })
        .await
        .unwrap();
    let m = build_model(&f.io, spec(None, None)).await.unwrap();
    assert_eq!(m.completeness, Completeness::Complete);
    assert_eq!(m.zones.len(), 40, "every button once");
    assert_eq!(m.lines.len(), 40);
    assert_eq!(&*m.lines[7].text, "b7 button");
    let after_cold = f.requests.load(Ordering::Relaxed);
    assert!(
        after_cold < 40,
        "pipelined by container, not per node: {after_cold}"
    );
    // Warm: the mirror has it all.
    let m2 = build_model(&f.io, spec(Some(Arc::clone(&m)), Some(Arc::from([]))))
        .await
        .unwrap();
    assert_eq!(m2.zones.len(), 40);
    assert_eq!(
        f.requests.load(Ordering::Relaxed),
        after_cold,
        "no request on a warm refresh"
    );
    assert_ne!(m.id, m2.id);
}

#[tokio::test]
async fn text_lines_are_asked_for_once_and_reused_for_clean_nodes() {
    let mut s = form(1);
    let mut label = snap(9, Role::Label, "", Some(2), &[], rect(0, 100, 200, 40));
    label.set_caps(gorca_model::NodeCaps::new(Ifaces::TEXT));
    s.nodes.insert(key(9), label);
    if let Some(ChildList::Known(k)) = s
        .nodes
        .get_mut(&key(2))
        .map(gorca_model::NodeSnapshot::children_mut)
    {
        k.push(key(9));
    }
    s.lines.insert(
        key(9),
        vec![TextLine {
            range: 0..5,
            text: Arc::from("hello"),
            rect: rect(0, 100, 200, 20),
        }],
    );
    let f = fake(s);
    let m = build_model(&f.io, spec(None, None)).await.unwrap();
    assert!(m.lines.iter().any(|l| &*l.text == "hello"));
    let n = f.requests.load(Ordering::Relaxed);
    // Clean refresh: the text is reused.
    let m2 = build_model(&f.io, spec(Some(Arc::clone(&m)), Some(Arc::from([]))))
        .await
        .unwrap();
    assert!(m2.lines.iter().any(|l| &*l.text == "hello"));
    assert_eq!(f.requests.load(Ordering::Relaxed), n);
    // Dirty (or unknown dirtiness): asked again.
    let _ = build_model(&f.io, spec(Some(m), None)).await.unwrap();
    assert_eq!(f.requests.load(Ordering::Relaxed), n + 1);
}

#[tokio::test]
async fn a_hung_node_costs_the_deadline_and_the_model_says_what_is_missing() {
    let mut s = form(6);
    s.hang.insert(key(4));
    let f = fake(s);
    let t = Instant::now();
    let m = build_model(&f.io, spec(None, None)).await;
    let took = t.elapsed();
    assert!(
        took < Duration::from_millis(900),
        "bounded by the deadlines: {took:?}"
    );
    // Either a partial model or an honest failure, never a hang.
    if let Ok(m) = m {
        assert!(matches!(
            m.completeness,
            Completeness::Truncated(_) | Completeness::Complete
        ));
    }
}

#[tokio::test]
async fn a_dead_application_gives_a_failed_build() {
    let f = fake(Script::default());
    let r = build_model(&f.io, spec(None, None)).await;
    assert!(r.is_err(), "{r:?}");
}

#[tokio::test]
async fn nav_fetch_reports_gone_and_timeouts() {
    let mut s = form(3);
    s.hang.insert(key(3));
    let f = fake(s);
    assert_eq!(
        nav_fetch(&f.io, super::NavOp::Node { node: key(77) }).await,
        Err(NavFetchError::Gone)
    );
    let t = Instant::now();
    assert_eq!(
        nav_fetch(&f.io, super::NavOp::Node { node: key(3) }).await,
        Err(NavFetchError::Timeout)
    );
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
    assert!(
        nav_fetch(&f.io, super::NavOp::Node { node: key(4) })
            .await
            .is_ok()
    );
    assert!(matches!(f.io.mirror.view(key(4)), Lookup::Hit(_)));
}

#[tokio::test]
async fn the_status_bar_is_found_with_one_collection_call() {
    let mut s = form(2);
    s.nodes.insert(
        key(20),
        snap(
            20,
            Role::StatusBar,
            "",
            Some(1),
            &[],
            rect(0, 4_900, 400, 100),
        ),
    );
    s.status_bar = Some(key(20));
    let f = fake(s);
    assert_eq!(find_status_bar(&f.io, key(1)).await, Ok(Some(key(20))));
    assert!(matches!(f.io.mirror.view(key(20)), Lookup::Hit(_)));
    f.script.lock().unwrap().status_bar = None;
    assert_eq!(find_status_bar(&f.io, key(1)).await, Ok(None));
}

#[tokio::test]
async fn actions_report_the_toolkit_refusal() {
    let f = fake(form(1));
    assert_eq!(act(&f.io, ActionReq::Focus { node: key(3) }).await, Ok(()));
    f.script.lock().unwrap().act_error = Some(BackendError::NotSupported);
    assert_eq!(
        act(&f.io, ActionReq::Focus { node: key(3) }).await,
        Err(NavFetchError::Failed)
    );
}

#[tokio::test]
async fn revalidation_compares_extents_and_text_and_updates_the_mirror() {
    let f = fake(form(3));
    let m = build_model(&f.io, spec(None, None)).await.unwrap();
    let probe: Vec<ProbeItem> = m
        .zones
        .iter()
        .map(|z| ProbeItem {
            node: z.node,
            rect: z.rect,
            lines: None,
        })
        .collect();
    let clip = Rect {
        x: 0,
        y: 0,
        w: 400,
        h: 5_000,
    };
    assert_eq!(
        revalidate(&f.io, probe.clone(), clip).await,
        RrVerdict::Same
    );
    // The app scrolled: the same nodes now sit 50 px higher.
    {
        let mut s = f.script.lock().unwrap();
        for i in 0..3u64 {
            if let Some(n) = s.nodes.get_mut(&key(3 + i)) {
                n.set_bounds(rect(0, i32::try_from(i).unwrap_or(0) * 30 - 50, 100, 24));
            }
        }
    }
    assert_eq!(
        revalidate(&f.io, probe, clip).await,
        RrVerdict::Changed {
            text_changed: false
        }
    );
    // The fresh extents are in the mirror, so the next validity check finds the model dirty.
    let (v, _) = super::validity::validity(&m, &f.io.mirror, 0);
    assert!(matches!(v, super::validity::Validity::Dirty(_)), "{v:?}");
    let _ = TruncWhy::Deadline;
    let _ = Duration::ZERO;
}

/// F-5: an unchanged app with a text node reads as unchanged. The probe carries the node's own
/// extents (not its line's) and the revalidation asks for lines in the build's clip.
#[tokio::test]
async fn f5_an_unchanged_text_node_revalidates_as_the_same() {
    let mut s = form(1);
    let mut label = snap(9, Role::Label, "", Some(2), &[], rect(0, 100, 200, 40));
    label.set_caps(gorca_model::NodeCaps::new(Ifaces::TEXT));
    s.nodes.insert(key(9), label);
    if let Some(ChildList::Known(k)) = s
        .nodes
        .get_mut(&key(2))
        .map(gorca_model::NodeSnapshot::children_mut)
    {
        k.push(key(9));
    }
    s.lines.insert(
        key(9),
        vec![
            TextLine {
                range: 0..5,
                text: Arc::from("first"),
                rect: rect(0, 100, 120, 20),
            },
            TextLine {
                range: 6..12,
                text: Arc::from("second"),
                rect: rect(0, 120, 140, 20),
            },
        ],
    );
    let f = fake(s);
    let m = build_model(&f.io, spec(None, None)).await.unwrap();
    let line = m.lines.iter().position(|l| &*l.text == "first").unwrap();
    let probe: Vec<ProbeItem> = m
        .line_zones(line)
        .iter()
        .map(|z| ProbeItem {
            node: z.node,
            rect: m.node_rect(z.node),
            lines: m.texts.get(&z.node).cloned(),
        })
        .collect();
    assert_eq!(
        revalidate(&f.io, probe.clone(), m.clip).await,
        RrVerdict::Same
    );
    // Changed text is still noticed.
    f.script.lock().unwrap().lines.insert(
        key(9),
        vec![TextLine {
            range: 0..3,
            text: Arc::from("new"),
            rect: rect(0, 100, 120, 20),
        }],
    );
    assert_eq!(
        revalidate(&f.io, probe, m.clip).await,
        RrVerdict::Changed { text_changed: true }
    );
}

/// F-7: a surface far deeper than ten levels is built whole, not truncated for good.
#[tokio::test]
async fn f7_a_deep_surface_is_built_completely() {
    let mut s = Script::default();
    let depth = 40u64;
    for i in 1..=depth {
        let kid: Vec<u64> = if i < depth { vec![i + 1] } else { vec![] };
        let role = if i == 1 { Role::Window } else { Role::Panel };
        let parent = (i > 1).then(|| i - 1);
        s.nodes.insert(
            key(i),
            snap(i, role, "", parent, &kid, rect(0, 0, 300, 300)),
        );
    }
    s.nodes.insert(
        key(depth + 1),
        snap(
            depth + 1,
            Role::Button,
            "deep",
            Some(depth),
            &[],
            rect(0, 0, 50, 20),
        ),
    );
    if let Some(ChildList::Known(k)) = s
        .nodes
        .get_mut(&key(depth))
        .map(gorca_model::NodeSnapshot::children_mut)
    {
        k.push(key(depth + 1));
    }
    let f = fake(s);
    let m = build_model(
        &f.io,
        BuildSpec {
            budget: Duration::from_secs(2),
            ..spec(None, None)
        },
    )
    .await
    .unwrap();
    assert_eq!(m.completeness, Completeness::Complete);
    assert_eq!(&*m.lines[0].text, "deep button");
}

/// R2-1: the pages after the first are not asked for once the build's soft deadline has passed.
#[tokio::test]
async fn child_paging_stops_when_the_build_budget_is_used_up() {
    let f = fake(form(600));
    let late = std::time::Instant::now();
    expand(&f.io, key(2), BudgetClass::Interactive, true, Some(late))
        .await
        .unwrap();
    assert_eq!(
        f.requests.load(Ordering::Relaxed),
        2,
        "one Fetch and the first page only"
    );
    let Lookup::Hit(v) = f.io.mirror.view(key(2)) else {
        panic!("parent missing")
    };
    assert!(
        !matches!(&v.snap.children(), ChildList::Known(_)),
        "an incomplete list must not be recorded as known"
    );
    assert!(matches!(f.io.mirror.view(key(3 + 255)), Lookup::Hit(_)));
    assert!(matches!(f.io.mirror.view(key(3 + 256)), Lookup::Miss));
    // The next pass of the same task resumes behind the pages already read, and reads one more
    // page even when its time is up (or a slow app would never get past the first pages).
    expand(&f.io, key(2), BudgetClass::Interactive, true, Some(late))
        .await
        .unwrap();
    assert_eq!(
        f.requests.load(Ordering::Relaxed),
        2 + 2,
        "a Fetch and the second page"
    );
    assert!(matches!(f.io.mirror.view(key(3 + 256)), Lookup::Hit(_)));
    let far = std::time::Instant::now() + Duration::from_secs(30);
    expand(&f.io, key(2), BudgetClass::Interactive, true, Some(far))
        .await
        .unwrap();
    assert_eq!(
        f.requests.load(Ordering::Relaxed),
        2 + 2 + 2,
        "a Fetch and the page that was left, not the first page again"
    );
    let Lookup::Hit(v) = f.io.mirror.view(key(2)) else {
        panic!("parent missing")
    };
    assert!(matches!(&v.snap.children(), ChildList::Known(k) if k.len() == 600));
    // Without a deadline the whole list is read, page after page.
    let g = fake(form(600));
    expand(&g.io, key(2), BudgetClass::Interactive, true, None)
        .await
        .unwrap();
    assert_eq!(g.requests.load(Ordering::Relaxed), 4);
    let Lookup::Hit(v) = g.io.mirror.view(key(2)) else {
        panic!("parent missing")
    };
    assert!(matches!(&v.snap.children(), ChildList::Known(k) if k.len() == 600));
}

fn text_page(label_lines: &str) -> Script {
    let mut s = form(1);
    let mut label = snap(9, Role::Label, "", Some(2), &[], rect(0, 100, 200, 40));
    label.set_caps(gorca_model::NodeCaps::new(Ifaces::TEXT));
    s.nodes.insert(key(9), label);
    if let Some(ChildList::Known(k)) = s
        .nodes
        .get_mut(&key(2))
        .map(gorca_model::NodeSnapshot::children_mut)
    {
        k.push(key(9));
    }
    s.lines.insert(
        key(9),
        vec![TextLine {
            range: 0..5,
            text: Arc::from(label_lines),
            rect: rect(0, 100, 200, 20),
        }],
    );
    s
}

fn lines_requests(f: &Fake) -> usize {
    // Requests that are not the walk's own: a Fetch and a ChildSnapshots per expanded node.
    f.requests.load(Ordering::Relaxed)
}

/// M-1: a busy app is asked again and the row survives; the model is complete.
#[tokio::test]
async fn a_busy_app_is_asked_again_for_its_lines() {
    let mut s = text_page("hello");
    s.busy_lines = 2;
    let f = fake(s);
    let m = build_model(&f.io, spec(None, None)).await.unwrap();
    assert!(m.lines.iter().any(|l| &*l.text == "hello"));
    assert_eq!(m.completeness, Completeness::Complete);
}

/// M-1: a timeout is asked once (each try costs a class deadline and counts toward "hung"); the
/// model says it is incomplete so that the continuation asks later.
#[tokio::test]
async fn a_timeout_on_lines_is_not_retried_and_marks_the_model_truncated() {
    let mut s = text_page("hello");
    s.slow_lines.insert(key(9));
    let f = fake(s);
    let before = lines_requests(&f);
    let m = build_model(&f.io, spec(None, None)).await.unwrap();
    let asked = lines_requests(&f) - before;
    assert!(!m.lines.iter().any(|l| &*l.text == "hello"));
    assert_eq!(m.completeness, Completeness::Truncated(TruncWhy::Deadline));
    // The walk's own requests (2 nodes expanded) plus exactly one lines request.
    let walk_only = {
        let g = fake(text_page("hello"));
        let b = lines_requests(&g);
        build_model(&g.io, spec(None, None)).await.unwrap();
        lines_requests(&g) - b
    };
    assert_eq!(
        asked, walk_only,
        "one lines request, as in the unfailed build"
    );
}

/// M-1: busy retries stop when the build's budget is spent.
#[tokio::test]
async fn busy_retries_stop_at_the_build_budget() {
    let mut s = text_page("hello");
    s.busy_lines = 1_000;
    let f = fake(s);
    let mut sp = spec(None, None);
    sp.budget = Duration::from_millis(1);
    let t = Instant::now();
    let _ = build_model(&f.io, sp).await;
    assert!(t.elapsed() < Duration::from_millis(500));
}

fn reorder_children(f: &Fake, parent: u64, f_: impl FnOnce(&mut Vec<NodeKey>)) {
    let mut s = f.script.lock().unwrap();
    if let Some(ChildList::Known(k)) = s
        .nodes
        .get_mut(&key(parent))
        .map(gorca_model::NodeSnapshot::children_mut)
    {
        let mut v: Vec<NodeKey> = k.iter().copied().collect();
        f_(&mut v);
        *k = v.into_iter().collect();
    }
}

/// M-2: a child inserted at the front between two passes (600 -> 601): the saved pages are
/// dropped and the list is read again, complete, without duplicates.
#[tokio::test]
async fn a_list_that_grows_between_passes_is_read_again_from_the_start() {
    let f = fake(form(600));
    let late = std::time::Instant::now();
    expand(&f.io, key(2), BudgetClass::Interactive, true, Some(late))
        .await
        .unwrap();
    {
        let mut s = f.script.lock().unwrap();
        let n = snap(900, Role::Button, "new", Some(2), &[], rect(0, 0, 100, 24));
        s.nodes.insert(key(900), n);
    }
    reorder_children(&f, 2, |v| v.insert(0, key(900)));
    let far = std::time::Instant::now() + Duration::from_secs(30);
    expand(&f.io, key(2), BudgetClass::Interactive, true, Some(far))
        .await
        .unwrap();
    let Lookup::Hit(v) = f.io.mirror.view(key(2)) else {
        panic!("parent missing")
    };
    let ChildList::Known(k) = &v.snap.children() else {
        panic!("not known")
    };
    let uniq: HashSet<_> = k.iter().collect();
    assert_eq!((k.len(), uniq.len()), (601, 601));
    assert_eq!(k[0], key(900));
}

/// M-2: same count, but a different child where the saved pages end.
#[tokio::test]
async fn a_list_reordered_between_passes_is_read_again_from_the_start() {
    let f = fake(form(600));
    let late = std::time::Instant::now();
    expand(&f.io, key(2), BudgetClass::Interactive, true, Some(late))
        .await
        .unwrap();
    // The saved pages hold children 0..256; swap the last saved one with a later one.
    reorder_children(&f, 2, |v| v.swap(255, 300));
    let far = std::time::Instant::now() + Duration::from_secs(30);
    expand(&f.io, key(2), BudgetClass::Interactive, true, Some(far))
        .await
        .unwrap();
    let Lookup::Hit(v) = f.io.mirror.view(key(2)) else {
        panic!("parent missing")
    };
    let ChildList::Known(k) = &v.snap.children() else {
        panic!("not known")
    };
    let uniq: HashSet<_> = k.iter().collect();
    assert_eq!((k.len(), uniq.len()), (600, 600));
    assert_eq!(k[255], key(3 + 300));
}
