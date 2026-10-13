//! Scenarios: steady-state mixed load (readers + one paced or flat-out writer + visibility
//! prober) and the 100k-node subtree removal.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::hist::Hist;
use crate::store::{NONE, Reader, Session, Store, Writer};
use crate::wl::{PROBE, ReadGen, WriteGen, build, cpu_secs, loadavg};

const WARM: u8 = 0;
const MEASURE: u8 = 1;
const STOP: u8 = 255;
const TICK: Duration = Duration::from_millis(1);
const FLAT_OUT_BATCH: u64 = 32;

#[derive(Clone, Copy, Debug)]
pub enum Rate {
    /// No writer.
    Off,
    /// Mutations per second, applied in 1 ms batches.
    Hz(u64),
    /// Flat out in batches of 32.
    Max,
}

impl Rate {
    pub fn label(self) -> String {
        match self {
            Rate::Off => "0".into(),
            Rate::Hz(h) => h.to_string(),
            Rate::Max => "max".into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub cmds_per_s: f64,
    pub read: Hist,
    pub writes_per_s: f64,
    pub batch: Hist,
    pub vis: Hist,
    pub proc_cpu: f64,
    pub writer_cpu: f64,
    pub load1: f64,
}

fn since(base: Instant) -> u64 {
    base.elapsed().as_nanos() as u64
}

fn wait_until(deadline: Instant) {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let rem = deadline - now;
        if rem > Duration::from_micros(120) {
            std::thread::sleep(rem - Duration::from_micros(80));
        } else {
            std::hint::spin_loop();
        }
    }
}

pub fn run_cell<S: Store>(
    n: u64,
    rate: Rate,
    readers: usize,
    warm: Duration,
    secs: Duration,
) -> Metrics {
    let store = S::new();
    build(&mut store.writer(), n);
    let phase = AtomicU8::new(WARM);
    let base = Instant::now();
    let w_ops = AtomicU64::new(0);
    let mut out = Metrics {
        load1: loadavg(),
        ..Metrics::default()
    };
    let mut p0 = 0.0;
    let mut wall = secs.as_secs_f64();

    std::thread::scope(|sc| {
        let mut rh = Vec::new();
        for t in 0..readers {
            let (store, phase) = (&store, &phase);
            rh.push(sc.spawn(move || {
                let mut r = store.reader();
                let mut g = ReadGen::new(1000 + t as u64, n);
                let mut h = Hist::default();
                let mut sink = 0u64;
                loop {
                    let ph = phase.load(Ordering::Relaxed);
                    if ph == STOP {
                        break;
                    }
                    let t0 = Instant::now();
                    sink = sink.wrapping_add(g.command(&mut r));
                    let dt = t0.elapsed().as_nanos() as u64;
                    if ph == MEASURE {
                        h.record(dt);
                    }
                }
                std::hint::black_box(sink);
                h
            }));
        }
        let prober = (!matches!(rate, Rate::Off)).then(|| {
            let (store, phase) = (&store, &phase);
            sc.spawn(move || {
                let mut r = store.reader();
                let mut h = Hist::default();
                let mut last = 0u32;
                loop {
                    let ph = phase.load(Ordering::Relaxed);
                    if ph == STOP {
                        break;
                    }
                    let seen = {
                        let s = r.session();
                        s.get(PROBE).map(|p| (p.version, p.states))
                    };
                    if let Some((seq, t_write)) = seen {
                        if seq != last {
                            last = seq;
                            if ph == MEASURE {
                                h.record(since(base).saturating_sub(t_write));
                            }
                        }
                    }
                }
                h
            })
        });
        let writer = (!matches!(rate, Rate::Off)).then(|| {
            let (store, phase, w_ops) = (&store, &phase, &w_ops);
            sc.spawn(move || {
                let mut w = store.writer();
                let mut g = WriteGen::new(7, n);
                let (per_tick, paced) = match rate {
                    Rate::Hz(h) => ((h / 1000).max(1), true),
                    _ => (FLAT_OUT_BATCH, false),
                };
                let mut batch = Hist::default();
                let mut seq = 0u32;
                let mut next = Instant::now();
                let mut cpu0 = None;
                let mut counted = 0u64;
                loop {
                    let ph = phase.load(Ordering::Relaxed);
                    if ph == STOP {
                        break;
                    }
                    if ph == MEASURE && cpu0.is_none() {
                        cpu0 = Some(cpu_secs(true));
                        g.mutations = 0;
                    }
                    let t0 = Instant::now();
                    let m0 = g.mutations;
                    while g.mutations - m0 < per_tick {
                        g.op(&mut w);
                    }
                    seq = seq.wrapping_add(1).max(1);
                    let stamp = since(base);
                    let mut probe = crate::wl::mk_leaf(PROBE, NONE, seq);
                    if let Some(p) = Arc::get_mut(&mut probe) {
                        p.states = stamp;
                    }
                    w.put(PROBE, probe);
                    w.publish();
                    if ph == MEASURE {
                        batch.record(t0.elapsed().as_nanos() as u64);
                        counted = g.mutations;
                    }
                    if paced {
                        next += TICK;
                        let now = Instant::now();
                        if now > next + Duration::from_millis(20) {
                            next = now; // fell behind: drop the backlog instead of bursting
                        }
                        wait_until(next);
                    }
                }
                let cpu = cpu_secs(true) - cpu0.unwrap_or(0.0);
                w_ops.store(counted, Ordering::Relaxed);
                (batch, cpu)
            })
        });

        std::thread::sleep(warm);
        p0 = cpu_secs(false);
        let t_start = Instant::now();
        phase.store(MEASURE, Ordering::Relaxed);
        std::thread::sleep(secs);
        out.proc_cpu = cpu_secs(false) - p0;
        wall = t_start.elapsed().as_secs_f64();
        phase.store(STOP, Ordering::Relaxed);

        for h in rh {
            if let Ok(h) = h.join() {
                out.read.merge(&h);
            }
        }
        if let Some(p) = prober {
            if let Ok(h) = p.join() {
                out.vis = h;
            }
        }
        if let Some(w) = writer {
            if let Ok((b, cpu)) = w.join() {
                out.batch = b;
                out.writer_cpu = cpu;
            }
        }
    });
    out.proc_cpu /= wall;
    out.writer_cpu /= wall;
    out.cmds_per_s = out.read.count() as f64 / wall;
    out.writes_per_s = w_ops.load(Ordering::Relaxed) as f64 / wall;
    out
}

#[derive(Clone, Debug, Default)]
pub struct Removal {
    pub base: Hist,
    pub during: Hist,
    pub after: Hist,
    pub base_cmds_per_s: f64,
    pub during_cmds_per_s: f64,
    pub removal_ms: f64,
    pub survivors: u64,
    pub load1: f64,
}

/// Tree of `2 * doomed` nodes; the keys `doomed..2*doomed` are removed in `chunk`-key batches
/// (one publish per chunk).
pub fn run_removal<S: Store>(doomed: u64, chunk: usize, readers: usize) -> Removal {
    let n = doomed * 2;
    let store = S::new();
    build(&mut store.writer(), n);
    let phase = AtomicU8::new(0);
    let mut out = Removal {
        load1: loadavg(),
        ..Removal::default()
    };
    let base_dur = Duration::from_millis(1200);
    let mut during_secs = 0.0;
    std::thread::scope(|sc| {
        let mut rh = Vec::new();
        for t in 0..readers {
            let (store, phase) = (&store, &phase);
            rh.push(sc.spawn(move || {
                let mut r = store.reader();
                let mut g = ReadGen::new(500 + t as u64, n);
                let mut hs = [
                    Hist::default(),
                    Hist::default(),
                    Hist::default(),
                    Hist::default(),
                ];
                let mut sink = 0u64;
                loop {
                    let ph = phase.load(Ordering::Relaxed);
                    if ph == STOP {
                        break;
                    }
                    let t0 = Instant::now();
                    sink = sink.wrapping_add(g.command(&mut r));
                    hs[usize::from(ph).min(3)].record(t0.elapsed().as_nanos() as u64);
                }
                std::hint::black_box(sink);
                hs
            }));
        }
        std::thread::sleep(Duration::from_millis(300)); // warm-up (phase 0, discarded)
        phase.store(1, Ordering::Relaxed);
        std::thread::sleep(base_dur);
        phase.store(2, Ordering::Relaxed);
        let keys: Vec<u64> = (doomed..n).collect();
        let t0 = Instant::now();
        {
            let mut w = store.writer();
            for c in keys.chunks(chunk) {
                for k in c {
                    w.remove(*k);
                }
                w.publish();
            }
        }
        out.removal_ms = t0.elapsed().as_secs_f64() * 1e3;
        during_secs = t0.elapsed().as_secs_f64();
        phase.store(3, Ordering::Relaxed);
        std::thread::sleep(base_dur);
        phase.store(STOP, Ordering::Relaxed);
        for h in rh {
            if let Ok(hs) = h.join() {
                out.base.merge(&hs[1]);
                out.during.merge(&hs[2]);
                out.after.merge(&hs[3]);
            }
        }
    });
    out.base_cmds_per_s = out.base.count() as f64 / base_dur.as_secs_f64();
    out.during_cmds_per_s = out.during.count() as f64 / during_secs.max(1e-9);
    // Verify the removal really happened.
    let mut r = store.reader();
    let s = r.session();
    out.survivors = (doomed..n).filter(|k| s.get(*k).is_some()).count() as u64;
    out
}
