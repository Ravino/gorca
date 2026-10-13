//! The invalidation conformance suite IC-1..IC-23 (B-1 SPEC 5.7) against the real daemon: the
//! real connector, app actors, mirror, driver and Reader run in this process on isolated buses,
//! `b1-fakeapp` processes play the applications, the presentation log (PW-10) is what the suite
//! judges, and the test-only debug accessor ([`DaemonProbe`]) lets each case prove that it
//! exercised the stale value it claims to.
//!
//! A case whose precondition this daemon (R0, real time) cannot offer is reported as skipped by
//! name, never as passed: virtual time (IC-10), latency knobs (IC-11), cache fill (IC-12, IC-22),
//! hostile-ref counters (IC-16), where-am-I (IC-23).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::disallowed_methods
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gorca_daemon::{DaemonProbe, Options, PolicyFactory, ProbeRead, SpeechMode, run_with_policy};
use gorca_mirror::fill::scripted::PrefetchAllPolicy;
use gorca_rt::CancellationToken;
use gorca_testkit::MockSpeechServer;
use gorca_testkit::b1::ic::{self, Outcome};
use gorca_testkit::b1::proto::{AppStats, Cmd, CmdReply, ModelSpec, TruthLine};
use gorca_testkit::b1::real::{MirrorProbe, RealConfig, RealSut, default_models_dir};
use gorca_testkit::b1::sut::{Feature, Presentation, Probe, Sut, SutResult};
use gorca_testkit::b1::world::state;
use gorca_testkit::harness::Session;

/// Worker threads of the daemon's runtime in these tests.
const DAEMON_WORKERS: usize = 4;

static BUILT: std::sync::Once = std::sync::Once::new();

/// `target/<profile>/b1-fakeapp`, built (a no-op when current) so that it never lags the source.
fn fakeapp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(|d| d.parent()).unwrap().to_path_buf();
    let bin = dir.join("b1-fakeapp");
    BUILT.call_once(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let mut cmd = std::process::Command::new(cargo);
        cmd.args(["build", "-p", "b1-fakeapp", "--offline"]);
        if dir.file_name().is_some_and(|n| n == "release") {
            cmd.arg("--release");
        }
        let ok = cmd.status().is_ok_and(|s| s.success());
        assert!(ok, "b1-fakeapp does not build");
    });
    assert!(bin.exists(), "b1-fakeapp is not built");
    bin
}

#[derive(Debug)]
struct Probing(DaemonProbe);

impl MirrorProbe for Probing {
    fn probe(&self, bus: &str, path: &str) -> Probe {
        match self.0.read(bus, path) {
            ProbeRead::Miss => Probe::Miss,
            ProbeRead::Gone => Probe::Gone,
            ProbeRead::Hit {
                name,
                states,
                value,
                stale,
                partial,
            } => Probe::Hit {
                name,
                states: states
                    .iter()
                    .filter_map(|n| state::by_name(n))
                    .fold(0u64, |m, b| m | (1u64 << b)),
                value,
                stale,
                partial,
            },
        }
    }

    fn nodes(&self, bus: &str) -> Option<usize> {
        self.0.nodes(bus)
    }
}

/// One fake desktop: a private session, the daemon running in a thread of this process, and the
/// `RealSut` that starts the fake applications and reads the presentation log.
struct DaemonSut {
    sut: RealSut,
    token: CancellationToken,
    thread: Option<JoinHandle<()>>,
    /// The fill policy is the test policy that requests everything (IC-12, IC-22).
    fills: bool,
    /// The daemon's runtime, to keep its workers busy (`stall_reader`).
    runtime: tokio::runtime::Handle,
    /// Apps are not told that a listener registered until the case says so (IC-21).
    defer_registration: bool,
    _session: Session,
}

