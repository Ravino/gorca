// SPDX-License-Identifier: GPL-3.0-or-later
//! Drain conformance (SPEC section 3): framing, mapping, routing, robustness and lifecycle,
//! against the mock broker so every byte on the wire is under the test's control.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::disallowed_methods,
    clippy::manual_is_multiple_of,
    clippy::needless_pass_by_value,
    clippy::similar_names
)]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::Fixture;
use gorca_bench_b2::bus::{Bus, BusKind};
use gorca_bench_b2::feeder::AppPlan;
use gorca_bench_b2::mockbus::MockOpts;
use gorca_bench_b2::model::Ev;
use gorca_bench_b2::rig::{ConsumerCfg, HarnessRouter};
use gorca_bench_b2::runner::{AppSpec, RunSpec, execute};
use gorca_bench_b2::wire::{
    Any, Enc, Fields, METHOD_RETURN, SIGNAL, encode_event_body, encode_message,
};
use gorca_bench_b2::workload::{
    FENCE_ID, Gen, GenEvent, IFACE_FOCUS, IFACE_OBJECT, Kind, Mix, Scratch, canon, local_of,
    node_path,
};
use gorca_dbus::drain::{DrainConfig, DrainEnd, spawn_drain};

fn fold_cfg() -> ConsumerCfg {
    ConsumerCfg {
        fold: true,
        check_order: true,
        ..ConsumerCfg::default()
    }
}

fn signal(sender: &str, iface: &str, member: &str, path: &str, body: &[u8], sig: &str) -> Vec<u8> {
    let mut out = Vec::new();
    encode_message(
        &mut out,
        SIGNAL,
        1,
        &Fields {
            path: Some(path),
            interface: Some(iface),
            member: Some(member),
            sender: Some(sender),
            signature: Some(sig),
            ..Fields::default()
        },
        body,
    );
    out
}

fn caret(sender: &str, node: u64, off: i32) -> Vec<u8> {
    let mut p = String::new();
    node_path(node, &mut p);
    let mut b = Enc::default();
    encode_event_body(&mut b, "", off, 0, Any::Int(0), Some(1));
    signal(
        sender,
        IFACE_OBJECT,
        "TextCaretMoved",
        &p,
        &b.buf,
        "siiva{sv}",
    )
}

fn focus(sender: &str, node: u64) -> Vec<u8> {
    let mut sc = Scratch::default();
    let mut out = Vec::new();
    GenEvent {
        node,
        kind: Kind::FocusSignal,
        seq: 0,
    }
    .encode(&mut out, &mut sc, 1, Some(sender));
    out
}

fn fenced(
    f: Fixture,
) -> (
    gorca_bench_b2::rig::ConsumerResult,
    gorca_dbus::drain::DrainStats,
) {
    f.link.send(&focus(&f.sender, FENCE_ID));
    assert!(
        f.router.wait_fence("app", Duration::from_secs(10)),
        "fence never delivered"
    );
    f.finish()
}

/// C-D1: framing and mapping at small scale for every workload mix, ground truth compared.
#[test]
fn every_mix_matches_the_ground_truth() {
    for mix in [Mix::Fakeapp, Mix::Terminal, Mix::Realistic, Mix::Lossless] {
        let mut spec = RunSpec::new(
            BusKind::Mock(MockOpts::default()),
            Duration::from_millis(700),
        );
        spec.verify = true;
        for i in 0..2u64 {
            let mut plan = AppPlan::new(&format!("a{i}"), 30 + i, mix, 8_000.0);
            plan.slots = 200;
            spec.apps.push(AppSpec {
                plan,
                cons: ConsumerCfg {
                    drain_delay: Duration::from_millis(i * 3),
                    ..fold_cfg()
                },
            });
        }
        let o = execute(&spec).unwrap();
        assert!(o.settled && !o.disconnected(), "{mix:?}: not settled");
        for a in &o.apps {
            let v = a.verdict.as_ref().unwrap();
            assert!(v.ok(), "{mix:?}/{}: {:?}", a.role, v.violations);
            assert!(
                v.compared > 2,
                "{mix:?}: vacuous comparison ({})",
                v.compared
            );
            assert_eq!(
                a.cons.order_violations, 0,
                "{mix:?}: {:?}",
                a.cons.first_order_violation
            );
            assert_eq!(a.cons.misrouted, 0);
        }
        assert_eq!(o.drain.malformed, 0);
        assert_eq!(o.drain.unrouted, 0);
    }
}

