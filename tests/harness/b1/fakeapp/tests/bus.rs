// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake app on a private bus: protocol, serial service time, modes, signals, quirks.

#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::type_complexity,
    clippy::redundant_closure_for_method_calls,
    clippy::items_after_statements,
    clippy::too_many_lines
)]

mod common;

use std::time::{Duration, Instant};

use common::{Bus, call};
use futures_lite::StreamExt;
use gorca_testkit::b1::proto::{Cmd, ConnKind, ModelSpec, P2pMode, ServiceSpec, Shape};
use zbus::MessageStream;
use zvariant::OwnedObjectPath;

const ACC: &str = "org.a11y.atspi.Accessible";

fn spec(shape: Shape, p2p: P2pMode, p50: f64, p99: f64) -> ModelSpec {
    ModelSpec {
        shape,
        p2p,
        nodes_idle: 400,
        sidebar_rows: 20,
        list_rows: 20,
        list_items: 200,
        quirks: gorca_testkit::b1::proto::Quirks {
            spurious_parent_add: false,
            never_remove_accessible: false,
            ..gorca_testkit::b1::proto::Quirks::pessimistic_gtk4()
        },
        service: ServiceSpec {
            p50_ms: p50,
            p99_ms: p99,
        },
        ..ModelSpec::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_accessible_calls_and_unknown_objects() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "s")
        .await;
    let c = bus.client().await;
    let dest = app.ready.bus_name.clone();
    let root = app.ready.root.clone();
    let r = call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
    assert_eq!(r.body().deserialize::<u32>().unwrap(), 75);
    let r = call(&c, &dest, &root, ACC, "GetChildren", &())
        .await
        .unwrap();
    let kids: Vec<(String, OwnedObjectPath)> = r.body().deserialize().unwrap();
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].0, dest);
    let e = call(
        &c,
        &dest,
        "/org/a11y/atspi/accessible/nope",
        ACC,
        "GetRole",
        &(),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("UnknownObject"), "{e}");
    let e = call(&c, &dest, &root, ACC, "Nope", &()).await.unwrap_err();
    assert!(e.to_string().contains("UnknownMethod"), "{e}");
    let items = call(
        &c,
        &dest,
        "/org/a11y/atspi/cache",
        "org.a11y.atspi.Cache",
        "GetItems",
        &(),
    )
    .await
    .unwrap();
    let v: Vec<(
        (String, OwnedObjectPath),
        (String, OwnedObjectPath),
        (String, OwnedObjectPath),
        i32,
        i32,
        Vec<String>,
        String,
        u32,
        String,
        Vec<u32>,
    )> = items.body().deserialize().unwrap();
    assert!(v.len() > 300, "{}", v.len());
    let s = serde_json::from_value::<gorca_testkit::b1::proto::AppStats>(
        app.control(Cmd::Stats).await.data,
    )
    .unwrap();
    assert_eq!(s.member_calls("GetItems"), 1);
    assert_eq!(s.calls_unknown_object, 1);
    assert_eq!(s.bus_callers, 1);
}

