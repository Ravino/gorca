// SPDX-License-Identifier: GPL-3.0-or-later
//! SPS-6: the Rust scheduler reproduces `tools/research/plugin_sched/sps.py` bit for bit on the
//! recorded log of 17 594 calls (oracle adversary, seed 7, 3 000 keypresses, `dump_replay.py`).
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use gorca_plugin_sched::{
    Assignment, Label, Observation, Params, Place, PlaceMode, Scheduler, SpinBucket, Start, Tier,
    Triggered, TurnTimes,
};

const LOG: &str = include_str!("data/sps-replay-3000-seed7.log");

fn list(s: &str) -> Vec<usize> {
    if s == "-" {
        Vec::new()
    } else {
        s.split(',').map(|v| v.parse().unwrap()).collect()
    }
}

fn mask(v: &[usize]) -> u32 {
    v.iter().fold(0, |m, p| m | (1 << p))
}

#[test]
fn rust_equals_the_reference_on_the_recorded_log() {
    let mut plugins = Vec::new();
    for l in LOG.lines().filter(|l| l.starts_with("H ")) {
        let f: Vec<&str> = l.split(' ').collect();
        let label = match f[2] {
            "interactive" => Label::Interactive,
            "default" => Label::Unlabelled,
            "refinement" => Label::Refinement,
            other => panic!("label {other}"),
        };
        let tier = match f[3] {
            "first" => Tier::First,
            "trusted" => Tier::Trusted,
            "third" => Tier::Third,
            other => panic!("tier {other}"),
        };
        plugins.push((label, tier));
    }
    let p = Params {
        reference_negative_bucket: true,
        reference_prewake_ignores_exclusion: true,
        ..Params::default()
    };
    let mut s = Scheduler::new(&plugins, p);
    let mut bucket = SpinBucket::new(p.spin_bucket_us, p.spin_rate_us_per_s);
    let (mut calls, mut mismatches) = (0u64, 0u64);
    // FNV over the Rust outputs, to pin the log against silent edits.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut put = |v: u64| {
        for b in v.to_le_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for l in LOG.lines().filter(|l| !l.starts_with("H ")) {
        calls += 1;
        let (head, out) = l.split_once(" | ").map_or((l, ""), |(a, b)| (a, b));
        let f: Vec<&str> = head.split(' ').collect();
        match f[0] {
            "K" => {
                let ctx: usize = f[1].parse().unwrap();
                let now_ms: u64 = f[2].parse().unwrap();
                let k = s.keydown_ctx(ctx, false, now_ms * 1000, &mut bucket);
                // The reference settles through explicit `R` lines.
                s.disarm_all();
                let want: u8 = out.trim().parse().unwrap();
                if k.threads != want && mismatches < 6 {
                    eprintln!("call {calls} {l}: got k={}", k.threads);
                }
                mismatches += u64::from(k.threads != want);
                put(u64::from(k.threads));
            }
            "R" => bucket.settle(f[1].parse().unwrap(), f[2].parse().unwrap()),
            "C" => s.observe_context(f[1].parse().unwrap(), mask(&list(f[2]))),
            "D" => {
                let k: usize = f[1].parse().unwrap();
                let trig: Vec<Triggered> = list(f[2])
                    .into_iter()
                    .map(|pid| Triggered { pid, busy: false })
                    .collect();
                let plan = s.plan(
                    &trig,
                    TurnTimes {
                        now_us: 0,
                        spent_us: None,
                    },
                    PlaceMode::Queues(k),
                );
                let parts: Vec<&str> = out.split(" | ").collect();
                let t_star: u32 = parts[0].parse().unwrap();
                let wait = list(parts[1]);
                let got_wait: Vec<usize> = plan.wait_set().map(|a| a.pid).collect();
                let want_asg: Vec<(usize, i64, u32, u32, u64, u32)> = parts[2]
                    .split(' ')
                    .map(|a| {
                        let v: Vec<&str> = a.split(':').collect();
                        (
                            v[0].parse().unwrap(),
                            v[1].parse().unwrap(),
                            v[2].parse().unwrap(),
                            v[3].parse().unwrap(),
                            v[4].parse().unwrap(),
                            v[5].parse().unwrap(),
                        )
                    })
                    .collect();
                let got_asg: Vec<(usize, i64, u32, u32, u64, u32)> = plan
                    .assignments()
                    .iter()
                    .map(|a| {
                        let hot = match a.place {
                            Place::Hot(k) => i64::from(k),
                            Place::Parked => -1,
                        };
                        (
                            a.pid,
                            hot,
                            a.offset_us,
                            a.deadline_us,
                            a.grant_fuel,
                            a.hold_us,
                        )
                    })
                    .collect();
                if t_star != plan.t_star_us || wait != got_wait || want_asg != got_asg {
                    mismatches += 1;
                    if mismatches < 4 {
                        eprintln!(
                            "line {l}\n  got t*={} wait={got_wait:?} {got_asg:?}",
                            plan.t_star_us
                        );
                    }
                }
                for a in &got_asg {
                    put(a.0 as u64);
                    put(a.4);
                    put(u64::from(a.5));
                }
            }
            "O" => {
                let pid: usize = f[1].parse().unwrap();
                let state = if f[2] == "hot" {
                    Start::Hot
                } else {
                    Start::Cold
                };
                let a = Assignment {
                    pid,
                    place: Place::Parked,
                    offset_us: 0,
                    deadline_us: f[8].parse().unwrap(),
                    grant_fuel: 0,
                    exec_allow_us: f[10].parse().unwrap(),
                    clipped: f[11] == "1",
                    hold_us: f[9].parse().unwrap(),
                };
                s.observe(&Observation {
                    pid,
                    state,
                    start_us: f[3].parse().unwrap(),
                    exec_us: f[4].parse().unwrap(),
                    resp_us: f[5].parse().unwrap(),
                    assignment: &a,
                    waited: f[6] == "1",
                    useful: f[7] == "1",
                });
            }
            other => panic!("line kind {other}"),
        }
    }
    assert_eq!(
        calls, 17_594,
        "the corpus is the 17 594-call log of the report"
    );
    assert_eq!(mismatches, 0, "decisions differ from sps.py");
    assert!(s.fallback().is_none());
    eprintln!("replay: {calls} calls, 0 mismatches, rust output hash {hash:016x}");
}