/// C-D2: the byte stream arrives in arbitrary pieces, down to single bytes.
#[test]
fn fragmented_streams_are_reassembled() {
    for chunk in [1usize, 3, 7, 61, 4096] {
        let mut spec = RunSpec::new(
            BusKind::Mock(MockOpts {
                chunk: Some(chunk),
                ..MockOpts::default()
            }),
            Duration::from_millis(600),
        );
        spec.verify = true;
        let mut plan = AppPlan::new("a", 7, Mix::Realistic, 600.0);
        plan.slots = 100;
        spec.apps.push(AppSpec {
            plan,
            cons: fold_cfg(),
        });
        spec.settle = Duration::from_secs(20);
        let o = execute(&spec).unwrap();
        let v = o.apps[0].verdict.as_ref().unwrap();
        assert!(
            o.settled && v.ok(),
            "chunk {chunk}: {:?} settled {}",
            v.violations,
            o.settled
        );
        assert_eq!(o.drain.malformed, 0, "chunk {chunk}");
    }
}

/// C-D3: a message far larger than the read buffer, between normal ones.
#[test]
fn a_huge_message_does_not_break_framing() {
    let f = Fixture::start(MockOpts::default(), &fold_cfg());
    let mut p = String::new();
    node_path(5, &mut p);
    let mut b = Enc::default();
    let big = "y".repeat(3_000_000);
    encode_event_body(&mut b, "", 2, 0, Any::Str(&big), None);
    f.link.send(&caret(&f.sender, 1, 10));
    f.link.send(&signal(
        &f.sender,
        IFACE_OBJECT,
        "Announcement",
        &p,
        &b.buf,
        "siiva{sv}",
    ));
    f.link.send(&caret(&f.sender, 1, 11));
    let (cons, stats) = fenced(f);
    assert_eq!(stats.malformed, 0);
    assert_eq!(cons.fold.announcements, 1);
    assert_eq!(cons.fold.nodes[&local_of(1)].caret, Some(11));
}

/// C-D4: bodies that do not parse are counted and skipped; neighbours are unaffected and
/// events whose mapping needs no body never look at it.
#[test]
fn malformed_bodies_are_survivable() {
    let f = Fixture::start(MockOpts::default(), &fold_cfg());
    let s = f.sender.clone();
    let path = |n| {
        let mut p = String::new();
        node_path(n, &mut p);
        p
    };
    let mut garbage = Vec::new();
    // wrong signature / truncated / claimed length beyond the body / invalid UTF-8
    let mut trunc = Enc::default();
    encode_event_body(&mut trunc, "insert", 1, 1, Any::Int(0), None);
    garbage.push(signal(
        &s,
        IFACE_OBJECT,
        "TextCaretMoved",
        &path(1),
        &[1, 2, 3],
        "u",
    ));
    garbage.push(signal(
        &s,
        IFACE_OBJECT,
        "TextChanged",
        &path(2),
        &trunc.buf[..7],
        "siiva{sv}",
    ));
    garbage.push(signal(
        &s,
        IFACE_OBJECT,
        "TextCaretMoved",
        &path(3),
        &[0xff, 0xff, 0xff, 0x7f, b'a', 0, 0, 0],
        "siiva{sv}",
    ));
    garbage.push(signal(
        &s,
        IFACE_OBJECT,
        "StateChanged",
        &path(4),
        &[2, 0, 0, 0, 0xc3, 0x28, 0, 0],
        "siiva{sv}",
    ));
    garbage.push(signal(
        &s,
        IFACE_OBJECT,
        "TextCaretMoved",
        &path(5),
        &[],
        "siiva{sv}",
    ));
    let n_garbage = garbage.len() as u64;
    for g in &garbage {
        f.link.send(g);
    }
    // These never need a body: SelectionChanged, Window, Document, Focus:Focus.
    f.link.send(&signal(
        &s,
        IFACE_OBJECT,
        "SelectionChanged",
        &path(6),
        &[9, 9],
        "u",
    ));
    f.link.send(&signal(
        &s,
        "org.a11y.atspi.Event.Window",
        "Activate",
        &path(7),
        &[],
        "",
    ));
    f.link
        .send(&signal(&s, IFACE_FOCUS, "Focus", &path(8), &[1], "u"));
    f.link.send(&caret(&s, 9, 77));
    let (cons, stats) = fenced(f);
    assert_eq!(
        stats.malformed, n_garbage,
        "every unparsable body is counted exactly once"
    );
    let n = &cons.fold.nodes;
    assert!(n[&local_of(6)].selection);
    assert_eq!(n[&local_of(7)].window, Some(0));
    assert_eq!(n[&local_of(9)].caret, Some(77));
    for bad in 1..=5 {
        assert!(
            !n.contains_key(&local_of(bad)),
            "garbage produced state for node {bad}"
        );
    }
}

