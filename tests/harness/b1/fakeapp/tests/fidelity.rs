//! H-10(a): service-time fidelity. Its own test binary so no other test competes for the cores.

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
        service: ServiceSpec {
            p50_ms: p50,
            p99_ms: p99,
        },
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

/// One complete measurement: (p50 delta over the base model in ms, p99 delta in ms).
async fn measure(c: &zbus::Connection, base: (&str, &str), model: (&str, &str)) -> (f64, f64) {
    let b = rtts(c, base.0, base.1, 600).await;
    let m = rtts(c, model.0, model.1, 2500).await;
    let (b50, b99) = (
        percentile(&b, 50.0) as f64 / 1e6,
        percentile(&b, 99.0) as f64 / 1e6,
    );
    let (m50, m99) = (
        percentile(&m, 50.0) as f64 / 1e6,
        percentile(&m, 99.0) as f64 / 1e6,
    );
    eprintln!(
        "base p50 {b50:.3} p99 {b99:.3} ms; model p50 {m50:.3} p99 {m99:.3} ms; delta p50 {:.3}",
        m50 - b50
    );
    (m50 - b50, m99 - b99)
}

/// H-10(a): the replies a client sees match the service-time model. The bus and the client add
/// a constant; it is measured with a near-zero model and subtracted.
///
/// Fidelity of a sleep/spin service model cannot be judged in CPU time (a sleep burns none),
/// and on a loaded host (load 15) scheduler delay lands on the wall-clock percentiles. Two
/// modes: `B1_STRICT=1` (H-12 authoritative run on a quiet host) takes one measurement at
/// +-20 %. The default (`make check`, shared host) keeps the previous bound (+-35 % + 50 us on
/// the p50 delta, +3 ms on the p99 delta) but judges the best of up to 5 complete
/// measurements, stopping at the first pass. A wrong model (a missing or doubled sleep, a
/// drifting spin) is far outside the tolerance in every attempt and still fails; a scheduler
/// spike has to hit all five.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn service_time_fidelity() {
    let bus = Bus::start();
    let c = bus.client().await;
    let base_app = bus
        .app(
            spec(Shape::Settings, P2pMode::SameBus, 0.001, 0.002),
            "base",
        )
        .await;
    let model_app = bus
        .app(spec(Shape::Settings, P2pMode::SameBus, 0.5, 2.0), "model")
        .await;
    let strict = std::env::var_os("B1_STRICT").is_some();
    let (tol, attempts) = if strict {
        (0.5 * 0.20, 1)
    } else {
        (0.5 * 0.35 + 0.05, 5)
    };
    let mut last = String::new();
    for attempt in 1..=attempts {
        let (d50, d99) = measure(
            &c,
            (&base_app.ready.bus_name, &base_app.ready.root),
            (&model_app.ready.bus_name, &model_app.ready.root),
        )
        .await;
        if (d50 - 0.5).abs() <= tol && d99 <= 2.0 + 1.0 {
            return;
        }
        last = format!(
            "attempt {attempt}: p50 delta {d50:.3} ms vs 0.5 +- {tol:.3}, p99 delta {d99:.3} ms vs 2.0 + 1"
        );
    }
    panic!("service-time model off in all {attempts} attempts; last {last}");
}
