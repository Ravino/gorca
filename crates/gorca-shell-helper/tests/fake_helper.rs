// SPDX-License-Identifier: GPL-3.0-or-later
//! The helper client against a fake `org.gorca.ShellHelper` on a private `dbus-daemon`
//! (ADR-0025 MO-2): probe and availability, owner check (MI-7), `not-after`, deadlines, queue
//! rules (EX-3), expiry after a stall (C-1), error mapping. Skips when `dbus-daemon` is missing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test support code

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gorca_output_api::{
    Button, ButtonAct, ClickCount, ClickOpts, IfCovered, OutcomeKind, PointTarget, PointerAvail,
    PointerError, PointerSink,
};
use gorca_rt::{Deadline, monotonic_ns, watch};
use gorca_shell_helper::ShellHelperClient;
use zbus::zvariant::{OwnedValue, Value};

// ---- private bus -------------------------------------------------------------------------

struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Self> {
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(&dir).ok()?;
        let cfg = dir.join(format!("shell-helper-bus-{}.conf", std::process::id()));
        std::fs::write(
            &cfg,
            format!(
                "<busconfig><type>session</type><listen>unix:dir={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\"/><allow own=\"*\"/>\
                 <allow receive_sender=\"*\"/></policy></busconfig>",
                dir.display()
            ),
        )
        .ok()?;
        let mut child = Command::new("dbus-daemon")
            .arg("--config-file")
            .arg(&cfg)
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut line = String::new();
        BufReader::new(child.stdout.take()?)
            .read_line(&mut line)
            .ok()?;
        let address = line.trim().to_owned();
        if address.is_empty() {
            let _ = child.kill();
            return None;
        }
        Some(Self { child, address })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        // Our own child only (spawned above); nothing is matched by name.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn connect(address: &str) -> zbus::Connection {
    zbus::connection::Builder::address(address)
        .unwrap()
        .method_timeout(Duration::from_secs(10))
        .build()
        .await
        .unwrap()
}

// ---- the fake helper ---------------------------------------------------------------------

#[derive(Default)]
struct State {
    version: u32,
    caps: Vec<String>,
    /// Pretend the Shell is stalled: every call sleeps this long before running.
    stall: Duration,
    /// `Ping` and everything else answers AccessDenied.
    deny: bool,
    /// `Error.Failed` text for injecting calls.
    fail: Option<String>,
    /// Every call: (method, args summary).
    calls: Vec<String>,
    /// `not-after` of the last injecting call minus the monotonic clock when it arrived (us).
    last_not_after_ahead_us: Option<i64>,
    /// Injections that were actually performed (not expired).
    injected: u32,
    held: u32,
}

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.gorca.ShellHelper.Error")]
enum HelperErr {
    #[zbus(error)]
    ZBus(zbus::Error),
    AccessDenied(String),
    Failed(String),
}

#[derive(Clone)]
struct Fake(Arc<Mutex<State>>);

impl Fake {
    fn new(version: u32, caps: &[&str]) -> Self {
        Self(Arc::new(Mutex::new(State {
            version,
            caps: caps.iter().map(|s| (*s).to_owned()).collect(),
            ..State::default()
        })))
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    async fn gate(&self, name: &str) -> Result<(), HelperErr> {
        let (stall, deny) = self.with(|s| {
            s.calls.push(name.to_owned());
            (s.stall, s.deny)
        });
        if !stall.is_zero() {
            tokio::time::sleep(stall).await;
        }
        if deny {
            return Err(HelperErr::AccessDenied("not allow-listed".to_owned()));
        }
        Ok(())
    }

    fn outcome(
        &self,
        opts: &HashMap<String, OwnedValue>,
        x: f64,
        y: f64,
    ) -> Result<HashMap<String, OwnedValue>, HelperErr> {
        let not_after = opts
            .get("not-after")
            .and_then(|v| v.downcast_ref::<i64>().ok())
            .ok_or_else(|| HelperErr::Failed("bad-args: not-after required".to_owned()))?;
        let now = i64::try_from(monotonic_ns() / 1000).unwrap_or(0);
        if let Some(f) = self.with(|s| s.fail.clone()) {
            return Err(HelperErr::Failed(f));
        }
        let expired = now > not_after;
        self.with(|s| {
            s.last_not_after_ahead_us = Some(not_after - now);
            if !expired {
                s.injected += 1;
            }
        });
        let ov = |v: Value<'_>| OwnedValue::try_from(v).unwrap();
        let mut m = HashMap::new();
        m.insert("x".to_owned(), ov(Value::from(x)));
        m.insert("y".to_owned(), ov(Value::from(y)));
        m.insert("window-at".to_owned(), ov(Value::from(0_u64)));
        m.insert("occluded".to_owned(), ov(Value::from(false)));
        m.insert("clamped".to_owned(), ov(Value::from(false)));
        m.insert(
            "outcome".to_owned(),
            ov(Value::from(if expired { "expired" } else { "done" })),
        );
        m.insert("held".to_owned(), ov(Value::from(self.with(|s| s.held))));
        Ok(m)
    }
}

#[zbus::interface(name = "org.gorca.ShellHelper")]
impl Fake {
    async fn ping(&self) -> Result<u32, HelperErr> {
        self.gate("Ping").await?;
        Ok(self.with(|s| s.version))
    }

