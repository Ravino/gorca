// SPDX-License-Identifier: GPL-3.0-or-later
//! M22b determinism property P1 (fusion): the published model is a function of the **set** of
//! facts, not of arrival order, batching or repetition, and the incremental engine equals a
//! from-scratch recompute. Statements and proof sketches:
//! `docs/reports/m22b-determinism-properties.md`.
//!
//! What is already covered elsewhere and is not repeated here: `props.rs` F1 (side-table fact
//! order, duplicates), F2 (suffix replay) and F10-F18; `ivm.rs` F7 (incremental equals scratch
//! after every drained batch under random ops) and batching/budget independence. This file adds
//! the **engine-level** statements the cut-over replay depends on:
//!
//! * D1 full arrival-order permutation plus duplicates of the register inputs (across sources
//!   and nodes, `Ivm::apply` in arbitrary batches) gives the same published model;
//! * D2 re-applying the entire stream (or any part of it) changes nothing and costs no fusion;
//! * D3 the incremental result equals `full_check` after every batch of a shuffled stream;
//! * D4 the *intermediate* publication stream: deterministic where it is claimed to be
//!   (`bump_global`, `run` with call budgets), and the two places where it is not
//!   (`known_gap_*`: `touch_priority_nodes` iterates a `RandomState` map; the synchronous drain
//!   is bounded by wall-clock time).
//!
//! `proptest` is not vendored in the project-local cargo home: seeded xorshift, the seed is in
//! every failure message. Scale with `GORCA_FUSE_TRIALS`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use gorca_fuse::{
    AnchorId, Budget, FactIn, Grant, Input, Ivm, NoPatches, NoQuirks, NodeIdx, Payload,
    PrimarySlot, Profile, SourceInfo, SourceState, UserOp, Write,
};
use gorca_model::fact::{Basis, SourceId};
use gorca_model::sem::{Group, SemEntry, SemRole, SourceClass, TrustTier};
use serde_json::{Value, json};
use support::{toy_rules, write_from_toy};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

