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
/// A wall-clock fidelity check of a sleep/spin model cannot be reliable on an overloaded host
/// (scheduler delay lands on the percentiles), so there are two modes:
/// - `B1_STRICT=1` (set by `bench/b1/run-all.sh`, authoritative runs on a quiet host): one
///   measurement, p50 delta within +-20 % of the model and p99 delta within +1 ms of it.
///   `run-all.sh` records the result in the run's host meta and marks a failing run INVALID.
/// - default (`make check`, shared host, load 10 to 70): a gross-breakage sanity check only. The
///   p50 delta must be at least a third of the model (an ignored model, zero delay, fails every
///   attempt: that bound cannot be inflated by load) and at most 20x of it (a hang or a runaway
///   sleep; a loaded host measured 2.8 to 3.8 ms for the 0.5 ms model, so a tighter ceiling
///   judges the scheduler), best of up to 3 measurements. A model that is merely too slow by a
///   small factor is caught by the `B1_STRICT` run, which is the authoritative fidelity gate.
///   The p99 is not checked, since it is dominated by scheduler noise.
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
    // `B1_FIDELITY_TEST_SCALE` is a test-only knob to prove the relaxed check catches a broken
    // model (0 = zero delay, 40 = a 20 ms runaway); unset in normal runs. A 10x model is only
    // caught by the `B1_STRICT` run.
    let scale: f64 = std::env::var("B1_FIDELITY_TEST_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    let model_app = bus
        .app(
            spec(Shape::Settings, P2pMode::SameBus, 0.5 * scale, 2.0 * scale),
            "model",
        )
        .await;
    let strict = std::env::var_os("B1_STRICT").is_some();
    let attempts = if strict { 1 } else { 3 };
    let mut last = String::new();
    for attempt in 1..=attempts {
        let (d50, d99) = measure(
            &c,
            (&base_app.ready.bus_name, &base_app.ready.root),
            (&model_app.ready.bus_name, &model_app.ready.root),
        )
        .await;
        let ok = if strict {
            (d50 - 0.5).abs() <= 0.5 * 0.20 && d99 <= 2.0 + 1.0
        } else {
            (0.5 / 3.0..=0.5 * 20.0).contains(&d50)
        };
        if ok {
            return;
        }
        last = format!(
            "attempt {attempt}: p50 delta {d50:.3} ms, p99 delta {d99:.3} ms (strict={strict})"
        );
    }
    panic!("service-time model off in all {attempts} attempts; last {last}");
}
