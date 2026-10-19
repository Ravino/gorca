// SPDX-License-Identifier: GPL-3.0-or-later
//! Incremental maintenance (M22a-B2): F7 (incremental equals from-scratch after every drained
//! batch), batching and budget independence, focus-chain priority, early cutoff, selector reads,
//! relation fan-out cap, F14 (revocation equals absence, silenced synchronously on the focus chain
//! and visible window), the reconnect path, TTL and `vis_rev` staleness, structure changes.
//!
//! Workloads are seeded; facts are stamped from the engine's own state the way a writer stamps
//! them (`resolve_anchor`, `vis_rev`, `clock`). `proptest` is not vendored, so a small xorshift
//! generator is used.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;

use gorca_fuse::{
    AnchorId, Budget, FactIn, Grant, Input, Ivm, NoPatches, NoQuirks, NodeIdx, PatchHit,
    PatchSource, Payload, PrimarySlot, Profile, S0Node, SelectorReads, SourceInfo, SourceState,
    UserOp, Write,
};
use gorca_model::fact::{Basis, SourceId};
use gorca_model::sem::SemEntry;
use gorca_model::sem::{EvidenceKind, Group, GroupMask, SemRole, SourceClass, TrustTier};
use serde_json::{Value, json};
use support::{ToyPatch, ToyPatches, ToyQuirks, toy_rules, write_from_toy};

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

const ROLES: &[&str] = &[
    "generic", "button", "checkbox", "link", "img", "heading", "text", "group", "listitem",
];
const NAMES: &[&str] = &["", "Close", "Save", "Menu", "Item", "Logo", "Open"];
const MODELLED: &[Group] = &[
    Group::Role,
    Group::Name,
    Group::Description,
    Group::Check,
    Group::Expand,
    Group::Avail,
    Group::Level,
    Group::Live,
    Group::Orientation,
];

