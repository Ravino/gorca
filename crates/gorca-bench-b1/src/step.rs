// SPDX-License-Identifier: GPL-3.0-or-later
//! H-7 / MA-6: in-process microbenchmark of one policy `step()` at the SPEC 7 caps.
//!
//! The harness does not know the policy type (it does not exist on `main` yet and each arm brings
//! its own `Candidate`). It generates the worst-case inputs as neutral data ([`StepInput`]) and
//! times any [`StepSubject`]; an adapter next to the arm's code (`benches/step.rs`, one `impl`)
//! converts the neutral inputs into `FillInput`s and calls `policy.step`. The result is the
//! `step.json` of a run directory: per round the worst step over all scenarios and iterations
//! (`max_us`, what B1-STEP.max_us gates on the median of), plus p50/p99 per scenario.

use std::fmt::Write as _;
use std::time::Instant;

use gorca_bench_b2::stats::summarize;

/// Caps of SPEC 7 / MA-6.
pub const CAP_NODES: u32 = 65_536;
pub const CAP_EVENT_BATCH: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Properties,
    Children,
    Removed,
    Text,
    Window,
}

#[derive(Clone, Copy, Debug)]
pub struct StepEvent {
    pub node: u32,
    pub kind: EventKind,
}

/// One policy input at the caps.
#[derive(Clone, Debug)]
pub struct StepInput {
    pub name: &'static str,
    /// Live nodes in the mirror while the step runs.
    pub mirror_nodes: u32,
    pub events: Vec<StepEvent>,
    /// `FillInput::Resync` with dirty-all.
    pub dirty_all: bool,
    /// `FillInput::Demand`/`FocusSettled`/`Tick` shaped inputs (the subject decides how).
    pub tick: bool,
}

/// The inputs MA-6 names: 65 536 nodes, dirty-all, a 4 096-event batch, plus the small ones that
/// run most often (a single event, a tick) so a policy cannot hide a slow common path.
pub fn scenarios(seed: u64) -> Vec<StepInput> {
    let mut rng = seed | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let kinds = [
        EventKind::Properties,
        EventKind::Children,
        EventKind::Removed,
        EventKind::Text,
        EventKind::Window,
    ];
    let mut batch = |n: usize, nodes: u32| -> Vec<StepEvent> {
        (0..n)
            .map(|_| StepEvent {
                node: (next() % u64::from(nodes)) as u32,
                kind: kinds[(next() % kinds.len() as u64) as usize],
            })
            .collect()
    };
    vec![
        StepInput {
            name: "batch-4096@65536",
            mirror_nodes: CAP_NODES,
            events: batch(CAP_EVENT_BATCH, CAP_NODES),
            dirty_all: false,
            tick: false,
        },
        StepInput {
            name: "dirty-all@65536",
            mirror_nodes: CAP_NODES,
            events: Vec::new(),
            dirty_all: true,
            tick: false,
        },
        StepInput {
            name: "dirty-all+batch-4096@65536",
            mirror_nodes: CAP_NODES,
            events: batch(CAP_EVENT_BATCH, CAP_NODES),
            dirty_all: true,
            tick: false,
        },
        StepInput {
            name: "single-event@65536",
            mirror_nodes: CAP_NODES,
            events: batch(1, CAP_NODES),
            dirty_all: false,
            tick: false,
        },
        StepInput {
            name: "tick@65536",
            mirror_nodes: CAP_NODES,
            events: Vec::new(),
            dirty_all: false,
            tick: true,
        },
        StepInput {
            name: "batch-4096@400",
            mirror_nodes: 400,
            events: batch(CAP_EVENT_BATCH, 400),
            dirty_all: false,
            tick: false,
        },
    ]
}

/// Something with a `step`. Implemented by the adapter of an arm and by the built-in subjects.
pub trait StepSubject {
    fn name(&self) -> String;
    /// Fill the mirror/state up to `nodes` before a scenario is timed. Not timed.
    fn prepare(&mut self, nodes: u32);
    /// Do one step; the return value is folded into a checksum so the work cannot be elided.
    fn step(&mut self, input: &StepInput) -> u64;
}

/// Does nothing but read the input: the harness floor.
#[derive(Debug, Default)]
pub struct NoopSubject;

impl StepSubject for NoopSubject {
    fn name(&self) -> String {
        "noop".into()
    }
    fn prepare(&mut self, _nodes: u32) {}
    fn step(&mut self, input: &StepInput) -> u64 {
        input.events.len() as u64 + u64::from(input.dirty_all)
    }
}

/// Spins for `us` microseconds per step: proves the bench notices a slow policy (sensitivity).
#[derive(Debug)]
pub struct SpinSubject {
    pub us: u64,
}

