// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;
use hist::{Hist, bucket};

/// A deterministic generator (no dependency, no clock).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn tz() -> TurnTimes {
    TurnTimes {
        now_us: 0,
        spent_us: None,
    }
}

fn trig(pids: &[usize]) -> Vec<Triggered> {
    pids.iter()
        .map(|p| Triggered {
            pid: *p,
            busy: false,
        })
        .collect()
}

fn wait_pids(p: &Plan) -> Vec<usize> {
    p.wait_set().map(|a| a.pid).collect()
}

// ---- histogram ---------------------------------------------------------------------------

#[test]
fn histogram_quantiles_are_upper_edges_and_the_hit_rate_is_a_lower_bound() {
    let mut h = Hist::new();
    for x in [3, 5, 9, 9, 20, 40, 70, 130, 300, 2000] {
        h.add(x);
    }
    assert_eq!(h.total(), 160);
    assert_eq!(h.quantile(Q / 2), 24, "5th sample (20) is in [16, 24)");
    assert_eq!(h.quantile(Q * 99 / 100), INF_US);
    assert_eq!(h.cdf_at(12), 4 * Q / 10, "3, 5, 9, 9 lie below 12");
    assert_eq!(
        h.cdf_at(11),
        2 * Q / 10,
        "the bucket [8,12) is not wholly below 11"
    );
    assert_eq!(h.cdf_at(-5), 0);
    assert_eq!(ewma_q16(Q, 0, 5), Q - (Q >> 5));
    let mut h2 = Hist::new();
    for _ in 0..64 {
        h2.add(10);
    }
    assert_eq!(h2.total(), 64 * 16 / 2, "64 samples halve all counts");
    assert_eq!(Hist::new().quantile(Q / 2), INF_US);
    assert_eq!(Hist::new().cdf_at(4000), 0);
}

#[test]
fn bucket_lookup_matches_the_edge_table() {
    for x in 0..1100u32 {
        let b = bucket(x);
        assert!(EDGES[b] <= x);
        assert!(b + 1 == NB || x < EDGES[b + 1]);
    }
    assert_eq!(bucket(1_000_000), NB - 1);
}

// ---- the reference vectors ---------------------------------------------------------------

fn reference_world() -> (Scheduler, SpinBucket) {
    let mut s = Scheduler::new(
        &[
            (Label::Interactive, Tier::First),
            (Label::Unlabelled, Tier::Third),
            (Label::Refinement, Tier::First),
        ],
        Params::default(),
    );
    let a = |pid, hold, dl, allow, clipped| Assignment {
        pid,
        place: Place::Hot(0),
        offset_us: 0,
        deadline_us: dl,
        grant_fuel: 0,
        exec_allow_us: allow,
        clipped,
        hold_us: hold,
    };
    let _ = a;
    for _ in 0..200 {
        let obs = |s: &mut Scheduler, pid, st, start, exec, resp, dl, allow, waited, useful| {
            let asg = Assignment {
                pid,
                place: Place::Parked,
                offset_us: 0,
                deadline_us: dl,
                grant_fuel: 0,
                exec_allow_us: allow,
                clipped: false,
                hold_us: 0,
            };
            s.observe(&Observation {
                pid,
                state: st,
                start_us: start,
                exec_us: exec,
                resp_us: resp,
                assignment: &asg,
                waited,
                useful,
            });
        };
        obs(&mut s, 0, Start::Hot, 2, 40, 42, 120, 64, true, true);
        obs(&mut s, 0, Start::Cold, 200, 40, 240, 290, 64, false, true);
        obs(&mut s, 1, Start::Hot, 2, 10, 12, 60, 24, true, false);
        obs(&mut s, 2, Start::Cold, 200, 150, 350, 290, 200, false, true);
    }
    for _ in 0..50 {
        s.observe_context(1, 0b111);
    }
    (s, SpinBucket::standard())
}