fn domain(g: Group) -> Vec<Value> {
    let s = |xs: &[&str]| xs.iter().map(|x| json!(x)).collect::<Vec<_>>();
    match g {
        Group::Role => s(ROLES),
        Group::Name => s(NAMES),
        Group::Description => s(&["", "Opens a dialog", "Required"]),
        Group::Check => s(&["na", "off", "on", "mixed"]),
        Group::Expand => s(&["na", "collapsed", "expanded"]),
        Group::Avail => s(&["na", "enabled", "disabled"]),
        Group::Level => vec![json!(1), json!(2), json!(3)],
        Group::Live => s(&["off", "polite", "assertive"]),
        Group::Orientation => s(&["na", "horizontal", "vertical"]),
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

fn id(slot: u32) -> SourceId {
    SourceId {
        slot,
        generation: 0,
    }
}

// (slot, name, class, tier, kind order, semantic groups, evidence)
type Src = (
    u32,
    &'static str,
    SourceClass,
    TrustTier,
    u8,
    &'static [Group],
    &'static [EvidenceKind],
);

const SOURCES: &[Src] = &[
    (
        2,
        "bus",
        SourceClass::Secondary,
        TrustTier::T3Untrusted,
        0,
        MODELLED,
        &[EvidenceKind::HintTitleAttr, EvidenceKind::LayoutCssHidden],
    ),
    (
        3,
        "ocr",
        SourceClass::Inferred,
        TrustTier::T2FirstParty,
        0,
        &[Group::Name],
        &[EvidenceKind::VisualOcrText],
    ),
    (
        4,
        "icon",
        SourceClass::Inferred,
        TrustTier::T2FirstParty,
        1,
        &[Group::Name, Group::Description],
        &[],
    ),
    (
        5,
        "ext.a",
        SourceClass::Inferred,
        TrustTier::T3Untrusted,
        2,
        &[Group::Name, Group::Description],
        &[EvidenceKind::HintTitleAttr],
    ),
];

fn source_info(s: &Src) -> SourceInfo {
    SourceInfo {
        id: id(s.0),
        name: Arc::from(s.1),
        class: s.2,
        tier: s.3,
        kind_order: s.4,
        grant: Grant::standard(),
    }
}

/// Toy patches whose selector-read set is declared (narrow) or left at the default (all).
struct Patches {
    inner: ToyPatches,
    narrow: bool,
}

impl PatchSource for Patches {
    fn hits(&self, node: &S0Node, nb: &gorca_fuse::Neighbours<'_>, out: &mut Vec<PatchHit>) {
        self.inner.hits(node, nb, out);
    }

    fn reads(&self) -> SelectorReads {
        if self.narrow {
            // The toy selectors read the parent's role and the first child's name only.
            SelectorReads {
                parent: GroupMask::EMPTY.with(Group::Role),
                children: GroupMask::EMPTY.with(Group::Name),
            }
        } else {
            SelectorReads::ALL
        }
    }
}

#[derive(Clone, Debug)]
enum Op {
    In(Input),
    Epoch {
        src: u32,
        n: u64,
        state: SourceState,
    },
    Patch {
        pid: String,
        seq: u64,
        inst: bool,
    },
    Focus(Vec<NodeIdx>),
    Visible(Vec<NodeIdx>),
}

struct Setup {
    parent: Vec<Option<usize>>,
    profile: Profile,
    patches: Vec<ToyPatch>,
}

fn setup(seed: u64, n: usize) -> Setup {
    let mut r = Rng::new(seed);
    let parent = (0..n)
        .map(|x| if x == 0 { None } else { Some(r.below(x)) })
        .collect();
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.default_role = SemRole::Generic;
    for g in MODELLED {
        if r.chance(40) {
            profile.set_pi(*g, &[PrimarySlot::P1, PrimarySlot::P0]);
        }
        if matches!(g, Group::Live | Group::Orientation | Group::Description) && r.chance(60) {
            profile.inexpressible = profile.inexpressible.with(*g);
        }
    }
    let patches = (0..r.below(4))
        .map(|i| {
            let g = *r.pick(&[Group::Name, Group::Role, Group::Level, Group::Avail]);
            let d = domain(g);
            let value = if g == Group::Name && r.chance(50) {
                json!("firstchild")
            } else {
                r.pick(&d).clone()
            };
            ToyPatch {
                pid: format!("p{i}"),
                ext_id: format!("org.ext{}", r.below(3)),
                spec: i64::try_from(r.below(2)).unwrap(),
                sel_role: (*r.pick(ROLES)).to_owned(),
                empty_name: r.chance(50),
                sel_parent: r.chance(30).then(|| (*r.pick(ROLES)).to_owned()),
                group: g,
                value,
            }
        })
        .collect();
    Setup {
        parent,
        profile,
        patches,
    }
}

type Engine = Ivm<ToyQuirks, Patches>;

fn engine(s: &Setup, narrow: bool) -> Engine {
    let mut e = Ivm::new(
        s.profile.clone(),
        ToyQuirks,
        Patches {
            inner: ToyPatches(Vec::new()),
            narrow,
        },
    );
    for src in SOURCES {
        e.set_source(source_info(src)).unwrap();
        // A new source is masked until its first explicit Active epoch.
        e.set_source_state(id(src.0), 1, SourceState::Active);
    }
    for (i, p) in s.parent.iter().enumerate() {
        e.add_node(
            NodeIdx(u32::try_from(i).unwrap()),
            p.map(|p| NodeIdx(u32::try_from(p).unwrap())),
        )
        .unwrap();
    }
    e
}

fn nid(x: usize) -> NodeIdx {
    NodeIdx(u32::try_from(x).unwrap())
}

/// Execute `ops` on `e` in batches of the given sizes, running with the given budgets between
/// batches. Patch toggles keep the installed set and bump the global epoch.
fn exec(e: &mut Engine, s: &Setup, ops: &[Op], batches: &mut dyn FnMut() -> (usize, Budget)) {
    let mut ps = BTreeMap::new();
    exec_with(e, s, ops, batches, &mut ps);
}

type PatchState = BTreeMap<String, (u64, bool)>;

fn exec_with(
    e: &mut Engine,
    s: &Setup,
    ops: &[Op],
    batches: &mut dyn FnMut() -> (usize, Budget),
    patch_state: &mut PatchState,
) {
    let mut i = 0;
    while i < ops.len() {
        let (size, budget) = batches();
        let end = (i + size.max(1)).min(ops.len());
        let mut inputs = Vec::new();
        for op in &ops[i..end] {
            match op {
                Op::In(x) => inputs.push(x.clone()),
                other => {
                    // Flush pending inputs first so ordering is preserved.
                    e.apply(std::mem::take(&mut inputs));
                    exec_special(e, s, other, patch_state);
                }
            }
        }
        e.apply(inputs);
        e.run(budget);
        i = end;
    }
}

fn exec_special(e: &mut Engine, s: &Setup, op: &Op, ps: &mut PatchState) {
    match op {
        Op::Epoch { src, n, state } => {
            e.set_source_state(id(*src), *n, *state);
        }
        Op::Patch { pid, seq, inst } => {
            let new = (*seq, *inst);
            if ps.get(pid).is_none_or(|cur| new > *cur) {
                ps.insert(pid.clone(), new);
                let mut installed: Vec<ToyPatch> = s
                    .patches
                    .iter()
                    .filter(|p| ps.get(&p.pid).is_some_and(|x| x.1))
                    .cloned()
                    .collect();
                installed.sort_by_key(|p| (-p.spec, p.ext_id.clone(), p.pid.clone()));
                e.patches_mut().inner = ToyPatches(installed);
                e.bump_global();
            }
        }
        Op::Focus(c) => e.set_focus_chain(c),
        Op::Visible(c) => e.set_visible(c),
        Op::In(_) => {}
    }
}

/// Generate ops, stamping facts from a shadow engine that applies each op as it is generated.
fn gen_ops(seed: u64, s: &Setup, count: usize) -> Vec<Op> {
    let n = s.parent.len();
    let mut r = Rng::new(seed ^ 0xABCD);
    let mut shadow = engine(s, false);
    let mut ops = Vec::new();
    let mut seq = 0u64;
    let push = |shadow: &mut Engine, ops: &mut Vec<Op>, op: Op| {
        if let Op::In(i) = &op {
            shadow.apply([i.clone()]);
        }
        ops.push(op);
    };
    for x in 0..n {
        seq += 1;
        push(
            &mut shadow,
            &mut ops,
            Op::In(Input::Life {
                node: nid(x),
                src: PrimarySlot::P0,
                seq,
                alive: true,
            }),
        );
        if r.chance(60) {
            seq += 1;
            push(
                &mut shadow,
                &mut ops,
                Op::In(Input::Life {
                    node: nid(x),
                    src: PrimarySlot::P1,
                    seq,
                    alive: true,
                }),
            );
        }
        if r.chance(60) {
            seq += 1;
            let i = format!("id{}", r.below((n / 2).max(2)));
            push(
                &mut shadow,
                &mut ops,
                Op::In(Input::Attr {
                    node: nid(x),
                    seq,
                    id: Some(Arc::from(i.as_str())),
                }),
            );
        }
    }
    let ids = (n / 2).max(2);
    let mut clock = 0u64;
    for _ in 0..count {
        let x = r.below(n);
        seq += 1;
        let src = if r.chance(50) {
            PrimarySlot::P0
        } else {
            PrimarySlot::P1
        };
        let roll = r.below(100);
        let op = if roll < 40 {
            let g = *r.pick(MODELLED);
            Op::In(Input::Obs {
                node: nid(x),
                src,
                seq,
                write: rand_write(&mut r, g),
            })
        } else if roll < 44 {
            Op::In(Input::Inval {
                node: nid(x),
                src,
                seq,
            })
        } else if roll < 47 {
            Op::In(Input::Life {
                node: nid(x),
                src,
                seq,
                alive: r.chance(80),
            })
        } else if roll < 53 {
            let g = *r.pick(MODELLED);
            Op::In(Input::User {
                node: nid(x),
                group: g,
                seq,
                op: if r.chance(25) {
                    UserOp::Clear
                } else {
                    UserOp::Set(rand_write(&mut r, g))
                },
            })
        } else if roll < 55 && !s.patches.is_empty() {
            Op::Patch {
                pid: r.pick(&s.patches).pid.clone(),
                seq,
                inst: r.chance(70),
            }
        } else if roll < 59 {
            Op::In(Input::Attr {
                node: nid(x),
                seq,
                id: (!r.chance(15)).then(|| Arc::from(format!("id{}", r.below(ids)).as_str())),
            })
        } else if roll < 62 {
            Op::In(Input::Vis {
                node: nid(x),
                rev: shadow.vis_rev(nid(x)) + 1,
            })
        } else if roll < 64 {
            clock += *r.pick(&[1_000u64, 5_000, 30_000, 120_000]);
            Op::In(Input::Tick(clock))
        } else if roll < 65 {
            let s = r.pick(SOURCES);
            Op::Epoch {
                src: s.0,
                n: seq,
                state: *r.pick(&[
                    SourceState::Active,
                    SourceState::Throttled,
                    SourceState::Detached,
                    SourceState::Paused,
                    SourceState::Revoked,
                ]),
            }
        } else if roll < 67 {
            let k = r.below(4);
            let chain: Vec<NodeIdx> = (0..k).map(|_| nid(r.below(n))).collect();
            if r.chance(50) {
                Op::Focus(chain)
            } else {
                Op::Visible(chain)
            }
        } else {
            let src = r.pick(SOURCES);
            let anchor = if r.chance(30) {
                AnchorId::Dom {
                    id: Arc::from(format!("id{}", r.below(ids)).as_str()),
                    count: if r.chance(80) { 1 } else { 2 },
                    frame: 0,
                    shadow: 0,
                }
            } else {
                AnchorId::Node(nid(x))
            };
            let res = shadow.resolve_anchor(&anchor);
            let tx = res.node.unwrap_or(nid(x));
            let (group, payload) = if !src.6.is_empty() && r.chance(35) {
                let k = *r.pick(src.6);
                let p = if k == EvidenceKind::LayoutCssHidden {
                    Payload::Evidence(gorca_model::fact::EvidenceValue::Flag(r.chance(50)))
                } else {
                    let t = ["Close", "Delete", "Search", "", "Menu"];
                    Payload::Evidence(gorca_model::fact::EvidenceValue::Text(Arc::from(
                        *r.pick(&t),
                    )))
                };
                (k.group_code(), p)
            } else {
                let g = *r.pick(src.5);
                let p = if r.chance(5) {
                    Payload::Retract
                } else {
                    Payload::Sem(rand_write(&mut r, g))
                };
                (g.group_code(), p)
            };
            let vis = i64::from(shadow.vis_rev(tx)) - i64::from(r.chance(10));
            Op::In(Input::Fact(Box::new(FactIn {
                source: id(src.0),
                anchor,
                group,
                seq,
                payload,
                p_milli: u16::try_from(r.below(400) + 600).unwrap(),
                basis: Basis::VisRev(u32::try_from(vis + 1).unwrap()),
                bind_gen: res.generation,
                admitted_at_ms: shadow.clock(),
                ttl_ms: if src.2 == SourceClass::Inferred || r.chance(30) {
                    Some(*r.pick(&[5_000u64, 60_000, 300_000]))
                } else {
                    None
                },
            })))
        };
        push(&mut shadow, &mut ops, op);
    }
    ops
}

fn published(e: &Engine, n: usize) -> Vec<Option<SemEntry>> {
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

fn trials() -> u64 {
    std::env::var("GORCA_FUSE_TRIALS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
}

fn drain(e: &mut Engine) {
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(rep.pending, 0);
}

// ---- F7 -----------------------------------------------------------------------------------------

#[test]
fn f7_incremental_equals_from_scratch_after_every_drained_batch() {
    let mut checks = 0usize;
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 30).unwrap();
        let s = setup(seed, n);
        let ops = gen_ops(seed, &s, n * 8);
        for narrow in [false, true] {
            let mut e = engine(&s, narrow);
            let mut r = Rng::new(seed ^ 0xF7);
            // Random batch cuts; unlimited budget, drained and verified after every batch.
            let mut next = || (1 + r.below(12), Budget::UNLIMITED);
            let mut ps = PatchState::new();
            let mut done = 0;
            while done < ops.len() {
                let (size, b) = next();
                let end = (done + size).min(ops.len());
                exec_with(
                    &mut e,
                    &s,
                    &ops[done..end],
                    &mut || (usize::MAX, b),
                    &mut ps,
                );
                done = end;
                drain(&mut e);
                if let Err(bad) = e.verify() {
                    panic!("seed {seed} narrow={narrow} after {done} ops: {bad:?}");
                }
                checks += 1;
            }
        }
    }
    assert!(checks > 500);
}

#[test]
fn batching_and_budget_change_latency_never_the_result() {
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 30).unwrap();
        let s = setup(seed + 1000, n);
        let ops = gen_ops(seed + 1000, &s, n * 8);
        let mut whole = engine(&s, true);
        exec(&mut whole, &s, &ops, &mut || {
            (usize::MAX, Budget::UNLIMITED)
        });
        drain(&mut whole);
        let reference = published(&whole, n);
        assert!(whole.verify().is_ok(), "seed {seed}");
        for (label, size, calls) in [
            ("one by one", 1, 1usize),
            ("tiny budgets", 7, 2),
            ("big", 50, 5),
        ] {
            let mut e = engine(&s, false);
            exec(&mut e, &s, &ops, &mut || (size, Budget::calls(calls)));
            drain(&mut e);
            assert_eq!(published(&e, n), reference, "seed {seed} {label}");
        }
    }
}

