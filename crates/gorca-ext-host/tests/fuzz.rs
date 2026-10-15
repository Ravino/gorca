//! Fuzzing the WIT boundary (M5 acceptance: "no core panic from any extension input"). Seeded and
//! deterministic, so a failure reproduces; a failing seed is printed.
//!
//! Three directions:
//! - core to extension: random events, nodes and commands through the host's public hook points,
//!   and extreme values straight into a component instance;
//! - extension to core: an extension that answers with garbage (huge, many, empty);
//! - the component itself: corrupted bytes must be rejected or contained, never crash the host.
#![allow(
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_methods
)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{BACKENDS, Backend, app, host, info, snap, wasm_bytes};
use gorca_core::{ExtSource, ExtVerdict, MirrorDirectory, PresentContext};
use gorca_ext_api as api;
use gorca_ext_host::{
    Capabilities, ExtEngine, ExtHost, HookInstance, InstShared, MAX_SEGMENT_CHARS, MAX_SEGMENTS,
    Manifest, NativeFactory, SettingsKv, WasmModule,
};
use gorca_model::{
    CaretOrigin, DocKind, NodeEvent, NodeKey, Politeness, PropMask, Role, TextDelta, WindowKind,
};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn text(&mut self, max: usize) -> String {
        let n = self.below(max as u64 + 1) as usize;
        (0..n)
            .map(|_| match self.below(6) {
                0 => char::from_u32(0).unwrap_or('a'),
                1 => 'é',
                2 => '\u{1F600}',
                3 => '\u{10FFFF}',
                4 => char::from_u32(0xD7FF).unwrap_or('b'),
                _ => char::from(b'a' + u8::try_from(self.below(26)).unwrap_or(0)),
            })
            .collect()
    }
}

fn rand_key(r: &mut Rng) -> NodeKey {
    NodeKey {
        app: gorca_model::AppId {
            connector: gorca_model::ConnectorId(u16::try_from(r.below(4)).unwrap_or(0)),
            serial: u32::try_from(r.below(5)).unwrap_or(0),
        },
        local: gorca_model::LocalId(r.next()),
    }
}

fn rand_event(r: &mut Rng) -> NodeEvent {
    let node = rand_key(r);
    match r.below(11) {
        0 => NodeEvent::FocusChanged { node },
        1 => NodeEvent::ActiveDescendantChanged {
            container: node,
            child: Some(rand_key(r)),
        },
        2 => NodeEvent::PropertiesChanged {
            node,
            mask: PropMask::all(),
        },
        3 => NodeEvent::ChildrenChanged { node },
        4 => NodeEvent::TextChanged {
            node,
            delta: TextDelta::Replaced { text: None },
        },
        5 => NodeEvent::CaretMoved {
            node,
            offset: u32::try_from(r.next() & 0xffff_ffff).unwrap_or(0),
            origin: CaretOrigin::Unknown,
        },
        6 => NodeEvent::SelectionChanged { node },
        7 => NodeEvent::Announcement {
            node: Some(node),
            text: Arc::from(r.text(20_000)),
            politeness: Politeness::Assertive,
        },
        8 => NodeEvent::WindowEvent {
            node,
            kind: WindowKind::Activate,
        },
        9 => NodeEvent::DocumentEvent {
            node,
            kind: DocKind::Reload,
        },
        _ => NodeEvent::NodeRemoved { node },
    }
}

const ROLES: [Role; 6] = [
    Role::Unknown,
    Role::Button,
    Role::PasswordEntry,
    Role::DocumentWeb,
    Role::Terminal,
    Role::Alert,
];

#[test]
fn random_events_nodes_and_commands_never_panic_the_core() {
    for b in BACKENDS {
        let iters = if b == Backend::Wasm { 1500 } else { 20_000 };
        let (h, _sink, _) = host(b);
        let ae = h.attach(app(1), &info("fuzz")).expect("attached");
        let mut r = Rng(0x9e37_79b9_7f4a_7c15);
        for i in 0..iters {
            match r.below(4) {
                0 | 1 => {
                    let ev = rand_event(&mut r);
                    let _ = ae.on_event(&ev);
                }
                2 => {
                    let name = r.text(6000);
                    let role = ROLES[usize::try_from(r.below(ROLES.len() as u64)).unwrap_or(0)];
                    let n = snap(app(1), r.next(), role, Some(&name));
                    let _ = ae.present(&n, PresentContext::Focus, r.next());
                }
                _ => {
                    let ext = if r.below(2) == 0 {
                        "ext-test".to_owned()
                    } else {
                        r.text(10)
                    };
                    // Local id 2, 3 and 6 are the deliberate faults: skip them so the three
                    // strikes rule does not end the run early (it has its own test).
                    let local = u32::try_from(r.next() & 0xffff).unwrap_or(0);
                    if ![2, 3, 6].contains(&local) {
                        ae.command(&ext, local);
                    }
                }
            }
            assert!(i < iters);
        }
        // Still alive and answering.
        assert_eq!(
            ae.on_event(&NodeEvent::Announcement {
                node: None,
                text: Arc::from("suppress"),
                politeness: Politeness::Polite,
            }),
            ExtVerdict::Suppress,
            "{b:?}"
        );
        assert_eq!(h.stats().disabled, 0, "{b:?}: nothing here is a fault");
    }
}

