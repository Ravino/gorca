// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime tests against a real component built from `tests/guest` (behaviour by `cause` code).
//! See the guest source for the cause table.
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gorca_plugin_host::actor::{ActorRunner, Config};
use gorca_plugin_host::cache::{CacheError, CwasmCache};
use gorca_plugin_host::call::{
    DeriveJob, Exec, PluginExec, PluginReply, ReplySink, Turn, WLaneRunner,
};
use gorca_plugin_host::host::{Effect, Value};
use gorca_plugin_host::instance::{DirtyNode, EngineOptions, Loaded, build_engine};
use gorca_plugin_host::limits::{B1_BACKGROUND, FuelGrant, KAPPA_PROVISIONAL};
use gorca_plugin_host::slot::{Action, Event, FaultKind, Slot, SlotState};
use gorca_plugin_host::trust::Perms;
use gorca_plugin_host::view::ModelView;
use sha2::{Digest, Sha256};

// ---- fixtures ------------------------------------------------------------------------------

fn guest() -> &'static [u8] {
    static G: OnceLock<Vec<u8>> = OnceLock::new();
    G.get_or_init(|| {
        let tgt = std::env::var("CARGO_TARGET_DIR")
            .unwrap_or_else(|_| "/var/tmp/gorca-shared-target".into());
        let out = PathBuf::from(&tgt).join("plugin-test-guest-out");
        std::fs::create_dir_all(&out).unwrap();
        let out = out.join(format!("guest-{}.wasm", std::process::id()));
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/build-guest.sh");
        let st = Command::new(&script)
            .arg(&out)
            .status()
            .expect("run build-guest.sh");
        assert!(st.success(), "guest build failed");
        std::fs::read(&out).unwrap()
    })
}

