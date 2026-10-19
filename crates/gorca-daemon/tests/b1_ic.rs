// SPDX-License-Identifier: GPL-3.0-or-later
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

/// `b1-fakeapp`, built (a no-op when current) so that it never lags the source. The nested build has
/// its OWN target directory: building into the workspace's shared one while `cargo test` is running
/// replaces artifacts that later doc-tests link against (E0463).
fn fakeapp_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile = exe
        .parent()
        .and_then(|d| d.parent())
        .and_then(|d| d.file_name())
        .map_or_else(|| "debug".into(), std::ffi::OsStr::to_owned);
    let target = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("b1-fakeapp-target");
    let bin = target.join(&profile).join("b1-fakeapp");
    BUILT.call_once(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let mut cmd = std::process::Command::new(cargo);
        cmd.args(["build", "-p", "b1-fakeapp", "--offline", "--target-dir"]);
        cmd.arg(&target);
        if profile == "release" {
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
        // Never join unboundedly: a daemon thread that does not stop within 40 s is detached
        // (the runtime inside it is shut down with a timeout as well).
        if let Some(t) = self.thread.take() {
            let t0 = Instant::now();
            while !t.is_finished() && t0.elapsed() < Duration::from_secs(40) {
                std::thread::sleep(Duration::from_millis(20));
            }
            if t.is_finished() {
                let _ = t.join();
            }
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
    let policy: PolicyFactory = if fills {
        Arc::new(|| Box::new(PrefetchAllPolicy::default()))
    } else {
        Arc::new(|| Box::new(gorca_mirror::fill::NullPolicy))
    };
    make_with(policy, fills)
}

/// [`make`] with the fill policy of the apps given.
fn make_with(policy: PolicyFactory, fills: bool) -> Box<dyn Sut> {
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
            let _ = run_with_policy(opts, t, policy, move |p| {
                let _ = tx.send(p);
            })
            .await;
        });
        rt.shutdown_timeout(Duration::from_secs(10));
    });
    // A failed start-up must stop the daemon thread before the session goes away.
    let start_guard = token.clone().drop_guard();
    let runtime = rt_rx
        .recv_timeout(Duration::from_secs(120))
        .expect("runtime");
    let probe = rx
        .recv_timeout(Duration::from_secs(120))
        .expect("daemon up");
    // Events sent before the drain installed its match rules are lost: start the apps after.
    let t0 = Instant::now();
    while !probe.ready() {
        assert!(t0.elapsed() < Duration::from_secs(120), "drain never ready");
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
        register_listeners: false,
    };
    let sut = RealSut::new(cfg, "r0").with_probe(Arc::new(Probing(probe)));
    start_guard.disarm();
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
    judge(&reports, &only, 20);
}

/// The whole suite against the policy of the bake-off arm this tree was built from
/// (`gorca_mirror::fill::Candidate`, SPEC 10). Every case runs once with a policy that is free to
/// request things, so the cases that need one (IC-12, IC-22) apply to it. A case that is only
/// true of R0 or of the test policy is a harness defect (cases are policy-neutral, SPEC 5.7):
/// report it, do not weaken the arm.
#[test]
fn the_invalidation_suite_passes_with_the_candidate_policy() {
    if let Some(why) = Session::unavailable_reason() {
        assert!(
            std::env::var_os("GORCA_REQUIRE_HARNESS").is_none(),
            "the IC suite cannot run here: {why}"
        );
        eprintln!("SKIPPED (harness unavailable): {why}");
        return;
    }
    let only = std::env::var("GORCA_IC_CASES").unwrap_or_default();
    let only: Vec<&str> = only.split(',').filter(|x| !x.is_empty()).collect();
    let reports = ic::run_cases(&only, &|| {
        make_with(
            Arc::new(|| Box::<gorca_mirror::fill::Candidate>::default()),
            true,
        )
    });
    // IC-22 (b) is not applicable to a policy that never dumps, and IC-10, 11, 16 and 23b need
    // capabilities the real daemon lacks: 18 is the floor, not a target.
    judge(&reports, &only, 18);
}