#[test]
fn extreme_values_straight_into_a_component_are_contained() {
    let engine = ExtEngine::new().expect("engine");
    let module = WasmModule::new(&engine, wasm_bytes()).expect("module");
    let shared = Arc::new(InstShared::new(
        Arc::from("ext-test"),
        app(1),
        Capabilities::all(),
        Arc::new(MirrorDirectory::new()),
        Arc::default(),
        Arc::new(SettingsKv::memory()),
    ));
    let mut r = Rng(0x1234_5678_9abc_def1);
    let mut faults = 0;
    for i in 0..60 {
        let mut inst = module.instantiate(Arc::clone(&shared)).expect("instance");
        // Up to 24 MiB of text: lifting it needs more than the 32 MiB cap with copies, so some of
        // these are memory faults and must stay faults, not crashes.
        let len = match i % 4 {
            0 => 0,
            1 => 1_000,
            2 => 1 << 20,
            _ => usize::try_from(r.below(24 << 20)).unwrap_or(0),
        };
        let ev = api::Event::Announcement("x".repeat(len));
        if inst.on_event(&ev).is_err() {
            faults += 1;
        }
        let n = api::Node {
            key: (r.next(), r.next()),
            role: api::Role::from_index(u8::try_from(r.below(44)).unwrap_or(0)).unwrap(),
            states: r.next(),
            name: Some(r.text(5000)),
            description: None,
        };
        let _ = inst.present(
            &n,
            api::PresentCtx {
                epoch: r.next(),
                reason: api::PresentReason::Review,
            },
        );
        let _ = inst.nav_match(u32::try_from(r.next() & 0xffff_ffff).unwrap_or(0), &n);
        let _ = inst.nav_units();
    }
    println!("component instances faulted on extreme input: {faults} of 60 (contained)");
}

/// Answers with garbage.
#[derive(Default)]
struct Garbage;

impl api::Hooks for Garbage {
    fn on_event(&mut self, _ev: &api::Event) -> api::EventVerdict {
        api::EventVerdict::Rewrite(
            (0..200)
                .map(|_| api::Segment::text("y".repeat(5_000)))
                .collect(),
        )
    }
    fn present(&mut self, _n: &api::Node, _c: api::PresentCtx) -> Option<Vec<api::Segment>> {
        Some(
            (0..200)
                .map(|i| api::Segment::text(if i % 2 == 0 { "" } else { "z" }.repeat(5_000)))
                .collect(),
        )
    }
    fn command(&mut self, _id: u32) -> api::CommandResult {
        api::CommandResult::Speak(vec![api::Segment::text("w".repeat(100_000))])
    }
}

#[test]
fn garbage_answers_are_clamped() {
    let dir = Arc::new(MirrorDirectory::new());
    let mut b = ExtHost::builder(dir);
    let m = Manifest::parse(
        "name = \"garbage\"\nbackend = \"native\"\nhooks = [\"on-event\", \"present\", \"command\"]\n",
    )
    .unwrap();
    let f: NativeFactory = Arc::new(|| Box::new(Garbage));
    b.add_native(m, f).unwrap();
    let h = b.build();
    let sink = Arc::new(common::TestSink::default());
    h.set_sink(sink.clone());
    let ae = h.attach(app(1), &info("x")).unwrap();

    let ev = NodeEvent::FocusChanged {
        node: rand_key(&mut Rng(7)),
    };
    match ae.on_event(&ev) {
        ExtVerdict::Rewrite(s) => {
            assert!(s.len() <= MAX_SEGMENTS);
            let total: usize = s.iter().map(|x| x.text.chars().count()).sum();
            assert!(total <= gorca_ext_host::MAX_ANSWER_CHARS, "{total}");
            assert!(
                s.iter()
                    .all(|x| x.text.chars().count() <= MAX_SEGMENT_CHARS)
            );
        }
        v => panic!("{v:?}"),
    }
    let n = snap(app(1), 1, Role::Button, Some("n"));
    let segs = ae.present(&n, PresentContext::Focus, 1).expect("some");
    assert!(segs.len() <= MAX_SEGMENTS);
    assert!(segs.iter().all(|x| !x.text.is_empty()));
    ae.command("garbage", 1);
    let got = sink.wait(1, Duration::from_secs(5));
    assert!(
        got[0].0.chars().count() <= MAX_SEGMENT_CHARS,
        "{}",
        got[0].0.len()
    );
}

#[test]
fn corrupted_components_are_rejected_or_contained() {
    let good = wasm_bytes();
    let engine = ExtEngine::new().expect("engine");
    let mut r = Rng(0xfeed_f00d_dead_beef);
    let (mut rejected, mut loaded) = (0, 0);
    for i in 0..300 {
        let mut bytes = good.to_vec();
        let flips = 1 + r.below(8);
        for _ in 0..flips {
            // Mostly the header and the type/import/export sections: that is where structural
            // corruption is interesting and rejection is fast.
            let span = if i % 3 == 0 { bytes.len() as u64 } else { 4096 };
            let at = usize::try_from(r.below(span)).unwrap_or(0);
            bytes[at] ^= 1 << r.below(8);
        }
        if i % 10 == 0 {
            bytes.truncate(usize::try_from(r.below(bytes.len() as u64)).unwrap_or(0));
        }
        match WasmModule::new(&engine, &bytes) {
            Err(_) => rejected += 1,
            Ok(m) => {
                loaded += 1;
                let shared = Arc::new(InstShared::new(
                    Arc::from("c"),
                    app(1),
                    Capabilities::all(),
                    Arc::new(MirrorDirectory::new()),
                    Arc::default(),
                    Arc::new(SettingsKv::memory()),
                ));
                if let Ok(mut inst) = m.instantiate(shared) {
                    let _ = inst.on_event(&api::Event::Announcement("hi".into()));
                    let _ = inst.command(1);
                }
            }
        }
    }
    println!("corrupted components: {rejected} rejected, {loaded} loaded (contained)");
    assert!(rejected > 200, "corruption should usually be caught");
}
