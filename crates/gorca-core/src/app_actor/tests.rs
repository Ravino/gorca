#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::reader::reader_channels;
use gorca_connector_api::app_channel;
use gorca_ingress::IngressConfig;
use gorca_mirror::{MirrorRead, mirror};
use gorca_model::{
    ChildList, ConnectorId, Freshness, Ifaces, LocalId, Lookup, NodeSnapshot, Politeness, PropMask,
    Role, SnapshotBatch, StateSet,
};
use std::sync::Arc;

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

struct Rig {
    actor: AppActor,
    port: gorca_connector_api::AppPort,
    read: gorca_mirror::MirrorHandle,
    inbox: crate::reader::ReaderInbox,
}

fn rig(cfg: IngressConfig) -> Rig {
    let ch = app_channel(app(), cfg);
    let (writer, read) = mirror();
    let (tx, inbox) = reader_channels();
    let links = ReaderLinks {
        focus: tx.focus,
        background: tx.background,
    };
    Rig {
        actor: AppActor::new(app(), writer, ch.ingress, ch.control, links),
        port: ch.port,
        read,
        inbox,
    }
}

fn snap(n: u64) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: key(n),
        role: Role::Button,
        states: StateSet::empty(),
        name: None,
        description: None,
        value: None,
        parent: None,
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: gorca_model::AttrMap::default(),
        relations: gorca_model::Relations::default(),
        bounds: None,
        have: gorca_model::PropMask::PRESENT,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}

#[test]
fn focus_goes_to_the_focus_lane_and_snapshots_fill_the_mirror() {
    let mut r = rig(IngressConfig::default());
    r.port
        .push_snapshot(SnapshotBatch {
            nodes: vec![snap(1)],
            ..Default::default()
        })
        .unwrap();
    r.port.push(NodeEvent::FocusChanged { node: key(1) });
    r.port.push(NodeEvent::FocusChanged { node: key(2) });
    let route = r.actor.pump(Instant::now());
    assert_eq!(route.events, 1, "focus events are coalesced");
    assert_eq!(route.forwarded_to_focus_lane, 1);
    assert!(r.read.get(key(1)).hit().is_some());
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node })) if node == key(2)
    ));
}

#[test]
fn removal_tombstones_the_mirror_and_tells_the_reader() {
    let mut r = rig(IngressConfig::default());
    r.port
        .push_snapshot(SnapshotBatch {
            nodes: vec![snap(1)],
            ..Default::default()
        })
        .unwrap();
    r.actor.pump(Instant::now());
    r.port.push(NodeEvent::NodeRemoved { node: key(1) });
    r.actor.pump(Instant::now());
    assert_eq!(r.read.get(key(1)), Lookup::Gone);
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::NodeRemoved(
            _
        )))
    ));
}

#[test]
fn property_noise_invalidates_the_mirror_and_wakes_nobody() {
    let mut r = rig(IngressConfig::default());
    r.port
        .push_snapshot(SnapshotBatch {
            nodes: vec![snap(1)],
            ..Default::default()
        })
        .unwrap();
    r.actor.pump(Instant::now());
    r.port.push(NodeEvent::PropertiesChanged {
        node: key(1),
        mask: PropMask::NAME,
    });
    let route = r.actor.pump(Instant::now());
    assert_eq!(
        route.forwarded_to_focus_lane + route.forwarded_to_background,
        0
    );
    assert!(!r.actor.resync_needed(), "the table invalidated precisely");
    let v = r.read.view(key(1)).hit().unwrap();
    assert!(v.stale.contains(PropMask::NAME));
    assert!(r.inbox.poll().is_none());
}

#[test]
fn a_flood_ends_with_a_lazy_resync_and_a_healthy_app() {
    let mut r = rig(IngressConfig {
        max_keys: 8,
        ..IngressConfig::default()
    });
    for n in 0..100 {
        r.port.push(NodeEvent::ChildrenChanged { node: key(n) });
    }
    let route = r.actor.pump(Instant::now());
    let dirty = route.resync.expect("flooded ingress yields a resync set");
    assert!(dirty.contains(key(99)));
    assert_eq!(*r.actor.health(), AppHealth::Healthy);
}