// ---- F14 ----------------------------------------------------------------------------------------

#[test]
fn f14_revoked_source_equals_absent_and_the_priority_nodes_are_silenced_synchronously() {
    let mut compared = 0usize;
    for seed in 0..trials() {
        let n = 4 + usize::try_from(seed % 26).unwrap();
        let s = setup(seed + 2000, n);
        let ops = gen_ops(seed + 2000, &s, n * 8);
        let src = SOURCES[usize::try_from(seed % 4).unwrap()];
        // Drop the source's own epoch churn so the final state is determined by the revoke.
        let kept: Vec<Op> = ops
            .iter()
            .filter(|o| !matches!(o, Op::Epoch { src: x, .. } if *x == src.0))
            .cloned()
            .collect();
        let mut with = engine(&s, false);
        exec(&mut with, &s, &kept, &mut || {
            (usize::MAX, Budget::UNLIMITED)
        });
        drain(&mut with);
        // Priority sets: some focus chain and visible window.
        let mut r = Rng::new(seed);
        let chain: Vec<NodeIdx> = (0..3).map(|_| nid(r.below(n))).collect();
        let window: Vec<NodeIdx> = (0..5).map(|_| nid(r.below(n))).collect();
        with.set_focus_chain(&chain);
        with.set_visible(&window);
        let pubd = with.set_source_state(id(src.0), u64::MAX / 2, SourceState::Revoked);
        let _ = pubd;
        // Synchronous: the focus chain and the visible window already match from-scratch.
        let full = with.full_check();
        for node in chain.iter().chain(&window) {
            let have = with.entry_unmasked(*node).map(|a| {
                let mut e = (**a).clone();
                e.sem_rev = 0;
                e
            });
            assert_eq!(
                have, full[node.0 as usize],
                "seed {seed}: node {} not silenced",
                node.0
            );
        }
        drain(&mut with);
        // Equivalence with a run that never saw the source's facts.
        let without: Vec<Op> = kept
            .iter()
            .filter(|o| !matches!(o, Op::In(Input::Fact(f)) if f.source == id(src.0)))
            .cloned()
            .collect();
        let mut absent = engine(&s, false);
        exec(&mut absent, &s, &without, &mut || {
            (usize::MAX, Budget::UNLIMITED)
        });
        drain(&mut absent);
        assert_eq!(
            published(&with, n),
            published(&absent, n),
            "seed {seed} {}",
            src.1
        );
        compared += 1;
    }
    assert!(compared > 50);
}

