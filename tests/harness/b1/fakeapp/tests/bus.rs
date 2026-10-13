//! The fake app on a private bus: protocol, serial service time, modes, signals, quirks.

#![allow(clippy::disallowed_methods)]

mod common;

use std::time::{Duration, Instant};

use common::{Bus, call};
use futures_lite::StreamExt;
use gorca_testkit::b1::proto::{Cmd, ConnKind, ModelSpec, P2pMode, ServiceSpec, Shape};
use gorca_testkit::b1::service::percentile;
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
    let app = bus.app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "s").await;
    let c = bus.client().await;
    let dest = app.ready.bus_name.clone();
    let root = app.ready.root.clone();
    let r = call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
    assert_eq!(r.body().deserialize::<u32>().unwrap(), 75);
    let r = call(&c, &dest, &root, ACC, "GetChildren", &()).await.unwrap();
    let kids: Vec<(String, OwnedObjectPath)> = r.body().deserialize().unwrap();
    assert_eq!(kids.len(), 1);
    assert_eq!(kids[0].0, dest);
    let e = call(&c, &dest, "/org/a11y/atspi/accessible/nope", ACC, "GetRole", &())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("UnknownObject"), "{e}");
    let e = call(&c, &dest, &root, ACC, "Nope", &()).await.unwrap_err();
    assert!(e.to_string().contains("UnknownMethod"), "{e}");
    let items = call(&c, &dest, "/org/a11y/atspi/cache", "org.a11y.atspi.Cache", "GetItems", &())
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

async fn rtts(c: &zbus::Connection, dest: &str, path: &str, n: usize) -> Vec<u64> {
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        call(c, dest, path, ACC, "GetRole", &()).await.unwrap();
        v.push(u64::try_from(t.elapsed().as_nanos()).unwrap());
    }
    v
}

/// H-10(a): the replies a client sees match the service-time model. The bus and the client add
/// a constant; it is measured with a near-zero model and subtracted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_time_fidelity() {
    let bus = Bus::start();
    let c = bus.client().await;
    let base_app = bus.app(spec(Shape::Settings, P2pMode::SameBus, 0.001, 0.002), "base").await;
    let model_app = bus.app(spec(Shape::Settings, P2pMode::SameBus, 0.5, 2.0), "model").await;
    let b = rtts(&c, &base_app.ready.bus_name, &base_app.ready.root, 600).await;
    let m = rtts(&c, &model_app.ready.bus_name, &model_app.ready.root, 2500).await;
    let (b50, b99) = (percentile(&b, 50.0) as f64 / 1e6, percentile(&b, 99.0) as f64 / 1e6);
    let (m50, m99) = (percentile(&m, 50.0) as f64 / 1e6, percentile(&m, 99.0) as f64 / 1e6);
    let d50 = m50 - b50;
    eprintln!("base p50 {b50:.3} p99 {b99:.3} ms; model p50 {m50:.3} p99 {m99:.3} ms; delta p50 {d50:.3}");
    // The SPEC gate is +-20 % on a quiet host (H-12). `make check` runs on shared, loaded
    // machines (load 10+ measured), so this test allows +-35 % + 50 us; the runner applies the
    // strict gate to its own measurement.
    assert!((d50 - 0.5).abs() <= 0.5 * 0.35 + 0.05, "p50 off: delta {d50:.3} ms vs 0.5 ms");
    assert!(m99 - b99 <= 2.0 + 1.0, "p99 off: {m99:.3} - {b99:.3} ms vs 2.0 + 1 ms");
}