#[test]
fn a_full_background_lane_drops_and_requests_a_resync() {
    let mut r = rig(IngressConfig::default());
    for i in 0..70 {
        r.port.push(NodeEvent::Announcement {
            node: None,
            text: Arc::from(format!("a{i}")),
            politeness: Politeness::Polite,
        });
    }
    let route = r.actor.pump(Instant::now());
    assert_eq!(
        route.forwarded_to_background, 16,
        "ingress caps announcements per drain"
    );
    assert_eq!(route.dropped_background, 0);
    // Fill the lane to its bound, then overflow it.
    for round in 0..6 {
        for i in 0..16 {
            r.port.push(NodeEvent::Announcement {
                node: None,
                text: Arc::from(format!("r{round}-{i}")),
                politeness: Politeness::Polite,
            });
        }
        r.actor.pump(Instant::now());
    }
    assert!(r.actor.resync_needed());
}

#[test]
fn owner_gone_marks_the_app_gone_and_notifies_the_reader() {
    let mut r = rig(IngressConfig::default());
    r.port.lifecycle(AppLifecycle::Gone).unwrap();
    let route = r.actor.pump(Instant::now());
    assert!(route.gone);
    assert_eq!(*r.actor.health(), AppHealth::Gone);
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::AppGone(_)))
    ));
}

#[test]
fn hung_transition_tells_the_reader_once() {
    let mut r = rig(IngressConfig::default());
    let now = Instant::now();
    let t = || HealthMsg::Timeout {
        class: gorca_rt::BudgetClass::Interactive,
    };
    r.actor.on_health(t(), now);
    r.actor.on_health(t(), now);
    r.actor.on_health(t(), now);
    assert!(matches!(r.actor.health(), AppHealth::Hung { .. }));
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::AppHung(_)))
    ));
    assert!(r.inbox.poll().is_none());
}

fn shell_info() -> gorca_model::AppInfo {
    gorca_model::AppInfo {
        toolkit: Arc::from("clutter"),
        app_name: Arc::from("gnome-shell"),
        pid: None,
        connector_kind: gorca_model::ConnectorKind::Atspi,
        verified_shell: true,
        dump_complete: false,
        text_unit: gorca_model::TextUnit::Scalar,
        text_insert_slack: 0,
        page_events_settle_ms: 0,
        web_engine: Default::default(),
        root: None,
    }
}

fn selected(n: u64) -> NodeEvent {
    NodeEvent::PropertiesChanged {
        node: key(n),
        mask: PropMask::STATES | PropMask::SELECTED_NOW,
    }
}

#[test]
fn the_shell_forwards_only_the_newest_selection_of_a_batch() {
    let mut r = rig(IngressConfig::default());
    r.port
        .lifecycle(AppLifecycle::Appeared(shell_info()))
        .unwrap();
    r.port.push(selected(1));
    r.port.push(selected(2));
    r.port.push(selected(3));
    let route = r.actor.pump(Instant::now());
    assert!(r.actor.profile().speak_selection);
    assert_eq!(route.forwarded_to_focus_lane, 1);
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::ItemSelected { node })) if node == key(3)
    ));
    assert!(r.inbox.poll().is_none());
}

#[test]
fn a_spoofed_shell_never_speaks_selection() {
    let mut r = rig(IngressConfig::default());
    let mut info = shell_info();
    info.verified_shell = false;
    r.port.lifecycle(AppLifecycle::Appeared(info)).unwrap();
    r.port.push(selected(1));
    let route = r.actor.pump(Instant::now());
    assert!(!r.actor.profile().speak_selection);
    assert_eq!(route.forwarded_to_focus_lane, 0);
    assert!(r.inbox.poll().is_none());
}

#[test]
fn other_apps_never_speak_selection_but_a_selected_focus_is_an_s1_change() {
    let mut r = rig(IngressConfig::default());
    r.port.push(selected(1));
    let route = r.actor.pump(Instant::now());
    assert_eq!(route.forwarded_to_focus_lane, 0, "not the focus: nothing");
    assert!(r.inbox.poll().is_none());
    // The focused node of an ordinary app that becomes `selected` is a state change like any
    // other (IC-3 against the real daemon: it used to be dropped as selection speech).
    r.port.push(NodeEvent::FocusChanged { node: key(2) });
    r.actor.pump(Instant::now());
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(
            FocusLaneMsg::FocusChanged { .. }
        ))
    ));
    r.port.push(selected(2));
    let route = r.actor.pump(Instant::now());
    assert_eq!(route.forwarded_to_focus_lane, 1);
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::StateChanged { node, .. })) if node == key(2)
    ));
    assert!(r.inbox.poll().is_none());
}