// ---- priority, cutoff, reads, relations -----------------------------------------------------------

fn chain_engine(len: usize) -> Ivm<NoQuirks, NoPatches> {
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.default_role = SemRole::Generic;
    let mut e = Ivm::new(profile, NoQuirks, NoPatches);
    for i in 0..len {
        let p = (i > 0).then(|| nid(i - 1));
        e.add_node(nid(i), p).unwrap();
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
    e.run(Budget::UNLIMITED);
    e
}

fn name(e: &mut Ivm<NoQuirks, NoPatches>, i: usize, seq: u64, s: &str) {
    e.apply([Input::Obs {
        node: nid(i),
        src: PrimarySlot::P0,
        seq,
        write: write_from_toy(Group::Name, &json!(s)),
    }]);
}

#[test]
fn duplicate_and_older_inputs_do_no_work() {
    let mut e = chain_engine(5);
    name(&mut e, 4, 10, "Leaf");
    e.run(Budget::UNLIMITED);
    let before = e.entry_unmasked(nid(0)).cloned();
    name(&mut e, 4, 10, "Leaf");
    name(&mut e, 4, 3, "Older");
    assert_eq!(e.pending(), 0, "no register changed, nothing dirty");
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(rep.fuse_calls, 0);
    assert!(Arc::ptr_eq(
        &before.unwrap(),
        e.entry_unmasked(nid(0)).unwrap()
    ));
}

#[test]
fn content_name_propagates_up_a_chain_and_stops_at_the_first_unchanged_ancestor() {
    let mut e = chain_engine(8);
    name(&mut e, 7, 10, "Deep");
    let rep = e.run(Budget::UNLIMITED);
    // The leaf name flows to every content-named ancestor (button): one fusion each.
    assert_eq!(rep.fuse_calls, 8);
    for i in 0..8 {
        assert_eq!(
            e.entry_unmasked(nid(i)).unwrap().name.as_deref(),
            Some("Deep"),
            "{i}"
        );
    }
    assert!(e.verify().is_ok());
    // An explicit name on a middle node cuts the chain: changing the leaf again re-fuses only
    // the nodes up to it, and nothing above it changes (early cutoff).
    name(&mut e, 3, 11, "Mid");
    e.run(Budget::UNLIMITED);
    name(&mut e, 7, 12, "Deeper");
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(rep.fuse_calls, 5, "nodes 7..=3 only: {rep:?}");
    assert_eq!(
        e.entry_unmasked(nid(2)).unwrap().name.as_deref(),
        Some("Mid")
    );
    assert!(e.verify().is_ok());
}

#[test]
fn an_s0_change_that_no_selector_reads_does_not_dirty_the_neighbours() {
    let mut e = chain_engine(3);
    // `description` is read by no selector and is no part of the parent's name projection.
    e.apply([Input::Obs {
        node: nid(1),
        src: PrimarySlot::P0,
        seq: 10,
        write: write_from_toy(Group::Description, &json!("Required")),
    }]);
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(rep.fuse_calls, 1, "only the node itself: {rep:?}");
    assert!(e.verify().is_ok());
}

#[test]
fn the_focus_chain_is_fused_before_the_rest_under_a_tiny_budget() {
    // A star: the root reads its leaves, the leaves read nothing.
    let mut e = chain_engine(1);
    for i in 1..=40 {
        e.add_node(nid(i), Some(nid(0))).unwrap();
        e.apply([Input::Life {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 1,
            alive: true,
        }]);
    }
    e.run(Budget::UNLIMITED);
    // Every leaf publishes a change (a description no selector or parent reads).
    for i in 1..=40 {
        e.apply([Input::Obs {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 10,
            write: write_from_toy(Group::Description, &json!("Required")),
        }]);
    }
    e.set_focus_chain(&[nid(30)]);
    e.set_visible(&[nid(7)]);
    let rep = e.run(Budget::calls(2));
    assert_eq!(rep.fuse_calls, 2);
    let nodes: Vec<u32> = rep.published.iter().map(|p| p.node.0).collect();
    assert_eq!(
        nodes,
        vec![30, 7],
        "the focus chain goes before the visible window: {rep:?}"
    );
    assert_eq!(e.pending(), 38);
    // Carry-over changes latency only: draining gives the from-scratch result.
    e.run(Budget::UNLIMITED);
    assert!(e.verify().is_ok());
}

#[test]
fn relation_fan_out_is_capped_and_overflow_is_stale_then_refreshed() {
    let total = 100usize;
    let mut e = chain_engine(1);
    // Targets: node 0. Dependents: 100 siblings under node 0? Build them as children of 0.
    for i in 1..=total {
        e.add_node(nid(i), Some(nid(0))).unwrap();
        e.apply([Input::Life {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 1,
            alive: true,
        }]);
        e.set_relations(nid(i), vec![nid(0)]);
    }
    e.run(Budget::UNLIMITED);
    // Changing the target's text re-fuses the capped dependents and marks the rest stale.
    e.apply([Input::Obs {
        node: nid(0),
        src: PrimarySlot::P0,
        seq: 10,
        write: write_from_toy(Group::Description, &json!("Opens a dialog")),
    }]);
    let rep = e.run(Budget::calls(1));
    let stale: Vec<u32> = rep
        .published
        .iter()
        .filter(|p| {
            p.node != nid(0)
                && p.entry
                    .as_ref()
                    .is_some_and(|e| e.stale.has(Group::Name) && e.stale.has(Group::Description))
        })
        .map(|p| p.node.0)
        .collect();
    assert_eq!(
        stale.len(),
        total - gorca_fuse::RELATION_FANOUT_CAP,
        "{stale:?}"
    );
    assert!(e.pending() >= total - gorca_fuse::RELATION_FANOUT_CAP);
    // Background chunks refresh them; afterwards nothing is stale and the result is from-scratch.
    e.run(Budget::UNLIMITED);
    assert_eq!(e.pending(), 0);
    for i in 1..=total {
        assert!(e.entry_unmasked(nid(i)).unwrap().stale.is_empty(), "{i}");
    }
    assert!(e.verify().is_ok());
}

// ---- structure, TTL, vis_rev, reconnect --------------------------------------------------------------

#[test]
fn removing_a_subtree_publishes_removals_and_refuses_the_parent() {
    let mut e = chain_engine(4);
    name(&mut e, 3, 10, "Leaf");
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(0)).unwrap().name.as_deref(),
        Some("Leaf")
    );
    let gone = e.remove_subtree(nid(3)).unwrap();
    assert_eq!(gone.len(), 1);
    e.run(Budget::UNLIMITED);
    assert!(e.entry_unmasked(nid(3)).is_none());
    assert_eq!(e.entry_unmasked(nid(0)).unwrap().name.as_deref(), None);
    assert!(e.verify().is_ok());
    assert_eq!(
        e.add_node(nid(2), None),
        Err(gorca_fuse::IvmError::NodeExists)
    );
    assert_eq!(
        e.add_node(nid(9), Some(nid(77))),
        Err(gorca_fuse::IvmError::UnknownParent)
    );
}