/// Print every case and assert that none failed (and, for a full run, that enough ran).
fn judge(reports: &[ic::CaseReport], only: &[&str], min_passed: usize) {
    let mut failed = Vec::new();
    let (mut passed, mut skipped) = (0, 0);
    for r in reports {
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
        assert!(
            passed >= min_passed,
            "only {passed} cases ran: {skipped} skipped"
        );
    }
}

/// A policy that writes down what it is told, in order.
struct Recording(Arc<std::sync::Mutex<Vec<String>>>);

impl gorca_mirror::fill::FillPolicy for Recording {
    fn name(&self) -> &'static str {
        "test-recording"
    }
    fn step(
        &mut self,
        cx: &gorca_mirror::fill::FillCtx<'_>,
        input: gorca_mirror::fill::FillInput<'_>,
        _out: &mut gorca_mirror::fill::FillEffects,
    ) {
        use gorca_mirror::fill::FillInput as I;
        let tag = match input {
            I::Appeared { root } => format!("appeared:{:016x}:{:?}", root.local.0, cx.app_class),
            I::Ready => "ready".to_owned(),
            I::FocusSettled { .. } => "settled".to_owned(),
            I::Tick
            | I::Events(_)
            | I::Resync { .. }
            | I::Demand { .. }
            | I::WindowActivated { .. }
            | I::Reply { .. } => return,
        };
        if let Ok(mut l) = self.0.lock() {
            l.push(tag);
        }
    }
    fn memory_bytes(&self) -> usize {
        0
    }
}

/// FNV-1a 64 of the app root path: the key the connector gives the root (`LocalId`).
fn root_local_id() -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in "/org/a11y/atspi/accessible/root".bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The reader knows an application when the registry announces it, not at its first event, and
/// its fill policy is told so (with the root and the verified identity) before the first focus.
#[test]
fn the_policy_hears_of_a_new_app_before_it_does_anything() {
    use gorca_testkit::b1::sut::SutExt;
    if Session::unavailable_reason().is_some() {
        assert!(std::env::var_os("GORCA_REQUIRE_HARNESS").is_none());
        return;
    }
    let log = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let l2 = Arc::clone(&log);
    let mut s = make_with(
        Arc::new(move || Box::new(Recording(Arc::clone(&l2)))),
        false,
    );
    let seen = || log.lock().map(|l| l.clone()).unwrap_or_default();
    // A Shell that owns `org.gnome.Shell` (verified) and a plain app, both silent.
    s.start_app("shell", "shell").unwrap();
    s.start_app("settings", "settings").unwrap();
    // No event, no focus: the apps are announced and known.
    let t0 = Instant::now();
    while seen().len() < 4 && t0.elapsed() < Duration::from_secs(60) {
        s.advance(50);
    }
    let before = seen();
    assert!(
        before.iter().filter(|t| t.starts_with("appeared:")).count() == 2
            && before.iter().filter(|t| *t == "ready").count() == 2,
        "{before:?}"
    );
    assert!(
        !before.iter().any(|t| t == "settled"),
        "nothing focused yet"
    );
    let root = format!("appeared:{:016x}:", root_local_id());
    assert!(
        before
            .iter()
            .all(|t| !t.starts_with("appeared:") || t.starts_with(&root)),
        "{before:?}"
    );
    assert!(
        before.iter().any(|t| t.ends_with(":Shell")),
        "the verified Shell is announced as the Shell: {before:?}"
    );
    let bus = s.bus_name("settings");
    assert!(
        s.mirror_nodes(&bus).is_some(),
        "the reader has a mirror for the app"
    );
    // Then the first focus: it comes after everything above.
    let c = s.path_of("settings", "controls[1]").unwrap();
    s.focus("settings", &c).unwrap();
    let after = seen();
    assert_eq!(&after[..before.len()], before.as_slice());
    assert!(after.iter().any(|t| t == "settled"), "{after:?}");
}