// ---- B-1 5.1: the invalidation table, the policy inputs and the fill wiring ----

use gorca_mirror::fill::{FillCtx, FillEffect, FillEffects, FillOp, FillPolicy};
use gorca_model::{TextDelta, WindowKind};
use gorca_rt::BudgetClass;
use std::sync::Mutex;

fn node(n: u64, role: Role, parent: Option<u64>) -> Arc<NodeSnapshot> {
    let mut s = (*snap(n)).clone();
    s.role = role;
    s.name = Some(Arc::from(format!("n{n}")));
    s.parent = parent.map(key);
    Arc::new(s)
}

fn fill_node(r: &mut Rig, nodes: Vec<Arc<NodeSnapshot>>) {
    r.port
        .push_snapshot(SnapshotBatch {
            nodes,
            ..Default::default()
        })
        .unwrap();
    r.actor.pump(Instant::now());
}

fn stale(r: &Rig, n: u64) -> PropMask {
    r.read.view(key(n)).hit().unwrap().stale
}

#[test]
fn every_row_of_the_invalidation_table_marks_the_right_fields() {
    let mut r = rig(IngressConfig::default());
    fill_node(
        &mut r,
        (1..=8).map(|n| node(n, Role::Button, None)).collect(),
    );
    let ev = |e| r.port.push(e);
    ev(NodeEvent::PropertiesChanged {
        node: key(1),
        mask: PropMask::STATES | PropMask::BOUNDS,
    });
    ev(NodeEvent::ChildrenChanged { node: key(2) });
    ev(NodeEvent::TextChanged {
        node: key(3),
        delta: TextDelta::Replaced { text: None },
    });
    ev(NodeEvent::ActiveDescendantChanged {
        container: key(4),
        child: Some(key(8)),
    });
    ev(NodeEvent::WindowEvent {
        node: key(5),
        kind: WindowKind::Activate,
    });
    ev(NodeEvent::CaretMoved {
        node: key(6),
        offset: 1,
        origin: gorca_model::CaretOrigin::Unknown,
    });
    ev(NodeEvent::SelectionChanged { node: key(7) });
    r.actor.pump(Instant::now());
    assert_eq!(
        stale(&r, 1),
        PropMask::STATES | PropMask::BOUNDS,
        "IC-24: a bounds event invalidates BOUNDS (ADR-0006 17)"
    );
    assert_eq!(stale(&r, 2), PropMask::CHILDREN);
    assert_eq!(stale(&r, 3), PropMask::NAME | PropMask::VALUE);
    assert_eq!(stale(&r, 4), PropMask::STATES);
    assert_eq!(stale(&r, 5), PropMask::STATES | PropMask::NAME);
    for n in [6, 7, 8] {
        assert_eq!(stale(&r, n), PropMask::empty(), "node {n}: no effect");
    }
}

#[test]
fn removal_and_window_destroy_tombstone_and_dirty_the_parent() {
    let mut r = rig(IngressConfig::default());
    fill_node(
        &mut r,
        vec![
            node(1, Role::Panel, None),
            node(2, Role::Button, Some(1)),
            node(3, Role::Window, None),
        ],
    );
    r.port.push(NodeEvent::NodeRemoved { node: key(2) });
    r.port.push(NodeEvent::WindowEvent {
        node: key(3),
        kind: WindowKind::Destroy,
    });
    r.actor.pump(Instant::now());
    assert_eq!(r.read.get(key(2)), Lookup::Gone);
    assert_eq!(r.read.get(key(3)), Lookup::Gone);
    assert_eq!(stale(&r, 1), PropMask::CHILDREN);
    assert_eq!(r.read.stats().tombstones, 2);
}