fn fact_engine() -> Ivm<NoQuirks, NoPatches> {
    let mut e = chain_engine(2);
    e.set_source(source_info(&SOURCES[1])).unwrap();
    e.set_source(source_info(&SOURCES[0])).unwrap();
    e.set_source_state(id(3), 1, SourceState::Active);
    e.set_source_state(id(2), 1, SourceState::Active);
    e
}

fn ocr_name(node: usize, text: &str, e: &Ivm<NoQuirks, NoPatches>, ttl: u64) -> Input {
    let anchor = AnchorId::Node(nid(node));
    Input::Fact(Box::new(FactIn {
        source: id(3),
        group: Group::Name.group_code(),
        seq: 1,
        payload: Payload::Sem(write_from_toy(Group::Name, &json!(text))),
        p_milli: 990,
        basis: Basis::VisRev(e.vis_rev(nid(node)) + 1),
        bind_gen: e.resolve_anchor(&anchor).generation,
        admitted_at_ms: e.clock(),
        ttl_ms: Some(ttl),
        anchor,
    }))
}

#[test]
fn ttl_expiry_vis_rev_and_epochs_refuse_exactly_the_affected_node() {
    let mut e = fact_engine();
    let f = ocr_name(1, "Save", &e, 10_000);
    e.apply([f]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Save")
    );
    assert_eq!(
        e.entry_unmasked(nid(0)).unwrap().name.as_deref(),
        Some("Save"),
        "content name"
    );
    // The clock passes the TTL: the fact retires and the name goes with it.
    e.apply([Input::Tick(20_000)]);
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(e.entry_unmasked(nid(1)).unwrap().name.as_deref(), None);
    assert!(rep.fuse_calls >= 2 && rep.fuse_calls <= 3, "{rep:?}");
    // A fresh fact, then a vis_rev bump retires an Inferred one.
    let f = ocr_name(1, "Open", &e, 300_000);
    let Input::Fact(mut fi) = f else { panic!() };
    fi.seq = 2;
    e.apply([Input::Fact(fi)]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Open")
    );
    e.apply([Input::Vis {
        node: nid(1),
        rev: 5,
    }]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        None,
        "stale basis"
    );
    // A silencing epoch applies synchronously to the focus chain.
    let f = ocr_name(1, "Close", &e, 300_000);
    let Input::Fact(mut fi) = f else { panic!() };
    fi.seq = 3;
    e.apply([Input::Fact(fi)]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Close")
    );
    e.set_focus_chain(&[nid(1), nid(0)]);
    let pubd = e.set_source_state(id(3), 9, SourceState::Revoked);
    assert!(
        pubd.iter().any(|p| p.node == nid(1)),
        "silenced in the same call"
    );
    assert_eq!(e.entry_unmasked(nid(1)).unwrap().name.as_deref(), None);
    assert_eq!(e.entry_unmasked(nid(0)).unwrap().name.as_deref(), None);
    // Unrevoke (higher epoch) brings it back.
    e.set_source_state(id(3), 10, SourceState::Active);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Close")
    );
    assert!(e.verify().is_ok());
}

#[test]
fn a_reconnecting_source_releases_its_old_slot_before_registering() {
    let mut e = fact_engine();
    let f = ocr_name(1, "Save", &e, 300_000);
    e.apply([f]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Save")
    );
    // The new connection has a new slot but the same stable name: rejected while the old one lives.
    let mut next = source_info(&SOURCES[1]);
    next.id = SourceId {
        slot: 30,
        generation: 1,
    };
    assert_eq!(
        e.set_source(next.clone()),
        Err(gorca_fuse::SideError::DuplicateName)
    );
    // A failed reconnect (the name is another source's) changes nothing.
    let mut clash = next.clone();
    clash.name = Arc::from("bus");
    assert_eq!(
        e.reconnect(id(3), clash).err(),
        Some(gorca_fuse::SideError::DuplicateName)
    );
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Save")
    );
    // The explicit path: old slot and its registers go first, then the new registration. The
    // focus chain is re-fused before the call returns.
    e.set_focus_chain(&[nid(1)]);
    let pubd = e.reconnect(id(3), next).unwrap();
    assert!(
        pubd.iter().any(|p| p.node == nid(1)),
        "old facts silenced at once"
    );
    assert_eq!(e.entry_unmasked(nid(1)).unwrap().name.as_deref(), None);
    // Old id is unregistered (its facts are rejected); the new id works.
    let old_fact = ocr_name(1, "Stale", &e, 300_000);
    assert_eq!(
        e.apply([old_fact]).errors,
        vec![gorca_fuse::IvmError::Side(
            gorca_fuse::SideError::UnknownSource
        )]
    );
    let Input::Fact(mut fresh) = ocr_name(1, "Fresh", &e, 300_000) else {
        panic!()
    };
    fresh.source = SourceId {
        slot: 30,
        generation: 1,
    };
    // Masked until the new connection's first Active epoch.
    e.apply([Input::Fact(fresh.clone())]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        None,
        "new source starts masked"
    );
    e.set_source_state(fresh.source, 1, SourceState::Active);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Fresh")
    );
    // A permanent release drops the registration and registers.
    e.release_source(SourceId {
        slot: 30,
        generation: 1,
    });
    assert_eq!(e.entry_unmasked(nid(1)).unwrap().name.as_deref(), None);
    e.run(Budget::UNLIMITED);
    assert!(e.verify().is_ok());
}

#[test]
fn the_global_epoch_refuses_everything_with_priority_first() {
    let mut e = chain_engine(6);
    e.set_focus_chain(&[nid(5)]);
    let before = e.global_epoch();
    let pubd = e.bump_global();
    assert_eq!(e.global_epoch(), before + 1);
    assert!(pubd.is_empty(), "nothing changed, cutoff everywhere");
    assert!(e.pending() > 0, "non-priority nodes wait for run()");
    e.run(Budget::UNLIMITED);
    assert_eq!(e.pending(), 0);
    assert!(e.verify().is_ok());
}