fn sha(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

#[derive(Debug)]
struct TestView;

impl ModelView for TestView {
    fn generation(&self, i: u32) -> Option<u16> {
        (i < 10).then_some(1)
    }
    fn role(&self, i: u32) -> Option<u16> {
        (i < 10).then_some(7)
    }
    fn states(&self, _i: u32) -> u64 {
        0
    }
    fn name(&self, i: u32) -> Option<String> {
        (i < 10).then(|| "name".to_string())
    }
    fn text(&self, _i: u32, _s: u32, _l: u32) -> Option<String> {
        None
    }
    fn attr(&self, _i: u32, _k: &str) -> Option<String> {
        None
    }
    fn parent(&self, _i: u32) -> Option<u32> {
        None
    }
    fn children(&self, _i: u32, _m: u16) -> Vec<u32> {
        Vec::new()
    }
}

struct Env {
    engine: wasmtime::Engine,
    loaded: Loaded,
    _dir: tempfile::TempDir,
}

fn env() -> Env {
    let engine = build_engine(EngineOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cache = CwasmCache::open(&dir.path().join("cwasm")).unwrap();
    let wasm = guest();
    let comp = cache.load(&engine, wasm, &sha(wasm)).unwrap();
    let loaded = Loaded::new(&engine, &comp).unwrap();
    Env {
        engine,
        loaded,
        _dir: dir,
    }
}

fn job(
    plugin: u32,
    jobid: u64,
    slot_epoch: u64,
    cause: u8,
    interactive: bool,
    t: Duration,
) -> DeriveJob {
    DeriveJob {
        plugin,
        job: jobid,
        slot_epoch,
        interactive,
        epoch: 1,
        nodes: vec![DirtyNode {
            index: 3,
            watch: 1,
            cause,
        }],
        view: Arc::new(TestView),
        grant: FuelGrant::for_time(t, KAPPA_PROVISIONAL),
        queued_at: Instant::now(),
    }
}

fn run(exec: &mut PluginExec, cause: u8) -> PluginReply {
    exec.run(&job(1, 1, 0, cause, false, B1_BACKGROUND))
}

fn fault_kind(r: &PluginReply) -> Option<FaultKind> {
    match r {
        PluginReply::Fault { kind, .. } => Some(*kind),
        PluginReply::Done { .. } | PluginReply::Cancelled { .. } => None,
    }
}

fn number_fact(r: &PluginReply) -> Option<u32> {
    match r {
        PluginReply::Done { effects, .. } => effects.iter().find_map(|e| match e {
            Effect::Assert {
                value: Value::Number(n),
                ..
            } => Some(*n),
            _ => None,
        }),
        PluginReply::Fault { .. } | PluginReply::Cancelled { .. } => None,
    }
}

#[derive(Default)]
struct Mailbox {
    q: Mutex<Vec<PluginReply>>,
    cv: Condvar,
}

impl ReplySink for Mailbox {
    fn send(&self, r: PluginReply) {
        self.q.lock().unwrap().push(r);
        self.cv.notify_all();
    }
}

impl Mailbox {
    fn wait_n(&self, n: usize, t: Duration) -> Vec<PluginReply> {
        let mut g = self.q.lock().unwrap();
        let end = Instant::now() + t;
        while g.len() < n {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = self.cv.wait_timeout(g, left).unwrap().0;
        }
        std::mem::take(&mut *g)
    }
}

// ---- cache ---------------------------------------------------------------------------------

#[test]
fn cache_tamper_is_refused_and_never_deserialized() {
    let e = build_engine(EngineOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cwasm");
    let cache = CwasmCache::open(&path).unwrap();
    let wasm = guest();
    let h = sha(wasm);
    cache.load(&e, wasm, &h).unwrap();
    assert_eq!(cache.stats.misses.load(Relaxed), 1);
    // Clean hit.
    cache.load(&e, wasm, &h).unwrap();
    assert_eq!(cache.stats.hits.load(Relaxed), 1);
    assert_eq!(cache.stats.deserializes.load(Relaxed), 1);

    let key = CwasmCache::key(&e, &h);
    let file = path.join(format!("{key}.cwasm"));
    for offset in [10usize, 4 + 32 + 100] {
        let mut b = std::fs::read(&file).unwrap();
        b[offset] ^= 0x40;
        std::fs::write(&file, &b).unwrap();
        assert!(matches!(
            cache.read_verified(&key),
            Err(CacheError::MacMismatch(_))
        ));
    }
    let before = cache.stats.deserializes.load(Relaxed);
    let t0 = cache.stats.tampered.load(Relaxed);
    // `load` recovers by recompiling; the tampered bytes are never handed to wasmtime.
    cache.load(&e, wasm, &h).unwrap();
    assert_eq!(cache.stats.deserializes.load(Relaxed), before);
    assert_eq!(cache.stats.tampered.load(Relaxed), t0 + 1);
    // And the recompiled entry is good again.
    assert!(cache.read_verified(&key).unwrap().is_some());
}

#[test]
fn artifact_swapped_between_installs_fails_the_mac() {
    // A valid artifact of another package copied under this key must not verify.
    let e = build_engine(EngineOptions::default()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cwasm");
    let cache = CwasmCache::open(&path).unwrap();
    let wasm = guest();
    let h = sha(wasm);
    cache.load(&e, wasm, &h).unwrap();
    let key = CwasmCache::key(&e, &h);
    let other = CwasmCache::key(&e, &[9u8; 32]);
    std::fs::copy(
        path.join(format!("{key}.cwasm")),
        path.join(format!("{other}.cwasm")),
    )
    .unwrap();
    assert!(matches!(
        cache.read_verified(&other),
        Err(CacheError::MacMismatch(_))
    ));
}

#[test]
fn cache_directory_with_wrong_permissions_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cwasm");
    let cache = CwasmCache::open(&path).unwrap();
    drop(cache);
    for mode in [0o755, 0o770, 0o750, 0o701, 0o1700] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        assert!(
            matches!(CwasmCache::open(&path), Err(CacheError::BadDirectory(_))),
            "mode {mode:o}"
        );
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(CwasmCache::open(&path).is_ok());

    // A symlink to a good directory is refused too.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(matches!(
        CwasmCache::open(&link),
        Err(CacheError::BadDirectory(_))
    ));

    // A secret with loose permissions is refused.
    std::fs::set_permissions(path.join(".secret"), std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        CwasmCache::open(&path),
        Err(CacheError::BadSecret(_))
    ));
}

