//! Paced event feeders: one thread per simulated app writes preformatted signals into a sink
//! (the mock broker link, or a client connection to a real bus), stamping probe events with
//! the time just before the write.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::clock::now_ns;
use crate::mockbus::Link;
use crate::workload::{FENCE_ID, Gen, GenEvent, Kind, Mix, Scratch, probe_id};

/// Where a feeder writes. `send` may block (real socket) but must not on the mock link.
pub trait Sink: Send {
    /// `false` when the peer is gone (disconnected).
    fn send(&mut self, bytes: &[u8]) -> bool;
    /// Bytes known to be queued on the way to the reader.
    fn backlog(&self) -> usize;
}

#[derive(Debug)]
pub struct MockSink {
    pub link: Arc<Link>,
}

impl Sink for MockSink {
    fn send(&mut self, bytes: &[u8]) -> bool {
        self.link.send(bytes)
    }

    fn backlog(&self) -> usize {
        self.link.backlog()
    }
}

/// A client connection to a real bus (dbus-daemon or dbus-broker).
#[derive(Debug)]
pub struct SocketSink {
    pub stream: UnixStream,
}

impl Sink for SocketSink {
    fn send(&mut self, bytes: &[u8]) -> bool {
        self.stream.write_all(bytes).is_ok()
    }

    fn backlog(&self) -> usize {
        0
    }
}

#[derive(Clone, Debug)]
pub struct ProbePlan {
    /// One focus probe every `period` (id grows by one).
    pub period: Duration,
}

#[derive(Clone, Debug)]
pub struct AppPlan {
    pub role: String,
    pub seed: u64,
    pub mix: Mix,
    pub slots: u32,
    /// Data events per second; 0 = as fast as the sink takes them.
    pub rate: f64,
    /// Upper bound on events per write.
    pub burst: usize,
    /// Paced feeders (`rate > 0`) accumulate events for this long before one write, so a hot
    /// app produces multi-message `recv`s like a real dbus-broker socket under load (the
    /// broker forwards what it has read in one go). `Duration::ZERO` = one write per due
    /// event, the pre-2026-10-12 behaviour (each `recv` returns a single message).
    pub write_quantum: Duration,
    pub probe: Option<ProbePlan>,
    /// Send a final fence (a focus event) when done.
    pub fence: bool,
    /// Stop after this many data events (in addition to the duration).
    pub max_events: Option<u64>,
    /// Closed loop: do not write while the sink holds more than this many bytes for the reader.
    pub backlog_cap: Option<usize>,
}

