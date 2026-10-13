//! `b1-walk`: the walk driver of bake-off B-1 (H-3). It is what `bench/b1/driver.sh` runs through
//! `B1_WALK_DRIVER`, inside `tests/harness/run-isolated.sh`.
//!
//! Inputs (environment, set by the runner): `B1_ARM`, `B1_DAEMON` (path of the arm's
//! `gorca-daemon`), `B1_RUN_KIND` (`walk|ff|idle|big|soak`), `B1_REPEAT`, `B1_RUN_DIR`,
//! `B1_SEED`; and from the isolated session `AT_SPI_BUS_ADDRESS`, `DBUS_SESSION_BUS_ADDRESS`.
//! Optional: `B1_FAKEAPP` (the `b1-fakeapp` binary; default: next to this one), `B1_WALK_SPEC`
//! (default `tests/harness/b1/walks/w1.toml`), `B1_DAEMON_ARGS` (extra daemon arguments),
//! `B1_SOAK_S` (soak length, default 3600), `B1_QUICK=1` (a short walk for smoke runs),
//! `B1_FILL_POLICY=r0|candidate` (the daemon's `--bench-fill-policy`; default: `r0` for the arm
//! labelled `r0`, `candidate` for every other arm), `B1_FAKE_MUTTER` (path of `fake_mutter.py`).
//!
//! **Keyboard.** `walk`, `big` and `soak` runs start the emulated Mutter first and the daemon
//! with `--impersonate-orca`, so the walk's where-am-I step presses the daemon's real command
//! chord (Insert+Tab). The other kinds run the daemon without a keyboard.
//!
//! Output: the run directory of `crates/gorca-bench-b1/src/rundir.rs`.
//!
//! `--sim [fault]` runs the same walk against the virtual-time reference reader instead of a
//! daemon (no bus, no daemon, instant): the way to smoke-test the runner and to produce
//! known-good and known-bad run directories.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gorca_testkit::MockSpeechServer;
use gorca_testkit::b1::keys::{self, FakeMutter};
use gorca_testkit::b1::real::{RealConfig, RealSut, default_models_dir, proc_cpu_ns, proc_rss_kib};
use gorca_testkit::b1::sim::{Fault, SimSut};
use gorca_testkit::b1::sut::Sut;
use gorca_testkit::b1::walk::{self, WalkSpec};
use serde_json::{Value, json};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn die(msg: &str) -> ! {
    eprintln!("b1-walk: {msg}");
    std::process::exit(2);
}

fn fault_from(s: &str) -> Fault {
    match s {
        "none" | "" => Fault::None,
        "never-invalidate" => Fault::NeverInvalidate,
        "never-cache" => Fault::NeverCache,
        "no-rv1" => Fault::NoRv1,
        "dump-every-100ms" => Fault::DumpEvery100ms,
        other => die(&format!("unknown sim fault {other}")),
    }
}

fn spec() -> WalkSpec {
    let mut s = match env("B1_WALK_SPEC") {
        Some(p) => WalkSpec::from_toml(
            &std::fs::read_to_string(&p).unwrap_or_else(|e| die(&format!("{p}: {e}"))),
        )
        .unwrap_or_else(|e| die(&e)),
        None => WalkSpec::w1().unwrap_or_else(|e| die(&e)),
    };
    if env("B1_QUICK").is_some() {
        s.loops = 3;
        s.initial_tabs = 32;
        s.list_arrows = 70;
        s.panels = 3;
        s.l3_duration_ms = 3_000;
        s.step_ms_min = 20;
        s.step_ms_max = 40;
        s.pause_ms_min = 100;
        s.pause_ms_max = 200;
    }
    s
}