/// P9-3: a name change of an inner label also invalidates the names of ancestors that derive
/// theirs from content, up to and including the first that does not.
#[test]
fn a_name_change_reaches_content_named_ancestors() {
    let mut r = rig(IngressConfig::default());
    fill_node(
        &mut r,
        vec![
            node(1, Role::Window, None),
            node(2, Role::Panel, Some(1)),
            node(3, Role::Button, Some(2)),
            node(4, Role::Label, Some(3)),
            node(5, Role::Button, Some(4)),
        ],
    );
    r.port.push(NodeEvent::PropertiesChanged {
        node: key(4),
        mask: PropMask::NAME,
    });
    r.actor.pump(Instant::now());
    assert_eq!(stale(&r, 4), PropMask::NAME);
    assert_eq!(stale(&r, 3), PropMask::NAME, "button: named from content");
    assert_eq!(stale(&r, 2), PropMask::NAME, "the panel stops the climb");
    assert_eq!(stale(&r, 1), PropMask::empty(), "beyond the stop");
    assert_eq!(stale(&r, 5), PropMask::empty(), "descendants are untouched");
}

#[test]
fn a_flood_invalidates_the_dirty_nodes_and_dirty_all_is_o1() {
    let mut r = rig(IngressConfig {
        max_keys: 8,
        ..IngressConfig::default()
    });
    fill_node(
        &mut r,
        (1..=3).map(|n| node(n, Role::Button, None)).collect(),
    );
    // Overflow the key table: the batch ends with dirty-all.
    for n in (100..200).chain(1..=3) {
        r.port.push(NodeEvent::ChildrenChanged { node: key(n) });
    }
    let route = r.actor.pump(Instant::now());
    assert!(route.resync.is_some());
    for n in 1..=3 {
        assert_eq!(
            stale(&r, n),
            PropMask::ALL,
            "dirty nodes are stale, not dropped"
        );
    }
}

#[test]
fn only_the_focused_nodes_state_change_reaches_the_reader() {
    let mut r = rig(IngressConfig::default());
    fill_node(
        &mut r,
        (1..=2).map(|n| node(n, Role::CheckBox, None)).collect(),
    );
    r.port.push(NodeEvent::FocusChanged { node: key(1) });
    r.actor.pump(Instant::now());
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(
            FocusLaneMsg::FocusChanged { .. }
        ))
    ));
    for n in [1, 1, 2] {
        r.port.push(NodeEvent::PropertiesChanged {
            node: key(n),
            mask: PropMask::STATES,
        });
    }
    r.port.push(NodeEvent::PropertiesChanged {
        node: key(1),
        mask: PropMask::DESCRIPTION,
    });
    let route = r.actor.pump(Instant::now());
    assert_eq!(
        route.forwarded_to_focus_lane, 1,
        "one S1 message, for node 1"
    );
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::StateChanged { node, .. })) if node == key(1)
    ));
    assert!(r.inbox.poll().is_none());
}

#[derive(Clone, Default)]
struct Spy {
    seen: Arc<Mutex<Vec<String>>>,
    on_demand: Arc<Mutex<Vec<FillEffect>>>,
}

impl FillPolicy for Spy {
    fn name(&self) -> &'static str {
        "spy"
    }
    fn step(
        &mut self,
        cx: &FillCtx<'_>,
        input: gorca_mirror::fill::FillInput<'_>,
        out: &mut FillEffects,
    ) {
        use gorca_mirror::fill::FillInput as I;
        let tag = match &input {
            I::Appeared { .. } => "appeared".to_owned(),
            I::Ready => "ready".to_owned(),
            I::Events(e) => {
                // The invalidation of 5.1 is already applied when the policy sees the batch.
                let n = e.iter().find_map(NodeEvent::node);
                let stale = n.and_then(|k| cx.mirror.view(k).hit()).map(|v| v.stale);
                format!("events:{}:{:?}", e.len(), stale)
            }
            I::Resync { .. } => "resync".to_owned(),
            I::Demand { why, outcome, .. } => format!("demand:{why:?}:{outcome:?}"),
            I::FocusSettled { .. } => "settled".to_owned(),
            I::WindowActivated { .. } => "window".to_owned(),
            I::Reply { .. } => "reply".to_owned(),
            I::Tick => "tick".to_owned(),
        };
        if tag != "tick"
            && let Ok(mut s) = self.seen.lock()
        {
            s.push(tag);
        }
        if matches!(input, I::Demand { .. })
            && let Ok(mut q) = self.on_demand.lock()
        {
            out.extend(q.drain(..));
        }
    }
    fn memory_bytes(&self) -> usize {
        0
    }
}

