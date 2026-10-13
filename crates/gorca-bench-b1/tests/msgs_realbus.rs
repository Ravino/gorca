//! H-5 against a real private dbus-daemon: Debug.Stats presence, per-connection counters, and the
//! monitor under a match rule. Skips (with a note) when `dbus-daemon` is missing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods,
    clippy::items_after_statements,
    clippy::cast_possible_truncation
)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gorca_bench_b1::msgs::{Monitor, connection_stats_raw, stats_interface_available};
use gorca_bench_b2::wire::{BusConn, Enc};

struct Daemon {
    child: Child,
    addr: String,
}

impl Daemon {
    fn start() -> Option<Self> {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut line)
            .ok()?;
        let addr = line.trim().to_owned();
        (!addr.is_empty()).then_some(Self { child, addr })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const T: Duration = Duration::from_secs(5);

fn get_id(c: &mut BusConn) {
    c.call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetId",
        None,
        &[],
        T,
    )
    .unwrap();
}

#[test]
fn debug_stats_exists_but_connection_counters_are_gauges_not_totals() {
    let Some(d) = Daemon::start() else {
        eprintln!("SKIP: dbus-daemon not available");
        return;
    };
    let mut c = BusConn::connect(&d.addr, T).unwrap();
    assert!(
        stats_interface_available(&mut c, T),
        "Debian's dbus-daemon exposes Debug.Stats"
    );
    let me = c.unique.clone();
    for _ in 0..25 {
        get_id(&mut c);
    }
    let raw = connection_stats_raw(&mut c, &me, T).unwrap();
    // The reason H-5 uses a monitor: after 25 round trips the "message" counters are queue depths.
    assert_eq!(raw.get("IncomingMessages"), Some(&0), "{raw:?}");
    assert_eq!(raw.get("OutgoingMessages"), Some(&0), "{raw:?}");
}

#[test]
fn monitor_counts_calls_replies_and_per_destination_with_empty_rules() {
    let Some(d) = Daemon::start() else {
        eprintln!("SKIP: dbus-daemon not available");
        return;
    };
    let mon = Monitor::start(&d.addr, &[]).unwrap();
    let mut c = BusConn::connect(&d.addr, T).unwrap();
    for _ in 0..10 {
        get_id(&mut c);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while mon.snapshot().replies < 10 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let n = mon.stop();
    assert!(n.calls >= 10 && n.replies >= 10, "{n:?}");
    assert!(n.calls_to.get("org.freedesktop.DBus").copied().unwrap_or(0) >= 10);
    assert!(
        n.by_member
            .get("org.freedesktop.DBus.GetId")
            .copied()
            .unwrap_or(0)
            >= 10
    );
}

#[test]
fn monitor_counts_signals_under_its_rule_only() {
    let Some(d) = Daemon::start() else {
        eprintln!("SKIP: dbus-daemon not available");
        return;
    };
    let mon = Monitor::start(
        &d.addr,
        &["type='signal',interface='org.example.B1',member='Ping'"],
    )
    .unwrap();
    let mut c = BusConn::connect(&d.addr, T).unwrap();
    // Emit by sending signals through the bus (the daemon routes to monitors).
    let mut out = Vec::new();
    let mut body = Enc::default();
    body.u32(0);
    for (i, member) in ["Ping", "Ping", "Pong", "Ping"].iter().enumerate() {
        out.clear();
        gorca_bench_b2::wire::encode_message(
            &mut out,
            gorca_bench_b2::wire::SIGNAL,
            100 + i as u32,
            &gorca_bench_b2::wire::Fields {
                path: Some("/org/example"),
                interface: Some("org.example.B1"),
                member: Some(member),
                signature: Some("u"),
                ..Default::default()
            },
            &body.buf,
        );
        use std::io::Write as _;
        c.stream.write_all(&out).unwrap();
    }
    get_id(&mut c); // round trip: the signals above are routed before this returns
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while mon.snapshot().signals < 3 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let n = mon.stop();
    assert_eq!(n.signals, 3, "{n:?}");
    assert_eq!(n.by_member.get("org.example.B1.Ping"), Some(&3));
    assert_eq!(n.calls, 0, "method calls do not match a signal rule");
}