#[test]
fn directory_loosened_after_open_is_refused_on_use() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cwasm");
    let cache = CwasmCache::open(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(
        cache.read_verified("00"),
        Err(CacheError::BadDirectory(_))
    ));
    assert!(matches!(
        cache.write("00", b"x"),
        Err(CacheError::BadDirectory(_))
    ));
}

// ---- capabilities, handles, faults ---------------------------------------------------------

#[test]
fn capability_is_denied_by_default() {
    let env = env();
    // Only fact contribution granted: `name` (needs read-names) reads none and is counted.
    let mut x = PluginExec::new(env.loaded.clone(), Perms::CONTRIBUTE_FACTS, 16);
    let r = run(&mut x, 4);
    assert_eq!(number_fact(&r), Some(0), "name() must read none: {r:?}");
    assert_eq!(x.counters().unwrap().denied, 1);

    // With the permission the same call reads the name.
    let mut y = PluginExec::new(
        env.loaded.clone(),
        Perms::CONTRIBUTE_FACTS | Perms::READ_NAMES,
        16,
    );
    assert_eq!(number_fact(&run(&mut y, 4)), Some(1));
    assert_eq!(y.counters().unwrap().denied, 0);

    // No grants at all: even the fact is dropped (and counted), the call is not a fault.
    let mut z = PluginExec::new(env.loaded.clone(), Perms::empty(), 16);
    let r = run(&mut z, 0);
    assert!(matches!(r, PluginReply::Done { .. }));
    assert_eq!(number_fact(&r), None);
    assert_eq!(z.counters().unwrap().denied, 1);

    // `speak` is gated the same way.
    let mut s = PluginExec::new(env.loaded.clone(), Perms::empty(), 16);
    assert!(matches!(run(&mut s, 11), PluginReply::Done { effects, .. } if effects.is_empty()));
    let mut s2 = PluginExec::new(env.loaded.clone(), Perms::SPEAK, 16);
    assert!(matches!(run(&mut s2, 11), PluginReply::Done { effects, .. } if effects.len() == 1));
}

#[test]
fn forged_handle_is_rejected_and_counted() {
    let env = env();
    let mut x = PluginExec::new(env.loaded.clone(), Perms::CONTRIBUTE_FACTS, 16);
    // The real handle reads the role; a one-bit-flipped handle reads none.
    assert_eq!(number_fact(&run(&mut x, 7)), Some(7));
    assert_eq!(number_fact(&run(&mut x, 3)), Some(0));
    assert_eq!(x.counters().unwrap().forged, 1);
    // 16 forged handles in one call reject the whole call.
    let r = run(&mut x, 8);
    assert_eq!(fault_kind(&r), Some(FaultKind::Malformed), "{r:?}");
    assert!(!x.is_live(), "a malformed call drops the instance");
}

#[test]
fn trap_drops_the_instance_and_the_next_call_rebuilds_it() {
    let env = env();
    let mut x = PluginExec::new(env.loaded.clone(), Perms::CONTRIBUTE_FACTS, 16);
    assert_eq!(number_fact(&run(&mut x, 6)), Some(1));
    assert_eq!(number_fact(&run(&mut x, 6)), Some(2));
    assert_eq!(x.serial(), 1);
    let r = run(&mut x, 2);
    assert_eq!(fault_kind(&r), Some(FaultKind::Trap), "{r:?}");
    assert!(!x.is_live());
    // Rebuilt: a new instance (new serial, new handle key) with fresh guest state.
    assert_eq!(number_fact(&run(&mut x, 6)), Some(1));
    assert_eq!(x.serial(), 2);
    assert!(x.is_live());
}

