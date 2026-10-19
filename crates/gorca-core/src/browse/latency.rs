// SPDX-License-Identifier: GPL-3.0-or-later
//! Gorca-side cost of the browse hot paths on large mirrors: W1 (quick-nav key to speech),
//! BR1 (cursor key to speech) and J1 (focus join). They measure what the Reader does between
//! receiving the key and handing the utterance over; the end-to-end figures (with the key
//! source and the speech lane around it) come from the e2e harness.
//!
//! Gates (browse-mode design 15): W1 and BR1 p99 < 5 ms (target 2 ms), J1 p99 < 10 us. The debug
//! build gets a loose bound so `cargo test` stays meaningful anywhere; run
//! `cargo test --release -p gorca-core --lib browse::latency -- --ignored --nocapture` for the
//! numbers of the gates.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::unreadable_literal,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc::web::{DocOp, DocWriter, channel, synth};
use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_model::{AppId, ConnectorId, DocId, Epoch, LocalId, NodeKey};
use gorca_rt::{EpochCell, mpsc};

use super::*;
use crate::commands::CoreCmd;
use crate::directory::MirrorDirectory;
use crate::doc_actor::{DocHandle, DocNotice};
use crate::mode::ModeKeys;
use crate::reader::{Reader, ReaderMsg};

fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

struct Big {
    reader: Reader,
    writer: DocWriter,
    join: Arc<WebJoin>,
    epoch: Arc<EpochCell>,
    now: Instant,
}

fn big(nodes: usize) -> Big {
    let docs = Arc::new(DocDirectory::new());
    let join = Arc::new(WebJoin::new());
    let (mut writer, read) = channel(11);
    let spec = synth::page(nodes, 5);
    writer
        .apply(DocOp::InsertSubtree {
            parent: None,
            before: None,
            subtree: Arc::new(spec),
        })
        .unwrap();
    writer.publish();
    let (ctl, _ctl_rx) = mpsc::channel(4);
    let (ev, _ev_rx) = mpsc::channel(4);
    docs.insert(DocHandle {
        doc: DocId(1),
        app: app(),
        root: NodeKey {
            app: app(),
            local: LocalId(1),
        },
        read: read.clone(),
        ctl,
        events: ev,
        orphans: Arc::default(),
        repair_flag: Arc::default(),
        timeline: Arc::default(),
    });
    let epoch = Arc::new(EpochCell::new());
    let dir = Arc::new(MirrorDirectory::new());
    let mut reader = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default())
        .with_web(Arc::clone(&docs), Arc::clone(&join));
    let now = Instant::now();
    // Enter browse mode at the root through the first-useful path.
    reader.handle(ReaderMsg::Doc(DocNotice::WebApp { app: app() }), now);
    let root = read.with(gorca_doc::web::WebDoc::root).unwrap();
    join.set(
        NodeKey {
            app: app(),
            local: LocalId(1),
        },
        (DocId(1), root),
    );
    reader.handle(
        ReaderMsg::Focus(crate::FocusLaneMsg::FocusChanged {
            node: NodeKey {
                app: app(),
                local: LocalId(1),
            },
        }),
        now,
    );
    assert!(matches!(reader.web_mode(), WebMode::Browse { .. }));
    Big {
        reader,
        writer,
        join,
        epoch,
        now,
    }
}

impl Big {
    fn press(&mut self, cmd: CoreCmd) -> Duration {
        self.now += Duration::from_millis(40);
        let msg = ReaderMsg::Key(KeyIntent {
            event: KeyEvent {
                keycode: 1,
                keysym: 1,
                unichar: 0,
                modifiers: Modifiers::empty(),
                pressed: true,
            },
            class: KeyClass::Command(cmd.id()),
            kind: PressKind::Press,
            repeat: 1,
            chord: None,
            at: self.now,
            epoch: Epoch(self.epoch.current().0 + 1),
        });
        let t = Instant::now();
        let fx = self.reader.handle(msg, self.now);
        let d = t.elapsed();
        assert!(!fx.is_empty(), "{cmd:?} spoke nothing");
        d
    }
}

