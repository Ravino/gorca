//! RU: whole-daemon resource usage (CPU, RSS, PSS, threads) under an event rate.
//!
//! `RU-<events/s>`: the reader idles, a storm of that rate floods another application's signals,
//! `gorca+F12` chords run through the storm (K2-style key-to-speech probe), the storm stops and the
//! reader is watched while it recovers. `RU-0` is the idle baseline (no events, no keys), `RU-typ`
//! a synthetic typical-use mix (200 ev/s of background chatter plus a chord every 150-400 ms).
//! A sampler thread reads `/proc/<pid>` of the reader every 200 ms. If `RU_OUT` is passed through
//! (`GORCA_PASSTHROUGH=RU_OUT`) the raw samples go to `$RU_OUT/<id>-<pid>.csv`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use gorca_bench_b2::clock::Rng;

use super::apps::ReaderSpec;
use super::m1::{Before, chord, chord_series, m1_rig, time_chord};
use super::mock::Flavor;
use super::runner::{HostEnv, Rig};
use super::scenarios::{RunOut, pause, start_storm};

const SAMPLE_MS: u64 = 200;
const IDLE_S: f64 = 3.0;
const WARM_S: f64 = 1.5;
const RECOVER_S: f64 = 12.0;

#[derive(Clone, Copy, Debug)]
struct Sample {
    t: f64,
    cpu_s: f64,
    rss_kb: f64,
    pss_kb: f64,
    hwm_kb: f64,
    threads: f64,
}

fn kb(text: &str, key: &str) -> f64 {
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

fn read_sample(pid: u32, t0: Instant) -> Option<Sample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let f: Vec<&str> = rest.split_whitespace().collect();
    let tick = |i: usize| f.get(i).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
    let cpu_s = (tick(11) + tick(12)) / 100.0;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let smaps = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).unwrap_or_default();
    Some(Sample {
        t: t0.elapsed().as_secs_f64(),
        cpu_s,
        rss_kb: kb(&status, "VmRSS:"),
        pss_kb: kb(&smaps, "Pss:"),
        hwm_kb: kb(&status, "VmHWM:"),
        threads: kb(&status, "Threads:"),
    })
}

/// CPU in percent of one core between the first sample at or after `a` and the last at or before `b`.
fn cpu_pct(s: &[Sample], a: f64, b: f64) -> f64 {
    let lo = s.iter().find(|x| x.t >= a);
    let hi = s.iter().rev().find(|x| x.t <= b);
    match (lo, hi) {
        (Some(l), Some(h)) if h.t > l.t => (h.cpu_s - l.cpu_s) / (h.t - l.t) * 100.0,
        _ => 0.0,
    }
}

fn window<'a>(s: &'a [Sample], a: f64, b: f64) -> Vec<&'a Sample> {
    s.iter().filter(|x| x.t >= a && x.t <= b).collect()
}

fn mean(v: impl Iterator<Item = f64>) -> f64 {
    let (n, s) = v.fold((0usize, 0.0), |(n, s), x| (n + 1, s + x));
    if n == 0 { 0.0 } else { s / n as f64 }
}