fn spy_rig(spy: &Spy) -> (Rig, mpsc::Sender<FillMsg>) {
    let ch = app_channel(app(), IngressConfig::default());
    let (writer, read) = gorca_mirror::mirror_for(app(), None);
    let (tx, inbox) = reader_channels();
    let links = ReaderLinks {
        focus: tx.focus,
        background: tx.background,
    };
    let (ftx, frx) = mpsc::channel(16);
    let actor = AppActor::new(app(), writer, ch.ingress, ch.control, links)
        .with_policy(
            Box::new(spy.clone()),
            gorca_mirror::FillLimits::default(),
            None,
        )
        .with_fill_channel(frx);
    (
        Rig {
            actor,
            port: ch.port,
            read,
            inbox,
        },
        ftx,
    )
}

#[test]
fn the_policy_sees_events_after_the_invalidation_and_demands_drive_requests() {
    let spy = Spy::default();
    let (mut r, ftx) = spy_rig(&spy);
    fill_node(&mut r, vec![node(1, Role::Button, None)]);
    r.port.push(NodeEvent::PropertiesChanged {
        node: key(1),
        mask: PropMask::NAME,
    });
    r.actor.pump(Instant::now());
    assert_eq!(
        spy.seen.lock().unwrap().last().map(String::as_str),
        Some("events:1:Some(PropMask(NAME))")
    );
    // A Reader demand reaches the policy, whose Fetch effect is issued under its class.
    spy.on_demand.lock().unwrap().push(FillEffect::Request {
        op: FillOp::Fetch {
            node: key(1),
            want: PropMask::PRESENT,
        },
        class: BudgetClass::Interactive,
    });
    ftx.try_send(FillMsg::Demand {
        node: key(1),
        why: gorca_mirror::fill::DemandWhy::Focus,
        outcome: gorca_mirror::fill::DemandOutcome::Miss,
    })
    .unwrap();
    let route = r.actor.pump(Instant::now());
    assert_eq!(route.issued.len(), 1);
    assert_eq!(route.issued[0].class, BudgetClass::Interactive);
    assert!(
        spy.seen
            .lock()
            .unwrap()
            .iter()
            .any(|t| t.starts_with("demand:Focus:Miss"))
    );
}

#[test]
fn the_policy_hears_of_the_app_with_its_root_and_identity_before_the_first_focus() {
    let spy = Spy::default();
    let (mut r, _ftx) = spy_rig(&spy);
    let mut info = shell_info();
    info.dump_complete = true;
    info.root = Some(key(1));
    // Announced: no event has come from the application yet.
    r.port.lifecycle(AppLifecycle::Appeared(info)).unwrap();
    r.port.lifecycle(AppLifecycle::Ready).unwrap();
    r.actor.pump(Instant::now());
    assert_eq!(
        spy.seen.lock().unwrap().as_slice(),
        ["appeared", "ready"],
        "the policy knows the app before anything happens in it"
    );
    // Then the first focus.
    r.port.push(NodeEvent::FocusChanged { node: key(1) });
    r.actor.pump(Instant::now());
    let seen = spy.seen.lock().unwrap().clone();
    assert_eq!(seen[0], "appeared");
    assert!(
        r.actor.profile().speak_selection,
        "the identity is in place"
    );
    // An announcement without a root (a connector that has none) gives the policy no Appeared.
    let spy2 = Spy::default();
    let (mut r2, _f2) = spy_rig(&spy2);
    r2.port
        .lifecycle(AppLifecycle::Appeared(shell_info()))
        .unwrap();
    r2.actor.pump(Instant::now());
    assert!(spy2.seen.lock().unwrap().is_empty());
}

