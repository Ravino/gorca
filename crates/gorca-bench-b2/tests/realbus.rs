//! Real-bus fixtures: a private dbus-daemon and a private dbus-broker. Skipped (with a note)
//! when the binaries are not installed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::disallowed_methods
)]

use std::time::Duration;

use gorca_bench_b2::bus::BusKind;
use gorca_bench_b2::feeder::{AppPlan, ProbePlan};
use gorca_bench_b2::rig::ConsumerCfg;
use gorca_bench_b2::runner::{AppSpec, RunSpec, execute};
use gorca_bench_b2::scenarios::{Ctx, fixture};
use gorca_bench_b2::workload::Mix;

fn have(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

fn e2e(kind: BusKind) {
    let mut spec = RunSpec::new(kind, Duration::from_secs(2));
    spec.verify = true;
    for i in 0..3u64 {
        let mut plan = AppPlan::new(
            &format!("a{i}"),
            70 + i,
            if i == 0 { Mix::Realistic } else { Mix::Fakeapp },
            5_000.0,
        );
        plan.slots = 300;
        plan.probe = Some(ProbePlan {
            period: Duration::from_millis(10),
        });
        spec.apps.push(AppSpec {
            plan,
            cons: ConsumerCfg {
                fold: true,
                check_order: true,
                probes: 400,
                ..ConsumerCfg::default()
            },
        });
    }
    let o = execute(&spec).unwrap();
    assert!(
        o.settled && !o.disconnected(),
        "bus {}: settled={} end={:?}",
        o.bus,
        o.settled,
        o.drain_end
    );
    for a in &o.apps {
        let v = a.verdict.as_ref().unwrap();
        assert!(v.ok(), "{}/{}: {:?}", o.bus, a.role, v.violations);
        assert!(v.compared > 20);
        assert_eq!(a.cons.order_violations, 0);
        assert_eq!(a.cons.misrouted, 0);
    }
    assert_eq!(o.drain.malformed, 0);
    // The broker added the sender field itself: nothing may be unrouted.
    assert_eq!(o.drain.unrouted, 0);
}

#[test]
fn dbus_daemon_end_to_end() {
    if !have("dbus-daemon") {
        eprintln!("SKIP: dbus-daemon not installed");
        return;
    }
    e2e(BusKind::Daemon {
        max_outgoing: 1_000_000_000,
    });
}

#[test]
fn dbus_broker_end_to_end() {
    if !have("dbus-broker-launch") || !have("systemd-socket-activate") || !have("dbus-daemon") {
        eprintln!("SKIP: dbus-broker-launch, systemd-socket-activate or dbus-daemon not installed");
        return;
    }
    e2e(BusKind::Broker {
        pool_bytes: 64 * 1024 * 1024,
    });
}

/// The fixture reproduces F-3: dbus-broker disconnects a reader that does not read, dbus-daemon
/// drops signals instead, and a reader that keeps up is never disconnected by either.
#[test]
fn broker_fixture_reproduces_f3() {
    if !have("dbus-broker-launch") || !have("systemd-socket-activate") || !have("dbus-daemon") {
        eprintln!("SKIP: dbus-broker not installed");
        return;
    }
    let ctx = Ctx {
        quick: true,
        repeats: 1,
        real_buses: vec![],
        existing: None,
    };
    // The fixture runs real daemons; under heavy host load one attempt may be perturbed.
    let mut rows = fixture(&ctx).unwrap();
    if rows
        .iter()
        .any(|r| r.id.starts_with("F3.") && r.value.is_nan())
    {
        rows = fixture(&ctx).unwrap();
    }
    let get = |id: &str, bus: &str| {
        rows.iter()
            .find(|r| r.id == id && r.bus == bus)
            .map(|r| r.value)
    };
    assert_eq!(
        get("F3.slow_reader_eof", "dbus-broker"),
        Some(1.0),
        "dbus-broker must disconnect a slow reader"
    );
    let notes: Vec<String> = rows
        .iter()
        .filter(|r| r.id.starts_with("F3."))
        .map(|r| format!("{} {}: {} ({})", r.id, r.bus, r.value, r.note))
        .collect();
    assert_eq!(
        get("F3.active_reader_eof", "dbus-broker"),
        Some(0.0),
        "F3 rows: {notes:#?}"
    );
    assert_eq!(
        get("F3.slow_reader_eof", "dbus-daemon"),
        Some(0.0),
        "dbus-daemon drops instead of disconnecting"
    );
    assert_eq!(get("F3.active_reader_eof", "dbus-daemon"), Some(0.0));
}