fn trials() -> u64 {
    std::env::var("GORCA_FUSE_TRIALS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150)
}

fn nid(x: usize) -> NodeIdx {
    NodeIdx(u32::try_from(x).unwrap())
}

const ROLES: &[&str] = &[
    "generic", "button", "checkbox", "link", "heading", "text", "group",
];
const NAMES: &[&str] = &["", "Close", "Save", "Menu", "Item"];
const MODELLED: &[Group] = &[
    Group::Role,
    Group::Name,
    Group::Description,
    Group::Check,
    Group::Expand,
    Group::Avail,
];

fn domain(g: Group) -> Vec<Value> {
    let s = |xs: &[&str]| xs.iter().map(|x| json!(x)).collect::<Vec<_>>();
    match g {
        Group::Role => s(ROLES),
        Group::Name => s(NAMES),
        Group::Description => s(&["", "Required", "Opens a dialog"]),
        Group::Check => s(&["na", "off", "on", "mixed"]),
        Group::Expand => s(&["na", "collapsed", "expanded"]),
        Group::Avail => s(&["na", "enabled", "disabled"]),
        other => panic!("{other:?}"),
    }
}

fn rand_write(r: &mut Rng, g: Group) -> Write {
    if r.chance(10) {
        return Write::Absent(g);
    }
    let d = domain(g);
    write_from_toy(g, r.pick(&d))
}

fn engine(parent: &[Option<usize>]) -> Ivm<NoQuirks, NoPatches> {
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.default_role = SemRole::Generic;
    // P1 outranks P0 for roles, P0 for names: both primaries matter.
    profile.set_pi(Group::Role, &[PrimarySlot::P1, PrimarySlot::P0]);
    let mut e = Ivm::new(profile, NoQuirks, NoPatches);
    for (i, p) in parent.iter().enumerate() {
        e.add_node(nid(i), p.map(nid)).unwrap();
    }
    e
}

/// A random tree and a stream of register inputs with **globally unique, increasing** sequence
/// numbers (the per-source actor timeline of ADR-0022 section 2.2: ingress order for events).
fn workload(seed: u64, n: usize, count: usize) -> (Vec<Option<usize>>, Vec<Input>) {
    let mut r = Rng::new(seed);
    let parent: Vec<Option<usize>> = (0..n)
        .map(|x| if x == 0 { None } else { Some(r.below(x)) })
        .collect();
    let mut seq = 0u64;
    let mut ops = Vec::new();
    for x in 0..n {
        seq += 1;
        ops.push(Input::Life {
            node: nid(x),
            src: PrimarySlot::P0,
            seq,
            alive: true,
        });
    }
    for _ in 0..count {
        seq += 1;
        let x = r.below(n);
        let src = if r.chance(50) {
            PrimarySlot::P0
        } else {
            PrimarySlot::P1
        };
        let roll = r.below(100);
        ops.push(if roll < 62 {
            let g = *r.pick(MODELLED);
            Input::Obs {
                node: nid(x),
                src,
                seq,
                write: rand_write(&mut r, g),
            }
        } else if roll < 68 {
            Input::Inval {
                node: nid(x),
                src,
                seq,
            }
        } else if roll < 76 {
            Input::Life {
                node: nid(x),
                src,
                seq,
                alive: r.chance(85),
            }
        } else if roll < 92 {
            let g = *r.pick(MODELLED);
            Input::User {
                node: nid(x),
                group: g,
                seq,
                op: if r.chance(25) {
                    UserOp::Clear
                } else {
                    UserOp::Set(rand_write(&mut r, g))
                },
            }
        } else {
            Input::Attr {
                node: nid(x),
                seq,
                id: (!r.chance(20)).then(|| Arc::from(format!("id{}", r.below(4)).as_str())),
            }
        });
    }
    (parent, ops)
}

fn snapshot(e: &Ivm<NoQuirks, NoPatches>, n: usize) -> Vec<Option<SemEntry>> {
    (0..n)
        .map(|i| {
            e.entry_unmasked(nid(i)).map(|a| {
                let mut x = (**a).clone();
                x.sem_rev = 0;
                x
            })
        })
        .collect()
}

fn drain(e: &mut Ivm<NoQuirks, NoPatches>) {
    assert_eq!(e.run(Budget::UNLIMITED).pending, 0);
}

fn shuffled<T: Clone>(r: &mut Rng, xs: &[T], dups: bool) -> Vec<T> {
    let mut v: Vec<T> = xs.to_vec();
    if dups {
        for x in xs {
            while r.chance(25) {
                v.push(x.clone());
            }
        }
    }
    for i in (1..v.len()).rev() {
        v.swap(i, r.below(i + 1));
    }
    v
}

fn feed(
    e: &mut Ivm<NoQuirks, NoPatches>,
    r: &mut Rng,
    ops: &[Input],
    max_batch: usize,
    budget: Budget,
) {
    let mut i = 0;
    while i < ops.len() {
        let end = (i + 1 + r.below(max_batch)).min(ops.len());
        e.apply(ops[i..end].iter().cloned());
        e.run(budget);
        i = end;
    }
}

/// The in-order reference: one batch per op count, then drained.
fn reference(parent: &[Option<usize>], ops: &[Input]) -> Vec<Option<SemEntry>> {
    let mut e = engine(parent);
    e.apply(ops.iter().cloned());
    drain(&mut e);
    assert!(e.verify().is_ok());
    snapshot(&e, parent.len())
}

// ---- D1 ------------------------------------------------------------------------------------------

/// D1 For any permutation of the register inputs, with arbitrary repetition, in any batching and
/// with any call budget, the drained published model is the in-order result.
///
/// Why this holds (report, P1): every register is last-writer-wins on the writer-stamped `seq`
/// (a join in a max-semilattice), `Life`/`Inval`/`User`/`Attr` are registers of the same kind,
/// and FUSE is a pure function of the register state (Lemma 2), so the published model is a
/// function of the *set* of stamped inputs.
#[test]
fn d1_register_inputs_are_arrival_order_batching_and_repetition_independent() {
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 20).unwrap();
        let (parent, ops) = workload(seed, n, n * 6);
        let want = reference(&parent, &ops);
        let mut r = Rng::new(seed ^ 0xD1);
        for (label, dups, max_batch, budget) in [
            ("whole", false, usize::MAX / 2, Budget::UNLIMITED),
            ("batches", false, 9, Budget::UNLIMITED),
            ("dups", true, 9, Budget::UNLIMITED),
            ("tiny budget", true, 5, Budget::calls(2)),
            ("single", true, 1, Budget::calls(1)),
        ] {
            let stream = shuffled(&mut r, &ops, dups);
            let mut e = engine(&parent);
            feed(&mut e, &mut r, &stream, max_batch, budget);
            drain(&mut e);
            assert!(
                e.verify().is_ok(),
                "seed {seed} {label}: incremental != scratch"
            );
            assert_eq!(snapshot(&e, n), want, "seed {seed} {label}");
        }
    }
}