#[test]
fn fuel_exhaustion_is_a_fault_and_a_strike() {
    let env = env();
    let mut x = PluginExec::new(env.loaded.clone(), Perms::CONTRIBUTE_FACTS, 16);
    let t0 = Instant::now();
    let r = x.run(&job(1, 1, 0, 1, true, Duration::from_micros(200)));
    assert_eq!(fault_kind(&r), Some(FaultKind::OutOfFuel), "{r:?}");
    // The guest loops forever; fuel stopped it quickly (a 0.2 ms grant, generous wall bound).
    assert!(t0.elapsed() < Duration::from_millis(500));

    // The writer-side slot turns the fault into a strike and drops the instance.
    let mut slot = Slot::new();
    let now = Instant::now();
    slot.event(Event::PreloadDone, now);
    assert_eq!(slot.event(Event::CallNeeded(1), now), Action::Send);
    let ev = r.events();
    assert_eq!(ev, vec![Event::Fault(FaultKind::OutOfFuel)]);
    assert_eq!(
        slot.on_reply(r.slot_epoch(), ev[0], now),
        Action::DropInstance
    );
    assert_eq!(slot.strikes(), 1);
    assert_eq!(slot.state(), SlotState::Cold);
}

#[test]
fn memory_growth_past_the_cap_is_a_fault() {
    let env = env();
    let mut x = PluginExec::new(env.loaded.clone(), Perms::CONTRIBUTE_FACTS, 16);
    let r = x.run(&job(1, 1, 0, 5, false, Duration::from_millis(5)));
    assert_eq!(fault_kind(&r), Some(FaultKind::MemoryDenied), "{r:?}");
}

#[test]
fn oversize_arguments_are_rejected_before_they_are_copied() {
    let env = env();
    let mut x = PluginExec::new(env.loaded.clone(), Perms::CONTRIBUTE_FACTS, 16);
    // 1 MiB log message: over the per-call lift cap, refused by wasmtime before the copy.
    let r = run(&mut x, 9);
    assert_eq!(fault_kind(&r), Some(FaultKind::Malformed), "{r:?}");
    // 1 KiB: lifted (under the hostcall cap), refused by the per-field cap, call rejected.
    let mut y = PluginExec::new(env.loaded.clone(), Perms::CONTRIBUTE_FACTS, 16);
    let r = run(&mut y, 10);
    assert_eq!(fault_kind(&r), Some(FaultKind::Malformed), "{r:?}");
}

// ---- B' actor runner -----------------------------------------------------------------------

fn runner(env: &Env, n: u32, grants: Perms) -> ActorRunner {
    let execs: Vec<(u32, Box<dyn Exec>)> = (0..n)
        .map(|i| {
            (
                i,
                Box::new(PluginExec::new(env.loaded.clone(), grants, 16)) as Box<dyn Exec>,
            )
        })
        .collect();
    let loaded = env.loaded.clone();
    ActorRunner::new(
        execs,
        Box::new(move |_| Some(Box::new(PluginExec::new(loaded.clone(), grants, 16)))),
        Config::default(),
    )
    .unwrap()
}

#[test]
fn actor_round_trip_and_early_wake() {
    let env = env();
    let mut r = runner(&env, 3, Perms::CONTRIBUTE_FACTS);
    let mb = Arc::new(Mailbox::default());
    let sink: Arc<dyn ReplySink> = mb.clone();
    let jobs = (0..3)
        .map(|p| job(p, 10 + u64::from(p), 0, 0, true, Duration::from_micros(200)))
        .collect();
    // A generous deadline: the wait must end early because all three replied.
    let t0 = gorca_rt::process::monotonic_ns();
    let turn = Turn {
        tag: 1,
        deadline_ns: t0 + 200_000_000,
        wait: true,
    };
    let out = r.interactive(&turn, jobs, &sink);
    assert_eq!(out.asked, 3);
    assert_eq!(out.arrived, 3, "{out:?}");
    assert!(out.waited < Duration::from_millis(150), "{out:?}");
    let replies = mb.wait_n(3, Duration::from_secs(2));
    assert_eq!(replies.len(), 3);
    assert!(replies.iter().all(|x| number_fact(x) == Some(1)));
    r.shutdown();
}