#[test]
fn a_fill_reply_is_applied_with_the_late_reply_rules() {
    let spy = Spy::default();
    let (mut r, ftx) = spy_rig(&spy);
    spy.on_demand.lock().unwrap().push(FillEffect::Request {
        op: FillOp::Fetch {
            node: key(1),
            want: PropMask::PRESENT,
        },
        class: BudgetClass::Interactive,
    });
    ftx.try_send(FillMsg::Demand {
        node: key(1),
        why: gorca_mirror::fill::DemandWhy::NavStep,
        outcome: gorca_mirror::fill::DemandOutcome::Miss,
    })
    .unwrap();
    let issued = r.actor.pump(Instant::now()).issued;
    let f = &issued[0];
    // The node is removed while the request is in flight: the late reply must not revive it.
    r.port.push(NodeEvent::NodeRemoved { node: key(1) });
    r.actor.pump(Instant::now());
    r.actor.on_fill_reply(
        f.id,
        f.issued_at,
        Ok(gorca_connector_api::BackendReply::Node(node(
            1,
            Role::Button,
            None,
        ))),
        Instant::now(),
    );
    assert_eq!(r.read.get(key(1)), Lookup::Gone);
    assert_eq!(r.actor.fill_stats().dropped_tombstoned, 1);
}

#[test]
fn reader_fetch_replies_are_applied_and_gone_tombstones() {
    let spy = Spy::default();
    let (mut r, ftx) = spy_rig(&spy);
    ftx.try_send(FillMsg::ReaderFetch {
        node: key(1),
        issued_at: 0,
        result: Ok(node(1, Role::Button, None)),
    })
    .unwrap();
    ftx.try_send(FillMsg::ReaderFetch {
        node: key(2),
        issued_at: 0,
        result: Err(gorca_connector_api::BackendError::Gone),
    })
    .unwrap();
    r.actor.pump(Instant::now());
    assert!(r.read.get(key(1)).hit().is_some());
    assert_eq!(r.read.get(key(2)), Lookup::Gone);
}

#[test]
fn a_gone_app_releases_its_mirror_at_once() {
    let mut r = rig(IngressConfig::default());
    fill_node(
        &mut r,
        (1..=10).map(|n| node(n, Role::Button, None)).collect(),
    );
    r.port.lifecycle(AppLifecycle::Gone).unwrap();
    let route = r.actor.pump(Instant::now());
    assert!(route.gone);
    assert_eq!(r.read.get(key(1)), Lookup::Gone);
    assert_eq!(r.read.get(key(999)), Lookup::Gone);
    assert_eq!(r.read.stats().live, 0);
}

#[test]
fn only_a_verified_shell_is_protected_and_may_dump() {
    let mut r = rig(IngressConfig::default());
    let mut info = shell_info();
    info.dump_complete = true;
    r.port.lifecycle(AppLifecycle::Appeared(info)).unwrap();
    r.actor.pump(Instant::now());
    assert!(r.actor.profile().speak_selection);
    // An unverified app that claims to be complete is still not a dump candidate: the flag is
    // the connector's verdict, carried in AppInfo, and nothing else sets it.
    let mut r2 = rig(IngressConfig::default());
    let mut spoof = shell_info();
    spoof.verified_shell = false;
    spoof.dump_complete = false;
    r2.port.lifecycle(AppLifecycle::Appeared(spoof)).unwrap();
    r2.actor.pump(Instant::now());
    assert!(!r2.actor.profile().speak_selection);
}

#[test]
fn the_focused_app_is_protected_from_global_shedding_until_the_focus_leaves() {
    let focused = Arc::new(gorca_rt::FocusedApp::new());
    let ch = app_channel(app(), IngressConfig::default());
    let (writer, _read) = mirror();
    let (tx, _inbox) = reader_channels();
    let links = ReaderLinks {
        focus: tx.focus,
        background: tx.background,
    };
    let mut actor = AppActor::new(app(), writer, ch.ingress, ch.control, links)
        .with_focused(Arc::clone(&focused));
    actor.pump(Instant::now());
    assert!(!actor.mirror_protected());
    focused.publish(app());
    actor.pump(Instant::now());
    assert!(actor.mirror_protected(), "the focused app is protected");
    focused.demote();
    actor.pump(Instant::now());
    assert!(!actor.mirror_protected());
}