/// D1' sensitivity check (the generator can see order): if the writer stamp is **re-assigned in
/// arrival order** instead of carried by the fact, the shuffled stream gives a different model
/// for many seeds. This is why `seq` must be writer-stamped (ADR-0022 section 2.2) and why D1
/// is not vacuous.
#[test]
fn d1_prime_restamping_in_arrival_order_would_break_d1() {
    let restamp = |ops: &[Input]| -> Vec<Input> {
        ops.iter()
            .enumerate()
            .map(|(i, op)| {
                let s = i as u64 + 1;
                match op.clone() {
                    Input::Obs {
                        node, src, write, ..
                    } => Input::Obs {
                        node,
                        src,
                        seq: s,
                        write,
                    },
                    Input::Life {
                        node, src, alive, ..
                    } => Input::Life {
                        node,
                        src,
                        seq: s,
                        alive,
                    },
                    Input::Inval { node, src, .. } => Input::Inval { node, src, seq: s },
                    Input::User {
                        node, group, op, ..
                    } => Input::User {
                        node,
                        group,
                        seq: s,
                        op,
                    },
                    Input::Attr { node, id, .. } => Input::Attr { node, seq: s, id },
                    other => other,
                }
            })
            .collect()
    };
    let mut differing = 0u64;
    let total = trials();
    for seed in 0..total {
        let n = 3 + usize::try_from(seed % 20).unwrap();
        let (parent, ops) = workload(seed, n, n * 6);
        let want = reference(&parent, &ops);
        let mut r = Rng::new(seed ^ 0xD1);
        let stream = restamp(&shuffled(&mut r, &ops, false));
        let mut e = engine(&parent);
        e.apply(stream);
        drain(&mut e);
        if snapshot(&e, n) != want {
            differing += 1;
        }
    }
    assert!(
        differing * 2 > total,
        "only {differing} of {total} seeds are order sensitive"
    );
}

// ---- D2 ------------------------------------------------------------------------------------------

/// D2 Idempotence: applying the same facts again (the whole stream, a random suffix, a random
/// subset, in any order) changes nothing, publishes nothing and leaves nothing pending.
#[test]
fn d2_reapplying_facts_changes_nothing() {
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 20).unwrap();
        let (parent, ops) = workload(seed ^ 0x5, n, n * 6);
        let mut r = Rng::new(seed ^ 0xD2);
        let mut e = engine(&parent);
        e.apply(ops.iter().cloned());
        drain(&mut e);
        let want = snapshot(&e, n);
        for round in 0..4 {
            let again: Vec<Input> = match round {
                0 => ops.clone(),
                1 => ops[r.below(ops.len())..].to_vec(),
                2 => shuffled(&mut r, &ops, true),
                _ => ops.iter().filter(|_| r.chance(30)).cloned().collect(),
            };
            e.apply(again);
            let rep = e.run(Budget::UNLIMITED);
            assert_eq!(rep.pending, 0, "seed {seed} round {round}");
            assert!(
                rep.published.is_empty(),
                "seed {seed} round {round}: a repeated fact published {} entries",
                rep.published.len()
            );
            assert_eq!(snapshot(&e, n), want, "seed {seed} round {round}");
        }
    }
}

// ---- D3 ------------------------------------------------------------------------------------------