#[derive(Clone, Copy)]
enum Gate {
    P99,
    Median,
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort_unstable();
    let i = ((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1;
    v[i]
}

/// `n` presses of `cmd`, each from a different place: every `reset_every` presses the cursor
/// goes back to the top (that press is not measured), so a series never sits at an edge.
fn run_series(b: &mut Big, cmd: CoreCmd, n: usize, reset_every: usize) -> (Duration, Duration) {
    let mut v: Vec<Duration> = Vec::with_capacity(n);
    for i in 0..n {
        if i % reset_every == 0 {
            b.press(CoreCmd::BrowseCursorTop);
        }
        v.push(b.press(cmd));
    }
    (pct(&mut v, 0.5), pct(&mut v, 0.99))
}

/// `gate` picks the statistic the bound applies to. The strict p99 gates run in release on a
/// quiet host (the `#[ignore]` test below); the debug test gates the median, which a loaded
/// host cannot move but an O(n) regression on a 10k mirror still does (p99 is printed anyway).
fn report(nodes: usize, bound: Duration, gate: Gate) {
    let mut b = big(nodes);
    let len = b.writer.doc().len();
    // Warm.
    for _ in 0..50 {
        b.press(CoreCmd::BrowseCursorNextLine);
    }
    for (name, cmd, reset) in [
        ("W1 next heading", CoreCmd::QuicknavNextHeading, 20),
        ("W1 next link", CoreCmd::QuicknavNextLink, 50),
        ("BR1 next line", CoreCmd::BrowseCursorNextLine, 100),
        ("BR1 next word", CoreCmd::BrowseCursorNextWord, 100),
        ("BR1 next char", CoreCmd::BrowseCursorNextChar, 100),
        ("BR1 next page", CoreCmd::BrowseCursorPageDown, 5),
    ] {
        let (p50, p99) = run_series(&mut b, cmd, 400, reset);
        eprintln!("{nodes} nodes ({len} in mirror): {name}: p50 {p50:?} p99 {p99:?}");
        let gated = if matches!(gate, Gate::P99) { p99 } else { p50 };
        assert!(
            gated < bound,
            "{name}: p50 {p50:?} p99 {p99:?} over {bound:?}"
        );
    }
    // Far jumps: top of a 100k page, then to the bottom, then back.
    let mut far = Vec::new();
    for _ in 0..40 {
        far.push(b.press(CoreCmd::BrowseCursorBottom));
        far.push(b.press(CoreCmd::BrowseCursorTop));
    }
    let p99 = pct(&mut far, 0.99);
    eprintln!("{nodes} nodes: top/bottom p99 {p99:?}");
    let gated = if matches!(gate, Gate::P99) {
        p99
    } else {
        pct(&mut far, 0.5)
    };
    assert!(gated < bound, "top/bottom p99 {p99:?} gated {gated:?}");
}

/// J1: the focus join, `WebJoin` plus the `DocRead` check, over a 100k-key table.
fn join_series() -> (Duration, Duration) {
    let join = WebJoin::new();
    let (mut w, read) = channel(1);
    let spec = synth::page(2_000, 3);
    w.apply(DocOp::InsertSubtree {
        parent: None,
        before: None,
        subtree: Arc::new(spec),
    })
    .unwrap();
    w.publish();
    let ids: Vec<DocNodeId> = read.with(|d| d.preorder().collect());
    for i in 0..100_000u64 {
        join.set(
            NodeKey {
                app: app(),
                local: LocalId(i),
            },
            (DocId(1), ids[(i as usize) % ids.len()]),
        );
    }
    let mut samples = Vec::with_capacity(20_000);
    let mut x = 12345u64;
    for _ in 0..20_000 {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let k = NodeKey {
            app: app(),
            local: LocalId((x >> 33) % 100_000),
        };
        let t = Instant::now();
        let hit = join
            .get(k)
            .is_some_and(|(_, id)| read.with(|d| d.node(id).is_some()));
        let d = t.elapsed();
        assert!(hit);
        samples.push(d);
    }
    (pct(&mut samples, 0.5), pct(&mut samples, 0.99))
}

#[test]
fn hot_paths_stay_far_below_the_gates_on_a_10k_mirror_even_in_debug() {
    report(10_000, Duration::from_millis(10), Gate::Median);
}

#[test]
fn the_focus_join_is_microseconds_even_in_debug() {
    let (p50, p99) = join_series();
    eprintln!("J1: p50 {p50:?} p99 {p99:?}");
    // Median, not p99: 200 of the 20k samples are preemption victims on a loaded host. The strict
    // p99 gate is the release test below.
    assert!(
        p50 < Duration::from_micros(50),
        "J1 p50 {p50:?} (p99 {p99:?})"
    );
}

#[test]
#[ignore = "release-mode gate run: cargo test --release -p gorca-core --lib browse::latency -- --ignored --nocapture"]
fn gates_w1_br1_and_j1_on_10k_and_100k_mirrors() {
    report(10_000, Duration::from_millis(5), Gate::P99);
    report(100_000, Duration::from_millis(5), Gate::P99);
    let (p50, p99) = join_series();
    eprintln!("J1: p50 {p50:?} p99 {p99:?}");
    assert!(p99 < Duration::from_micros(10), "J1 p99 {p99:?}");
}

#[allow(dead_code)]
fn _keep(b: &Big) -> usize {
    b.join.len()
}