#[test]
fn golden_vector_of_sps_py() {
    let (mut s, mut b) = reference_world();
    let k = s.keydown_ctx(1, false, 1_000_000, &mut b);
    assert_eq!(k.threads, 1);
    let plan = s.plan(&trig(&[0, 1, 2]), tz(), PlaceMode::Queues(1));
    let got: Vec<(usize, Option<u8>, u32, u32, u64, u32)> = plan
        .assignments()
        .iter()
        .map(|a| {
            let hot = match a.place {
                Place::Hot(k) => Some(k),
                Place::Parked => None,
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
    assert_eq!(
        got,
        vec![
            (0, Some(0), 0, 60, 4_086_932, 60),
            (2, None, 0, 286, 4_347_800, 0),
            (1, Some(0), 48, 72, 2_043_466, 0),
        ]
    );
    assert_eq!(wait_pids(&plan), vec![0]);
    assert_eq!(plan.t_star_us, 60);
}

#[test]
fn offence_escalates_forgives_and_ignores_host_lateness() {
    let (mut s, _) = reference_world();
    let late = Assignment {
        pid: 1,
        place: Place::Hot(0),
        offset_us: 0,
        deadline_us: 60,
        grant_fuel: 0,
        exec_allow_us: 24,
        clipped: false,
        hold_us: 60,
    };
    let obs = |s: &mut Scheduler, a: &Assignment, st, start, exec, resp, waited| {
        s.observe(&Observation {
            pid: 1,
            state: st,
            start_us: start,
            exec_us: exec,
            resp_us: resp,
            assignment: a,
            waited,
            useful: false,
        });
    };
    obs(&mut s, &late, Start::Hot, 2, 290, 292, true);
    assert_eq!(
        (s.stats(1).unwrap().offences, s.stats(1).unwrap().excluded),
        (1, 2)
    );
    obs(&mut s, &late, Start::Hot, 2, 290, 292, true);
    assert_eq!(
        (s.stats(1).unwrap().offences, s.stats(1).unwrap().excluded),
        (2, 4)
    );
    // Late because of the host (exec within the allowance), or the deadline was clipped: no offence.
    obs(&mut s, &late, Start::Cold, 280, 10, 292, true);
    let clipped = Assignment {
        clipped: true,
        ..late
    };
    obs(&mut s, &clipped, Start::Hot, 2, 290, 292, true);
    assert_eq!(s.stats(1).unwrap().offences, 2);
    for _ in 0..4 {
        obs(&mut s, &late, Start::Hot, 2, 10, 12, false);
    }
    assert_eq!(
        s.stats(1).unwrap().excluded,
        0,
        "exclusion counts down on every reply"
    );
}

// ---- SPS-1 / SPS-2: the writer's wait ---------------------------------------------------

#[test]
fn the_absolute_clamp_bounds_the_wait_whatever_the_statistics_say() {
    let mut r = Lcg(1);
    for _ in 0..100_000 {
        let t0 = r.below(1 << 40);
        let snap = t0 + r.below(400);
        let t_star = r.below(1 << 20);
        let fuse = r.below(30);
        let end = wait_end_us(t0, snap, t_star, 300, fuse);
        assert!(end <= t0 + 300 - fuse, "end {end} t0 {t0}");
        assert!(end <= snap + t_star);
    }
}

#[test]
fn corrupt_histograms_and_random_reply_times_never_break_the_plan_bounds() {
    let mut r = Lcg(7);
    for case in 0..2_000u64 {
        let n = 1 + r.below(12) as usize;
        let plugins: Vec<(Label, Tier)> = (0..n)
            .map(|_| {
                (
                    [Label::Interactive, Label::Unlabelled, Label::Refinement][r.below(3) as usize],
                    [Tier::First, Tier::Trusted, Tier::Third][r.below(3) as usize],
                )
            })
            .collect();
        let mut s = Scheduler::new(&plugins, Params::default());
        // Random "bytes" in the statistics: arbitrary counts, arbitrary warm-up state.
        for st in &mut s.plugins {
            for c in &mut st.exec.c {
                *c = if r.below(3) == 0 { 0 } else { r.next() as u32 };
            }
            st.exec.n = r.below(40) as u32;
            st.exec.refresh();
            st.useful = r.below(1 << 20) as i64 - (1 << 17);
            st.excluded = r.below(3) as u32;
        }
        for c in &mut s.start_hot.h.c {
            *c = r.next() as u32;
        }
        s.start_hot.h.refresh();
        s.start_hot.pmf = s.start_hot.h.pmf();
        let all: Vec<usize> = (0..n).collect();
        let mut tr: Vec<Triggered> = Vec::new();
        for p in &all {
            if r.below(4) != 0 {
                tr.push(Triggered {
                    pid: *p,
                    busy: r.below(5) == 0,
                });
            }
        }
        let spent = if r.below(2) == 0 {
            None
        } else {
            Some(r.below(400) as u32)
        };
        let mode = if r.below(2) == 0 {
            PlaceMode::Own
        } else {
            PlaceMode::Queues(r.below(8) as usize)
        };
        let mut b = SpinBucket::standard();
        let _ = s.keydown_ctx(r.below(6) as usize, false, 1_000_000 + case, &mut b);
        let plan = s.plan(
            &tr,
            TurnTimes {
                now_us: 1_000_000 + case,
                spent_us: spent,
            },
            mode,
        );
        let dw = (s.params().d_us - s.params().fuse_us) - spent.map_or(6, i64::from);
        assert_eq!(
            plan.assignments().len(),
            tr.len(),
            "SPS-4: every trigger is dispatched"
        );
        for a in plan.assignments() {
            assert!(
                i64::from(a.hold_us) <= dw.max(0),
                "hold {} dw {dw}",
                a.hold_us
            );
            assert!(a.hold_us <= a.deadline_us || a.hold_us == 0);
            let tier = s.stats(a.pid).unwrap().tier;
            if tier == Tier::Third {
                assert!(a.hold_us <= 100, "SPS-2: third party hold {}", a.hold_us);
            }
            let busy = tr.iter().any(|t| t.pid == a.pid && t.busy);
            if busy {
                assert_eq!(a.hold_us, 0, "a busy actor is never waited for");
            }
        }
        assert!(i64::from(plan.t_star_us) <= dw.max(0));
        // Random reply times, including "never": the clamp bounds the end whatever they are.
        let replies: Vec<(u32, Option<u32>)> = plan
            .wait_set()
            .map(|a| (a.hold_us, (r.below(4) != 0).then(|| r.below(5000) as u32)))
            .collect();
        assert!(i64::from(wait_end_of(&replies)) <= dw.max(0));
    }
}

#[test]
fn one_more_waited_plugin_raises_the_end_by_at_most_its_hold() {
    let mut r = Lcg(11);
    for _ in 0..20_000 {
        let n = 1 + r.below(8) as usize;
        let w: Vec<(u32, Option<u32>)> = (0..n)
            .map(|_| {
                (
                    1 + r.below(280) as u32,
                    (r.below(3) != 0).then(|| r.below(400) as u32),
                )
            })
            .collect();
        let end = wait_end_of(&w);
        for i in 0..n {
            let mut rest = w.clone();
            let (h, _) = rest.remove(i);
            assert!(end - wait_end_of(&rest) <= h, "SPS-2");
        }
    }
}

// ---- SPS-7: labels ----------------------------------------------------------------------

#[test]
fn labels_are_constraints_on_every_plan() {
    let mut r = Lcg(5);
    let plugins = [
        (Label::Interactive, Tier::First),
        (Label::Refinement, Tier::First),
        (Label::Unlabelled, Tier::Third),
        (Label::Interactive, Tier::Third),
        (Label::Unlabelled, Tier::Trusted),
        (Label::Refinement, Tier::Third),
        (Label::Interactive, Tier::First),
        (Label::Unlabelled, Tier::First),
    ];
    let mut s = Scheduler::new(&plugins, Params::default());
    let mut b = SpinBucket::standard();
    let mut now = 1_000_000u64;
    for i in 0..5_000u64 {
        now += 24_000;
        let pw = s.keydown_ctx(r.below(6) as usize, false, now, &mut b);
        for (pid, (l, t)) in plugins.iter().enumerate() {
            if pw.who & (1 << pid) != 0 {
                assert_ne!(
                    *l,
                    Label::Refinement,
                    "L-1: a refinement plugin never pre-wakes"
                );
                let _ = t;
            }
        }
        assert!(
            (0..plugins.len())
                .filter(|p| pw.who & (1 << p) != 0 && plugins[*p].1 == Tier::Third)
                .count()
                <= 1,
            "L-2: at most one third-party worker"
        );
        let tr: Vec<Triggered> = (0..plugins.len())
            .filter(|_| r.below(2) == 0)
            .map(|pid| Triggered { pid, busy: false })
            .collect();
        let plan = s.plan(
            &tr,
            TurnTimes {
                now_us: now,
                spent_us: None,
            },
            PlaceMode::Own,
        );
        let mut seen_third = false;
        let mut third_hot = 0;
        for a in plan.assignments() {
            let (l, t) = plugins[a.pid];
            if l == Label::Refinement {
                assert_eq!(a.hold_us, 0, "L-1: never waited");
                assert_eq!(a.place, Place::Parked, "L-1: never a hot place");
            }
            if t == Tier::Third {
                seen_third = true;
                third_hot += usize::from(matches!(a.place, Place::Hot(_)));
                assert!(a.hold_us <= 100, "L-2");
            } else {
                assert!(!seen_third, "L-2: third parties are ordered last");
            }
            assert!(s.stats(a.pid).unwrap().value() <= t.value_cap(), "L-3");
        }
        assert!(third_hot <= 1);
        // Feed replies back: hot/cold start, assorted exec times, some useful.
        let mask = tr.iter().fold(0u32, |m, t| m | (1 << t.pid));
        s.end_key(mask);
        for a in plan.assignments() {
            s.observe(&Observation {
                pid: a.pid,
                state: a.predicted_start(),
                start_us: 1 + r.below(if a.place == Place::Parked { 150 } else { 4 }) as u32,
                exec_us: 3 + r.below(if i % 50 == 0 { 250 } else { 70 }) as u32,
                resp_us: 10 + r.below(300) as u32,
                assignment: a,
                waited: a.hold_us > 0,
                useful: r.below(2) == 0,
            });
        }
        // SPS-4 bounds.
        for pid in 0..plugins.len() {
            let st = s.stats(pid).unwrap();
            assert!(st.excluded() <= s.params().excl_max, "e <= 256");
            assert!(st.denied() <= 24, "g <= 24, got {}", st.denied());
        }
    }
}

#[test]
fn owner_override_applies_to_first_party_only() {
    let mut s = Scheduler::with_overrides(
        &[
            (Label::Unlabelled, Tier::First, Override::NeverWait),
            (Label::Unlabelled, Tier::Third, Override::AlwaysWait),
            (Label::Unlabelled, Tier::First, Override::AlwaysWait),
        ],
        Params::default(),
    );
    let plan = s.plan(&trig(&[0, 1, 2]), tz(), PlaceMode::Own);
    assert_eq!(plan.of(0).unwrap().hold_us, 0, "never-wait");
    assert_eq!(
        s.stats(1).unwrap().over,
        Override::None,
        "a third party has no override"
    );
    assert!(
        plan.of(2).unwrap().hold_us > 0,
        "always-wait while the window allows"
    );
}

// ---- pre-wake and the token bucket ------------------------------------------------------

#[test]
fn typing_learns_to_wake_nothing_and_navigation_keeps_waking() {
    let mut s = Scheduler::new(
        &[
            (Label::Interactive, Tier::First),
            (Label::Unlabelled, Tier::First),
        ],
        Params::default(),
    );
    let mut b = SpinBucket::standard();
    let mut now = 1_000_000u64;
    let mut typed_woke = Vec::new();
    for _ in 0..60 {
        now += 100_000;
        typed_woke.push(s.on_key_down(KeyClass::Type, now, &mut b).threads);
        // No focus change follows a typed character: the next key-down flushes the outcome.
    }
    assert!(
        typed_woke[..3].iter().any(|t| *t > 0),
        "the prior wakes at first"
    );
    assert_eq!(
        *typed_woke.last().unwrap(),
        0,
        "after about 30 keys typing wakes nothing"
    );
    for _ in 0..40 {
        now += 100_000;
        let pw = s.on_key_down(KeyClass::Nav, now, &mut b);
        let tr = trig(&[0, 1]);
        let plan = s.plan(
            &tr,
            TurnTimes {
                now_us: now + 3_000,
                spent_us: None,
            },
            PlaceMode::Own,
        );
        s.end_key(0b11);
        for a in plan.assignments() {
            s.observe(&Observation {
                pid: a.pid,
                state: a.predicted_start(),
                // A parked worker starts late: that is what a pre-wake buys.
                start_us: if a.place == Place::Parked { 300 } else { 2 },
                exec_us: 20,
                resp_us: 25,
                assignment: a,
                waited: a.hold_us > 0,
                useful: true,
            });
        }
        let _ = pw;
    }
    // The typing phase spun whole windows for nothing and left the bucket in debt; time repays it.
    now += 20_000_000;
    assert!(s.on_key_down(KeyClass::Nav, now, &mut b).threads >= 1);
}

#[test]
fn the_bucket_bounds_the_pre_wake_cpu() {
    // SPS-9 by construction: every key wakes as much as it can and every worker spins the whole
    // 50 ms window (the worst case); the charged cost is what the bucket allows.
    let p = Params::default();
    let mut s = Scheduler::new(
        &(0..6)
            .map(|_| (Label::Interactive, Tier::First))
            .collect::<Vec<_>>(),
        p,
    );
    let mut b = SpinBucket::standard();
    let mut spun_us: i64 = 0;
    let mut now = 1_000_000u64;
    let secs = 60u64;
    let mut keys = 0;
    while now < 1_000_000 + secs * 1_000_000 {
        now += 33_333; // a held key
        keys += 1;
        let pw = s.on_key_down(KeyClass::Nav, now, &mut b);
        // The worker spins its whole window, and its real cost is settled at the next key.
        spun_us += i64::from(pw.threads) * (p.prewake_window_us as i64 + p.spin_tail_us);
        s.end_key(0);
    }
    let allowed = p.spin_bucket_us + (p.spin_rate_us_per_s * secs as i64) + 6 * p.spin_est_us;
    assert!(keys > 1_000);
    // Settlement lags by one key and the cost is only known after the window.
    assert!(
        spun_us <= allowed + 6 * 53_000,
        "spun {spun_us} us over {secs} s, allowed {allowed}"
    );
    let idle_before = b.available_us();
    let s2 = Scheduler::new(&[(Label::Interactive, Tier::First)], p);
    let mut b2 = SpinBucket::standard();
    b2.refill(10_000_000);
    assert_eq!(
        b2.available_us(),
        idle_before.max(p.spin_bucket_us).min(p.spin_bucket_us)
    );
    let _ = s2.params();
}

#[test]
fn an_empty_bucket_degrades_to_the_parked_path() {
    let p = Params::default();
    let mut s = Scheduler::new(&[(Label::Interactive, Tier::First)], p);
    let mut b = SpinBucket::new(p.spin_bucket_us, p.spin_rate_us_per_s);
    b.charge(p.spin_bucket_us + 500_000);
    assert!(b.available_us() < 0);
    assert_eq!(s.on_key_down(KeyClass::Nav, 5_000, &mut b).threads, 0);
    assert!(
        b.available_us() >= -p.spin_bucket_us,
        "the floor is -200 ms"
    );
    let plan = s.plan(&trig(&[0]), tz(), PlaceMode::Own);
    assert_eq!(plan.of(0).unwrap().place, Place::Parked);
}

#[test]
fn pre_armed_workers_are_hot_and_the_focus_change_settles_their_cost() {
    let p = Params::default();
    let mut s = Scheduler::new(
        &[
            (Label::Interactive, Tier::First),
            (Label::Interactive, Tier::First),
        ],
        p,
    );
    let mut b = SpinBucket::standard();
    let pw = s.on_key_down(KeyClass::Nav, 1_000_000, &mut b);
    assert!(pw.who != 0 && pw.until_us == 1_000_000 + p.prewake_window_us);
    let before = b.available_us();
    let plan = s.plan(
        &trig(&[0, 1]),
        TurnTimes {
            now_us: 1_003_000,
            spent_us: Some(5),
        },
        PlaceMode::Own,
    );
    let hot = plan
        .assignments()
        .iter()
        .filter(|a| matches!(a.place, Place::Hot(_)))
        .count();
    assert_eq!(hot, pw.threads as usize);
    s.end_key(0b11);
    // The next key-down credits the difference between the estimate (3 ms) and the 3 ms + tail.
    let _ = s.on_key_down(KeyClass::Nav, 1_100_000, &mut b);
    assert!(b.available_us() <= before + 20_000);
}

// ---- fallback ----------------------------------------------------------------------------

#[test]
fn an_invariant_failure_switches_to_the_labels_policy_for_good() {
    let mut s = Scheduler::new(
        &[
            (Label::Interactive, Tier::First),
            (Label::Refinement, Tier::First),
            (Label::Unlabelled, Tier::Third),
        ],
        Params::default(),
    );
    for _ in 0..20 {
        let a = Assignment {
            pid: 0,
            place: Place::Hot(0),
            offset_us: 0,
            deadline_us: 100,
            grant_fuel: 0,
            exec_allow_us: 100,
            clipped: false,
            hold_us: 0,
        };
        s.observe(&Observation {
            pid: 0,
            state: Start::Hot,
            start_us: 2,
            exec_us: 20,
            resp_us: 25,
            assignment: &a,
            waited: false,
            useful: true,
        });
    }
    // Corrupt: no mass after warm-up.
    s.plugins[0].exec.c = [0; NB];
    s.plugins[0].exec.refresh();
    let plan = s.plan(&trig(&[0, 1, 2]), tz(), PlaceMode::Own);
    assert_eq!(s.fallback(), Some(FallbackKind::HistEmpty));
    assert_eq!(plan.fallback, Some(FallbackKind::HistEmpty));
    assert!(
        plan.of(0).unwrap().hold_us > 0,
        "LABELS waits for non-refinement plugins"
    );
    assert_eq!(plan.of(1).unwrap().hold_us, 0, "never a refinement plugin");
    assert!(
        plan.of(2).unwrap().hold_us <= 100,
        "a third party keeps its cap"
    );
    assert_eq!(
        plan.assignments().last().unwrap().pid,
        2,
        "third party last"
    );
    // Pre-wake under LABELS: non-typing keys only, never a refinement plugin.
    let mut b = SpinBucket::standard();
    let pw = s.on_key_down(KeyClass::Nav, 1_000_000, &mut b);
    assert_eq!(pw.who, 0b101);
    assert_eq!(s.on_key_down(KeyClass::Type, 1_100_000, &mut b).threads, 0);
    assert!(s.fallback().is_some(), "sticky for the scope");
}

// ---- SPS-3 adversary harness -------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Strategy {
    Honest,
    Periodic,
    Random5,
    Burst,
    Slow,
    Oracle,
}

/// One adversary (trusted tier, so its deadline is not clipped away) next to two honest
/// plugins. Returns (offences, stalls of each activation).
fn adversary_run(strategy: Strategy, n: u64, seed: u64, adv_tier: Tier) -> (u64, Vec<u32>) {
    let mut r = Lcg(seed);
    let mut s = Scheduler::new(
        &[
            (Label::Interactive, Tier::First),
            (Label::Unlabelled, Tier::First),
            (Label::Interactive, adv_tier),
        ],
        Params::default(),
    );
    let mut b = SpinBucket::standard();
    let mut now = 1_000_000u64;
    let (mut offences, mut stalls) = (0u64, Vec::with_capacity(n as usize));
    for k in 0..n {
        now += 24_000;
        let _ = s.on_key_down(KeyClass::Nav, now, &mut b);
        let plan = s.plan(
            &trig(&[0, 1, 2]),
            TurnTimes {
                now_us: now + 3_000,
                spent_us: None,
            },
            PlaceMode::Own,
        );
        s.end_key(0b111);
        let mut w = Vec::new();
        let mut obs = Vec::new();
        for a in plan.assignments() {
            let hot = matches!(a.place, Place::Hot(_));
            let start = 1 + r.below(if hot { 3 } else { 60 }) as u32;
            let waited = a.hold_us > 0;
            let exec = match a.pid {
                0 => 30 + r.below(15) as u32,
                1 => 8 + r.below(6) as u32,
                _ => {
                    let stall = 190u32;
                    let fast = 8 + r.below(5) as u32;
                    match strategy {
                        Strategy::Honest => fast,
                        Strategy::Periodic => {
                            if k % 20 == 19 {
                                stall
                            } else {
                                fast
                            }
                        }
                        Strategy::Random5 => {
                            if r.below(100) < 5 {
                                stall
                            } else {
                                fast
                            }
                        }
                        Strategy::Burst => {
                            if k % 330 >= 300 {
                                stall
                            } else {
                                fast
                            }
                        }
                        Strategy::Slow => stall,
                        // Knows whether it is waited for (a side channel SPS-8 closes at the
                        // message level; here the harness grants it for free).
                        Strategy::Oracle => {
                            if waited {
                                stall
                            } else {
                                fast
                            }
                        }
                    }
                }
            };
            let resp = start + exec;
            if waited {
                w.push((a.hold_us, Some(resp)));
            }
            if waited && resp > a.hold_us && exec > a.exec_allow_us && !a.clipped {
                offences += 1;
            }
            obs.push((*a, a.predicted_start(), start, exec, resp, waited));
        }
        stalls.push(wait_end_of(&w));
        for (a, st, start, exec, resp, waited) in &obs {
            s.observe(&Observation {
                pid: a.pid,
                state: *st,
                start_us: *start,
                exec_us: *exec,
                resp_us: *resp,
                assignment: a,
                waited: *waited,
                useful: true,
            });
        }
    }
    (offences, stalls)
}

fn median(v: &[u32]) -> u32 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s[s.len() / 2]
}

#[test]
fn offences_stay_under_the_bound_for_every_adversary_strategy() {
    let n: u64 = if cfg!(debug_assertions) {
        20_000
    } else {
        100_000
    };
    let bound = n / 33 + n / 256 + 8 + 1;
    let (_, honest) = adversary_run(Strategy::Honest, n, 3, Tier::Third);
    for strategy in [
        Strategy::Honest,
        Strategy::Periodic,
        Strategy::Random5,
        Strategy::Burst,
        Strategy::Slow,
        Strategy::Oracle,
    ] {
        // A trusted (not clipped) adversary can offend: the offence bound.
        let (o, stalls) = adversary_run(strategy, n, 3, Tier::Trusted);
        assert!(
            o <= bound,
            "{strategy:?}: {o} offences over the bound {bound}"
        );
        eprintln!(
            "SPS-3 trusted {strategy:?}: offences {o} (bound {bound}), stall p50 {} us, max {} us",
            median(&stalls),
            stalls.iter().max().unwrap()
        );
        // A self-labelled third party (the adversary of the C-4 gate): its hold is at most the
        // 100 us cap, and a deadline that does not fit the cap is never waited for.
        let (o3, stalls3) = adversary_run(strategy, n, 3, Tier::Third);
        assert!(o3 <= bound, "{strategy:?}: third party offences {o3}");
        let d = median(&stalls3).abs_diff(median(&honest));
        eprintln!(
            "SPS-3 third   {strategy:?}: stall p50 {} us (honest {}), max {} us",
            median(&stalls3),
            median(&honest),
            stalls3.iter().max().unwrap()
        );
        assert!(
            d <= 10,
            "{strategy:?}: median stall {} vs honest {}",
            median(&stalls3),
            median(&honest)
        );
        assert!(
            *stalls3.iter().max().unwrap() <= 160,
            "third-party stall bounded by its cap"
        );
    }
}

#[test]
fn a_self_labelled_interactive_third_party_costs_at_most_its_cap() {
    let mut r = Lcg(9);
    let mut s = Scheduler::new(
        &[
            (Label::Interactive, Tier::First),
            (Label::Interactive, Tier::Third),
        ],
        Params::default(),
    );
    let mut b = SpinBucket::standard();
    let mut now = 1_000_000u64;
    let mut worst = 0;
    for _ in 0..20_000 {
        now += 24_000;
        let _ = s.on_key_down(KeyClass::Nav, now, &mut b);
        let plan = s.plan(
            &trig(&[0, 1]),
            TurnTimes {
                now_us: now,
                spent_us: None,
            },
            PlaceMode::Own,
        );
        s.end_key(0b11);
        let mut w = Vec::new();
        for a in plan.assignments() {
            let exec = if a.pid == 1 {
                190
            } else {
                30 + r.below(10) as u32
            };
            let resp = 2 + exec;
            if a.hold_us > 0 {
                w.push((a.hold_us, Some(resp)));
            }
            s.observe(&Observation {
                pid: a.pid,
                state: a.predicted_start(),
                start_us: 2,
                exec_us: exec,
                resp_us: resp,
                assignment: a,
                waited: a.hold_us > 0,
                useful: true,
            });
        }
        worst = worst.max(wait_end_of(&w));
    }
    assert!(worst <= 100 + 40, "SPS-2: worst stall {worst} us");
}

// ---- SPS-4 / SPS-6 ------------------------------------------------------------------------

#[test]
fn a_denied_plugin_is_promoted_and_nothing_is_dropped() {
    // More valuable jobs than hot places: everyone gets a place within a bounded number of turns.
    let plugins: Vec<(Label, Tier)> = (0..8).map(|_| (Label::Unlabelled, Tier::First)).collect();
    let mut s = Scheduler::new(&plugins, Params::default());
    let mut got = [0u32; 8];
    for i in 0..400 {
        let tr = trig(&[0, 1, 2, 3, 4, 5, 6, 7]);
        let plan = s.plan(&tr, tz(), PlaceMode::Queues(1));
        assert_eq!(
            plan.assignments().len(),
            8,
            "SPS-4: the dispatched set is the triggered set"
        );
        for a in plan.assignments() {
            if matches!(a.place, Place::Hot(_)) {
                got[a.pid] += 1;
            }
            s.observe(&Observation {
                pid: a.pid,
                state: a.predicted_start(),
                start_us: 2,
                exec_us: 20 + (a.pid as u32 * 3) + (i % 3),
                resp_us: 24,
                assignment: a,
                waited: a.hold_us > 0,
                useful: true,
            });
        }
        for pid in 0..8 {
            assert!(s.stats(pid).unwrap().denied() <= 24);
        }
    }
    assert!(
        got.iter().all(|g| *g > 0),
        "nobody is starved of a hot place: {got:?}"
    );
}

#[test]
fn a_recorded_log_replays_to_the_same_decisions() {
    let plugins = [
        (Label::Interactive, Tier::First),
        (Label::Unlabelled, Tier::Trusted),
        (Label::Unlabelled, Tier::First),
        (Label::Refinement, Tier::First),
        (Label::Interactive, Tier::Third),
    ];
    let mut r = Lcg(21);
    let mut s = Scheduler::new(&plugins, Params::default());
    s.record(100_000);
    let mut b = SpinBucket::standard();
    let mut now = 1_000_000u64;
    for _ in 0..3_000 {
        now += 10_000 + r.below(40_000);
        let class = [
            KeyClass::Nav,
            KeyClass::Tab,
            KeyClass::Type,
            KeyClass::Enter,
        ][r.below(4) as usize];
        let _ = s.on_key_down(class, now, &mut b);
        if class != KeyClass::Type && r.below(10) != 0 {
            let mut tr: Vec<Triggered> = Vec::new();
            for pid in 0..plugins.len() {
                if r.below(2) == 0 {
                    tr.push(Triggered {
                        pid,
                        busy: r.below(8) == 0,
                    });
                }
            }
            let mode = if r.below(2) == 0 {
                PlaceMode::Own
            } else {
                PlaceMode::Queues(1 + r.below(3) as usize)
            };
            let fuse = (r.below(3) == 0).then(|| r.below(100) as u32);
            let plan = s.plan_with(
                &tr,
                TurnTimes {
                    now_us: now + 2_000,
                    spent_us: Some(r.below(30) as u32),
                },
                mode,
                fuse,
            );
            s.end_key(tr.iter().fold(0, |m, t| m | (1 << t.pid)));
            for a in plan.assignments() {
                s.observe(&Observation {
                    pid: a.pid,
                    state: if r.below(4) == 0 {
                        Start::Cold
                    } else {
                        a.predicted_start()
                    },
                    start_us: r.below(200) as u32,
                    exec_us: r.below(260) as u32,
                    resp_us: r.below(400) as u32,
                    assignment: a,
                    waited: a.hold_us > 0,
                    useful: r.below(2) == 0,
                });
            }
        }
    }
    let ring = s.recorded().unwrap();
    assert!(!ring.wrapped());
    let rep = log::replay(ring, &plugins, Params::default()).unwrap();
    assert!(rep.entries > 3_000);
    assert_eq!(rep.mismatches, 0, "SPS-6: bit-equal decisions on replay");
    // The log is numbers only: every line starts with a record letter and holds digits and spaces.
    for line in ring.to_text().lines() {
        assert!(
            line.chars()
                .all(|c| c.is_ascii_hexdigit() || c == ' ' || matches!(c, 'K' | 'C' | 'P' | 'O'))
        );
    }
}

#[test]
fn a_wrapped_ring_cannot_be_replayed() {
    let mut ring = log::Ring::new(4);
    for i in 0..10 {
        ring.push(log::Rec::Context {
            ctx: 0,
            triggered: i,
        });
    }
    assert!(ring.wrapped());
    assert_eq!(ring.iter().count(), 4);
    assert!(log::replay(&ring, &[], Params::default()).is_none());
}

#[test]
fn a_larger_reserve_shortens_the_window_and_every_hold_and_replays() {
    let (mut s, _) = reference_world();
    s.record(1000);
    let tr = trig(&[0, 1, 2]);
    let times = TurnTimes {
        now_us: 5_000_000,
        spent_us: Some(40),
    };
    let wide = s.plan_with(&tr, times, PlaceMode::Own, Some(8));
    let narrow = s.plan_with(&tr, times, PlaceMode::Own, Some(120));
    // D - spent - reserve is the most a hold may be: 252 against 140.
    assert!(
        wide.t_star_us <= 252 && narrow.t_star_us <= 140,
        "{wide:?} {narrow:?}"
    );
    assert!(narrow.t_star_us <= wide.t_star_us);
    for a in narrow.assignments() {
        assert!(a.hold_us <= 140 && a.deadline_us <= 140);
    }
    // A reserve larger than the whole budget leaves no wait at all.
    let none = s.plan_with(&tr, times, PlaceMode::Own, Some(400));
    assert_eq!(none.t_star_us, 0);
    assert!(none.wait_set().next().is_none());
    // The ring holds the three plans with their reserve (the recording started after warm-up,
    // so it is not replayed whole here; `a_recorded_log_replays_to_the_same_decisions` does).
    assert!(
        s.recorded()
            .unwrap()
            .to_text()
            .lines()
            .filter(|l| l.starts_with("P "))
            .count()
            == 3
    );
}