pub fn ru(
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    rate: u32,
    seed: u64,
    out: &mut RunOut,
) -> Result<()> {
    let typ = rate == u32::MAX;
    let id = if typ { "RU-typ".to_owned() } else { format!("RU-{rate}") };
    let mut rig = m1_rig(host, reader, flavor)?;
    let pid = rig.reader.pid;
    std::thread::sleep(Duration::from_secs(2)); // startup settles
    let t0 = Instant::now();
    let samples: Arc<Mutex<Vec<Sample>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (samples, stop) = (Arc::clone(&samples), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("ru-sampler".to_owned())
            .spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    if let Some(s) = read_sample(pid, t0) {
                        if let Ok(mut g) = samples.lock() {
                            g.push(s);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(SAMPLE_MS));
                }
            })?
    };
    std::thread::sleep(Duration::from_secs_f64(IDLE_S));
    let t_load = t0.elapsed().as_secs_f64();
    let mut t_steady0 = t_load;
    let mut t_steady1 = t_load;
    let mut t_stop = t_load;
    let mut achieved = 0.0;
    let mut max_lag_ms = 0.0;

    if rate == 0 {
        // Idle baseline: nothing at all for a further 8 s.
        std::thread::sleep(Duration::from_secs(8));
        t_steady1 = t0.elapsed().as_secs_f64();
        t_stop = t_steady1;
        let _ = rig.account_speech();
    } else {
        let storm_rate = if typ { 200.0 } else { f64::from(rate) };
        let storm = start_storm(&rig.env.a11y_addr, storm_rate, 600.0)?;
        t_steady0 = t0.elapsed().as_secs_f64() + WARM_S;
        std::thread::sleep(Duration::from_secs_f64(WARM_S));
        let mut storm_slot = Some(storm);
        let mut result = (0.0, 0.0);
        let mut hook = |_rig: &mut Rig| {
            t_steady1 = t0.elapsed().as_secs_f64();
            if let Some(st) = storm_slot.take() {
                st.stop.store(true, Ordering::Release);
                if let Ok(fr) = st.thread.join() {
                    result = (fr.achieved_rate(), fr.max_lag_ns as f64 / 1e6);
                }
            }
            t_stop = t0.elapsed().as_secs_f64();
            std::thread::sleep(Duration::from_secs_f64(RECOVER_S));
        };
        if typ {
            let mut rng = Rng::new(seed);
            let mut ty = super::m1::Typist::new();
            let c = time_chord();
            let end = Instant::now() + Duration::from_secs(10);
            while Instant::now() < end {
                chord(&mut rig, &mut ty, &c)?;
                pause(&mut rng, 150, 400);
            }
            hook(&mut rig);
            let _ = rig.account_speech();
        } else {
            chord_series(
                &mut rig,
                out,
                "k2_ms",
                &time_chord(),
                700,
                seed,
                Before::Nothing,
                Some(&mut hook),
            )?;
        }
        achieved = result.0;
        max_lag_ms = result.1;
    }
    stop.store(true, Ordering::Release);
    let _ = sampler.join();
    let s = samples.lock().map(|g| g.clone()).unwrap_or_default();

    if let Ok(dir) = std::env::var("RU_OUT") {
        let mut csv = String::from("t_s,cpu_s,rss_kb,pss_kb,hwm_kb,threads\n");
        for x in &s {
            csv.push_str(&format!(
                "{:.3},{:.2},{},{},{},{}\n",
                x.t, x.cpu_s, x.rss_kb, x.pss_kb, x.hwm_kb, x.threads
            ));
        }
        let _ = std::fs::write(format!("{dir}/{id}-{pid}.csv"), csv);
        if let Some(log) = rig.reader_log.as_deref() {
            let _ = std::fs::copy(log, format!("{dir}/{id}-{pid}.reader.log"));
        }
        out.notes.push(format!("raw samples: {dir}/{id}-{pid}.csv"));
    }

    let idle_cpu = cpu_pct(&s, 0.0, t_load);
    let pre = window(&s, 0.0, t_load);
    let steady = window(&s, t_steady0, t_steady1);
    let mib = |kb: f64| kb / 1024.0;
    out.x("idle_cpu_pct", idle_cpu, "% of one core");
    out.x("pre_rss_mib", mib(mean(pre.iter().map(|x| x.rss_kb))), "MiB");
    out.x("pre_pss_mib", mib(mean(pre.iter().map(|x| x.pss_kb))), "MiB");
    out.x("steady_cpu_pct", cpu_pct(&s, t_steady0, t_steady1), "% of one core");
    // 1 s windows over the steady phase: spread of the CPU figure.
    let mut wins = Vec::new();
    let mut a = t_steady0;
    while a + 1.0 <= t_steady1 {
        wins.push(cpu_pct(&s, a, a + 1.0));
        a += 1.0;
    }
    if !wins.is_empty() {
        out.s("cpu_pct_1s_windows", wins);
    }
    out.x("steady_rss_avg_mib", mib(mean(steady.iter().map(|x| x.rss_kb))), "MiB");
    out.x("steady_pss_avg_mib", mib(mean(steady.iter().map(|x| x.pss_kb))), "MiB");
    out.x(
        "peak_rss_hwm_mib",
        mib(s.iter().map(|x| x.hwm_kb).fold(0.0, f64::max)),
        "MiB",
    );
    out.x(
        "peak_pss_mib",
        mib(s.iter().map(|x| x.pss_kb).fold(0.0, f64::max)),
        "MiB",
    );
    out.x("threads_max", s.iter().map(|x| x.threads).fold(0.0, f64::max), "threads");
    out.x("storm_achieved_eps", achieved, "ev/s");
    out.x("storm_max_lag_ms", max_lag_ms, "ms");
    // Recovery: first 1 s window after the storm below max(2 x idle, idle + 0.5 %).
    let thr = (idle_cpu * 2.0).max(idle_cpu + 0.5);
    let mut rec = RECOVER_S;
    let mut b = t_stop;
    while b + 1.0 <= t_stop + RECOVER_S {
        if cpu_pct(&s, b, b + 1.0) <= thr {
            rec = b - t_stop;
            break;
        }
        b += 0.2;
    }
    out.x("recovery_cpu_s", if rate == 0 { 0.0 } else { rec }, "s (cap 12)");
    let end = window(&s, t_stop + RECOVER_S - 2.0, t_stop + RECOVER_S + 1.0);
    out.x(
        "after_rss_mib",
        mib(mean(end.iter().map(|x| x.rss_kb))),
        "MiB",
    );
    out.x(
        "after_cpu_pct",
        cpu_pct(&s, t_stop + RECOVER_S - 3.0, t_stop + RECOVER_S),
        "% of one core",
    );
    out.notes.push(format!(
        "{id}: reader pid {pid}, steady window {t_steady0:.1}..{t_steady1:.1} s, storm stop {t_stop:.1} s, {} samples",
        s.len()
    ));
    Ok(())
}