/// D-1: one request at a time across all connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_from_all_connections_are_served_one_at_a_time() {
    let bus = Bus::start();
    let app = bus.app(spec(Shape::Settings, P2pMode::SameBus, 2.0, 2.0), "serial").await;
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
    sp.getitems_us_per_item = 30.0; // ~30 ms
    let app = bus.app(sp, "hol").await;
    let (dest, root) = (app.ready.bus_name.clone(), app.ready.root.clone());
    let c1 = bus.client().await;
    let c2 = bus.client().await;
    let d2 = dest.clone();
    let dump = tokio::spawn(async move {
        call(&c1, &d2, "/org/a11y/atspi/cache", "org.a11y.atspi.Cache", "GetItems", &()).await.unwrap();
    });
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
    let app = bus.app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "sig").await;
    let c = bus.client().await;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .unwrap()
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &c, None).await.unwrap();
    let r = app
        .control(Cmd::Rename { node: "controls[0]".into(), name: "Loud".into(), evented: true })
        .await;
    assert!(r.ok, "{r:?}");
    let path = r.str("path").unwrap().to_owned();
    let m = tokio::time::timeout(Duration::from_secs(2), stream.next()).await.unwrap().unwrap().unwrap();
    let h = m.header();
    assert_eq!(h.member().map(|m| m.as_str()), Some("PropertyChange"));
    assert_eq!(h.path().map(|p| p.as_str()), Some(path.as_str()));
    let body: (String, i32, i32, zvariant::OwnedValue, std::collections::HashMap<String, zvariant::OwnedValue>) =
        m.body().deserialize().unwrap();
    assert_eq!(body.0, "accessible-name");
    // Silent: nothing arrives, but a read returns the new name.
    let r = app
        .control(Cmd::Rename { node: "controls[1]".into(), name: "Quiet".into(), evented: false })
        .await;
    assert!(r.ok);
    assert!(tokio::time::timeout(Duration::from_millis(200), stream.next()).await.is_err());
    let p2 = r.str("path").unwrap().to_owned();
    let got = call(&c, &app.ready.bus_name, &p2, "org.freedesktop.DBus.Properties", "Get", &(ACC, "Name"))
        .await
        .unwrap();
    let v: zvariant::OwnedValue = got.body().deserialize().unwrap();
    assert_eq!(String::try_from(v).unwrap(), "Quiet");
    // The truth log has both, with the evented flag.
    let truth = std::fs::read_to_string(bus.dir.join("sig.truth")).unwrap();
    let lines: Vec<gorca_testkit::b1::proto::TruthLine> =
        truth.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].evented && !lines[1].evented);
    assert!(lines[0].t_ns > 0 && lines[1].t_ns >= lines[0].t_ns);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn peer_mode_serves_a_real_socket_and_counts_it() {
    let bus = Bus::start();
    let app = bus.app(spec(Shape::Settings, P2pMode::Peer, 0.01, 0.05), "peer").await;
    let addr = app.ready.p2p_address.clone();
    assert!(addr.starts_with("unix:path="), "{addr}");
    let path = addr.trim_start_matches("unix:path=").split(',').next().unwrap().to_owned();
    let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    let p = zbus::connection::Builder::unix_stream(stream).p2p().build().await.unwrap();
    let r = p
        .call_method(None::<&str>, app.ready.root.as_str(), Some(ACC), "GetRole", &())
        .await
        .unwrap();
    assert_eq!(r.body().deserialize::<u32>().unwrap(), 75);
    // GetApplicationBusAddress answers the peer address.
    let c = bus.client().await;
    let r = call(&c, &app.ready.bus_name, &app.ready.root, "org.a11y.atspi.Application", "GetApplicationBusAddress", &())
        .await
        .unwrap();
    assert_eq!(r.body().deserialize::<String>().unwrap(), addr);
    let s: gorca_testkit::b1::proto::AppStats = serde_json::from_value(app.control(Cmd::Stats).await.data).unwrap();
    assert_eq!(s.conn_calls(ConnKind::Peer), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_bus_and_none_modes_answer_per_spec() {
    let bus = Bus::start();
    let c = bus.client().await;
    for (mode, want) in [(P2pMode::SameBus, Some(bus.address.clone())), (P2pMode::None, Some(String::new()))] {
        let app = bus.app(spec(Shape::Generic, mode, 0.01, 0.05), "m").await;
        let r = call(&c, &app.ready.bus_name, &app.ready.root, "org.a11y.atspi.Application", "GetApplicationBusAddress", &())
            .await
            .unwrap();
        assert_eq!(Some(r.body().deserialize::<String>().unwrap()), want);
        assert!(app.ready.p2p_address.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hang_never_and_delayed_replies() {
    let bus = Bus::start();
    let app = bus.app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "hang").await;
    let c = bus.client().await;
    let (dest, root) = (app.ready.bus_name.clone(), app.ready.root.clone());
    // Delay exactly one reply.
    app.control(Cmd::DelayReplies { member: "GetRole".into(), ms: 150, count: 1, conn: None }).await;
    let t = Instant::now();
    call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
    assert!(t.elapsed() >= Duration::from_millis(140));
    let t = Instant::now();
    call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
    assert!(t.elapsed() < Duration::from_millis(100));
    // Never: no reply, the call times out client side; recovers on Off.
    app.control(Cmd::Hang { mode: gorca_testkit::b1::proto::HangMode::Never, ms: 0 }).await;
    let r = tokio::time::timeout(Duration::from_millis(300), call(&c, &dest, &root, ACC, "GetRole", &())).await;
    assert!(r.is_err(), "a hung app must not answer");
    app.control(Cmd::Hang { mode: gorca_testkit::b1::proto::HangMode::Off, ms: 0 }).await;
    call(&c, &dest, &root, ACC, "GetRole", &()).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expired_lease_and_destroyed_nodes_answer_unknown_object() {
    let bus = Bus::start();
    let app = bus.app(spec(Shape::Settings, P2pMode::SameBus, 0.01, 0.05), "lease").await;
    let c = bus.client().await;
    let r = app.control(Cmd::ExpireLease { node: "controls[2]".into() }).await;
    let p = r.str("path").unwrap().to_owned();
    let e = call(&c, &app.ready.bus_name, &p, ACC, "GetRole", &()).await.unwrap_err();
    assert!(e.to_string().contains("UnknownObject"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn storm_emits_signals_at_roughly_the_requested_rate() {
    let bus = Bus::start();
    let app = bus.app(spec(Shape::Storm, P2pMode::None, 0.01, 0.05), "storm").await;
    let c = bus.client().await;
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("org.a11y.atspi.Event.Object")
        .unwrap()
        .build();
    let mut stream = MessageStream::for_match_rule(rule, &c, Some(100_000)).await.unwrap();
    let r = app.control(Cmd::Storm { rate_per_s: 10_000, duration_ms: 500 }).await;
    assert!(r.ok);
    let mut n = 0u32;
    let end = Instant::now() + Duration::from_millis(900);
    while Instant::now() < end {
        if let Ok(Some(Ok(_))) = tokio::time::timeout(Duration::from_millis(100), stream.next()).await {
            n += 1;
        }
    }
    assert!((3500..=6000).contains(&n), "{n} events in 0.5 s at 10k/s");
}