    async fn capabilities(&self) -> Result<Vec<String>, HelperErr> {
        self.gate("Capabilities").await?;
        Ok(self.with(|s| s.caps.clone()))
    }

    async fn get_pointer(&self) -> Result<HashMap<String, OwnedValue>, HelperErr> {
        self.gate("GetPointer").await?;
        let ov = |v: Value<'_>| OwnedValue::try_from(v).unwrap();
        let mut m = HashMap::new();
        m.insert("x".to_owned(), ov(Value::from(10.0_f64)));
        m.insert("y".to_owned(), ov(Value::from(20.0_f64)));
        m.insert("window".to_owned(), ov(Value::from(42_u64)));
        m.insert("wx".to_owned(), ov(Value::from(1.0_f64)));
        m.insert("wy".to_owned(), ov(Value::from(2.0_f64)));
        m.insert("monitor".to_owned(), ov(Value::from(0_i32)));
        Ok(m)
    }

    async fn pointer_move(
        &self,
        window: u64,
        origin: &str,
        x: f64,
        y: f64,
        opts: HashMap<String, OwnedValue>,
    ) -> Result<HashMap<String, OwnedValue>, HelperErr> {
        self.gate(&format!("PointerMove {window} {origin} {x} {y}"))
            .await?;
        self.outcome(&opts, x, y)
    }

    #[allow(clippy::too_many_arguments)]
    async fn pointer_click_at(
        &self,
        window: u64,
        origin: &str,
        x: f64,
        y: f64,
        button: u32,
        opts: HashMap<String, OwnedValue>,
    ) -> Result<HashMap<String, OwnedValue>, HelperErr> {
        let mut keys: Vec<&String> = opts.keys().collect();
        keys.sort();
        self.gate(&format!(
            "PointerClickAt {window} {origin} {x} {y} b{button} {keys:?}"
        ))
        .await?;
        self.outcome(&opts, x, y)
    }

    async fn pointer_button(
        &self,
        button: u32,
        action: &str,
        opts: HashMap<String, OwnedValue>,
    ) -> Result<HashMap<String, OwnedValue>, HelperErr> {
        self.gate(&format!("PointerButton b{button} {action}"))
            .await?;
        let bit = 1 << (button - 1);
        self.with(|s| match action {
            "press" => s.held |= bit,
            "release" => s.held &= !bit,
            _ => {}
        });
        self.outcome(&opts, 0.0, 0.0)
    }