#[test]
fn text_events_of_a_focused_but_unprotected_app_are_counted_not_silently_lost() {
    let ch = app_channel(app(), IngressConfig::default());
    let (writer, _read) = mirror();
    let (tx, _inbox) = reader_channels();
    let links = ReaderLinks {
        focus: tx.focus,
        background: tx.background,
    };
    let (ttx, mut trx) = tokio::sync::mpsc::channel(8);
    let focused = Arc::new(gorca_rt::FocusedApp::new());
    let mut actor = AppActor::new(app(), writer, ch.ingress, ch.control, links)
        .with_text(TextLink {
            tx: ttx,
            dropped: Arc::default(),
        })
        .with_focused(Arc::clone(&focused));
    let port = ch.port;
    let caret = |n, offset| NodeEvent::CaretMoved {
        node: key(n),
        offset,
        origin: gorca_model::CaretOrigin::Unknown,
    };
    // The app has a focus of its own, but nobody published it as the owner yet.
    port.push(NodeEvent::FocusChanged { node: key(1) });
    port.push(caret(1, 1));
    port.push(NodeEvent::SelectionChanged { node: key(1) });
    let route = actor.pump(Instant::now());
    assert_eq!(route.text_unprotected, 2);
    assert_eq!(actor.text_events_unprotected(), 2);
    assert!(trx.try_recv().is_err(), "nothing was forwarded");
    // Once the Reader earned the protection the events flow and nothing more is counted.
    focused.publish(app());
    port.push(caret(1, 5));
    let route = actor.pump(Instant::now());
    assert_eq!(route.text_unprotected, 0);
    assert_eq!(actor.text_events_unprotected(), 2);
    assert!(trx.try_recv().is_ok());
}

/// A fake extension host: the hook answers by the text of an announcement.
#[derive(Debug)]
struct FakeExt;

#[derive(Debug)]
struct FakeApp;

impl crate::AppExt for FakeApp {
    fn on_event(&self, ev: &NodeEvent) -> crate::ExtVerdict {
        if let NodeEvent::Announcement { text, .. } = ev {
            match &**text {
                "suppress" => crate::ExtVerdict::Suppress,
                "rewrite" => crate::ExtVerdict::Rewrite(vec![gorca_model::Segment {
                    text: Arc::from("said instead"),
                    kind: gorca_model::SegmentKind::Text,
                }]),
                "reevaluate" => crate::ExtVerdict::ReEvaluate,
                _ => crate::ExtVerdict::Pass,
            }
        } else if matches!(ev, NodeEvent::FocusChanged { .. }) {
            // Even a Suppress on focus must not break the actor's bookkeeping.
            crate::ExtVerdict::Suppress
        } else {
            crate::ExtVerdict::Pass
        }
    }

    fn present(
        &self,
        _snap: &NodeSnapshot,
        _ctx: crate::PresentContext,
        _epoch: u64,
    ) -> Option<Vec<gorca_model::Segment>> {
        None
    }

    fn command(&self, _ext: &str, _local: u32) {}
}

impl crate::ExtSource for FakeExt {
    fn attach(&self, _app: AppId, _info: &gorca_model::AppInfo) -> Option<Arc<dyn crate::AppExt>> {
        Some(Arc::new(FakeApp))
    }
    fn detach(&self, _app: AppId) {}
    fn for_app(&self, _app: AppId) -> Option<Arc<dyn crate::AppExt>> {
        None
    }
    fn set_sink(&self, _sink: Arc<dyn crate::ExtSink>) {}
}

fn announce(text: &str) -> NodeEvent {
    NodeEvent::Announcement {
        node: None,
        text: Arc::from(text),
        politeness: Politeness::Polite,
    }
}

#[test]
fn on_event_verdicts_decide_what_the_reader_hears() {
    let mut r = rig(IngressConfig::default());
    let actor = r.actor;
    r.actor = actor.with_ext(Arc::new(FakeExt));
    r.port
        .lifecycle(AppLifecycle::Appeared(shell_info()))
        .unwrap();
    r.port.push(announce("plain"));
    r.port.push(announce("suppress"));
    r.port.push(announce("rewrite"));
    r.port.push(announce("reevaluate"));
    let route = r.actor.pump(Instant::now());
    // `plain` and `reevaluate` are routed; `suppress` is dropped; `rewrite` is replaced by speech.
    assert_eq!(route.forwarded_to_background, 3);
    let mut got = Vec::new();
    while let Some(m) = r.inbox.poll() {
        got.push(m);
    }
    let texts: Vec<String> = got
        .iter()
        .filter_map(|m| {
            if let crate::reader::ReaderMsg::Background(b) = m {
                match b {
                    BackgroundMsg::Announcement { text, .. } => {
                        Some(format!("announcement:{text}"))
                    }
                    BackgroundMsg::ExtSpeech { text, interrupt } => {
                        Some(format!("ext:{text}:{interrupt}"))
                    }
                    BackgroundMsg::Window(_)
                    | BackgroundMsg::Showing { .. }
                    | BackgroundMsg::LevelChanged { .. }
                    | BackgroundMsg::LevelRead { .. }
                    | BackgroundMsg::Notice { .. }
                    | BackgroundMsg::ResyncNeeded(_) => None,
                }
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        texts,
        [
            "announcement:plain",
            "ext:said instead:false",
            "announcement:reevaluate"
        ]
    );
    assert!(r.actor.resync_needed(), "ReEvaluate asks for revalidation");
}

#[test]
fn a_suppress_verdict_on_focus_never_breaks_focus_routing() {
    let mut r = rig(IngressConfig::default());
    let actor = r.actor;
    r.actor = actor.with_ext(Arc::new(FakeExt));
    r.port
        .lifecycle(AppLifecycle::Appeared(shell_info()))
        .unwrap();
    r.port.push(NodeEvent::FocusChanged { node: key(7) });
    let route = r.actor.pump(Instant::now());
    assert_eq!(route.forwarded_to_focus_lane, 1);
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node })) if node == key(7)
    ));
}