impl AppPlan {
    pub fn new(role: &str, seed: u64, mix: Mix, rate: f64) -> Self {
        Self {
            role: role.to_owned(),
            seed,
            mix,
            slots: 2000,
            rate,
            burst: 64,
            write_quantum: Duration::from_micros(200),
            probe: None,
            fence: true,
            max_events: None,
            backlog_cap: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct FeedResult {
    /// Data events sent (excludes probes and fence).
    pub sent: u64,
    pub probes_sent: u32,
    /// `now_ns()` just before the write that carried probe `k`.
    pub probe_send_ns: Vec<u64>,
    pub fence_send_ns: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub bytes: u64,
    /// Largest amount by which the feeder was behind its schedule.
    pub max_lag_ns: u64,
    /// Highest backlog reported by the sink.
    pub backlog_hwm: usize,
    /// The sink reported the peer gone.
    pub peer_gone: bool,
}

impl FeedResult {
    pub fn achieved_rate(&self) -> f64 {
        let s = (self.end_ns.saturating_sub(self.start_ns)) as f64 / 1e9;
        if s > 0.0 { self.sent as f64 / s } else { 0.0 }
    }
}

/// Run one feeder to completion on the calling thread.
pub fn run_feeder(
    plan: &AppPlan,
    sender: Option<&str>,
    sink: &mut dyn Sink,
    duration: Duration,
    stop: &AtomicBool,
) -> FeedResult {
    let mut generator = Gen::new(plan.seed, plan.mix, plan.slots);
    let mut sc = Scratch::default();
    let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut r = FeedResult::default();
    let mut serial = 0u32;
    let mut next_serial = move || {
        serial = serial.wrapping_add(1).max(1);
        serial
    };
    let dur_ns = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
    let probe_period = plan
        .probe
        .as_ref()
        .map(|p| u64::try_from(p.period.as_nanos()).unwrap_or(u64::MAX));
    // Events that accrue in one write quantum, at least 1 (slow apps write per event) and at
    // most `burst`.
    let min_batch = ((plan.rate * plan.write_quantum.as_secs_f64()).ceil() as u64)
        .clamp(1, plan.burst.max(1) as u64);
    let sleepy = plan.rate > 0.0 && plan.rate < 20_000.0;
    let t0 = now_ns();
    r.start_ns = t0;
    let mut next_probe = t0 + probe_period.unwrap_or(0);
    let mut probes_in_batch: Vec<u32> = Vec::new();
    loop {
        let now = now_ns();
        let el = now - t0;
        if el >= dur_ns || stop.load(Ordering::Relaxed) {
            break;
        }
        if plan.max_events.is_some_and(|m| r.sent >= m) {
            if plan.max_events == Some(0) {
                // An idle app: stay silent for the whole duration.
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            break;
        }
        let want: u64 = if plan.rate > 0.0 {
            let due = (el as f64 * plan.rate / 1e9) as u64;
            let behind = due.saturating_sub(r.sent);
            if behind > 0 {
                r.max_lag_ns = r.max_lag_ns.max((behind as f64 * 1e9 / plan.rate) as u64);
            }
            behind.min(plan.burst as u64)
        } else {
            plan.burst as u64
        };
        let probe_due = probe_period.is_some() && now >= next_probe;
        if plan.rate > 0.0 && !probe_due && want < min_batch {
            std::hint::spin_loop();
            continue;
        }
        let mut n = want;
        if let Some(cap) = plan.backlog_cap
            && sink.backlog() >= cap
        {
            std::hint::spin_loop();
            continue;
        }
        if let Some(m) = plan.max_events {
            n = n.min(m - r.sent);
        }
        out.clear();
        probes_in_batch.clear();
        if let Some(p) = probe_period
            && now >= next_probe
        {
            let k = r.probes_sent;
            r.probes_sent += 1;
            next_probe += p;
            probes_in_batch.push(k);
            let ev = GenEvent {
                node: probe_id(k),
                kind: Kind::FocusSignal,
                seq: 0,
            };
            ev.encode(&mut out, &mut sc, next_serial(), sender);
        }
        for _ in 0..n {
            generator
                .next_event()
                .encode(&mut out, &mut sc, next_serial(), sender);
        }
        if out.is_empty() {
            if sleepy {
                std::thread::sleep(Duration::from_micros(20));
            } else {
                std::hint::spin_loop();
            }
            continue;
        }
        let stamp = now_ns();
        for &k in &probes_in_batch {
            debug_assert_eq!(r.probe_send_ns.len(), k as usize);
            r.probe_send_ns.push(stamp);
        }
        if !sink.send(&out) {
            r.peer_gone = true;
            break;
        }
        r.sent += n;
        r.bytes += out.len() as u64;
        r.backlog_hwm = r.backlog_hwm.max(sink.backlog());
    }
    if plan.fence && !r.peer_gone {
        out.clear();
        GenEvent {
            node: FENCE_ID,
            kind: Kind::FocusSignal,
            seq: 0,
        }
        .encode(&mut out, &mut sc, next_serial(), sender);
        r.fence_send_ns = now_ns();
        if !sink.send(&out) {
            r.peer_gone = true;
        }
        r.bytes += out.len() as u64;
    }
    r.end_ns = now_ns();
    r
}

/// Ground truth for a feeder run: replay the generator for `res.sent` events and fold them,
/// plus the probes and the fence (focus events). Returns the fold and the number of mapped
/// events (what `stats.pushed` must add up to).
pub fn truth_for(plan: &AppPlan, res: &FeedResult, app: gorca_model::AppId) -> crate::model::Fold {
    use crate::model::{Ev, Fold};
    use crate::workload::local_of;
    let mut g = Gen::new(plan.seed, plan.mix, plan.slots);
    let mut f = Fold::default();
    for _ in 0..res.sent {
        for e in g.next_event().expected(app) {
            f.pushed += 1;
            f.apply(&e);
        }
    }
    for k in 0..res.probes_sent {
        f.pushed += 1;
        f.apply(&Ev::Focus(local_of(probe_id(k))));
    }
    if plan.fence && !res.peer_gone {
        f.pushed += 1;
        f.apply(&Ev::Focus(local_of(FENCE_ID)));
    }
    f
}
