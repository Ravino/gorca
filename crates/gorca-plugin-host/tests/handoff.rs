// SPDX-License-Identifier: GPL-3.0-or-later
//! The planned handoff of the actor runner (ADR-0026 section 2.4.7.4): a hold per waited job, jobs
//! that are dispatched but not waited for, the absolute clamp, hot and cold start reports, and the
//! single batched wake of parked workers. Fake executors: no wasm, only the runner's mechanics.
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use gorca_plugin_host::actor::{ActorRunner, Config};
use gorca_plugin_host::call::{
    DeriveJob, Exec, PlannedJob, PluginReply, ReplySink, ReplyTiming, Turn, WLaneRunner,
};
use gorca_plugin_host::limits::{FuelGrant, KAPPA_PROVISIONAL};
use gorca_plugin_host::view::ModelView;
use gorca_rt::process::monotonic_ns;

#[derive(Debug)]
struct NoView;

impl ModelView for NoView {
    fn generation(&self, _i: u32) -> Option<u16> {
        None
    }
    fn role(&self, _i: u32) -> Option<u16> {
        None
    }
    fn states(&self, _i: u32) -> u64 {
        0
    }
    fn name(&self, _i: u32) -> Option<String> {
        None
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

/// Answers `Cancelled` after sleeping `delay`.
struct Delay(Duration);

impl Exec for Delay {
    fn run(&mut self, job: &DeriveJob) -> PluginReply {
        if !self.0.is_zero() {
            std::thread::sleep(self.0);
        }
        PluginReply::Cancelled {
            plugin: job.plugin,
            job: job.job,
            slot_epoch: job.slot_epoch,
        }
    }
    fn reset(&mut self) {}
}

#[derive(Default)]
struct Mailbox {
    q: Mutex<Vec<(u32, Option<ReplyTiming>)>>,
    cv: Condvar,
}

impl ReplySink for Mailbox {
    fn send(&self, reply: PluginReply) {
        self.push(reply, None);
    }
    fn send_timed(&self, reply: PluginReply, timing: ReplyTiming) {
        self.push(reply, Some(timing));
    }
}

impl Mailbox {
    fn push(&self, reply: PluginReply, t: Option<ReplyTiming>) {
        let plugin = match reply {
            PluginReply::Done { plugin, .. }
            | PluginReply::Fault { plugin, .. }
            | PluginReply::Cancelled { plugin, .. } => plugin,
        };
        self.q.lock().unwrap().push((plugin, t));
        self.cv.notify_all();
    }

    fn wait_n(&self, n: usize, timeout: Duration) -> Vec<(u32, Option<ReplyTiming>)> {
        let mut g = self.q.lock().unwrap();
        let end = std::time::Instant::now() + timeout;
        while g.len() < n {
            let left = end.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            g = self.cv.wait_timeout(g, left).unwrap().0;
        }
        g.clone()
    }
}

fn runner(delays: &[Duration]) -> ActorRunner {
    let execs: Vec<(u32, Box<dyn Exec>)> = delays
        .iter()
        .enumerate()
        .map(|(i, d)| (i as u32, Box::new(Delay(*d)) as Box<dyn Exec>))
        .collect();
    ActorRunner::new(
        execs,
        Box::new(|_| None),
        Config {
            // A long job must not be called lost by the supervisor during these tests.
            lost_after: Duration::from_secs(10),
            ..Config::default()
        },
    )
    .unwrap()
}

fn job(plugin: u32, id: u64) -> DeriveJob {
    DeriveJob {
        plugin,
        job: id,
        slot_epoch: 0,
        interactive: true,
        epoch: 1,
        nodes: Vec::new(),
        view: Arc::new(NoView),
        grant: FuelGrant::for_time(Duration::from_micros(200), KAPPA_PROVISIONAL),
        queued_at: std::time::Instant::now(),
    }
}

fn planned(plugin: u32, id: u64, hold: Option<Duration>) -> PlannedJob {
    PlannedJob {
        job: job(plugin, id),
        hold,
    }
}

fn sink(mb: &Arc<Mailbox>) -> Arc<dyn ReplySink> {
    Arc::clone(mb) as Arc<dyn ReplySink>
}

#[test]
fn the_wait_ends_at_the_longest_hold_of_the_jobs_still_unanswered() {
    // A answers at once; B and C take 60 ms. Holds: A 20 ms, B 3 ms, C 6 ms. The wait ends when A
    // has replied and C's hold has passed: about 6 ms, never B's or A's 20 ms.
    let mut r = runner(&[
        Duration::ZERO,
        Duration::from_millis(60),
        Duration::from_millis(60),
    ]);
    let mb = Arc::new(Mailbox::default());
    let t0 = monotonic_ns();
    let turn = Turn {
        deadline_ns: t0 + 1_000_000_000,
        wait: true,
    };
    let out = r.interactive_planned(
        &turn,
        vec![
            planned(0, 1, Some(Duration::from_millis(20))),
            planned(1, 2, Some(Duration::from_millis(3))),
            planned(2, 3, Some(Duration::from_millis(6))),
        ],
        &sink(&mb),
    );
    assert_eq!(out.asked, 3);
    assert_eq!(out.arrived, 1, "{out:?}");
    assert!(out.waited >= Duration::from_millis(6), "{out:?}");
    assert!(out.waited < Duration::from_millis(40), "{out:?}");
    // The late replies still arrive through the mailbox.
    assert_eq!(mb.wait_n(3, Duration::from_secs(2)).len(), 3);
    r.shutdown();
}

#[test]
fn a_job_without_a_hold_is_dispatched_but_never_waited_for() {
    let mut r = runner(&[Duration::ZERO, Duration::from_millis(80)]);
    let mb = Arc::new(Mailbox::default());
    let t0 = monotonic_ns();
    let turn = Turn {
        deadline_ns: t0 + 1_000_000_000,
        wait: true,
    };
    let out = r.interactive_planned(
        &turn,
        vec![
            planned(1, 2, None),
            planned(0, 1, Some(Duration::from_millis(30))),
        ],
        &sink(&mb),
    );
    assert_eq!((out.asked, out.arrived), (1, 1), "{out:?}");
    assert!(out.waited < Duration::from_millis(30), "{out:?}");
    // The unwaited job ran and replied later.
    let all = mb.wait_n(2, Duration::from_secs(2));
    assert_eq!(all.len(), 2);
    r.shutdown();
}

#[test]
fn the_turn_deadline_clamps_every_hold() {
    let mut r = runner(&[Duration::from_millis(80)]);
    let mb = Arc::new(Mailbox::default());
    let t0 = monotonic_ns();
    let turn = Turn {
        deadline_ns: t0 + 3_000_000,
        wait: true,
    };
    let out = r.interactive_planned(
        &turn,
        vec![planned(0, 1, Some(Duration::from_secs(5)))],
        &sink(&mb),
    );
    assert_eq!(out.arrived, 0);
    let total = Duration::from_nanos(monotonic_ns() - t0);
    assert!(total >= Duration::from_millis(3), "{total:?}");
    assert!(total < Duration::from_millis(40), "{total:?}");
    r.shutdown();
}

#[test]
fn a_turn_that_may_not_wait_returns_at_once() {
    let mut r = runner(&[Duration::from_millis(50)]);
    let mb = Arc::new(Mailbox::default());
    let turn = Turn {
        deadline_ns: monotonic_ns() + 1_000_000_000,
        wait: false,
    };
    let out = r.interactive_planned(
        &turn,
        vec![planned(0, 1, Some(Duration::from_secs(1)))],
        &sink(&mb),
    );
    assert!(out.waited < Duration::from_millis(20), "{out:?}");
    assert_eq!(mb.wait_n(1, Duration::from_secs(2)).len(), 1);
    r.shutdown();
}

#[test]
fn replies_report_hot_and_cold_starts_and_arming_makes_a_worker_hot() {
    let mut r = runner(&[Duration::ZERO, Duration::ZERO]);
    let handle = r.prewake_handle();
    let mb = Arc::new(Mailbox::default());
    // Let both workers park (idle tail 40 us).
    std::thread::sleep(Duration::from_millis(30));
    // Arm worker 0 only, for a while.
    assert!(handle.arm_set(0b01, monotonic_ns() + 500_000_000));
    std::thread::sleep(Duration::from_millis(5));
    let turn = Turn {
        deadline_ns: monotonic_ns() + 50_000_000,
        wait: true,
    };
    let out = r.interactive_planned(
        &turn,
        vec![
            planned(0, 1, Some(Duration::from_millis(30))),
            planned(1, 2, Some(Duration::from_millis(30))),
        ],
        &sink(&mb),
    );
    assert_eq!(out.arrived, 2, "{out:?}");
    let all = mb.wait_n(2, Duration::from_secs(2));
    let t = |p: u32| {
        all.iter()
            .find(|(pl, _)| *pl == p)
            .and_then(|(_, t)| *t)
            .unwrap()
    };
    assert!(!t(0).cold, "the armed worker was spinning: hot");
    assert!(t(1).cold, "the other one had parked: cold");
    assert!(t(0).at_ns > 0 && t(1).at_ns > 0);
    // The handoff released the arm: both park again, and the next job is cold for both.
    std::thread::sleep(Duration::from_millis(30));
    let out = r.interactive_planned(
        &Turn {
            deadline_ns: monotonic_ns() + 50_000_000,
            wait: true,
        },
        vec![planned(0, 3, Some(Duration::from_millis(30)))],
        &sink(&mb),
    );
    assert_eq!(out.arrived, 1);
    let all = mb.wait_n(3, Duration::from_secs(2));
    let last = all.iter().rev().find(|(p, _)| *p == 0).unwrap();
    assert!(last.1.unwrap().cold, "disarmed: parked again");
    r.shutdown();
}

#[test]
fn one_batched_wake_serves_every_parked_worker() {
    let n = 8;
    let mut r = runner(&vec![Duration::ZERO; n]);
    let mb = Arc::new(Mailbox::default());
    let mut complete = 0;
    let rounds = 60;
    for round in 0..rounds {
        // Long enough for all workers to have parked (idle tail 40 us).
        std::thread::sleep(Duration::from_millis(3));
        let jobs = (0..n as u32)
            .map(|p| {
                planned(
                    p,
                    round as u64 * 100 + u64::from(p),
                    Some(Duration::from_millis(8)),
                )
            })
            .collect();
        let out = r.interactive_planned(
            &Turn {
                deadline_ns: monotonic_ns() + 50_000_000,
                wait: true,
            },
            jobs,
            &sink(&mb),
        );
        complete += usize::from(out.arrived == out.asked);
    }
    // A lost wake would leave the worker asleep until its 20 ms park timeout, past the 8 ms hold.
    assert!(
        complete * 100 >= rounds * 90,
        "{complete} of {rounds} handoffs complete"
    );
    assert_eq!(
        mb.wait_n(rounds * n, Duration::from_secs(5)).len(),
        rounds * n
    );
    r.shutdown();
}