impl StepSubject for SpinSubject {
    fn name(&self) -> String {
        format!("spin-{}us", self.us)
    }
    fn prepare(&mut self, _nodes: u32) {}
    fn step(&mut self, input: &StepInput) -> u64 {
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed().as_micros() < u128::from(self.us) {
            x = x.wrapping_add(1);
        }
        x.wrapping_add(input.events.len() as u64)
    }
}

#[derive(Clone, Debug)]
pub struct ScenarioResult {
    pub name: &'static str,
    pub p50_us: f64,
    pub p99_us: f64,
    pub max_us: f64,
}

#[derive(Clone, Debug)]
pub struct Round {
    pub max_us: f64,
    pub scenarios: Vec<ScenarioResult>,
}

#[derive(Clone, Debug)]
pub struct StepResult {
    pub subject: String,
    pub iters: usize,
    pub rounds: Vec<Round>,
    pub checksum: u64,
}

/// `rounds` rounds of `iters` timed steps per scenario, after `warmup` untimed ones.
pub fn measure(
    subject: &mut dyn StepSubject,
    inputs: &[StepInput],
    rounds: usize,
    iters: usize,
    warmup: usize,
) -> StepResult {
    let mut checksum = 0u64;
    let mut out = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        let mut worst = 0.0f64;
        let mut per = Vec::new();
        for input in inputs {
            subject.prepare(input.mirror_nodes);
            for _ in 0..warmup {
                checksum = checksum.wrapping_add(subject.step(input));
            }
            let mut us = Vec::with_capacity(iters);
            for _ in 0..iters {
                let t = Instant::now();
                let r = subject.step(input);
                let d = t.elapsed();
                checksum = checksum.wrapping_add(r);
                us.push(d.as_secs_f64() * 1e6);
            }
            let s = summarize(&us);
            worst = worst.max(s.max);
            per.push(ScenarioResult {
                name: input.name,
                p50_us: s.p50,
                p99_us: s.p99,
                max_us: s.max,
            });
        }
        out.push(Round {
            max_us: worst,
            scenarios: per,
        });
    }
    StepResult {
        subject: subject.name(),
        iters,
        rounds: out,
        checksum,
    }
}

impl StepResult {
    /// `step.json` for the run directory.
    pub fn json(&self) -> String {
        let mut s = format!(
            "{{\"subject\": \"{}\", \"iters\": {}, \"checksum\": {}, \"rounds\": [",
            self.subject, self.iters, self.checksum
        );
        for (i, r) in self.rounds.iter().enumerate() {
            let _ = write!(
                s,
                "{}{{\"max_us\": {:.3}, \"scenarios\": {{",
                if i == 0 { "" } else { ", " },
                r.max_us
            );
            for (j, sc) in r.scenarios.iter().enumerate() {
                let _ = write!(
                    s,
                    "{}\"{}\": {{\"p50_us\": {:.3}, \"p99_us\": {:.3}, \"max_us\": {:.3}}}",
                    if j == 0 { "" } else { ", " },
                    sc.name,
                    sc.p50_us,
                    sc.p99_us,
                    sc.max_us
                );
            }
            s.push_str("}}");
        }
        s.push_str("]}\n");
        s
    }

    /// Median over rounds of the per-round worst step (what the gate reads).
    pub fn median_max_us(&self) -> f64 {
        let v: Vec<f64> = self.rounds.iter().map(|r| r.max_us).collect();
        gorca_bench_b2::stats::spread(&v).median
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_hit_the_caps() {
        let s = scenarios(7);
        assert!(s.iter().any(|x| x.mirror_nodes == CAP_NODES && x.dirty_all));
        assert!(
            s.iter()
                .any(|x| x.events.len() == CAP_EVENT_BATCH && x.mirror_nodes == CAP_NODES)
        );
        assert!(
            s.iter()
                .all(|x| x.events.iter().all(|e| e.node < x.mirror_nodes))
        );
        // Deterministic for a seed.
        assert_eq!(
            scenarios(7)[0].events.iter().map(|e| e.node).sum::<u32>(),
            s[0].events.iter().map(|e| e.node).sum::<u32>()
        );
    }

    #[test]
    fn noop_is_far_below_the_gate_and_a_slow_subject_is_caught() {
        let inputs = scenarios(1);
        let fast = measure(&mut NoopSubject, &inputs, 7, 50, 5);
        assert!(
            fast.median_max_us() < 250.0,
            "floor {} us",
            fast.median_max_us()
        );
        let slow = measure(&mut SpinSubject { us: 400 }, &inputs[..1], 3, 5, 1);
        assert!(slow.median_max_us() >= 400.0, "{}", slow.median_max_us());
    }

    #[test]
    fn json_is_parseable_and_has_the_gate_field() {
        let r = measure(&mut NoopSubject, &scenarios(3)[..2], 2, 3, 1);
        let v: serde_json::Value = serde_json::from_str(&r.json()).unwrap();
        assert_eq!(v["rounds"].as_array().map(Vec::len), Some(2));
        assert!(v["rounds"][0]["max_us"].as_f64().is_some());
    }
}