#[test]
fn bounded_wait_ends_at_the_deadline_with_a_runaway_plugin() {
    let env = env();
    let mut r = runner(&env, 2, Perms::CONTRIBUTE_FACTS);
    let mb = Arc::new(Mailbox::default());
    let sink: Arc<dyn ReplySink> = mb.clone();
    // Plugin 0 loops until its (large) grant runs out; plugin 1 answers at once.
    let jobs = vec![
        job(0, 1, 0, 1, true, Duration::from_millis(20)),
        job(1, 2, 0, 0, true, Duration::from_micros(200)),
    ];
    // Warm the guest code paths first (first-call page faults are the pre-warm of the ADR
    // reference arm; production pre-warm of derive is a later slice).
    r.background(
        vec![
            job(0, 90, 0, 0, false, B1_BACKGROUND),
            job(1, 91, 0, 0, false, B1_BACKGROUND),
        ],
        &sink,
    );
    assert_eq!(mb.wait_n(2, Duration::from_secs(5)).len(), 2);
    let start = gorca_rt::process::monotonic_ns();
    let turn = Turn::starting_at(7, start, true);
    let out = r.interactive(&turn, jobs, &sink);
    let total = Duration::from_nanos(gorca_rt::process::monotonic_ns() - start);
    assert_eq!(out.asked, 2);
    assert!(
        out.arrived < out.asked,
        "the runaway cannot have replied: {out:?}"
    );
    // D = 0.3 ms. Generous bound for a loaded CI host; the exact p99 is an M18a-0/PX-4 number.
    assert!(total < Duration::from_millis(20), "waited {total:?}");
    // The runaway is eventually stopped by fuel and its reply arrives through the mailbox.
    let replies = mb.wait_n(2, Duration::from_secs(5));
    assert_eq!(replies.len(), 2);
    let k = replies.iter().find_map(fault_kind);
    assert_eq!(k, Some(FaultKind::OutOfFuel));
    r.shutdown();
}

#[test]
fn a_wedged_actor_is_replaced_and_answers_actor_lost() {
    struct Wedge(Arc<std::sync::atomic::AtomicBool>);
    impl Exec for Wedge {
        fn run(&mut self, _j: &DeriveJob) -> PluginReply {
            while !self.0.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            PluginReply::Cancelled {
                plugin: 0,
                job: 0,
                slot_epoch: 0,
            }
        }
        fn reset(&mut self) {}
    }
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let env = env();
    let loaded = env.loaded.clone();
    let mut r = ActorRunner::new(
        vec![(0, Box::new(Wedge(Arc::clone(&release))) as Box<dyn Exec>)],
        Box::new(move |_| {
            Some(Box::new(PluginExec::new(
                loaded.clone(),
                Perms::CONTRIBUTE_FACTS,
                16,
            )))
        }),
        Config {
            lost_after: Duration::from_millis(100),
            ..Config::default()
        },
    )
    .unwrap();
    let mb = Arc::new(Mailbox::default());
    let sink: Arc<dyn ReplySink> = mb.clone();
    r.background(vec![job(0, 5, 3, 0, false, B1_BACKGROUND)], &sink);
    let replies = mb.wait_n(1, Duration::from_secs(5));
    assert_eq!(replies.len(), 1);
    assert_eq!(fault_kind(&replies[0]), Some(FaultKind::ActorLost));
    assert_eq!(replies[0].slot_epoch(), 3);
    assert!(r.replaced(0) >= 1);
    // The replacement serves jobs; the wedged thread's late result is discarded.
    r.background(vec![job(0, 6, 4, 0, false, B1_BACKGROUND)], &sink);
    release.store(true, Relaxed);
    let replies = mb.wait_n(1, Duration::from_secs(5));
    assert_eq!(replies.len(), 1);
    assert_eq!(number_fact(&replies[0]), Some(1));
    r.shutdown();
}

#[test]
fn engine_options_portable_has_a_distinct_cache_key() {
    let a = build_engine(EngineOptions::default()).unwrap();
    let b = build_engine(EngineOptions { portable: true }).unwrap();
    let h = [1u8; 32];
    assert_ne!(CwasmCache::key(&a, &h), CwasmCache::key(&b, &h));
    let _ = env().engine;
}
