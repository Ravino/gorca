//! H-10(a): service-time fidelity. Its own test binary so no other test competes for the cores.

#![allow(clippy::disallowed_methods)]

mod common;

use std::time::Instant;

use common::{Bus, call};
use gorca_testkit::b1::proto::{ModelSpec, P2pMode, ServiceSpec, Shape};
use gorca_testkit::b1::service::percentile;

const ACC: &str = "org.a11y.atspi.Accessible";

fn spec(shape: Shape, p2p: P2pMode, p50: f64, p99: f64) -> ModelSpec {
    ModelSpec {
        shape,
        p2p,
        nodes_idle: 400,
        sidebar_rows: 20,
        service: ServiceSpec { p50_ms: p50, p99_ms: p99 },
        ..ModelSpec::default()
    }
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