/// D3 Incremental equals full recompute at every drained batch boundary of a shuffled stream
/// (the F7 statement, here on arrival orders F7's generator never produces).
#[test]
fn d3_incremental_equals_full_recompute_on_shuffled_streams() {
    let mut checks = 0usize;
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 25).unwrap();
        let (parent, ops) = workload(seed ^ 0xA, n, n * 6);
        let mut r = Rng::new(seed ^ 0xD3);
        let stream = shuffled(&mut r, &ops, true);
        let mut e = engine(&parent);
        let mut i = 0;
        while i < stream.len() {
            let end = (i + 1 + r.below(12)).min(stream.len());
            e.apply(stream[i..end].iter().cloned());
            drain(&mut e);
            if let Err(bad) = e.verify() {
                panic!("seed {seed} after {end} inputs: {bad:?}");
            }
            let full = e.full_check();
            assert_eq!(snapshot(&e, n), full, "seed {seed} after {end} inputs");
            checks += 1;
            i = end;
        }
    }
    assert!(checks > 300);
}

// ---- D4 ------------------------------------------------------------------------------------------

fn id(slot: u32) -> SourceId {
    SourceId {
        slot,
        generation: 0,
    }
}

fn ocr_info() -> SourceInfo {
    SourceInfo {
        id: id(3),
        name: Arc::from("ocr"),
        class: SourceClass::Inferred,
        tier: TrustTier::T2FirstParty,
        kind_order: 0,
        grant: Grant::standard(),
    }
}

/// A root with `n` children, every child visible and carrying an OCR name fact; the synchronous
/// drain cap is `cap`, far below `n`, so the cap bites.
fn fact_star(n: usize, cap: usize) -> Ivm<NoQuirks, NoPatches> {
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.default_role = SemRole::Generic;
    profile.limits.max_sync_drain = cap;
    let mut e = Ivm::new(profile, NoQuirks, NoPatches);
    e.add_node(nid(0), None).unwrap();
    for i in 1..=n {
        e.add_node(nid(i), Some(nid(0))).unwrap();
    }
    for i in 0..=n {
        e.apply([
            Input::Life {
                node: nid(i),
                src: PrimarySlot::P0,
                seq: 1,
                alive: true,
            },
            Input::Obs {
                node: nid(i),
                src: PrimarySlot::P0,
                seq: 2,
                write: write_from_toy(Group::Role, &json!("button")),
            },
        ]);
    }
    e.set_source(ocr_info()).unwrap();
    e.set_source_state(id(3), 1, SourceState::Active);
    for i in 1..=n {
        let anchor = AnchorId::Node(nid(i));
        let f = FactIn {
            source: id(3),
            group: Group::Name.group_code(),
            seq: i as u64,
            payload: Payload::Sem(write_from_toy(Group::Name, &json!("Save"))),
            p_milli: 990,
            basis: Basis::VisRev(e.vis_rev(nid(i)) + 1),
            bind_gen: e.resolve_anchor(&anchor).generation,
            admitted_at_ms: e.clock(),
            ttl_ms: Some(300_000),
            anchor,
        };
        e.apply([Input::Fact(Box::new(f))]);
    }
    e.run(Budget::UNLIMITED);
    let win: Vec<NodeIdx> = (1..=n).map(nid).collect();
    e.set_visible(&win);
    e
}

fn published_nodes(p: &[gorca_fuse::Published]) -> Vec<u32> {
    p.iter().map(|x| x.node.0).collect()
}

/// D4.1 `bump_global` is a deterministic publication stream: its synchronous part is chosen by
/// the dirty key `(class, order, node)` only, so fresh engines publish the same nodes in the same
/// order, whatever the process hash seeds are.
#[test]
fn d4_1_bump_global_publishes_the_same_nodes_in_the_same_order_on_every_engine() {
    let mut seen: BTreeSet<Vec<u32>> = BTreeSet::new();
    for _ in 0..16 {
        let mut e = fact_star(40, 4);
        // Make the drain independent of the wall clock: a bound no test can reach.
        e.set_sync_drain_time(Duration::from_secs(3600));
        seen.insert(published_nodes(&e.bump_global()));
    }
    assert_eq!(seen.len(), 1, "distinct streams: {seen:?}");
}