impl Drop for DaemonSut {
    fn drop(&mut self) {
        self.token.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl DaemonSut {
    /// The daemon registered its event classes before the application started. The registry of a
    /// real desktop tells a starting application so (atk-bridge lifts the GTK4 listener gate on
    /// it); the isolated bus has no registry daemon, so the fixture says it.
    fn listener_registered(&mut self, label: &str) -> SutResult<()> {
        if self.defer_registration {
            return Ok(());
        }
        self.sut.cmd(label, Cmd::RegisterListener).map(|_| ())
    }
}

macro_rules! delegate {
    ($(fn $name:ident(&self $(, $a:ident: $t:ty)*) -> $r:ty;)*) => {
        $(fn $name(&self $(, $a: $t)*) -> $r { self.sut.$name($($a),*) })*
    };
}

impl Sut for DaemonSut {
    delegate! {
        fn arm(&self) -> String;
        fn now_ns(&self) -> u64;
        fn presentations(&self) -> Vec<Presentation>;
        fn truth(&self) -> Vec<TruthLine>;
        fn probe(&self, app: &str, path: &str) -> Probe;
        fn mirror_nodes(&self, bus_name: &str) -> Option<usize>;
        fn bus_name(&self, app: &str) -> String;
    }

    fn supports(&self, f: Feature) -> bool {
        match f {
            Feature::CacheFill => self.fills,
            Feature::LatencyKnobs
            | Feature::Probe
            | Feature::VirtualTime
            | Feature::WhereAmI
            | Feature::Malformed => self.sut.supports(f),
        }
    }
    fn stall_reader(&mut self, ms: u64) {
        // Every worker of the daemon's runtime sleeps: the app actors cannot run, while the
        // event drain (a thread of its own) keeps filling their ingress.
        for _ in 0..DAEMON_WORKERS {
            self.runtime.spawn(async move {
                std::thread::sleep(Duration::from_millis(ms));
            });
        }
    }
    fn defer_listener_registration(&mut self, on: bool) {
        self.defer_registration = on;
    }
    fn register_listener(&mut self, app: &str) -> SutResult<()> {
        self.sut.cmd(app, Cmd::RegisterListener).map(|_| ())
    }
    fn start_app(&mut self, model: &str, label: &str) -> SutResult<()> {
        self.start_app_with(model, label, &|_| {})
    }
    fn start_app_with(
        &mut self,
        model: &str,
        label: &str,
        edit: &dyn Fn(&mut ModelSpec),
    ) -> SutResult<()> {
        // The Shell is whoever owns `org.gnome.Shell` on the session bus (isolated here).
        let shell = model == "shell";
        self.sut.start_app_with(model, label, &|m| {
            if shell && m.owns_name.is_none() {
                m.owns_name = Some("org.gnome.Shell".into());
            }
            edit(m);
        })?;
        self.listener_registered(label)
    }
    fn restart_app(&mut self, label: &str) -> SutResult<()> {
        self.sut.restart_app(label)?;
        self.listener_registered(label)
    }
    fn cmd(&mut self, app: &str, c: Cmd) -> SutResult<CmdReply> {
        self.sut.cmd(app, c)
    }
    fn advance(&mut self, ms: u64) {
        self.sut.advance(ms);
    }
    fn calls(&mut self, app: &str) -> SutResult<AppStats> {
        self.sut.calls(app)
    }
    fn set_latency(&mut self, event_us: u64, reply_us: u64) {
        self.sut.set_latency(event_us, reply_us);
    }
}

/// A fresh desktop with the daemon under test. `fills`: the apps get the test policy that asks
/// for everything instead of R0, which never requests a thing.
fn make(fills: bool) -> Box<dyn Sut> {
    let session = Session::start().expect("isolated session");
    let dir = session.dir().join("ic");
    std::fs::create_dir_all(&dir).unwrap();
    let present_log = dir.join("present.jsonl");
    let vdir = dir.join("v");
    std::fs::create_dir_all(&vdir).unwrap();
    let (a11y, sess) = (
        session.a11y_address().to_owned(),
        session.session_address().to_owned(),
    );
    let token = CancellationToken::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let (rt_tx, rt_rx) = std::sync::mpsc::channel();
    let t = token.clone();
    let log = present_log.clone();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(DAEMON_WORKERS)
            .enable_all()
            .build()
            .unwrap();
        let _ = rt_tx.send(rt.handle().clone());
        rt.block_on(async move {
            // The speech server the daemon talks to; the cases judge the presentation log.
            let Ok(voicego) = MockSpeechServer::start_voicego(&vdir).await else {
                return;
            };
            let mut opts =
                Options::isolated(&a11y, &sess, voicego.socket_path(), &vdir.join("ssip.sock"));
            opts.speech = SpeechMode::Voicego;
            opts.present_log = Some(log);
            let policy: PolicyFactory = if fills {
                Arc::new(|| Box::new(PrefetchAllPolicy::default()))
            } else {
                Arc::new(|| Box::new(gorca_mirror::fill::NullPolicy))
            };
            let _ = run_with_policy(opts, t, policy, move |p| {
                let _ = tx.send(p);
            })
            .await;
        });
    });
    let runtime = rt_rx
        .recv_timeout(Duration::from_secs(20))
        .expect("runtime");
    let probe = rx.recv_timeout(Duration::from_secs(20)).expect("daemon up");
    // Events sent before the drain installed its match rules are lost: start the apps after.
    let t0 = Instant::now();
    while !probe.ready() {
        assert!(t0.elapsed() < Duration::from_secs(20), "drain never ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let cfg = RealConfig {
        fakeapp_bin: fakeapp_bin(),
        models_dir: default_models_dir(),
        a11y_bus: session.a11y_address().to_owned(),
        session_bus: Some(session.session_address().to_owned()),
        work_dir: dir,
        present_log,
        scale_nodes: None,
        allow_any_bus: false,
    };
    let sut = RealSut::new(cfg, "r0").with_probe(Arc::new(Probing(probe)));
    Box::new(DaemonSut {
        sut,
        token,
        thread: Some(thread),
        fills,
        runtime,
        defer_registration: false,
        _session: session,
    })
}

#[test]
fn the_invalidation_suite_passes_against_the_real_daemon() {
    if let Some(why) = Session::unavailable_reason() {
        // A host without the harness must not turn the gate green by running nothing.
        assert!(
            std::env::var_os("GORCA_REQUIRE_HARNESS").is_none(),
            "the IC suite cannot run here: {why}"
        );
        eprintln!("SKIPPED (harness unavailable): {why}");
        return;
    }
    // R0 for every case; the two cases that exercise shared fill code (hidden ghosts, dump
    // eligibility) need a policy that requests things, so they run again with the test policy
    // and that result replaces R0's skip.
    // `GORCA_IC_CASES=IC-17,IC-3` runs just those (debugging); the totals are then not checked.
    let only = std::env::var("GORCA_IC_CASES").unwrap_or_default();
    let only: Vec<&str> = only.split(',').filter(|x| !x.is_empty()).collect();
    let mut reports = ic::run_cases(&only, &|| make(false));
    let fill_ids: Vec<&str> = ["IC-12", "IC-22"]
        .into_iter()
        .filter(|id| only.is_empty() || only.contains(id))
        .collect();
    let filled = if fill_ids.is_empty() {
        Vec::new()
    } else {
        ic::run_cases(&fill_ids, &|| make(true))
    };
    for f in filled {
        if let Some(slot) = reports.iter_mut().find(|r| r.id == f.id) {
            *slot = f;
        }
    }
    let mut failed = Vec::new();
    let (mut passed, mut skipped) = (0, 0);
    for r in &reports {
        match &r.result {
            Ok(Outcome::Pass) => {
                passed += 1;
                eprintln!("{}: PASS", r.id);
            }
            Ok(Outcome::Skipped(why)) => {
                skipped += 1;
                eprintln!("{}: SKIPPED ({why})", r.id);
            }
            Err(e) => {
                eprintln!("{}: FAIL {}", r.id, e.0);
                failed.push(format!("{}: {}", r.id, e.0));
            }
        }
    }
    eprintln!("IC against the real daemon: {passed} passed, {skipped} skipped");
    assert!(failed.is_empty(), "{failed:#?}");
    if only.is_empty() {
        assert_eq!(
            reports.len(),
            24,
            "IC-1..IC-23 with IC-23b are all accounted for"
        );
        assert!(passed >= 20, "only {passed} cases ran: {skipped} skipped");
    }
}