/// C-D5: replies, errors, unknown interfaces and unknown members are ignored and counted.
#[test]
fn non_events_are_ignored() {
    let f = Fixture::start(MockOpts::default(), &fold_cfg());
    let s = f.sender.clone();
    let mut ret = Vec::new();
    encode_message(
        &mut ret,
        METHOD_RETURN,
        9,
        &Fields {
            reply_serial: Some(1),
            destination: Some(":1.1000"),
            sender: Some(&s),
            ..Fields::default()
        },
        &[],
    );
    f.link.send(&ret);
    f.link
        .send(&signal(&s, "org.example.Other", "Whatever", "/x", &[], ""));
    f.link
        .send(&signal(&s, IFACE_OBJECT, "BoundsChanged", "/x", &[], ""));
    f.link.send(&signal(
        &s,
        "org.a11y.atspi.Event.Mouse",
        "Abs",
        "/x",
        &[],
        "",
    ));
    let before = f.drain.stats();
    f.link.send(&caret(&s, 1, 1));
    let (cons, stats) = fenced(f);
    assert_eq!(cons.fold.nodes[&local_of(1)].caret, Some(1));
    assert!(
        stats.ignored >= 4,
        "ignored = {} (before {})",
        stats.ignored,
        before.ignored
    );
    assert_eq!(stats.malformed, 0);
}

/// C-D6: unknown senders are unrouted and dropped; known senders are unaffected.
#[test]
fn unknown_senders_are_unrouted() {
    let f = Fixture::start(MockOpts::default(), &fold_cfg());
    for i in 0..10 {
        f.link.send(&caret(":1.999", 1, i));
    }
    f.link.send(&caret(&f.sender, 2, 5));
    let (cons, stats) = fenced(f);
    assert_eq!(stats.unrouted, 10);
    assert_eq!(cons.fold.nodes.len(), 1);
}