/// D4.2 whatever the intermediate stream, the *drained* state after a revocation is the same on
/// every engine, and equals the from-scratch recompute (this is the property the cut-over
/// relies on).
#[test]
fn d4_2_the_drained_state_after_a_revocation_is_engine_independent() {
    let mut finals: BTreeSet<Vec<Option<String>>> = BTreeSet::new();
    for _ in 0..16 {
        let mut e = fact_star(40, 4);
        e.set_source_state(id(3), 5, SourceState::Revoked);
        drain(&mut e);
        assert!(e.verify().is_ok());
        finals.insert(
            (0..=40)
                .map(|i| {
                    e.entry_view(nid(i))
                        .unwrap()
                        .name
                        .as_deref()
                        .map(str::to_owned)
                })
                .collect(),
        );
    }
    assert_eq!(finals.len(), 1);
}

/// KNOWN GAP G-P1-a: `Ivm::touch_priority_nodes` (`ivm.rs:1636`) collects the priority nodes that
/// carry facts by iterating `self.prio.keys()`, a `std::collections::HashMap` with the default
/// `RandomState`, and sends the first `max_sync_drain` of them to the synchronous drain. When the
/// cap bites, WHICH nodes are re-fused before `set_source_state` returns, and in what order the
/// rest reach the background queue, changes from engine to engine. The drained result is the same
/// (D4.2); the intermediate publication stream is not.
///
/// Fix proposal: sort `nodes` by the dirty key `(class, order, idx)` the set already stores
/// (`prio: HashMap<NodeIdx, (u8, u32)>`) before the `cap` split. Make `prio` an `FxMap` or a
/// `BTreeMap` for the same reason.
#[test]
fn known_gap_touch_priority_nodes_stream_differs_between_engines() {
    let mut seen: BTreeSet<Vec<u32>> = BTreeSet::new();
    for _ in 0..24 {
        let mut e = fact_star(40, 4);
        e.set_sync_drain_time(Duration::from_secs(3600));
        seen.insert(published_nodes(&e.set_source_state(
            id(3),
            5,
            SourceState::Revoked,
        )));
    }
    assert!(
        seen.len() > 1,
        "today the synchronous part differs between engines; this flips when G-P1-a is fixed: {seen:?}"
    );
}

/// KNOWN GAP G-P1-b: the synchronous drain stops on wall-clock time too
/// (`ivm.rs:1665,1674,1683`, `SYNC_DRAIN_TIME`), and `Budget::until(Instant)` reads the clock
/// (`ivm.rs:139`). With a zero time bound the drain fuses exactly one node, with a large one it
/// fuses up to the cap: the published prefix is a function of machine speed. A replay that must
/// be bit-identical has to pin the bound (`set_sync_drain_time`) and use call budgets.
#[test]
fn known_gap_the_synchronous_drain_length_depends_on_a_wall_clock_bound() {
    let run = |t: Duration| {
        let mut e = fact_star(40, 8);
        e.set_sync_drain_time(t);
        e.set_source_state(id(3), 5, SourceState::Revoked).len()
    };
    let (zero, large) = (run(Duration::ZERO), run(Duration::from_secs(3600)));
    assert_eq!(zero, 1, "a zero bound stops after one fusion");
    assert_eq!(large, 8, "a large bound stops at the call cap");
    assert!(
        large > zero,
        "{large} vs {zero}: the bound, not the data, decided the prefix"
    );
    // With the bound pinned the stream is a function of the data.
    assert_eq!(run(Duration::from_secs(3600)), large);
}

/// D4.3 call-budgeted `run` is deterministic as an intermediate stream: same inputs, same budget,
/// same published nodes in the same order on every engine (the dirty heap is totally ordered by
/// `(class, order, node)`; aging is tick based, not time based).
#[test]
fn d4_3_call_budgeted_runs_publish_a_deterministic_stream() {
    let (parent, ops) = workload(7, 24, 150);
    let mut streams: BTreeSet<Vec<Vec<u32>>> = BTreeSet::new();
    for _ in 0..12 {
        let mut e = engine(&parent);
        let mut stream = Vec::new();
        for chunk in ops.chunks(11) {
            e.apply(chunk.iter().cloned());
            let rep = e.run(Budget::calls(3));
            stream.push(published_nodes(&rep.published));
        }
        while e.pending() > 0 {
            stream.push(published_nodes(&e.run(Budget::calls(3)).published));
        }
        streams.insert(stream);
    }
    assert_eq!(streams.len(), 1);
}