/// D-1: one request at a time across all connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_from_all_connections_are_served_one_at_a_time() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Settings, P2pMode::SameBus, 2.0, 2.0), "serial")
        .await;
    let (dest, root) = (app.ready.bus_name.clone(), app.ready.root.clone());
    let clients = [bus.client().await, bus.client().await, bus.client().await];
    let t = Instant::now();
    let mut tasks = Vec::new();
    for c in clients {
        let (d, r) = (dest.clone(), root.clone());
        tasks.push(tokio::spawn(async move {
            for _ in 0..10 {
                call(&c, &d, &r, ACC, "GetRole", &()).await.unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let el = t.elapsed().as_secs_f64() * 1e3;
    // 30 calls x 2 ms in series = 60 ms; in parallel it would be ~20 ms.
    assert!(el >= 54.0, "served in parallel? {el:.1} ms");
}

/// D-1 head-of-line: a GetItems of several hundred nodes delays the call queued behind it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_items_blocks_the_calls_queued_behind_it() {
    let bus = Bus::start();
    let mut sp = spec(Shape::Settings, P2pMode::SameBus, 0.05, 0.1);
    sp.nodes_idle = 1000;
    sp.getitems_us_per_item = 400.0; // ~400 ms
    let app = bus.app(sp, "hol").await;
    let (dest, root) = (app.ready.bus_name.clone(), app.ready.root.clone());
    let c1 = bus.client().await;
    let c2 = bus.client().await;
    let d2 = dest.clone();
    let dump = tokio::spawn(async move {
        call(
            &c1,
            &d2,
            "/org/a11y/atspi/cache",
            "org.a11y.atspi.Cache",
            "GetItems",
            &(),
        )
        .await
        .unwrap();
    });
    // The control channel queues behind the dump, so the order cannot be observed; instead the
    // dump is long (400 ms) and the second call is issued 5 ms in: it takes a 400 ms scheduling
    // stall to miss the window, not the 25 ms a short dump would tolerate.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let t = Instant::now();
    call(&c2, &dest, &root, ACC, "GetRole", &()).await.unwrap();
    let el = t.elapsed().as_secs_f64() * 1e3;
    dump.await.unwrap();
    assert!(el > 15.0, "no head-of-line blocking: {el:.1} ms");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evented_mutations_emit_signals_silent_ones_do_not() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "sig")
        .await;
    let c = bus.client().await;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .unwrap()
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &c, None).await.unwrap();
    let r = app
        .control(Cmd::Rename {
            node: "controls[0]".into(),
            name: "Loud".into(),
            evented: true,
        })
        .await;
    assert!(r.ok, "{r:?}");
    let path = r.str("path").unwrap().to_owned();
    let m = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let h = m.header();
    assert_eq!(h.member().map(|m| m.as_str()), Some("PropertyChange"));
    assert_eq!(h.path().map(|p| p.as_str()), Some(path.as_str()));
    let body: (
        String,
        i32,
        i32,
        zvariant::OwnedValue,
        std::collections::HashMap<String, zvariant::OwnedValue>,
    ) = m.body().deserialize().unwrap();
    assert_eq!(body.0, "accessible-name");
    // Silent: nothing arrives, but a read returns the new name.
    let r = app
        .control(Cmd::Rename {
            node: "controls[1]".into(),
            name: "Quiet".into(),
            evented: false,
        })
        .await;
    assert!(r.ok);
    assert!(
        tokio::time::timeout(Duration::from_millis(200), stream.next())
            .await
            .is_err()
    );
    let p2 = r.str("path").unwrap().to_owned();
    let got = call(
        &c,
        &app.ready.bus_name,
        &p2,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(ACC, "Name"),
    )
    .await
    .unwrap();
    let v: zvariant::OwnedValue = got.body().deserialize().unwrap();
    assert_eq!(String::try_from(v).unwrap(), "Quiet");
    // The truth log has both, with the evented flag.
    let truth = std::fs::read_to_string(bus.dir.join("sig.truth")).unwrap();
    let lines: Vec<gorca_testkit::b1::proto::TruthLine> = truth
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].evented && !lines[1].evented);
    assert!(lines[0].t_ns > 0 && lines[1].t_ns >= lines[0].t_ns);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_mode_serves_a_real_socket_and_counts_it() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Settings, P2pMode::Peer, 0.01, 0.05), "peer")
        .await;
    let addr = app.ready.p2p_address.clone();
    assert!(addr.starts_with("unix:path="), "{addr}");
    let path = addr
        .trim_start_matches("unix:path=")
        .split(',')
        .next()
        .unwrap()
        .to_owned();
    let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let p = zbus::connection::Builder::unix_stream(stream)
        .p2p()
        .build()
        .await
        .unwrap();
    let r = p
        .call_method(
            None::<&str>,
            app.ready.root.as_str(),
            Some(ACC),
            "GetRole",
            &(),
        )
        .await
        .unwrap();
    assert_eq!(r.body().deserialize::<u32>().unwrap(), 75);
    // GetApplicationBusAddress answers the peer address.
    let c = bus.client().await;
    let r = call(
        &c,
        &app.ready.bus_name,
        &app.ready.root,
        "org.a11y.atspi.Application",
        "GetApplicationBusAddress",
        &(),
    )
    .await
    .unwrap();
    assert_eq!(r.body().deserialize::<String>().unwrap(), addr);
    let s: gorca_testkit::b1::proto::AppStats =
        serde_json::from_value(app.control(Cmd::Stats).await.data).unwrap();
    assert_eq!(s.conn_calls(ConnKind::Peer), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_bus_and_none_modes_answer_per_spec() {
    let bus = Bus::start();
    let c = bus.client().await;
    for (mode, want) in [
        (P2pMode::SameBus, Some(bus.address.clone())),
        (P2pMode::None, Some(String::new())),
    ] {
        let app = bus.app(spec(Shape::Generic, mode, 0.01, 0.05), "m").await;
        let r = call(
            &c,
            &app.ready.bus_name,
            &app.ready.root,
            "org.a11y.atspi.Application",
            "GetApplicationBusAddress",
            &(),
        )
        .await
        .unwrap();
        assert_eq!(Some(r.body().deserialize::<String>().unwrap()), want);
        assert!(app.ready.p2p_address.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hang_never_and_delayed_replies() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "hang")
        .await;
    let c = bus.client().await;
    let (dest, root) = (app.ready.bus_name.clone(), app.ready.root.clone());
    // Delay exactly one reply.
    app.control(Cmd::DelayReplies {
        member: "GetRole".into(),
        ms: 1500,
        count: 1,
        conn: None,
    })
    .await;
    let t = Instant::now();
    call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(1400));
    // Upper wall bound: best of 3 (a stuck delay would show in all; a preemption in one). The
    // delay is 1.5 s so the margin dwarfs scheduling noise on a loaded host.
    let mut fastest = Duration::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
        fastest = fastest.min(t.elapsed());
    }
    assert!(fastest < Duration::from_millis(1000), "{fastest:?}");
    // Never: no reply, the call times out client side; recovers on Off.
    app.control(Cmd::Hang {
        mode: gorca_testkit::b1::proto::HangMode::Never,
        ms: 0,
    })
    .await;
    let r = tokio::time::timeout(
        Duration::from_millis(300),
        call(&c, &dest, &root, ACC, "GetRole", &()),
    )
    .await;
    assert!(r.is_err(), "a hung app must not answer");
    app.control(Cmd::Hang {
        mode: gorca_testkit::b1::proto::HangMode::Off,
        ms: 0,
    })
    .await;
    call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_lease_and_destroyed_nodes_answer_unknown_object() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "lease")
        .await;
    let c = bus.client().await;
    let r = app
        .control(Cmd::ExpireLease {
            node: "controls[2]".into(),
        })
        .await;
    let p = r.str("path").unwrap().to_owned();
    let e = call(&c, &app.ready.bus_name, &p, ACC, "GetRole", &())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("UnknownObject"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn storm_emits_signals_at_roughly_the_requested_rate() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Storm, P2pMode::None, 0.01, 0.05), "storm")
        .await;
    let c = bus.client().await;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .unwrap()
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &c, Some(100_000))
        .await
        .unwrap();
    let t0 = Instant::now();
    let r = app
        .control(Cmd::Storm {
            rate_per_s: 10_000,
            duration_ms: 500,
            distinct_nodes: 0,
            targets: Vec::new(),
        })
        .await;
    assert!(r.ok);
    // The storm's count is fixed (rate x duration = 5000); only its pace depends on the clock,
    // so the outcome is asserted on counts and the pacing only as a lower bound on wall time
    // (a ceiling would measure host load). Generous deadlines, polled.
    let deadline = Instant::now() + Duration::from_secs(60);
    let sent = loop {
        let s: gorca_testkit::b1::proto::AppStats =
            serde_json::from_value(app.control(Cmd::Stats).await.data).unwrap();
        if s.signals_sent >= 5_000 || Instant::now() > deadline {
            break s.signals_sent;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let took = t0.elapsed();
    assert_eq!(sent, 5_000, "the storm emits exactly rate x duration");
    assert!(
        took >= Duration::from_millis(400),
        "paced at the requested rate, not blasted out: {took:?}"
    );
    let mut n = 0u32;
    let deadline = Instant::now() + Duration::from_secs(60);
    while n < 5_000 && Instant::now() < deadline {
        if let Ok(Some(Ok(_))) =
            tokio::time::timeout(Duration::from_millis(500), stream.next()).await
        {
            n += 1;
        }
    }
    assert_eq!(n, 5_000, "every emitted signal reaches the listener");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn listener_gate_and_realised_dump_follow_the_p9_profile() {
    let bus = Bus::start();
    let mut sp = spec(Shape::Settings, P2pMode::None, 0.01, 0.05);
    sp.quirks.listener_gate = true;
    sp.quirks.dump_realised_only = true;
    let app = bus.app(sp, "gate").await;
    let c = bus.client().await;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .unwrap()
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &c, None).await.unwrap();
    // No listener: an evented rename emits nothing (P9-1).
    app.control(Cmd::Rename {
        node: "controls[0]".into(),
        name: "a".into(),
        evented: true,
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(200), stream.next())
            .await
            .is_err()
    );
    app.control(Cmd::RegisterListener).await;
    app.control(Cmd::Rename {
        node: "controls[0]".into(),
        name: "b".into(),
        evented: true,
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .is_ok()
    );
    // A fresh dump has 2 items; it grows once a client walks the tree (P-9).
    type Items = Vec<(
        (String, OwnedObjectPath),
        (String, OwnedObjectPath),
        (String, OwnedObjectPath),
        i32,
        i32,
        Vec<String>,
        String,
        u32,
        String,
        Vec<u32>,
    )>;
    let dump = |c: &zbus::Connection| {
        let (c, d) = (c.clone(), app.ready.bus_name.clone());
        async move {
            let r = call(
                &c,
                &d,
                "/org/a11y/atspi/cache",
                "org.a11y.atspi.Cache",
                "GetItems",
                &(),
            )
            .await
            .unwrap();
            r.body().deserialize::<Items>().unwrap().len()
        }
    };
    assert_eq!(dump(&c).await, 2);
    let r = call(
        &c,
        &app.ready.bus_name,
        &app.ready.root,
        ACC,
        "GetChildren",
        &(),
    )
    .await
    .unwrap();
    let kids: Vec<(String, OwnedObjectPath)> = r.body().deserialize().unwrap();
    call(
        &c,
        &app.ready.bus_name,
        kids[0].1.as_str(),
        ACC,
        "GetChildren",
        &(),
    )
    .await
    .unwrap();
    assert!(dump(&c).await > 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closing_peers_ends_the_peer_connection_and_new_ones_work() {
    let bus = Bus::start();
    let app = bus
        .app(spec(Shape::Settings, P2pMode::Peer, 0.01, 0.05), "peer2")
        .await;
    let path = app
        .ready
        .p2p_address
        .trim_start_matches("unix:path=")
        .split(',')
        .next()
        .unwrap()
        .to_owned();
    let connect = || async {
        let s = tokio::net::UnixStream::connect(&path).await.unwrap();
        zbus::connection::Builder::unix_stream(s)
            .p2p()
            .build()
            .await
            .unwrap()
    };
    let p = connect().await;
    p.call_method(
        None::<&str>,
        app.ready.root.as_str(),
        Some(ACC),
        "GetRole",
        &(),
    )
    .await
    .unwrap();
    app.control(Cmd::ClosePeers).await;
    // The close is asynchronous to the control reply: a call racing it may still be served.
    // The property is that the old connection fails promptly once the close lands, so poll
    // with a generous deadline instead of asserting on one call inside a tight timeout.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = None;
    while Instant::now() < deadline {
        let r = tokio::time::timeout(
            Duration::from_secs(10),
            p.call_method(
                None::<&str>,
                app.ready.root.as_str(),
                Some(ACC),
                "GetRole",
                &(),
            ),
        )
        .await;
        let failed = matches!(r, Ok(Err(_)));
        last = Some(r.map(|r| r.is_ok()));
        if failed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        last.as_ref().is_some_and(|r| matches!(r, Ok(false))),
        "the old peer connection must fail: {last:?}"
    );
    let p2 = connect().await;
    p2.call_method(
        None::<&str>,
        app.ready.root.as_str(),
        Some(ACC),
        "GetRole",
        &(),
    )
    .await
    .unwrap();
}