/// C-D7: the router is consulted once per distinct sender, not once per message.
#[test]
fn routes_are_cached_per_sender() {
    let mut spec = RunSpec::new(
        BusKind::Mock(MockOpts::default()),
        Duration::from_millis(500),
    );
    for i in 0..3u64 {
        spec.apps.push(AppSpec {
            plan: AppPlan::new(&format!("a{i}"), i, Mix::Fakeapp, 4000.0),
            cons: ConsumerCfg::default(),
        });
    }
    // execute() hides the router, so build the same thing by hand.
    let _ = spec;
    let bus = Bus::start(BusKind::Mock(MockOpts::default())).unwrap();
    let router = HarnessRouter::new();
    for i in 0..3 {
        router.register(
            &format!(":1.{}", 200 + i),
            &format!("a{i}"),
            &ConsumerCfg::default(),
        );
    }
    let drain = spawn_drain(
        DrainConfig::new(bus.address.clone()),
        Arc::clone(&router) as _,
    )
    .unwrap();
    let link = bus
        .mock()
        .unwrap()
        .next_link(Duration::from_secs(5))
        .unwrap();
    let mut gens: Vec<Gen> = (0..3).map(|i| Gen::new(i, Mix::Fakeapp, 100)).collect();
    let mut sc = Scratch::default();
    for round in 0..300 {
        let mut out = Vec::new();
        for (i, g) in gens.iter_mut().enumerate() {
            for _ in 0..10 {
                g.next_event().encode(
                    &mut out,
                    &mut sc,
                    round + 1,
                    Some(&format!(":1.{}", 200 + i)),
                );
            }
        }
        out.extend(caret(":1.555", 1, 1));
        link.send(&out);
    }
    link.flush_all(Duration::from_secs(5));
    let end = Instant::now() + Duration::from_secs(5);
    while drain.stats().messages < 9_300 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    let stats = drain.stop();
    assert_eq!(stats.messages, 9_300);
    assert_eq!(
        router.route_calls.load(Ordering::Relaxed),
        4,
        "router must be called once per distinct sender (3 apps + 1 unknown)"
    );
}

/// C-D8: big-endian messages are decoded.
#[test]
fn big_endian_messages_are_decoded() {
    let f = Fixture::start(MockOpts::default(), &fold_cfg());
    let mut p = String::new();
    node_path(3, &mut p);
    // Header fields in big-endian, hand encoded.
    let mut m = Vec::new();
    let mut body = Vec::new();
    body.extend(0u32.to_be_bytes());
    body.push(0);
    body.extend([0, 0, 0]);
    body.extend(1234i32.to_be_bytes()); // d1
    body.extend(0i32.to_be_bytes()); // d2
    body.extend([1, b'i', 0, 0]); // variant sig "i" + pad
    body.extend(0i32.to_be_bytes());
    body.extend(0u32.to_be_bytes()); // empty a{sv}
    body.extend(0u32.to_be_bytes());
    let mut fields = Vec::new();
    let field = |out: &mut Vec<u8>, code: u8, sig: &str, v: &str| {
        while out.len() % 8 != 0 {
            out.push(0);
        }
        out.push(code);
        out.extend([1, sig.as_bytes()[0], 0]);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend((v.len() as u32).to_be_bytes());
        out.extend(v.as_bytes());
        out.push(0);
    };
    field(&mut fields, 1, "o", &p);
    field(&mut fields, 2, "s", IFACE_OBJECT);
    field(&mut fields, 3, "s", "TextCaretMoved");
    field(&mut fields, 7, "s", &f.sender);
    while fields.len() % 8 != 0 {
        fields.push(0);
    }
    // signature field last
    let mut sigf = vec![8u8, 1, b'g', 0, 9];
    sigf.extend(b"siiva{sv}\0");
    let mut all_fields = fields.clone();
    // fields length excludes trailing padding, so trim the zero padding we added before the sig field
    while all_fields.len() % 8 != 0 {
        all_fields.push(0);
    }
    all_fields.extend(sigf);
    let flen = all_fields.len() as u32;
    m.extend([b'B', 4, 0, 1]);
    m.extend((body.len() as u32).to_be_bytes());
    m.extend(7u32.to_be_bytes());
    m.extend(flen.to_be_bytes());
    m.extend(&all_fields);
    while m.len() % 8 != 0 {
        m.push(0);
    }
    m.extend(&body);
    f.link.send(&m);
    let (cons, stats) = fenced(f);
    assert_eq!(stats.malformed, 0);
    assert_eq!(cons.fold.nodes[&local_of(3)].caret, Some(1234));
}

/// C-D9: the SPEC match rules are installed with AddMatch.
#[test]
fn match_rules_are_installed() {
    let f = Fixture::start(MockOpts::default(), &ConsumerCfg::default());
    let rules = f.link.rules();
    for want in gorca_dbus::drain::default_match_rules() {
        assert!(
            rules.contains(&want),
            "missing rule {want}; installed {rules:?}"
        );
    }
    f.drain.stop();
}

