//! Reviewer probes for M4a (2026-10-14). Temporary: not part of the branch.
#![allow(
    clippy::wildcard_enum_match_arm,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_mirror::{ApplyOrigin, MirrorRead, MirrorWriter, mirror};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Epoch, Freshness, Ifaces, LocalId, NodeKey,
    NodeSnapshot, PropMask, Rect, Relations, Role, SnapshotBatch, StateSet,
};
use gorca_rt::EpochCell;

use super::ReviewMsg;
use super::build::{HandleSrc, RrVerdict};
use super::cluster::{BuildInput, build_line_model};
use super::collect::collect;
use crate::commands::CoreCmd;
use crate::directory::MirrorDirectory;
use crate::mode::ModeKeys;
use crate::reader::{Reader, ReaderEffect, ReaderEffects, ReaderMsg};

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
fn rect(x: i32, y: i32, w: u32, h: u32) -> Option<Rect> {
    Some(Rect { x, y, w, h })
}
fn node(n: u64, role: Role, name: &str, parent: Option<u64>, kids: &[u64], r: Option<Rect>) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: key(n),
        role,
        states: StateSet::SHOWING | StateSet::VISIBLE | StateSet::FOCUSABLE,
        name: (!name.is_empty()).then(|| Arc::from(name)),
        description: None,
        value: None,
        parent: parent.map(key),
        children: ChildList::Known(kids.iter().map(|k| key(*k)).collect()),
        interfaces: Ifaces::empty(),
        attrs: AttrMap::default(),
        relations: Relations::default(),
        bounds: r,
        have: PropMask::PRESENT | PropMask::CHILDREN | PropMask::PARENT | PropMask::BOUNDS,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}
fn dialog() -> Vec<Arc<NodeSnapshot>> {
    vec![
        node(1, Role::Window, "Settings", None, &[2], rect(0, 0, 400, 300)),
        node(2, Role::Panel, "", Some(1), &[4, 5, 6], rect(0, 0, 400, 200)),
        node(4, Role::TextEntry, "Name", Some(2), &[], rect(80, 10, 200, 20)),
        node(5, Role::Button, "Apply", Some(2), &[], rect(10, 50, 80, 24)),
        node(6, Role::Button, "Cancel", Some(2), &[], rect(10, 90, 80, 24)),
    ]
}

struct Fx {
    reader: Reader,
    writer: MirrorWriter,
    epoch: Arc<EpochCell>,
    now: Instant,
}
fn fixture() -> Fx {
    let epoch = Arc::new(EpochCell::new());
    let dir = Arc::new(MirrorDirectory::new());
    let (writer, handle) = mirror();
    dir.insert(app(), &handle);
    let reader = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default());
    Fx { reader, writer, epoch, now: Instant::now() }
}
impl Fx {
    fn load(&mut self, nodes: Vec<Arc<NodeSnapshot>>) {
        self.writer.apply(SnapshotBatch { nodes, ..Default::default() }, ApplyOrigin::Push);
    }
    fn focus(&mut self, n: u64) {
        self.reader.handle(ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged { node: key(n) }), self.now);
        self.now += Duration::from_millis(60);
        self.reader.handle(ReaderMsg::Timer, self.now);
    }
    fn run(&mut self, cmd: CoreCmd) -> ReaderEffects {
        let e = self.epoch.advance();
        let k = KeyIntent {
            event: KeyEvent { keycode: 79, keysym: 0xff95, unichar: 0, modifiers: Modifiers::empty(), pressed: true },
            kind: PressKind::Press,
            class: KeyClass::Command(cmd.id()),
            repeat: 1,
            chord: None,
            epoch: Epoch(e.0),
            at: Instant::now(),
        };
        self.reader.handle(ReaderMsg::Key(k), self.now)
    }
    fn review(&mut self, m: ReviewMsg) -> ReaderEffects {
        self.reader.handle(ReaderMsg::Review(m), self.now)
    }
}
fn said(fx: &ReaderEffects) -> Vec<String> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::Speak(s) => Some(s.utterance.segments.iter().map(|g| &*g.text).collect::<Vec<_>>().join(" ")),
            _ => None,
        })
        .collect()
}
fn nav_req(fx: &ReaderEffects) -> Option<super::NavReqId> {
    fx.iter().find_map(|e| match e {
        ReaderEffect::ReviewNav { req, .. } => Some(*req),
        _ => None,
    })
}
fn timer(fx: &ReaderEffects) -> Option<Instant> {
    fx.iter().find_map(|e| match e {
        ReaderEffect::ArmTimer(t) => Some(*t),
        _ => None,
    })
}