#[test]
fn deep_and_wide_trees_stay_bounded() {
    // A 3000-deep chain: a leaf name change walks the chain once and the strings stay capped.
    let mut e = chain_engine(3000);
    name(&mut e, 2999, 10, &"é".repeat(2000));
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(rep.fuse_calls, 3000);
    assert!(
        e.entry_unmasked(nid(0))
            .unwrap()
            .name
            .as_deref()
            .unwrap()
            .len()
            <= 1024
    );
    // 5000 siblings under one parent: a change past the content prefix never re-fuses the parent.
    let mut e = chain_engine(1);
    for i in 1..=5000 {
        e.add_node(nid(i), Some(nid(0))).unwrap();
        e.apply([Input::Life {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 1,
            alive: true,
        }]);
    }
    e.run(Budget::UNLIMITED);
    e.apply([Input::Obs {
        node: nid(5000),
        src: PrimarySlot::P0,
        seq: 5,
        write: write_from_toy(Group::Name, &json!("Far")),
    }]);
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(
        rep.fuse_calls, 1,
        "beyond max_children: the parent never reads it"
    );
    e.apply([Input::Obs {
        node: nid(1),
        src: PrimarySlot::P0,
        seq: 5,
        write: write_from_toy(Group::Name, &json!("Near")),
    }]);
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(rep.fuse_calls, 2);
    assert!(e.verify().is_ok());
}

#[cfg(feature = "fuse_full_check")]
#[test]
fn the_full_check_feature_reports_no_mismatch_after_a_drained_run() {
    let mut e = chain_engine(4);
    name(&mut e, 3, 10, "X");
    let rep = e.run(Budget::UNLIMITED);
    assert_eq!(rep.check_mismatches, Some(0));
}

#[test]
fn a_parent_s0_change_that_a_selector_reads_refuses_the_children() {
    // "A generic whose parent is a group becomes a button": the child depends on the parent's S0
    // role through the selector-read set.
    for narrow in [false, true] {
        let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
        profile.default_role = SemRole::Generic;
        let patch = ToyPatch {
            pid: "p0".into(),
            ext_id: "org.ext0".into(),
            spec: 1,
            sel_role: "generic".into(),
            empty_name: false,
            sel_parent: Some("group".into()),
            group: Group::Role,
            value: json!("button"),
        };
        let mut e = Ivm::new(
            profile,
            ToyQuirks,
            Patches {
                inner: ToyPatches(vec![patch]),
                narrow,
            },
        );
        e.add_node(nid(0), None).unwrap();
        e.add_node(nid(1), Some(nid(0))).unwrap();
        let obs = |node: usize, seq: u64, role: &str| Input::Obs {
            node: nid(node),
            src: PrimarySlot::P0,
            seq,
            write: write_from_toy(Group::Role, &json!(role)),
        };
        let life = |node: usize| Input::Life {
            node: nid(node),
            src: PrimarySlot::P0,
            seq: 1,
            alive: true,
        };
        e.apply([life(0), life(1), obs(0, 2, "text"), obs(1, 2, "generic")]);
        e.run(Budget::UNLIMITED);
        assert_eq!(e.entry_unmasked(nid(1)).unwrap().sem.role, SemRole::Generic);
        e.apply([obs(0, 3, "group")]);
        e.run(Budget::UNLIMITED);
        assert_eq!(
            e.entry_unmasked(nid(1)).unwrap().sem.role,
            SemRole::Button,
            "narrow={narrow}"
        );
        assert!(e.verify().is_ok());
    }
}

// ---- security and scheduler fixes -----------------------------------------------------------------

fn star(n: usize) -> Ivm<NoQuirks, NoPatches> {
    let mut e = chain_engine(1);
    for i in 1..=n {
        e.add_node(nid(i), Some(nid(0))).unwrap();
        e.apply([Input::Life {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 1,
            alive: true,
        }]);
    }
    e.run(Budget::UNLIMITED);
    e
}

#[test]
fn a_silenced_source_cannot_escape_by_reconnecting_or_re_registering() {
    let mut e = fact_engine();
    let f = ocr_name(1, "Save", &e, 300_000);
    e.apply([f]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Save")
    );
    e.set_focus_chain(&[nid(1), nid(0)]);
    e.set_source_state(id(3), 5, SourceState::Revoked);
    assert_eq!(e.entry_unmasked(nid(1)).unwrap().name.as_deref(), None);
    // Reconnect: the new id inherits the revocation by stable name; an Active epoch with a lower
    // number cannot lift it, a later one can.
    let new = SourceId {
        slot: 40,
        generation: 1,
    };
    let mut info = source_info(&SOURCES[1]);
    info.id = new;
    e.reconnect(id(3), info).unwrap();
    let Input::Fact(mut f) = ocr_name(1, "Again", &e, 300_000) else {
        panic!()
    };
    f.source = new;
    f.seq = 9;
    e.apply([Input::Fact(f)]);
    e.set_source_state(new, 1, SourceState::Active);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        None,
        "a revoked source stays revoked across a reconnect"
    );
    e.set_source_state(new, 6, SourceState::Active);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Again")
    );
    // A permanent release keeps the silence for the name too.
    e.set_source_state(new, 7, SourceState::Quarantined);
    e.release_source(new);
    let back = SourceId {
        slot: 41,
        generation: 2,
    };
    let mut info = source_info(&SOURCES[1]);
    info.id = back;
    e.set_source(info).unwrap();
    let Input::Fact(mut f) = ocr_name(1, "Back", &e, 300_000) else {
        panic!()
    };
    f.source = back;
    e.apply([Input::Fact(f)]);
    e.run(Budget::UNLIMITED);
    assert_eq!(e.entry_unmasked(nid(1)).unwrap().name.as_deref(), None);
    assert!(e.verify().is_ok());
}

#[test]
fn a_new_source_is_masked_until_an_explicit_active_epoch() {
    let mut e = chain_engine(2);
    e.set_source(source_info(&SOURCES[1])).unwrap();
    let f = ocr_name(1, "Save", &e, 300_000);
    e.apply([f]);
    e.run(Budget::UNLIMITED);
    assert_eq!(e.entry_unmasked(nid(1)).unwrap().name.as_deref(), None);
    e.set_source_state(id(3), 1, SourceState::Active);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(1)).unwrap().name.as_deref(),
        Some("Save")
    );
}

#[test]
fn a_life_seq_beyond_32_bits_fails_closed_instead_of_colliding() {
    let mut e = chain_engine(1);
    // `BindGen` is 32-bit: a seq that does not fit makes the anchor unbound, never u32::MAX.
    e.apply([Input::Life {
        node: nid(0),
        src: PrimarySlot::P0,
        seq: u64::from(u32::MAX) + 5,
        alive: true,
    }]);
    let r = e.resolve_anchor(&AnchorId::Node(nid(0)));
    assert_eq!(r.node, None);
    e.apply([Input::Life {
        node: nid(0),
        src: PrimarySlot::P0,
        seq: 7,
        alive: true,
    }]);
    // The older, smaller seq cannot resurrect it: the register keeps the larger one.
    assert_eq!(e.resolve_anchor(&AnchorId::Node(nid(0))).node, None);
}