/// C-D10: stop() returns promptly, idle and under traffic, and reports `Stopped`.
#[test]
fn stop_is_prompt() {
    let f = Fixture::start(MockOpts::default(), &ConsumerCfg::default());
    let t = Instant::now();
    let h = f.drain;
    assert_eq!(h.end(), DrainEnd::Running);
    h.stop();
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "idle stop took {:?}",
        t.elapsed()
    );

    let f = Fixture::start(MockOpts::default(), &ConsumerCfg::default());
    let mut out = Vec::new();
    for i in 0..5000 {
        out.extend(caret(&f.sender, i % 50, i as i32));
    }
    f.link.send(&out);
    let t = Instant::now();
    let h = f.drain;
    h.stop();
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "busy stop took {:?}",
        t.elapsed()
    );
}

/// C-D11: a broker-side disconnect is reported, not swallowed, and does not hang or panic.
#[test]
fn disconnect_is_reported() {
    let f = Fixture::start(MockOpts::default(), &ConsumerCfg::default());
    f.link.disconnect();
    let end = Instant::now() + Duration::from_secs(2);
    while matches!(f.drain.end(), DrainEnd::Running) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        matches!(f.drain.end(), DrainEnd::Disconnected(_)),
        "end = {:?}",
        f.drain.end()
    );
    f.drain.stop();
}

/// C-D12: a broker that never answers AddMatch cannot hang start-up past the deadline.
#[test]
fn start_up_has_a_deadline() {
    let bus = Bus::start(BusKind::Mock(MockOpts {
        reply_addmatch: false,
        ..MockOpts::default()
    }))
    .unwrap();
    let router = HarnessRouter::new();
    let mut cfg = DrainConfig::new(bus.address.clone());
    cfg.connect_deadline = Duration::from_millis(400);
    let t = Instant::now();
    let r = spawn_drain(cfg, router as _);
    assert!(
        r.is_err(),
        "spawn_drain succeeded against a broker that never replies"
    );
    assert!(
        t.elapsed() < Duration::from_secs(3),
        "gave up after {:?}",
        t.elapsed()
    );
}

/// C-D13: a stalled consumer never stops the drain reading (D-1): the broker-side backlog stays
/// small, nobody is disconnected, and the state after the consumer resumes is correct.
#[test]
fn a_stalled_consumer_does_not_stall_the_socket() {
    let mut spec = RunSpec::new(
        BusKind::Mock(MockOpts {
            quota: 8 * 1024 * 1024,
            ..MockOpts::default()
        }),
        Duration::from_secs(3),
    );
    spec.verify = true;
    let mut plan = AppPlan::new("a", 9, Mix::Realistic, 20_000.0);
    plan.slots = 5_000;
    spec.apps.push(AppSpec {
        plan,
        cons: ConsumerCfg {
            stall: Some((500_000_000, 2_200_000_000)),
            ..fold_cfg()
        },
    });
    let o = execute(&spec).unwrap();
    assert!(!o.disconnected(), "disconnected during a consumer stall");
    assert!(o.settled);
    let worst = o.backlog_samples.iter().map(|s| s.1).max().unwrap_or(0);
    assert!(
        worst < 1024 * 1024,
        "socket backlog reached {worst} bytes while only the consumer was stalled"
    );
    let a = &o.apps[0];
    assert!(
        a.verdict.as_ref().unwrap().ok(),
        "{:?}",
        a.verdict.as_ref().unwrap().violations
    );
    assert!(
        a.cons.flooded_batches >= 1,
        "the stall should have forced Flooded"
    );
}

/// Sanity of the harness itself: canonical conversion is the inverse of the mapping for focus.
#[test]
fn harness_self_check_focus_probe_ids() {
    assert_ne!(
        local_of(FENCE_ID),
        local_of(gorca_bench_b2::workload::probe_id(0))
    );
    let e = gorca_model::NodeEvent::FocusChanged {
        node: gorca_model::NodeKey {
            app: gorca_model::AppId {
                connector: gorca_model::ConnectorId(0),
                serial: 1,
            },
            local: gorca_model::LocalId(5),
        },
    };
    assert_eq!(canon(&e), Ev::Focus(5));
}