// ---- M4b: window activation and showing routes ----

fn showing(n: u64) -> NodeEvent {
    NodeEvent::PropertiesChanged {
        node: key(n),
        mask: PropMask::STATES | PropMask::SHOWING_NOW,
    }
}

fn alert_snap(n: u64) -> Arc<NodeSnapshot> {
    let mut s = (*snap(n)).clone();
    s.role = Role::Alert;
    Arc::new(s)
}

#[test]
fn window_activation_goes_to_the_focus_lane() {
    let mut r = rig(IngressConfig::default());
    r.port.push(NodeEvent::WindowEvent {
        node: key(4),
        kind: WindowKind::Activate,
    });
    let route = r.actor.pump(Instant::now());
    assert_eq!(route.forwarded_to_focus_lane, 1);
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::WindowActivated { window })) if window == key(4)
    ));
    r.port.push(NodeEvent::WindowEvent {
        node: key(4),
        kind: WindowKind::Deactivate,
    });
    r.actor.pump(Instant::now());
    assert!(r.inbox.poll().is_none(), "only an activation is spoken");
}

#[test]
fn the_shell_forwards_an_unknown_node_that_became_visible() {
    let mut r = rig(IngressConfig::default());
    r.port
        .lifecycle(AppLifecycle::Appeared(shell_info()))
        .unwrap();
    r.port.push(showing(7));
    let route = r.actor.pump(Instant::now());
    assert!(r.actor.profile().watch_showing);
    assert_eq!(route.forwarded_to_background, 1);
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Background(
            BackgroundMsg::Showing { node }
        )) if node == key(7)
    ));
}

#[test]
fn another_app_forwards_only_a_known_alert_that_became_visible() {
    let mut r = rig(IngressConfig::default());
    r.port
        .push_snapshot(SnapshotBatch {
            nodes: vec![alert_snap(1), snap(2)],
            ..Default::default()
        })
        .unwrap();
    r.actor.pump(Instant::now());
    r.port.push(showing(1));
    r.port.push(showing(2));
    r.port.push(showing(3));
    let route = r.actor.pump(Instant::now());
    assert_eq!(
        route.forwarded_to_background, 1,
        "the alert only: a button is known, an unknown node of a non-Shell app is not asked about"
    );
    assert!(matches!(
        r.inbox.poll(),
        Some(crate::reader::ReaderMsg::Background(
            BackgroundMsg::Showing { node }
        )) if node == key(1)
    ));
    assert!(r.inbox.poll().is_none());
}

#[test]
fn a_flood_of_showing_events_is_bounded_and_never_asks_for_a_resync() {
    let mut r = rig(IngressConfig::default());
    r.port
        .lifecycle(AppLifecycle::Appeared(shell_info()))
        .unwrap();
    for n in 100..260 {
        r.port.push(showing(n));
    }
    let route = r.actor.pump(Instant::now());
    assert_eq!(
        route.forwarded_to_background,
        usize::try_from(SHOWING_BURST).unwrap()
    );
    assert_eq!(
        route.dropped_background, 0,
        "dropping a showing is not a loss"
    );
    assert!(!r.actor.resync_needed);
}