fn merge_counters(dir: &Path, extra: &Value) {
    let p = dir.join("counters.json");
    let mut c: Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    if let (Some(o), Some(e)) = (c.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    let _ = std::fs::write(&p, c.to_string());
}

/// Run `kind` against `sut`; `rewrite_present` is false when a daemon writes the log itself.
fn run_kind<S: Sut + ?Sized>(
    kind: &str,
    sut: &mut S,
    dir: &Path,
    seed: u64,
    rewrite_present: bool,
) -> std::io::Result<()> {
    let mut sp = spec();
    match kind {
        "walk" | "big" => {
            if kind == "big" {
                sp.loops = 1;
            }
            let run = walk::execute(sut, &sp, seed);
            walk::write_run_dir_opts(dir, sut, &sp, &run, rewrite_present)?;
            if !run.steps_failed.is_empty() {
                eprintln!(
                    "b1-walk: {} step(s) failed, first: {}",
                    run.steps_failed.len(),
                    run.steps_failed[0]
                );
            }
        }
        "soak" => {
            let secs: u64 = env("B1_SOAK_S")
                .and_then(|v| v.parse().ok())
                .unwrap_or(3600);
            sp.loops = u32::try_from((secs / 360).max(1)).unwrap_or(10);
            let run = walk::execute(sut, &sp, seed);
            walk::write_run_dir_opts(dir, sut, &sp, &run, rewrite_present)?;
        }
        "ff" => {
            let ff = walk::run_ff(sut, 50).unwrap_or_else(|e| die(&e.to_string()));
            walk::write_ff_dir(dir, sut, &ff, &std::collections::BTreeMap::new())?;
        }
        "idle" => {
            let (w, delta) = walk::run_idle(sut, 120).unwrap_or_else(|e| die(&e.to_string()));
            walk::write_idle_dir(dir, sut, w, &delta)?;
        }
        other => die(&format!("unknown run kind {other}")),
    }
    Ok(())
}

fn find_fakeapp() -> PathBuf {
    if let Some(p) = env("B1_FAKEAPP") {
        return PathBuf::from(p);
    }
    let me = std::env::current_exe().unwrap_or_default();
    let beside = me.with_file_name("b1-fakeapp");
    if beside.exists() {
        return beside;
    }
    die("b1-fakeapp not found: build it (cargo build -p b1-fakeapp) or set B1_FAKEAPP")
}

struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[allow(clippy::too_many_lines)]
fn real(kind: &str, dir: &Path, seed: u64) {
    let a11y = env("AT_SPI_BUS_ADDRESS")
        .or_else(|| env("GORCA_A11Y_BUS_ADDRESS"))
        .unwrap_or_else(|| die("no private a11y bus (run inside tests/harness/run-isolated.sh)"));
    if !a11y.contains("/gorca-h.") {
        die(&format!("refusing a11y bus {a11y}: not a harness session"));
    }
    let session = env("DBUS_SESSION_BUS_ADDRESS");
    let work = std::env::temp_dir().join(format!("b1-walk-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap_or_else(|e| die(&e.to_string()));
    std::fs::create_dir_all(dir).unwrap_or_else(|e| die(&e.to_string()));
    let present = std::path::absolute(dir.join("present.jsonl"))
        .unwrap_or_else(|e| die(&format!("run directory: {e}")));
    let _ = std::fs::remove_file(&present);

    // The speech server the daemon talks to.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap_or_else(|e| die(&e.to_string()));
    let speech = rt
        .block_on(MockSpeechServer::start_voicego(&work))
        .unwrap_or_else(|e| die(&format!("mock speech server: {e}")));

    let daemon_bin = env("B1_DAEMON").unwrap_or_else(|| die("B1_DAEMON is not set"));
    let arm = env("B1_ARM").unwrap_or_else(|| "arm".into());
    let policy = env("B1_FILL_POLICY")
        .unwrap_or_else(|| if arm == "r0" { "r0" } else { "candidate" }.to_owned());
    if !matches!(policy.as_str(), "r0" | "candidate") {
        die(&format!(
            "B1_FILL_POLICY={policy}: expected r0 or candidate"
        ));
    }
    // The emulated Mutter must own its name before the daemon looks for it.
    let keyboard = matches!(kind, "walk" | "big" | "soak");
    if keyboard && !session.as_deref().is_some_and(|s| s.contains("/gorca-h.")) {
        die("refusing to take the Orca identity: the session bus is not a harness session");
    }
    let mutter = keyboard.then(|| {
        let script = env("B1_FAKE_MUTTER").map_or_else(keys::default_script, PathBuf::from);
        FakeMutter::spawn(&script).unwrap_or_else(|e| die(&format!("{}: {e}", script.display())))
    });
    let mut cmd = Command::new(&daemon_bin);
    cmd.args(env("B1_DAEMON_ARGS").unwrap_or_default().split_whitespace())
        .arg(format!("--bench-fill-policy={policy}"));
    if keyboard {
        // Desktop facts the daemon would ask `gsettings` for: GNOME defaults, no XKB options.
        cmd.arg("--impersonate-orca")
            .env("GORCA_REPEAT_DELAY_MS", "500")
            .env("GORCA_XKB_OPTIONS", "")
            .env("GORCA_CLOCK_24H", "1");
    }
    cmd.env("GORCA_A11Y_BUS_ADDRESS", &a11y)
        .env("GORCA_SPEECH_BACKEND", "voicego")
        .env("GORCA_SPEECH_SOCKET", speech.socket_path())
        .arg(format!("--bench-present-log={}", present.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let daemon = Daemon {
        child: cmd
            .spawn()
            .unwrap_or_else(|e| die(&format!("spawn {daemon_bin}: {e}"))),
    };
    let pid = daemon.child.id();
    #[allow(clippy::disallowed_methods)] // a driver process: real time must pass
    std::thread::sleep(Duration::from_millis(2_000)); // startup, registration

    let mut mutter = mutter;
    if let Some(m) = mutter.as_mut()
        && !m.wait_grabs(Duration::from_secs(15))
    {
        die("the daemon never registered key grabs with the emulated Mutter (--impersonate-orca)");
    }
    let cfg = RealConfig {
        fakeapp_bin: find_fakeapp(),
        models_dir: default_models_dir(),
        a11y_bus: a11y,
        session_bus: session,
        work_dir: work.clone(),
        present_log: present,
        scale_nodes: (kind == "big").then_some(20_000),
        allow_any_bus: false,
        register_listeners: true,
    };
    let mut sut = RealSut::new(cfg, &arm);
    if let Some(m) = mutter {
        sut = sut.with_keyboard(m);
    }

    // RSS and CPU of the daemon.
    let rss_before = proc_rss_kib(pid);
    let series: Arc<Mutex<Vec<(f64, u64)>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (series, stop) = (Arc::clone(&series), Arc::clone(&stop));
        std::thread::spawn(move || {
            let t0 = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(mut s) = series.lock() {
                    s.push((t0.elapsed().as_secs_f64(), proc_rss_kib(pid)));
                }
                #[allow(clippy::disallowed_methods)] // sampler thread
                std::thread::sleep(Duration::from_secs(5));
            }
        })
    };
    let cpu0 = proc_cpu_ns(pid);
    if let Err(e) = run_kind(kind, &mut sut, dir, seed, false) {
        die(&e.to_string());
    }
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.join();
    let series = series.lock().map(|s| s.clone()).unwrap_or_default();
    let peak = series.iter().map(|x| x.1).max().unwrap_or(rss_before);
    merge_counters(
        dir,
        &json!({
            "cpu_ns": proc_cpu_ns(pid).saturating_sub(cpu0),
            "rss_before_kib": rss_before,
            "rss_peak_kib": peak,
            "rss_series_kib": series.iter().map(|(t, k)| json!([t, k])).collect::<Vec<_>>(),
        }),
    );
    drop(daemon); // the daemon flushes its presentation log on SIGKILL-safe line boundaries
    // The truth log was uniquified with the walk; the daemon's log only now is complete.
    let _ = walk::uniquify_paths(dir, false, true);
    let _ = std::fs::remove_dir_all(&work);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let kind = env("B1_RUN_KIND").unwrap_or_else(|| "walk".into());
    let seed: u64 = env("B1_SEED").and_then(|s| s.parse().ok()).unwrap_or(1)
        + env("B1_REPEAT").and_then(|s| s.parse().ok()).unwrap_or(0);
    let dir = PathBuf::from(env("B1_RUN_DIR").unwrap_or_else(|| die("B1_RUN_DIR is not set")));
    if args.first().map(String::as_str) == Some("--sim") {
        let fault = fault_from(args.get(1).map_or("none", String::as_str));
        let mut sut = SimSut::with_fault(fault);
        if let Err(e) = run_kind(&kind, &mut sut, &dir, seed, true) {
            die(&e.to_string());
        }
        println!(
            "b1-walk: sim run ({:?}) written to {}",
            fault,
            dir.display()
        );
        return;
    }
    real(&kind, &dir, seed);
}