#[test]
fn the_priority_set_is_bounded_on_deep_and_wide_trees() {
    // Deep: a 5000-deep chain of content-named buttons, focus at the root.
    let mut e = chain_engine(5000);
    let limit = e.profile().limits.max_priority;
    for i in 0..5000 {
        e.apply([Input::Inval {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 9,
        }]);
    }
    e.set_focus_chain(&[nid(0)]);
    assert!(e.priority_work() <= limit, "deep: {}", e.priority_work());
    e.set_visible(&[nid(2500)]);
    assert!(e.priority_work() <= limit);
    // Wide: 10_000 children, the root is focused; only the first max_children join.
    let mut w = star(10_000);
    for i in 1..=10_000 {
        w.apply([Input::Inval {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 9,
        }]);
    }
    w.set_focus_chain(&[nid(0)]);
    assert!(w.priority_work() <= w.profile().limits.max_children + 8);
    // Re-keying never walks the dirty backlog: running in small slices makes steady progress.
    let before = w.pending();
    let rep = w.run(Budget::calls(50));
    assert!(rep.fuse_calls <= 50);
    assert!(w.pending() < before);
    w.run(Budget::UNLIMITED);
    assert!(w.verify().is_ok());
}

#[test]
fn a_shallow_change_is_fused_within_a_few_runs_while_a_deep_leaf_keeps_changing() {
    let mut e = chain_engine(50);
    e.apply([Input::Obs {
        node: nid(1),
        src: PrimarySlot::P0,
        seq: 100,
        write: write_from_toy(Group::Description, &json!("Required")),
    }]);
    let mut fused_at = None;
    for k in 0..40u64 {
        e.apply([Input::Obs {
            node: nid(49),
            src: PrimarySlot::P0,
            seq: 200 + k,
            write: write_from_toy(
                Group::Description,
                &json!(if k.is_multiple_of(2) {
                    "Opens a dialog"
                } else {
                    "Required"
                }),
            ),
        }]);
        let rep = e.run(Budget::calls(1));
        if fused_at.is_none() && rep.published.iter().any(|p| p.node.0 == 1) {
            fused_at = Some(k);
        }
    }
    let k = fused_at.expect("node 1 starved");
    assert!(k <= 6, "aging should serve it within a few runs, took {k}");
}

#[test]
fn background_work_gets_a_share_even_while_the_dirty_set_is_busy() {
    // A global epoch queues every node; deep churn keeps the dirty set non-empty, yet the sweep
    // still reaches the shallow nodes.
    let mut e = chain_engine(60);
    e.bump_global();
    let mut k = 0u64;
    while e.sweep_remaining() > 0 && k < 400 {
        // Ten deep nodes change every round: the dirty set never drains under a 2-call budget.
        for d in 50..60usize {
            e.apply([Input::Obs {
                node: nid(d),
                src: PrimarySlot::P0,
                seq: 1000 + k,
                write: write_from_toy(
                    Group::Description,
                    &json!(if k.is_multiple_of(2) {
                        "Required"
                    } else {
                        "Opens a dialog"
                    }),
                ),
            }]);
        }
        e.run(Budget::calls(2));
        k += 1;
    }
    e.run(Budget::UNLIMITED);
    assert!(k < 400, "the sweep starved");
    assert!(e.verify().is_ok());
}

#[test]
fn a_lifecycle_change_re_resolves_only_the_anchors_it_can_move() {
    let mut e = fact_engine();
    for i in 2..=400 {
        e.add_node(nid(i), Some(nid(0))).unwrap();
        e.apply([Input::Life {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 1,
            alive: true,
        }]);
        let Input::Fact(mut f) = ocr_name(i, "X", &e, 300_000) else {
            panic!()
        };
        f.seq = i as u64;
        e.apply([Input::Fact(f)]);
    }
    e.run(Budget::UNLIMITED);
    let before = e.rebind_count();
    e.apply([Input::Life {
        node: nid(77),
        src: PrimarySlot::P0,
        seq: 5,
        alive: true,
    }]);
    let delta = e.rebind_count() - before;
    assert!(delta <= 3, "re-resolved {delta} anchors for one node");
    e.run(Budget::UNLIMITED);
    assert!(e.verify().is_ok());
}

#[test]
fn the_synchronous_priority_drain_is_bounded_and_the_rest_carries_over() {
    let mut e = star(2000);
    e.set_visible(&[nid(0)]);
    // The root reads its first 256 children: all join the priority set, but the synchronous part
    // never exceeds the cap; the remainder stays dirty in the front class.
    let cap = e.profile().limits.max_sync_drain;
    let pubd = e.bump_global();
    assert!(pubd.len() <= cap);
    assert!(e.pending() > 0);
    e.run(Budget::UNLIMITED);
    assert_eq!(e.pending(), 0);
    assert!(e.verify().is_ok());
}

/// Release-mode timing: issuing a focus change or a global epoch with a huge dirty backlog must
/// not walk the backlog (S-1). Skipped in debug builds, where timings mean nothing.
#[test]
fn focus_change_and_global_epoch_with_100k_dirty_nodes_stay_under_half_a_millisecond() {
    if cfg!(debug_assertions) {
        return;
    }
    let n = 100_000;
    let mut e = star(n);
    for i in 1..=n {
        e.apply([Input::Obs {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 10,
            write: write_from_toy(Group::Description, &json!("Required")),
        }]);
    }
    assert!(e.pending() >= n);
    let mut best_focus = std::time::Duration::MAX;
    for k in 0..7usize {
        let t = std::time::Instant::now();
        e.set_focus_chain(&[nid(3 + k), nid(0)]);
        best_focus = best_focus.min(t.elapsed());
    }
    e.set_visible(&[nid(0)]);
    let mut best_epoch = std::time::Duration::MAX;
    for _ in 0..5 {
        let t = std::time::Instant::now();
        let _ = e.bump_global();
        best_epoch = best_epoch.min(t.elapsed());
    }
    eprintln!("focus change {best_focus:?}, bump_global sync {best_epoch:?} (100k dirty)");
    assert!(
        best_focus < std::time::Duration::from_micros(500),
        "{best_focus:?}"
    );
    assert!(
        best_epoch < std::time::Duration::from_micros(500),
        "{best_epoch:?}"
    );
}

// ---- the silence mask -------------------------------------------------------------------------------