/// OB-3 / R4: "a step suspends at most 3 times, all within one Interactive deadline of 120 ms;
/// never > 120 ms + 5 ms". Each suspension gets its own fresh 160 ms deadline instead.
#[test]
fn probe_ob3_one_step_can_wait_far_beyond_125ms() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(3); // button 5 missing: every fetch "succeeds" but brings nothing
    f.load(nodes);
    f.focus(4);
    let t0 = f.now;
    let fx = f.run(CoreCmd::ObjectNext);
    let mut req = nav_req(&fx).expect("suspend 1");
    let mut last_deadline = timer(&fx).expect("timer 1");
    let mut suspensions = 1;
    loop {
        // The app answers just before each deadline (a slow but healthy app).
        f.now = last_deadline - Duration::from_millis(1);
        let fx = f.review(ReviewMsg::NavFetched { req, result: Ok(()) });
        if let (Some(r), Some(d)) = (nav_req(&fx), timer(&fx)) {
            suspensions += 1;
            req = r;
            last_deadline = d;
            continue;
        }
        let total = f.now.duration_since(t0);
        eprintln!("PROBE ob3: suspensions={suspensions} total_wait={total:?} said={:?}", said(&fx));
        assert!(
            total <= Duration::from_millis(125),
            "one object step waited {total:?} over {suspensions} suspensions (ADR-0006 OB-3/R4: <= 125 ms)"
        );
        break;
    }
}

/// A key that arrives after a pending op's deadline but before the timer message: the stale
/// command is executed degraded AND spoken, then the new one is spoken too (two utterances
/// under the same epoch for one key; "only the last speaks" is violated).
#[test]
fn probe_late_pending_and_new_key_both_speak() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(3);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    assert!(nav_req(&fx).is_some());
    f.now += Duration::from_millis(400);
    let fx = f.run(CoreCmd::ObjectCurrent);
    let s = said(&fx);
    eprintln!("PROBE late: said={s:?}");
    assert!(s.len() <= 1, "one key produced {} utterances: {s:?}", s.len());
}

/// RR-1: "If the spoken line's text changed, a short follow-up is spoken with the new text, as
/// an RV-1 correction". The Reader speaks nothing on `Changed{text_changed: true}`.
#[test]
fn probe_rr1_text_change_is_not_corrected() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.run(CoreCmd::ReviewNextLine);
    let build = fx
        .iter()
        .find_map(|e| match e {
            ReaderEffect::ReviewBuild(b) => Some(b.build),
            _ => None,
        })
        .expect("build");
    let h = f.writer.reader();
    let c = collect(&HandleSrc(&h), key(1), super::ReviewLimits::default());
    let model = Arc::new(build_line_model(
        &BuildInput { app: app(), surface: key(1), nodes: c.nodes, built_rev: h.revision(), built_at: Instant::now(), truncated: c.truncated },
        &super::ReviewLimits::default(),
    ));
    f.review(ReviewMsg::Built { build, result: Ok(Arc::clone(&model)) });
    f.now += Duration::from_secs(3);
    let fx = f.run(CoreCmd::ReviewCurLine);
    let rr = fx.iter().find_map(|e| match e {
        ReaderEffect::ReviewRevalidate { model, .. } => Some(*model),
        _ => None,
    });
    let rr = rr.expect("RR-1 probe after the TTL");
    let fx = f.review(ReviewMsg::Revalidated { model: rr, verdict: RrVerdict::Changed { text_changed: true } });
    eprintln!("PROBE rr1: said={:?}", said(&fx));
    assert!(!said(&fx).is_empty(), "the spoken line changed but no correction was spoken");
}


fn model_now(f: &Fx) -> Arc<super::LineModel> {
    let h = f.writer.reader();
    let c = collect(&HandleSrc(&h), key(1), super::ReviewLimits::default());
    Arc::new(build_line_model(
        &BuildInput { app: app(), surface: key(1), nodes: c.nodes, built_rev: h.revision(), built_at: Instant::now(), truncated: c.truncated },
        &super::ReviewLimits::default(),
    ))
}
fn build_req(fx: &ReaderEffects) -> Option<super::BuildId> {
    fx.iter().find_map(|e| match e { ReaderEffect::ReviewBuild(b) => Some(b.build), _ => None })
}