    async fn pointer_release(&self) -> Result<u32, HelperErr> {
        self.gate("PointerRelease").await?;
        Ok(self.with(|s| {
            let n = s.held.count_ones();
            s.held = 0;
            n
        }))
    }
}

/// A fake that owns both `org.gorca.ShellHelper` and `org.gnome.Shell` (the owners match).
async fn serve(address: &str, fake: &Fake, names: &[&str]) -> zbus::Connection {
    let mut b = zbus::connection::Builder::address(address)
        .unwrap()
        .serve_at("/org/gorca/ShellHelper", fake.clone())
        .unwrap();
    for n in names {
        b = b.name(*n).unwrap();
    }
    b.build().await.unwrap()
}

const BOTH: [&str; 2] = ["org.gorca.ShellHelper", "org.gnome.Shell"];

// ---- helpers -----------------------------------------------------------------------------

async fn wait_avail(
    rx: &mut watch::Receiver<PointerAvail>,
    want: impl Fn(PointerAvail) -> bool,
    secs: u64,
) -> PointerAvail {
    let r = tokio::time::timeout(Duration::from_secs(secs), async {
        loop {
            let cur = *rx.borrow_and_update();
            if want(cur) {
                return cur;
            }
            if rx.changed().await.is_err() {
                return *rx.borrow();
            }
        }
    })
    .await;
    r.unwrap_or_else(|_| *rx.borrow())
}

fn dl(ms: u64) -> Deadline {
    Deadline(Instant::now() + Duration::from_millis(ms))
}

macro_rules! bus_or_skip {
    () => {
        match Bus::start() {
            Some(b) => b,
            None => {
                eprintln!("skipped: dbus-daemon unavailable");
                return;
            }
        }
    };
}

async fn ready_client(
    bus: &Bus,
    fake: &Fake,
) -> (ShellHelperClient, zbus::Connection, zbus::Connection) {
    let srv = serve(&bus.address, fake, &BOTH).await;
    let conn = connect(&bus.address).await;
    let (client, _task) = ShellHelperClient::start(conn.clone());
    let mut rx = client.availability();
    let a = wait_avail(&mut rx, PointerAvail::can_call, 5).await;
    assert_eq!(a, PointerAvail::Ready { version: 2 });
    (client, srv, conn)
}

// ---- availability --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn absent_then_ready_then_absent_again_with_the_owner() {
    let bus = bus_or_skip!();
    let conn = connect(&bus.address).await;
    let (client, _t) = ShellHelperClient::start(conn);
    let mut rx = client.availability();
    assert_eq!(*rx.borrow(), PointerAvail::Absent);
    let fake = Fake::new(2, &["capture", "pointer"]);
    let srv = serve(&bus.address, &fake, &BOTH).await;
    assert_eq!(
        wait_avail(&mut rx, PointerAvail::can_call, 5).await,
        PointerAvail::Ready { version: 2 }
    );
    drop(srv); // helper hot reload / shell restart: the owner vanishes
    assert_eq!(
        wait_avail(&mut rx, |a| a == PointerAvail::Absent, 5).await,
        PointerAvail::Absent
    );
    // a new owner (the extension reloaded) is picked up again
    let _srv2 = serve(&bus.address, &fake, &BOTH).await;
    assert_eq!(
        wait_avail(&mut rx, PointerAvail::can_call, 5).await,
        PointerAvail::Ready { version: 2 }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_squatter_owning_the_helper_name_is_never_called() {
    let bus = bus_or_skip!();
    let squatter = Fake::new(2, &["capture", "pointer"]);
    let _s1 = serve(&bus.address, &squatter, &["org.gorca.ShellHelper"]).await;
    let shell = Fake::new(2, &[]);
    let _s2 = serve(&bus.address, &shell, &["org.gnome.Shell"]).await;
    let (client, _t) = ShellHelperClient::start(connect(&bus.address).await);
    let mut rx = client.availability();
    assert_eq!(
        wait_avail(&mut rx, |a| a == PointerAvail::Squatted, 5).await,
        PointerAvail::Squatted
    );
    let r = client.position(dl(500)).await;
    assert_eq!(
        r.unwrap_err(),
        PointerError::Unavailable(PointerAvail::Squatted)
    );
    assert!(
        squatter.with(|s| s.calls.is_empty()),
        "no call may reach a squatter"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn too_old_no_cap_and_denied_are_distinguished() {
    for (fake, want) in [
        (
            Fake::new(1, &["capture"]),
            PointerAvail::TooOld { version: 1 },
        ),
        (Fake::new(2, &["capture"]), PointerAvail::NoCap),
        (
            {
                let f = Fake::new(2, &["capture", "pointer"]);
                f.with(|s| s.deny = true);
                f
            },
            PointerAvail::Denied,
        ),
    ] {
        let bus = bus_or_skip!();
        let _srv = serve(&bus.address, &fake, &BOTH).await;
        let (client, _t) = ShellHelperClient::start(connect(&bus.address).await);
        let mut rx = client.availability();
        assert_eq!(wait_avail(&mut rx, |a| a == want, 5).await, want);
        assert!(!want.gate_open());
        // Fixing the allow-list is noticed by the poll or by any later probe; here the owner edit is
        // simulated by flipping the fake and waiting for the 10 s poll would be slow, so only the
        // refusal is asserted.
        assert_eq!(
            client.position(dl(300)).await.unwrap_err(),
            PointerError::Unavailable(want)
        );
    }
}

// ---- calls ---------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn click_at_a_stage_point_sends_the_wire_request_with_not_after() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    let opts = ClickOpts {
        if_covered: IfCovered::Proceed,
        count: ClickCount::Two,
    };
    let o = client
        .click_at(
            PointTarget::Stage { x: 12.5, y: 40.0 },
            Button::Right,
            opts,
            dl(150),
        )
        .await
        .unwrap();
    assert_eq!(o.kind, OutcomeKind::Done);
    assert_eq!((o.x, o.y), (12.5, 40.0));
    let calls = fake.with(|s| s.calls.clone());
    let last = calls.last().unwrap();
    assert!(
        last.starts_with("PointerClickAt 0 stage 12.5 40 b3"),
        "{last}"
    );
    assert!(
        last.contains("count") && last.contains("if-occluded") && last.contains("not-after"),
        "{last}"
    );
    let ahead = fake.with(|s| s.last_not_after_ahead_us).unwrap();
    assert!(
        (0..=150_000).contains(&ahead),
        "not-after must be within the call deadline, was {ahead} us ahead"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buttons_release_and_position_round_trip() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    let o = client
        .button(Button::Left, ButtonAct::Press, dl(300))
        .await
        .unwrap();
    assert!(o.holds(Button::Left));
    let o = client
        .button(Button::Right, ButtonAct::Press, dl(300))
        .await
        .unwrap();
    assert!(o.holds(Button::Left) && o.holds(Button::Right));
    assert_eq!(client.release_all(dl(300)).await.unwrap(), 2);
    let p = client.position(dl(300)).await.unwrap();
    assert_eq!((p.window, p.monitor), (42, 0));
    client
        .button(Button::Left, ButtonAct::DoubleClick, dl(300))
        .await
        .unwrap();
    let calls = fake.with(|s| s.calls.clone());
    assert!(calls.iter().any(|c| c == "PointerButton b1 click"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_window_target_has_no_location_until_the_matcher_exists() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    let top = gorca_output_api::ToplevelHint {
        pid: 1,
        title: Arc::from("t"),
        atspi_origin: (0, 0),
        atspi_size: (100, 100),
        child_space: gorca_output_api::ChildSpace::Toplevel,
    };
    let r = client
        .click_at(
            PointTarget::Window {
                top,
                x: 1.0,
                y: 1.0,
            },
            Button::Left,
            ClickOpts::default(),
            dl(300),
        )
        .await;
    assert_eq!(r.unwrap_err(), PointerError::NoLocation);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn helper_errors_map_to_pointer_errors() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    for (text, want) in [
        ("locked: session is locked", PointerError::Locked),
        ("busy: another caller holds a button", PointerError::Busy),
        (
            "rate: pointer rate limit exceeded",
            PointerError::RateLimited,
        ),
        ("no-window: no such window 5", PointerError::NoWindow),
    ] {
        fake.with(|s| s.fail = Some(text.to_owned()));
        let r = client
            .click_at(
                PointTarget::Stage { x: 1.0, y: 1.0 },
                Button::Left,
                ClickOpts::default(),
                dl(300),
            )
            .await;
        assert_eq!(r.unwrap_err(), want, "{text}");
    }
    // These are answers of a live helper: the availability stays Ready.
    assert_eq!(
        *client.availability().borrow(),
        PointerAvail::Ready { version: 2 }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn access_denied_on_a_call_reprobes_and_closes_the_gate() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    fake.with(|s| s.deny = true); // the owner removed gorca from the allow-list
    let r = client
        .click_at(
            PointTarget::Stage { x: 1.0, y: 1.0 },
            Button::Left,
            ClickOpts::default(),
            dl(300),
        )
        .await;
    assert_eq!(r.unwrap_err(), PointerError::Denied);
    let mut rx = client.availability();
    assert_eq!(
        wait_avail(&mut rx, |a| a == PointerAvail::Denied, 5).await,
        PointerAvail::Denied
    );
}

// ---- stalls: deadlines, overdue stop, expiry, unresponsive ----------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_shell_times_out_drops_the_queue_and_injects_nothing_late() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    let client = Arc::new(client);
    fake.with(|s| s.stall = Duration::from_millis(500));
    let injected0 = fake.with(|s| s.injected);

    let t0 = Instant::now();
    let c1 = {
        let c = Arc::clone(&client);
        tokio::spawn(async move {
            c.click_at(
                PointTarget::Stage { x: 1.0, y: 1.0 },
                Button::Left,
                ClickOpts::default(),
                dl(150),
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    let c2 = {
        let c = Arc::clone(&client);
        tokio::spawn(async move {
            c.click_at(
                PointTarget::Stage { x: 2.0, y: 2.0 },
                Button::Left,
                ClickOpts::default(),
                dl(5_000),
            )
            .await
        })
    };
    let r1 = c1.await.unwrap();
    assert_eq!(r1.unwrap_err(), PointerError::Timeout);
    assert!(
        t0.elapsed() < Duration::from_millis(350),
        "caller waited {:?}",
        t0.elapsed()
    );
    // the waiting request is dropped, not dispatched, with NotResponding
    assert_eq!(c2.await.unwrap().unwrap_err(), PointerError::NotResponding);
    // new requests are refused while the call is overdue
    let r3 = client
        .click_at(
            PointTarget::Stage { x: 3.0, y: 3.0 },
            Button::Left,
            ClickOpts::default(),
            dl(300),
        )
        .await;
    assert_eq!(r3.unwrap_err(), PointerError::NotResponding);
    // the Shell wakes up, runs the stale click after its not-after: nothing is injected
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        fake.with(|s| s.injected),
        injected0,
        "no click may be injected after not-after"
    );
    // the late answer (outcome expired) ended the overdue state
    fake.with(|s| s.stall = Duration::ZERO);
    let mut ok = None;
    for _ in 0..30 {
        let r = client
            .click_at(
                PointTarget::Stage { x: 4.0, y: 4.0 },
                Button::Left,
                ClickOpts::default(),
                dl(300),
            )
            .await;
        if let Ok(o) = r {
            ok = Some(o);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(ok.map(|o| o.kind), Some(OutcomeKind::Done));
    assert_eq!(fake.with(|s| s.injected), injected0 + 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_misses_make_it_unresponsive_keep_the_gate_and_recover_by_probe() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    let mut rx = client.availability();
    fake.with(|s| s.stall = Duration::from_millis(400));
    for i in 0..3 {
        let r = client
            .click_at(
                PointTarget::Stage { x: 1.0, y: 1.0 },
                Button::Left,
                ClickOpts::default(),
                dl(100),
            )
            .await;
        assert!(r.is_err(), "miss {i}: {r:?}");
        // wait for the overdue call to end so the next one is dispatched
        tokio::time::sleep(Duration::from_millis(900)).await;
    }
    let a = wait_avail(&mut rx, |a| a == PointerAvail::Unresponsive, 5).await;
    assert_eq!(a, PointerAvail::Unresponsive);
    assert!(a.gate_open(), "C-3: the gate stays open");
    let calls_before = fake.with(|s| s.calls.len());
    let r = client
        .click_at(
            PointTarget::Stage { x: 1.0, y: 1.0 },
            Button::Left,
            ClickOpts::default(),
            dl(300),
        )
        .await;
    assert_eq!(r.unwrap_err(), PointerError::NotResponding);
    assert_eq!(
        fake.with(|s| s.calls.len()),
        calls_before,
        "no helper call while unresponsive"
    );
    // the Shell recovers: the re-probe (2 s) finds it
    fake.with(|s| s.stall = Duration::ZERO);
    assert_eq!(
        wait_avail(&mut rx, PointerAvail::can_call, 8).await,
        PointerAvail::Ready { version: 2 }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_queue_holds_one_in_flight_plus_four_and_refuses_the_rest() {
    let bus = bus_or_skip!();
    let fake = Fake::new(2, &["capture", "pointer"]);
    let (client, _srv, _conn) = ready_client(&bus, &fake).await;
    let client = Arc::new(client);
    fake.with(|s| s.stall = Duration::from_millis(150));
    let mut tasks = Vec::new();
    for i in 0..8 {
        let c = Arc::clone(&client);
        tasks.push(tokio::spawn(async move {
            c.click_at(
                PointTarget::Stage {
                    x: f64::from(i),
                    y: 1.0,
                },
                Button::Left,
                ClickOpts::default(),
                dl(10_000),
            )
            .await
        }));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let mut ok = 0;
    let mut busy = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(PointerError::Busy) => busy += 1,
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
    assert_eq!(ok + busy, 8);
    assert!((5..=6).contains(&ok) && busy >= 2, "ok={ok} busy={busy}");
}