/// A star of `n` leaves, each named "Save" by the (active) OCR source; the first `window` leaves are
/// the visible window.
fn named_star(n: usize, window: usize) -> Ivm<NoQuirks, NoPatches> {
    let mut e = star(n);
    e.set_source(source_info(&SOURCES[1])).unwrap();
    e.set_source_state(id(3), 1, SourceState::Active);
    for i in 1..=n {
        let Input::Fact(mut f) = ocr_name(i, "Save", &e, 300_000) else {
            panic!()
        };
        f.seq = i as u64;
        e.apply([Input::Fact(f)]);
    }
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(n)).unwrap().name.as_deref(),
        Some("Save")
    );
    let win: Vec<NodeIdx> = (1..=window).map(nid).collect();
    e.set_visible(&win);
    e
}

fn revoke_scenario(n: usize, window: usize) -> std::time::Duration {
    let mut e = named_star(n, window);
    // Dirty every node so the backlog is huge.
    for i in 1..=n {
        e.apply([Input::Obs {
            node: nid(i),
            src: PrimarySlot::P0,
            seq: 10,
            write: write_from_toy(Group::Description, &json!("Required")),
        }]);
    }
    assert!(
        window > e.profile().limits.max_sync_drain,
        "the cap must bite"
    );
    let t = std::time::Instant::now();
    e.set_source_state(id(3), 5, SourceState::Revoked);
    let took = t.elapsed();
    assert!(e.pending() > n / 2, "most of the backlog is still pending");
    // Right after the call no node reads a revoked value, drained or not.
    for i in 0..=n {
        let v = e.entry_view(nid(i)).unwrap();
        assert_ne!(
            v.name.as_deref(),
            Some("Save"),
            "node {i} still speaks the revoked source"
        );
    }
    // The raw published entry of an unreached node still carries it (the proper re-fusion is
    // pending), which is exactly what the mask covers.
    assert!(
        (window + 1..=n).any(|i| e.entry_unmasked(nid(i)).unwrap().name.as_deref() == Some("Save")),
        "the cap left nodes unfused"
    );
    e.run(Budget::UNLIMITED);
    for i in 0..=n {
        assert_eq!(e.entry_view(nid(i)).unwrap().name.as_deref(), None);
    }
    assert!(e.verify().is_ok());
    took
}

#[test]
fn a_revoked_source_is_never_read_even_beyond_the_sync_drain_cap() {
    // 1024 priority nodes, far above the cap of 200, and a backlog behind them.
    revoke_scenario(3000, 1024);
}

#[test]
fn the_silence_mask_follows_content_names_and_lifts_with_the_epoch() {
    let mut e = fact_engine();
    let f = ocr_name(1, "Save", &e, 300_000);
    e.apply([f]);
    e.run(Budget::UNLIMITED);
    assert_eq!(
        e.entry_unmasked(nid(0)).unwrap().name.as_deref(),
        Some("Save"),
        "derived through content"
    );
    // No priority nodes at all: the mask alone silences both the child and the derived parent.
    e.set_source_state(id(3), 5, SourceState::Revoked);
    assert_eq!(e.entry_view(nid(1)).unwrap().name.as_deref(), None);
    assert_eq!(e.entry_view(nid(0)).unwrap().name.as_deref(), None);
    assert_eq!(
        e.entry_view(nid(0)).unwrap().prov.get(Group::Name),
        Some(gorca_model::sem::ProvCode::Default)
    );
    // Lifting the epoch lifts the mask; the (unchanged) published value reads again.
    e.set_source_state(id(3), 6, SourceState::Active);
    assert_eq!(e.entry_view(nid(1)).unwrap().name.as_deref(), Some("Save"));
    e.run(Budget::UNLIMITED);
    assert_eq!(e.entry_view(nid(0)).unwrap().name.as_deref(), Some("Save"));
    // A released source is masked too, until its nodes are re-fused.
    e.release_source(id(3));
    assert_eq!(e.entry_view(nid(1)).unwrap().name.as_deref(), None);
    e.run(Budget::UNLIMITED);
    assert_eq!(e.entry_view(nid(1)).unwrap().name.as_deref(), None);
    assert!(e.verify().is_ok());
}

#[test]
fn revoking_with_a_huge_backlog_stays_under_half_a_millisecond_and_silences_every_node() {
    if cfg!(debug_assertions) {
        return;
    }
    let took = revoke_scenario(50_000, 1024);
    eprintln!("revoke sync part (1024 priority nodes, 50k dirty): {took:?}");
    assert!(took < std::time::Duration::from_micros(500), "{took:?}");
}

#[test]
fn stats_expose_the_resident_s0_store_and_the_recompute_count() {
    // No selector reads anything: S0 is recomputed per fusion and nothing is kept resident.
    let e = chain_engine(5);
    let st = e.stats();
    assert_eq!(st.nodes, 5);
    assert_eq!(
        st.s0_resident_nodes, 0,
        "S0 is rebuilt from the published entries: nothing is stored"
    );
    assert_eq!(st.s0_resident_bytes, 0);
    assert!(st.s0_computes >= 5 && st.fusions >= 5);
    assert!(st.node_record_bytes > 0 && st.node_record_bytes < 400);
    // A patch re-roles a child, so its S0 role differs from what the published entry rebuilds:
    // exactly that exception is stored (the delta), for the one node that has it.
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.default_role = SemRole::Generic;
    let patch = ToyPatch {
        pid: "p0".into(),
        ext_id: "org.ext0".into(),
        spec: 1,
        sel_role: "generic".into(),
        empty_name: false,
        sel_parent: Some("group".into()),
        group: Group::Role,
        value: json!("button"),
    };
    let mut p = Ivm::new(
        profile,
        ToyQuirks,
        Patches {
            inner: ToyPatches(vec![patch]),
            narrow: true,
        },
    );
    p.add_node(nid(0), None).unwrap();
    p.add_node(nid(1), Some(nid(0))).unwrap();
    let obs = |node: usize, role: &str| Input::Obs {
        node: nid(node),
        src: PrimarySlot::P0,
        seq: 2,
        write: write_from_toy(Group::Role, &json!(role)),
    };
    let life = |node: usize| Input::Life {
        node: nid(node),
        src: PrimarySlot::P0,
        seq: 1,
        alive: true,
    };
    p.apply([life(0), life(1), obs(0, "group"), obs(1, "generic")]);
    p.run(Budget::UNLIMITED);
    assert_eq!(p.entry_unmasked(nid(1)).unwrap().sem.role, SemRole::Button);
    let st = p.stats();
    assert_eq!(
        st.s0_resident_nodes, 1,
        "only the re-roled child keeps an exception"
    );
    assert!(st.s0_resident_bytes > 0);
    assert!(st.s0_neighbour_hits > 0);
    assert!(p.verify().is_ok());
}