/// F-4 r2: the corrected line is spoken when the rebuilt model arrives.
#[test]
fn probe_r2_rr1_correction_after_rebuild() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let fx = f.run(CoreCmd::ReviewNextLine);
    let b = build_req(&fx).expect("build");
    let m = model_now(&f);
    let fx = f.review(ReviewMsg::Built { build: b, result: Ok(m) });
    eprintln!("PROBE r2-f4 first: {:?}", said(&fx));
    f.now += Duration::from_secs(3);
    let fx = f.run(CoreCmd::ReviewCurLine);
    eprintln!("PROBE r2-f4 cur: {:?}", said(&fx));
    let rr = fx.iter().find_map(|e| match e { ReaderEffect::ReviewRevalidate { model, .. } => Some(*model), _ => None }).expect("rr");
    // The app renamed the button without an event.
    f.load(vec![node(5, Role::Button, "Apply now", Some(2), &[], rect(10, 50, 80, 24))]);
    let fx = f.review(ReviewMsg::Revalidated { model: rr, verdict: RrVerdict::Changed { text_changed: true } });
    let b2 = build_req(&fx).expect("background rebuild");
    let m2 = model_now(&f);
    let fx = f.review(ReviewMsg::Built { build: b2, result: Ok(m2) });
    eprintln!("PROBE r2-f4 correction: {:?}", said(&fx));
    assert_eq!(said(&fx), vec!["Apply now button".to_owned()]);
}

/// F-2 r2: an object step whose app never answers speaks when the budget expires (never silence),
/// and a Timer that fires early for something else re-arms the review deadline.
#[test]
fn probe_r2_budget_expiry_is_never_silent() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(3);
    f.load(nodes);
    f.focus(4);
    let t0 = f.now;
    let fx = f.run(CoreCmd::ObjectNext);
    let d = timer(&fx).expect("armed");
    eprintln!("PROBE r2-f2 deadline after {:?}", d.duration_since(t0));
    // An unrelated timer fires first: the review deadline must be armed again.
    f.now = t0 + Duration::from_millis(50);
    let fx = f.reader.handle(ReaderMsg::Timer, f.now);
    assert_eq!(timer(&fx), Some(d), "re-armed");
    assert!(said(&fx).is_empty());
    f.now = d;
    let fx = f.reader.handle(ReaderMsg::Timer, f.now);
    eprintln!("PROBE r2-f2 at deadline: {:?}", said(&fx));
    assert!(!said(&fx).is_empty(), "silence when the budget expired");
}

/// F-2 r2: same, with three more presses queued behind it. Something must be spoken.
#[test]
fn probe_r2_budget_expiry_with_queue_is_never_silent() {
    let mut f = fixture();
    let mut nodes = dialog();
    nodes.remove(3);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    let d = timer(&fx).expect("armed");
    for _ in 0..3 { f.now += Duration::from_millis(10); assert!(said(&f.run(CoreCmd::ObjectNext)).is_empty()); }
    f.now = d;
    let fx = f.reader.handle(ReaderMsg::Timer, f.now);
    eprintln!("PROBE r2-f2q at deadline: {:?}", said(&fx));
    assert!(!said(&fx).is_empty(), "silence when the budget expired");
}

/// F-2 r2: a cold flat build against a dead app: something is spoken at the deadline.
#[test]
fn probe_r2_cold_build_timeout_is_never_silent() {
    let mut f = fixture();
    f.load(dialog());
    f.focus(4);
    let t0 = f.now;
    let fx = f.run(CoreCmd::ReviewNextLine);
    let d = timer(&fx).expect("armed");
    f.now = d;
    let fx = f.reader.handle(ReaderMsg::Timer, f.now);
    eprintln!("PROBE r2-build waited {:?}: {:?}", d.duration_since(t0), said(&fx));
    assert!(!said(&fx).is_empty(), "silence");
}

/// F-2 r2: the reply arrives within budget, but a key that speaks nothing (passed to the app)
/// was pressed meanwhile: the review answer is dropped.
#[test]
fn probe_r2_reply_after_unrelated_key_is_dropped() {
    let mut f = fixture();
    let mut nodes = dialog();
    let missing = nodes.remove(3);
    f.load(nodes);
    f.focus(4);
    let fx = f.run(CoreCmd::ObjectNext);
    let req = nav_req(&fx).expect("pending");
    f.epoch.advance(); // any non-modifier key press, e.g. a letter typed into the app
    f.load(vec![missing]);
    f.now += Duration::from_millis(30);
    let fx = f.review(ReviewMsg::NavFetched { req, result: Ok(()) });
    eprintln!("PROBE r2-superseded: {:?}", said(&fx));
}
