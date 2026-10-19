// SPDX-License-Identifier: GPL-3.0-or-later
//! Property tests F1-F6, F9-F13, F15 and F18 over seeded random workloads (trees of up to 400
//! nodes, two primaries, Secondary and Inferred sources, users, patches, vis bumps, ticks, epochs
//! and DOM-id moves). Facts are **raw** `FactIn`s stamped the way a writer stamps them (running
//! index for `BindGen`, running `vis_rev`, the clock); candidates exist only through the side
//! table's validity filter.
//!
//! `proptest` is not vendored in the project-local cargo home, so the generator is a small
//! deterministic xorshift PRNG; every failure message carries the seed. F7 (incremental equals
//! from-scratch) is B2; F6's second half (provenance-only transitions are silent) needs the
//! utterance model of M22b; F14 (revocation equals absence) is covered by the epoch cases of the
//! oracle corpus and `revoked_source_equals_absent_source`; F16 and F19 are B3.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use gorca_fuse::{
    AnchorId, AnchorIndex, FactIn, Grant, Layer, NodeIdx, Payload, PrimarySlot, Profile, Slot,
    UserOp, Write, grant_class, invariant_violations,
};
use gorca_model::fact::{Basis, BindGen, EvidenceValue, SourceId};
use gorca_model::sem::{
    BindQuality, ConfClass, EvidenceKind, Group, GroupMask, ProvCode, SemEntry, SemRole, TrustTier,
};
use serde_json::{Value, json};
use support::{
    Case, NoQ, ToyIndex, ToyPatch, TreeOut, World, empty_world, evaluate_full, normalise_expected,
    slot_json, toy_from_entry, toy_rules, toy_str, trails_json, write_from_toy,
};

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
const DESCS: &[&str] = &["", "Opens a dialog", "Required"];
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

fn toy_domain(g: Group) -> Vec<Value> {
    let s = |xs: &[&str]| xs.iter().map(|x| json!(x)).collect::<Vec<_>>();
    match g {
        Group::Role => s(ROLES),
        Group::Name => s(NAMES),
        Group::Description => s(DESCS),
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
    let dom = toy_domain(g);
    write_from_toy(g, r.pick(&dom))
}

/// The static tables of the synthetic world: the same sources as the oracle corpus.
fn synthetic_tables() -> Value {
    json!({"sources": {
        "atspi": [0, 0, "Primary", 1, null],
        "newton": [1, 0, "Primary", 1, null],
        "bus": [2, 0, "Secondary", 3, null],
        "ocr": [3, 0, "Inferred", 2, 0],
        "icon": [4, 0, "Inferred", 2, 1],
        "ext.a": [5, 0, "Inferred", 3, 2],
    }})
}

/// (slot, name, tier, semantic groups, evidence kinds) of the non-primary sources.
type SrcRow = (
    u32,
    &'static str,
    TrustTier,
    &'static [Group],
    &'static [EvidenceKind],
);

const SOURCES: &[SrcRow] = &[
    (
        2,
        "bus",
        TrustTier::T3Untrusted,
        MODELLED,
        &[EvidenceKind::HintTitleAttr, EvidenceKind::LayoutCssHidden],
    ),
    (
        3,
        "ocr",
        TrustTier::T2FirstParty,
        &[Group::Name],
        &[EvidenceKind::VisualOcrText],
    ),
    (
        4,
        "icon",
        TrustTier::T2FirstParty,
        &[Group::Name, Group::Description],
        &[],
    ),
    (
        5,
        "ext.a",
        TrustTier::T3Untrusted,
        &[Group::Name, Group::Description],
        &[EvidenceKind::HintTitleAttr],
    ),
];

#[derive(Clone, Debug)]
enum Ev {
    Obs {
        src: PrimarySlot,
        x: usize,
        seq: u64,
        w: Write,
    },
    Life {
        src: PrimarySlot,
        x: usize,
        seq: u64,
        alive: bool,
    },
    Inval {
        src: PrimarySlot,
        x: usize,
        seq: u64,
    },
    User {
        x: usize,
        g: Group,
        seq: u64,
        op: UserOp,
    },
    Patch {
        pid: String,
        seq: u64,
        inst: bool,
    },
    Fact(Box<FactIn>),
    Attr {
        x: usize,
        seq: u64,
        id: Option<String>,
    },
    Vis {
        x: usize,
        rev: u32,
    },
    Tick(u64),
    Epoch {
        src: &'static str,
        n: u64,
        state: &'static str,
    },
}

fn apply(w: &mut World, ev: &Ev) -> bool {
    match ev {
        Ev::Obs { src, x, seq, w: wr } => w.nodes[*x].src_mut(*src).apply_obs(*seq, wr.clone()),
        Ev::Life { src, x, seq, alive } => w.nodes[*x].src_mut(*src).apply_life(*seq, *alive),
        Ev::Inval { src, x, seq } => w.nodes[*x].src_mut(*src).apply_inval(*seq),
        Ev::User { x, g, seq, op } => w.nodes[*x].apply_user(*g, *seq, op.clone()),
        Ev::Patch { pid, seq, inst } => {
            let new = (*seq, *inst);
            match w.patch.get(pid) {
                Some(&cur) if new <= cur => false,
                _ => {
                    w.patch.insert(pid.clone(), new);
                    true
                }
            }
        }
        Ev::Fact(f) => w.side.insert((**f).clone()).unwrap(),
        Ev::Attr { x, seq, id } => {
            let key =
                |a: &(u64, Option<String>)| (a.0, a.1.clone().unwrap_or_else(|| "None".into()));
            let new = (*seq, id.clone());
            match w.attr.get(x) {
                Some(cur) if key(&new) <= key(cur) => false,
                _ => {
                    w.attr.insert(*x, new);
                    true
                }
            }
        }
        Ev::Vis { x, rev } => {
            if *rev > w.vis[*x] {
                w.vis[*x] = *rev;
                true
            } else {
                false
            }
        }
        Ev::Tick(t) => {
            if *t > w.clock {
                w.clock = *t;
                true
            } else {
                false
            }
        }
        Ev::Epoch { src, n, state } => {
            let new = (*n, (*state).to_owned());
            match w.epoch.get(*src) {
                Some(cur) if new <= *cur => false,
                _ => {
                    w.epoch.insert((*src).to_owned(), new);
                    true
                }
            }
        }
    }
}

struct Workload {
    case: Case,
    events: Vec<Ev>,
}

impl Workload {
    fn fresh_world(&self) -> World {
        empty_world(self.case.parent.len(), &self.case.tables)
    }

    fn run(&self, order: &[usize]) -> World {
        let mut w = self.fresh_world();
        for &i in order {
            apply(&mut w, &self.events[i]);
        }
        w
    }

    fn canon(&self) -> Vec<usize> {
        (0..self.events.len()).collect()
    }

    fn world(&self) -> World {
        self.run(&self.canon())
    }

    fn world_where(&self, keep: impl Fn(&Ev) -> bool) -> World {
        let mut w = self.fresh_world();
        for e in self.events.iter().filter(|e| keep(e)) {
            apply(&mut w, e);
        }
        w
    }
}

fn eval(wl: &Workload, w: &World) -> TreeOut {
    evaluate_full(&wl.case, w, &NoQ, false)
}

fn eval_traced(wl: &Workload, w: &World) -> TreeOut {
    evaluate_full(&wl.case, w, &NoQ, true)
}

fn rand_fact(r: &mut Rng, w: &World, profile: &Profile, n: usize, seq: u64) -> FactIn {
    let (slot, _name, _tier, groups, evidence) = *r.pick(SOURCES);
    let x = r.below(n);
    let index = ToyIndex::new(profile, w);
    let dom_ids: Vec<&String> = w.attr.values().filter_map(|(_, id)| id.as_ref()).collect();
    let anchor = if r.chance(30) && !dom_ids.is_empty() {
        AnchorId::Dom {
            id: Arc::from(dom_ids[r.below(dom_ids.len())].as_str()),
            count: if r.chance(80) { 1 } else { 2 },
            frame: 0,
            shadow: 0,
        }
    } else {
        AnchorId::Node(NodeIdx(u32::try_from(x).unwrap()))
    };
    let res = index.resolve(&anchor);
    let tx = res.node.map_or(x, |n| n.0 as usize);
    let (group, payload) = if !evidence.is_empty() && r.chance(35) {
        let k = *r.pick(evidence);
        let p = if k == EvidenceKind::LayoutCssHidden {
            Payload::Evidence(EvidenceValue::Flag(r.chance(50)))
        } else {
            let texts = ["Close", "Delete", "Search", "", "Menu"];
            Payload::Evidence(EvidenceValue::Text(Arc::from(*r.pick(&texts))))
        };
        (k.group_code(), p)
    } else {
        let g = *r.pick(groups);
        let p = if r.chance(5) {
            Payload::Retract
        } else {
            Payload::Sem(rand_write(r, g))
        };
        (g.group_code(), p)
    };
    let p_milli = u16::try_from(r.below(400) + 600).unwrap();
    let inferred = slot >= 3;
    let basis = i64::from(w.vis[tx]) - i64::from(r.chance(10));
    FactIn {
        source: SourceId {
            slot,
            generation: 0,
        },
        anchor,
        group,
        seq,
        payload,
        p_milli,
        basis: Basis::VisRev(u32::try_from(basis + 1).unwrap()),
        bind_gen: res.generation,
        admitted_at_ms: w.clock,
        ttl_ms: if inferred || r.chance(30) {
            Some(*r.pick(&[5_000u64, 60_000, 300_000]))
        } else {
            None
        },
    }
}

fn workload(seed: u64, n: usize, with_facts: bool) -> Workload {
    let mut r = Rng::new(seed);
    let parent: Vec<i64> = (0..n)
        .map(|x| {
            if x == 0 {
                -1
            } else {
                i64::try_from(r.below(x)).unwrap()
            }
        })
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
    let patches: Vec<ToyPatch> = (0..r.below(4))
        .map(|i| {
            let g = *r.pick(&[Group::Name, Group::Role, Group::Level, Group::Avail]);
            let dom = toy_domain(g);
            let value = if g == Group::Name && r.chance(50) {
                json!("firstchild")
            } else {
                r.pick(&dom).clone()
            };
            ToyPatch {
                pid: format!("p{i}"),
                ext_id: format!("org.ext{}", r.below(3)),
                spec: i64::try_from(r.below(2)).unwrap(),
                sel_role: (*r.pick(ROLES)).to_owned(),
                empty_name: r.chance(50),
                sel_parent: if r.chance(30) {
                    Some((*r.pick(ROLES)).to_owned())
                } else {
                    None
                },
                group: g,
                value,
            }
        })
        .collect();
    let mut case = Case::synthetic(format!("seed{seed}"), profile, parent, patches);
    case.tables = synthetic_tables();
    let mut wl = Workload {
        case,
        events: Vec::new(),
    };
    // A running world gives the writer-side stamps (binding generation, vis_rev, clock).
    let mut run = wl.fresh_world();
    let mut seq = 0u64;
    let emit = |wl: &mut Workload, run: &mut World, ev: Ev| {
        apply(run, &ev);
        wl.events.push(ev);
    };
    for x in 0..n {
        seq += 1;
        emit(
            &mut wl,
            &mut run,
            Ev::Life {
                src: PrimarySlot::P0,
                x,
                seq,
                alive: true,
            },
        );
        if r.chance(60) {
            seq += 1;
            emit(
                &mut wl,
                &mut run,
                Ev::Life {
                    src: PrimarySlot::P1,
                    x,
                    seq,
                    alive: true,
                },
            );
        }
        if r.chance(60) {
            seq += 1;
            let id = format!("id{}", r.below((n / 2).max(2)));
            emit(
                &mut wl,
                &mut run,
                Ev::Attr {
                    x,
                    seq,
                    id: Some(id),
                },
            );
        }
    }
    let ids = (n / 2).max(2);
    for _ in 0..n * 7 {
        let x = r.below(n);
        seq += 1;
        let src = if r.chance(50) {
            PrimarySlot::P0
        } else {
            PrimarySlot::P1
        };
        let roll = r.below(100);
        let ev = if roll < 45 {
            let g = *r.pick(MODELLED);
            Ev::Obs {
                src,
                x,
                seq,
                w: rand_write(&mut r, g),
            }
        } else if roll < 50 {
            Ev::Inval { src, x, seq }
        } else if roll < 54 {
            Ev::Life {
                src,
                x,
                seq,
                alive: r.chance(80),
            }
        } else if roll < 60 {
            let g = *r.pick(MODELLED);
            let op = if r.chance(25) {
                UserOp::Clear
            } else {
                UserOp::Set(rand_write(&mut r, g))
            };
            Ev::User { x, g, seq, op }
        } else if roll < 62 && !wl.case.patches.is_empty() {
            let pid = r.pick(&wl.case.patches).pid.clone();
            Ev::Patch {
                pid,
                seq,
                inst: r.chance(70),
            }
        } else if roll < 66 {
            let id = if r.chance(15) {
                None
            } else {
                Some(format!("id{}", r.below(ids)))
            };
            Ev::Attr { x, seq, id }
        } else if roll < 69 {
            Ev::Vis {
                x,
                rev: run.vis[x] + 1,
            }
        } else if roll < 71 {
            Ev::Tick(run.clock + *r.pick(&[1_000u64, 5_000, 30_000, 120_000]))
        } else if roll < 72 {
            Ev::Epoch {
                src: r.pick(&["bus", "ocr", "icon", "ext.a"]),
                n: seq,
                state: r.pick(&["Active", "Throttled", "Detached", "Paused", "Revoked"]),
            }
        } else if with_facts {
            Ev::Fact(Box::new(rand_fact(&mut r, &run, &wl.case.profile, n, seq)))
        } else {
            Ev::Inval { src, x, seq }
        };
        emit(&mut wl, &mut run, ev);
    }
    wl
}

fn shuffled(r: &mut Rng, n: usize, dups: bool) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).collect();
    for i in (1..v.len()).rev() {
        v.swap(i, r.below(i + 1));
    }
    if dups {
        for _ in 0..n / 10 {
            let k = v[r.below(v.len())];
            let at = r.below(v.len() + 1);
            v.insert(at, k);
        }
    }
    v
}

fn trials() -> u64 {
    std::env::var("GORCA_FUSE_TRIALS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120)
}

fn size(seed: u64, m: u64) -> usize {
    3 + usize::try_from(seed % m).unwrap()
}

// ---- F1 ---------------------------------------------------------------------------------------

#[test]
fn f1_order_independence_registers_side_table_entries_and_trails() {
    for seed in 0..trials() {
        let wl = workload(seed, size(seed, 38), true);
        let w0 = wl.world();
        let o0 = eval_traced(&wl, &w0);
        let t0 = trails_json(&o0);
        let mut r = Rng::new(seed ^ 0xF1);
        let rev: Vec<usize> = wl.canon().iter().rev().copied().collect();
        for (label, order) in [
            ("reversed", rev),
            ("shuffled", shuffled(&mut r, wl.events.len(), false)),
            ("shuffled+dups", shuffled(&mut r, wl.events.len(), true)),
        ] {
            let w = wl.run(&order);
            assert_eq!(w.nodes, w0.nodes, "seed {seed} {label}: registers differ");
            assert_eq!(w.patch, w0.patch, "seed {seed} {label}: patch registers");
            assert_eq!(
                w.side.dump(),
                w0.side.dump(),
                "seed {seed} {label}: side table"
            );
            let o = eval_traced(&wl, &w);
            assert_eq!(o.entries(), o0.entries(), "seed {seed} {label}: entries");
            assert_eq!(trails_json(&o), t0, "seed {seed} {label}: explain trails");
        }
    }
}

#[test]
fn f1_large_tree_400_nodes() {
    for seed in 1000..1004 {
        let wl = workload(seed, 400, true);
        let o0 = eval(&wl, &wl.world());
        let mut r = Rng::new(seed);
        let w = wl.run(&shuffled(&mut r, wl.events.len(), true));
        assert_eq!(eval(&wl, &w).entries(), o0.entries(), "seed {seed}");
    }
}

// ---- F2 ---------------------------------------------------------------------------------------

#[test]
fn f2_replaying_any_suffix_changes_nothing() {
    for seed in 0..trials() {
        let wl = workload(seed + 5000, size(seed, 30), true);
        let mut w = wl.world();
        let f0 = eval(&wl, &w).entries();
        let (before, side) = (w.nodes.clone(), w.side.dump());
        let start = Rng::new(seed).below(wl.events.len());
        for ev in &wl.events[start..] {
            assert!(
                !apply(&mut w, ev),
                "seed {seed}: replay of {ev:?} changed a register"
            );
        }
        assert_eq!(w.nodes, before);
        assert_eq!(w.side.dump(), side);
        assert_eq!(eval(&wl, &w).entries(), f0, "seed {seed}");
    }
}

// ---- F3 ---------------------------------------------------------------------------------------

fn prov_matches_layer(layer: Layer, prov: ProvCode, masked: bool) -> bool {
    if masked {
        return prov == ProvCode::Default;
    }
    match layer {
        Layer::Observed => prov == ProvCode::ObservedPrimary,
        Layer::Secondary => prov == ProvCode::ObservedSecondary,
        Layer::Patch => prov == ProvCode::Patch,
        Layer::User => prov == ProvCode::User,
        Layer::Derived => matches!(prov, ProvCode::Derived | ProvCode::DerivedTainted),
        Layer::Inferred => prov == ProvCode::Inferred,
        Layer::Default => prov == ProvCode::Default,
    }
}

/// The finalised toy value of a slot (Absent -> the group default), for comparisons.
fn toy_value(s: &Slot, g: Group) -> Value {
    normalise_expected(
        g,
        &match s {
            Slot::Absent => json!("-"),
            Slot::Unknown => panic!("unknown"),
            Slot::Val(_) => slot_json(g, s),
        },
    )
}

#[test]
fn f3_provenance_is_complete_and_sound() {
    let mut secondary = 0usize;
    let mut inferred = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 9000, size(seed, 38), true);
        let w = wl.world();
        let out = eval(&wl, &w);
        for (x, o) in out.nodes.iter().enumerate() {
            let Some(o) = o else { continue };
            let (f, e) = (&o.fused, &o.fused.entry);
            for g in Group::ALL.iter().copied() {
                let prov = e
                    .prov
                    .get(g)
                    .unwrap_or_else(|| panic!("seed {seed} {x} {g:?}: spare prov code"));
                let masked = f.masked.has(g);
                assert!(
                    prov_matches_layer(f.layer_of(g), prov, masked),
                    "seed {seed} node {x} {g:?}: {prov:?} vs {:?}",
                    f.layer_of(g)
                );
                let tainted = matches!(
                    prov,
                    ProvCode::ObservedSecondary | ProvCode::Inferred | ProvCode::DerivedTainted
                );
                if tainted {
                    assert!(e.taint.get(g) != 0, "seed {seed} node {x} {g:?}: no tier");
                    // N-2: a tainted/non-primary winner is always hedged.
                    assert!(e.hedge.get(g) != 0, "seed {seed} node {x} {g:?}: Asserted");
                } else {
                    assert_eq!(e.hedge.get(g), 0, "seed {seed} node {x} {g:?}");
                    assert_eq!(e.taint.get(g), 0, "seed {seed} node {x} {g:?}");
                }
                if prov != ProvCode::Default {
                    assert!(e.known.has(g), "seed {seed} node {x} {g:?}");
                }
                match prov {
                    ProvCode::User => {
                        let (_, wr) = w.nodes[x].user_value(g).unwrap();
                        assert_eq!(toy_value(&wr.slot(), g), toy_from_entry(e, g));
                    }
                    ProvCode::ObservedPrimary => {
                        let first = wl
                            .case
                            .profile
                            .pi(g)
                            .iter()
                            .find_map(|p| w.nodes[x].src(*p).get(g))
                            .unwrap();
                        assert_eq!(toy_value(&first.write.slot(), g), toy_from_entry(e, g));
                    }
                    ProvCode::ObservedSecondary => {
                        secondary += 1;
                        // Soundness: a Secondary wins only a statically inexpressible group
                        // that no primary reported.
                        assert!(
                            wl.case.profile.inexpressible.has(g),
                            "seed {seed} node {x} {g:?}"
                        );
                        assert!(
                            wl.case
                                .profile
                                .pi(g)
                                .iter()
                                .all(|p| w.nodes[x].src(*p).get(g).is_none()),
                            "seed {seed} node {x} {g:?}: primary known"
                        );
                    }
                    ProvCode::Inferred => inferred += 1,
                    _ => {}
                }
            }
        }
    }
    assert!(
        secondary > 0 && inferred > 0,
        "weak coverage {secondary} {inferred}"
    );
}

// ---- F4, F5, F6 ---------------------------------------------------------------------------------

fn ancestors(parent: &[i64], mut x: usize) -> Vec<usize> {
    let mut out = Vec::new();
    while parent[x] >= 0 {
        x = usize::try_from(parent[x]).unwrap();
        out.push(x);
    }
    out
}

/// The comparable state of one entry with `skip` groups removed.
fn project(e: &SemEntry, skip: GroupMask) -> Vec<String> {
    MODELLED
        .iter()
        .copied()
        .filter(|g| !skip.has(*g))
        .map(|g| {
            format!(
                "{g:?}:{}:{:?}:{}:{}:{}:{}:{}",
                toy_from_entry(e, g),
                e.prov.get(g),
                e.hedge.get(g),
                e.taint.get(g),
                e.known.has(g),
                e.stale.has(g),
                e.fallback.has(g)
            )
        })
        .collect()
}

#[test]
fn f4_f5_f6_override_dominance_locality_reversibility() {
    let mut dominated = 0usize;
    let mut masked_cases = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 20_000, size(seed, 30), true);
        let n = wl.case.parent.len();
        let mut w = wl.world_where(|e| !matches!(e, Ev::User { .. }));
        let base = eval(&wl, &w);
        let mut r = Rng::new(seed);
        let x = r.below(n);
        if base.nodes[x].is_none() {
            continue;
        }
        let g = *r.pick(MODELLED);
        let wr = rand_write(&mut r, g);
        let seq = 1_000_000;
        w.nodes[x].apply_user(g, seq, UserOp::Set(wr.clone()));
        let after = eval(&wl, &w);

        // F4: the effective value is the override, or repair masked it and the entry says so.
        let f = after.nodes[x].as_ref().unwrap();
        if f.fused.masked.has(g) {
            masked_cases += 1;
            assert_eq!(
                f.fused.entry.prov.get(g),
                Some(ProvCode::Default),
                "seed {seed}"
            );
        } else {
            dominated += 1;
            assert_eq!(
                toy_from_entry(&f.fused.entry, g),
                toy_value(&wr.slot(), g),
                "seed {seed}: override lost"
            );
            assert_eq!(
                f.fused.entry.prov.get(g),
                Some(ProvCode::User),
                "seed {seed}"
            );
        }

        // F5: only D(x, g) changes. Role changes repair every group of x; name or role changes
        // may change the derived names of the ancestors.
        let anc = ancestors(&wl.case.parent, x);
        let own_skip = if g == Group::Role {
            GroupMask(u32::MAX)
        } else {
            GroupMask::EMPTY.with(g)
        };
        for y in 0..n {
            let (Some(b), Some(a)) = (&base.nodes[y], &after.nodes[y]) else {
                assert_eq!(
                    base.nodes[y].is_some(),
                    after.nodes[y].is_some(),
                    "seed {seed}"
                );
                continue;
            };
            let skip = if y == x {
                own_skip
            } else if anc.contains(&y) && matches!(g, Group::Name | Group::Role) {
                GroupMask::EMPTY.with(Group::Name)
            } else {
                GroupMask::EMPTY
            };
            assert_eq!(
                project(&b.fused.entry, skip),
                project(&a.fused.entry, skip),
                "seed {seed}: override on ({x},{g:?}) leaked into node {y}"
            );
        }

        // F6: an override followed by Clear restores the previous state exactly.
        w.nodes[x].apply_user(g, seq + 1, UserOp::Clear);
        assert_eq!(
            eval(&wl, &w).entries(),
            base.entries(),
            "seed {seed}: Clear did not restore ({x},{g:?})"
        );
    }
    assert!(
        dominated > 20 && masked_cases > 0,
        "{dominated} {masked_cases}"
    );
}

// ---- F9 ---------------------------------------------------------------------------------------

#[test]
fn f9_flags_follow_the_winner_and_priority_beats_freshness() {
    let (mut checked, mut stale_seen, mut fb_seen) = (0usize, 0usize, 0usize);
    for seed in 0..trials() {
        let wl = workload(seed + 40_000, size(seed, 30), true);
        let w = wl.world();
        let out = eval(&wl, &w);
        for (x, o) in out.nodes.iter().enumerate() {
            let Some(o) = o else { continue };
            let (f, e) = (&o.fused, &o.fused.entry);
            for g in Group::ALL.iter().copied() {
                let flagged = e.stale.has(g) || e.fallback.has(g);
                if f.layer_of(g) != Layer::Observed || f.masked.has(g) {
                    assert!(
                        !flagged,
                        "seed {seed} node {x} {g:?}: flags on a non-observed winner"
                    );
                    continue;
                }
                // The winner is the first primary in PI order with a record, whatever the seqs.
                let order = wl.case.profile.pi(g);
                let Some((pos, p)) = order
                    .iter()
                    .enumerate()
                    .find(|(_, p)| w.nodes[x].src(**p).get(g).is_some())
                    .map(|(i, p)| (i, *p))
                else {
                    panic!("seed {seed} node {x} {g:?}: observed without a record")
                };
                let want_stale = w.nodes[x].src(p).is_stale(g);
                assert_eq!(e.stale.has(g), want_stale, "seed {seed} node {x} {g:?}");
                assert_eq!(e.fallback.has(g), pos > 0, "seed {seed} node {x} {g:?}");
                checked += 1;
                stale_seen += usize::from(want_stale);
                fb_seen += usize::from(pos > 0);
            }
        }
    }
    assert!(
        checked > 500 && stale_seen > 0 && fb_seen > 0,
        "{checked} {stale_seen} {fb_seen}"
    );
}

// ---- F10 --------------------------------------------------------------------------------------

#[test]
fn f10_invariants_hold_on_every_published_entry() {
    let mut nodes = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 60_000, size(seed, 38), true);
        let out = eval(&wl, &wl.world());
        for (x, o) in out.nodes.iter().enumerate() {
            let Some(o) = o else { continue };
            nodes += 1;
            let bad = invariant_violations(&wl.case.profile.rules, &o.fused.entry);
            assert!(bad.is_empty(), "seed {seed} node {x}: {bad:?}");
            for g in Group::ALL
                .iter()
                .copied()
                .filter(|g| o.fused.masked.has(*g))
            {
                assert_eq!(o.fused.entry.prov.get(g), Some(ProvCode::Default));
            }
        }
    }
    assert!(nodes > 1000);
}

// ---- F11 floor, F12 protection, F13 vis_rev, F15 ambiguity, F18 rebinding ----------------------

fn fact_events(wl: &Workload) -> Vec<&FactIn> {
    wl.events
        .iter()
        .filter_map(|e| match e {
            Ev::Fact(f) => Some(&**f),
            _ => None,
        })
        .collect()
}

/// The cell a fact can influence: (node, group) when its anchor binds, `None` otherwise.
fn fact_cell(wl: &Workload, w: &World, f: &FactIn) -> Option<(usize, Group)> {
    let res = ToyIndex::new(&wl.case.profile, w).resolve(&f.anchor);
    let x = res.node?.0 as usize;
    let g = match f.group.classify() {
        gorca_model::sem::GroupClass::Semantic(g) => g,
        gorca_model::sem::GroupClass::Evidence(
            EvidenceKind::HintTitleAttr | EvidenceKind::VisualOcrText,
        ) => Group::Name,
        _ => return None,
    };
    Some((x, g))
}

/// Entries of `a` and `b` agree everywhere except (optionally) the cell `(x, g)` and, when `g` is
/// the name, the ancestors' derived names.
fn assert_local(wl: &Workload, a: &TreeOut, b: &TreeOut, cell: Option<(usize, Group)>, ctx: &str) {
    let n = wl.case.parent.len();
    for y in 0..n {
        let (Some(pa), Some(pb)) = (&a.nodes[y], &b.nodes[y]) else {
            assert_eq!(a.nodes[y].is_some(), b.nodes[y].is_some(), "{ctx}");
            continue;
        };
        let skip = match cell {
            Some((x, g)) if y == x => GroupMask::EMPTY.with(g),
            Some((x, Group::Name)) if ancestors(&wl.case.parent, x).contains(&y) => {
                GroupMask::EMPTY.with(Group::Name)
            }
            _ => GroupMask::EMPTY,
        };
        assert_eq!(
            project(&pa.fused.entry, skip),
            project(&pb.fused.entry, skip),
            "{ctx}: node {y}"
        );
    }
}

#[test]
fn f11_a_fact_below_its_floor_never_wins_and_changes_only_its_own_cell() {
    let mut tried = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 70_000, size(seed, 30), true);
        let facts = fact_events(&wl);
        if facts.is_empty() {
            continue;
        }
        let mut r = Rng::new(seed);
        let f = (*r.pick(&facts)).clone();
        let base = wl.world();
        let cell = fact_cell(&wl, &base, &f);
        let mut lowered = f.clone();
        lowered.seq = u64::MAX / 2;
        lowered.p_milli = 0;
        lowered.payload = f.payload.clone();
        let mut retracted = lowered.clone();
        retracted.payload = Payload::Retract;
        let (mut w1, mut w2) = (wl.world(), wl.world());
        w1.side.insert(lowered).unwrap();
        w2.side.insert(retracted).unwrap();
        let (o0, o1, o2) = (eval(&wl, &base), eval(&wl, &w1), eval(&wl, &w2));
        assert_eq!(
            o1.entries(),
            o2.entries(),
            "seed {seed}: below-floor differs from retracted"
        );
        assert_local(&wl, &o0, &o1, cell, &format!("seed {seed} F11"));
        tried += 1;
    }
    assert!(tried > 50);
}

fn is_protected(g: Group, e: &SemEntry) -> bool {
    match e.prov.get(g) {
        Some(ProvCode::User | ProvCode::Patch | ProvCode::Derived) => true,
        // An empty primary name or description is fillable (A6), not protected.
        Some(ProvCode::ObservedPrimary) => match g {
            Group::Name => e.name.as_deref().is_some_and(|n| !n.is_empty()),
            Group::Description => e.sem.description().is_some_and(|d| !d.is_empty()),
            _ => true,
        },
        _ => false,
    }
}

#[test]
fn f12_non_primary_facts_never_change_a_protected_group() {
    let (mut protected, mut changed_unprotected) = (0usize, 0usize);
    for seed in 0..trials() {
        let wl = workload(seed + 80_000, size(seed, 38), true);
        let with = eval(&wl, &wl.world());
        let without = eval(&wl, &wl.world_where(|e| !matches!(e, Ev::Fact(_))));
        for (x, (a, b)) in without.nodes.iter().zip(&with.nodes).enumerate() {
            let (Some(a), Some(b)) = (a, b) else { continue };
            for g in Group::ALL.iter().copied() {
                let (ea, eb) = (&a.fused.entry, &b.fused.entry);
                if is_protected(g, ea) {
                    protected += 1;
                    if MODELLED.contains(&g) {
                        let keep = GroupMask(u32::MAX).without(g);
                        assert_eq!(
                            project(ea, keep),
                            project(eb, keep),
                            "seed {seed} node {x} {g:?}: protected group changed"
                        );
                        let only = |e: &SemEntry| {
                            (
                                toy_from_entry(e, g),
                                e.prov.get(g),
                                e.hedge.get(g),
                                e.taint.get(g),
                            )
                        };
                        assert_eq!(only(ea), only(eb), "seed {seed} node {x} {g:?}");
                    }
                } else if MODELLED.contains(&g) && toy_from_entry(ea, g) != toy_from_entry(eb, g) {
                    changed_unprotected += 1;
                }
                // No Secondary wins an expressible group.
                if eb.prov.get(g) == Some(ProvCode::ObservedSecondary) {
                    assert!(
                        wl.case.profile.inexpressible.has(g),
                        "seed {seed} node {x} {g:?}"
                    );
                }
            }
        }
    }
    assert!(
        protected > 500 && changed_unprotected > 0,
        "{protected} {changed_unprotected}"
    );
}

#[test]
fn f13_a_vis_bump_retires_inferred_facts_of_the_node() {
    let mut inferred_before = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 90_000, size(seed, 30), true);
        let mut w = wl.world();
        let before = eval(&wl, &w);
        for (x, o) in before.nodes.iter().enumerate() {
            let Some(o) = o else { continue };
            if Group::ALL
                .iter()
                .any(|g| o.fused.entry.prov.get(*g) == Some(ProvCode::Inferred))
            {
                inferred_before += 1;
                // Bump past every basis the writer could have stamped.
                apply(&mut w, &Ev::Vis { x, rev: 1_000_000 });
                let after = eval(&wl, &w);
                let a = after.nodes[x].as_ref().unwrap();
                for g in Group::ALL.iter().copied() {
                    assert_ne!(
                        a.fused.entry.prov.get(g),
                        Some(ProvCode::Inferred),
                        "seed {seed} node {x} {g:?}: a stale Inferred fact won"
                    );
                }
                break;
            }
        }
    }
    assert!(inferred_before > 10, "weak coverage {inferred_before}");
}

#[test]
fn f15_a_fact_bound_below_its_minimum_never_matters() {
    // Facts whose current binding is weaker than their grant's minimum (an ambiguous DomId is
    // Heuristic, never Unique) can be removed without changing any result.
    let mut weak_total = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 100_000, size(seed, 38), true);
        let w = wl.world();
        let index = ToyIndex::new(&wl.case.profile, &w);
        let weak = |f: &FactIn| {
            let q = index.resolve(&f.anchor).quality;
            !q.at_least(Grant::standard().min_bind(grant_class(f.group)))
                && q != BindQuality::Unbound
        };
        // An ambiguous DOM id is never `Unique`.
        for f in fact_events(&wl) {
            if let AnchorId::Dom { id, count, .. } = &f.anchor {
                let res = index.resolve(&f.anchor);
                let holders = w
                    .attr
                    .iter()
                    .filter(|(x, (_, i))| {
                        i.as_deref() == Some(&**id)
                            && index
                                .resolve(&AnchorId::Node(NodeIdx(u32::try_from(**x).unwrap())))
                                .node
                                .is_some()
                    })
                    .count();
                if *count != 1 || holders > 1 {
                    assert_ne!(res.quality, BindQuality::Unique, "seed {seed}: {id}");
                }
            }
        }
        let weak_facts: Vec<SideKeyLite> = fact_events(&wl)
            .into_iter()
            .filter(|f| weak(f))
            .map(SideKeyLite::of)
            .collect();
        weak_total += weak_facts.len();
        let pruned = wl.world_where(|e| match e {
            Ev::Fact(f) => !weak_facts.contains(&SideKeyLite::of(f)),
            _ => true,
        });
        assert_eq!(
            eval(&wl, &w).entries(),
            eval(&wl, &pruned).entries(),
            "seed {seed}"
        );
    }
    assert!(weak_total > 0, "no weakly bound fact generated");
}

#[derive(PartialEq, Eq)]
struct SideKeyLite(SourceId, AnchorId, gorca_model::sem::GroupCode);

impl SideKeyLite {
    fn of(f: &FactIn) -> SideKeyLite {
        SideKeyLite(f.source, f.anchor.clone(), f.group)
    }
}

#[test]
fn f18_after_a_rebind_no_old_generation_fact_wins() {
    let mut tried = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 110_000, size(seed, 30), true);
        let n = wl.case.parent.len();
        let mut r = Rng::new(seed);
        let x = r.below(n);
        let node_anchor = AnchorId::Node(NodeIdx(u32::try_from(x).unwrap()));
        let src = wl.case.profile.pi(Group::Role)[0];
        let bump = Ev::Life {
            src,
            x,
            seq: 10_000_000,
            alive: true,
        };
        let mut w = wl.world();
        apply(&mut w, &bump);
        let mut pruned = wl.world_where(|e| !matches!(e, Ev::Fact(f) if f.anchor == node_anchor));
        apply(&mut pruned, &bump);
        assert_eq!(
            eval(&wl, &w).entries(),
            eval(&wl, &pruned).entries(),
            "seed {seed}: an old-generation fact on node {x} still mattered"
        );
        tried += 1;
    }
    assert!(tried > 50);
}

// ---- N-2, S4 ranking, revocation ------------------------------------------------------------------

#[test]
fn fuse_is_deterministic_and_pure() {
    let wl = workload(77, 40, true);
    let a = eval(&wl, &wl.world());
    let b = eval(&wl, &wl.world());
    assert_eq!(a.entries(), b.entries());
}

#[test]
fn revoked_source_equals_absent_source() {
    // F14 in miniature: masking a source gives the run without its facts.
    let mut compared = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 120_000, size(seed, 30), true);
        let name = ["bus", "ocr", "icon", "ext.a"][usize::try_from(seed % 4).unwrap()];
        let slot = SOURCES.iter().find(|s| s.1 == name).map(|s| s.0).unwrap();
        let mut revoked = wl.world();
        apply(
            &mut revoked,
            &Ev::Epoch {
                src: name,
                n: u64::MAX / 2,
                state: "Revoked",
            },
        );
        let absent = wl.world_where(|e| !matches!(e, Ev::Fact(f) if f.source.slot == slot));
        assert_eq!(
            eval(&wl, &revoked).entries(),
            eval(&wl, &absent).entries(),
            "seed {seed} {name}"
        );
        compared += 1;
    }
    assert!(compared > 50);
}

fn two_inferred_sources(slot_a: u32, slot_b: u32) -> (Workload, World) {
    // Two Inferred sources with equal tier, kind order and class; only their stable names differ.
    let tables = json!({"sources": {
        "atspi": [0, 0, "Primary", 1, null],
        "newton": [1, 0, "Primary", 1, null],
        "alpha": [slot_a, 0, "Inferred", 2, 0],
        "omega": [slot_b, 0, "Inferred", 2, 0],
    }});
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.default_role = SemRole::Generic;
    let mut case = Case::synthetic("s4".into(), profile, vec![-1], Vec::new());
    case.tables = tables;
    let mut wl = Workload {
        case,
        events: Vec::new(),
    };
    wl.events.push(Ev::Life {
        src: PrimarySlot::P0,
        x: 0,
        seq: 1,
        alive: true,
    });
    for (slot, text) in [(slot_a, "FromAlpha"), (slot_b, "FromOmega")] {
        wl.events.push(Ev::Fact(Box::new(FactIn {
            source: SourceId {
                slot,
                generation: 0,
            },
            anchor: AnchorId::Node(NodeIdx(0)),
            group: Group::Name.group_code(),
            seq: 1,
            payload: Payload::Sem(Write::Val(gorca_fuse::Val::Name(Arc::from(text)))),
            p_milli: 990,
            basis: Basis::VisRev(1),
            bind_gen: BindGen(1),
            admitted_at_ms: 0,
            ttl_ms: Some(300_000),
        })));
    }
    let w = wl.world();
    (wl, w)
}

#[test]
fn s4_ties_are_broken_by_stable_name_never_by_source_id() {
    // The same two sources with their slots swapped (the allocation order differs) must rank the
    // same way.
    for (a, b) in [(7, 9), (9, 7), (1000, 2)] {
        let (wl, w) = two_inferred_sources(a, b);
        let out = eval(&wl, &w);
        let e = &out.nodes[0].as_ref().unwrap().fused.entry;
        assert_eq!(e.name.as_deref(), Some("FromAlpha"), "slots ({a}, {b})");
        assert_eq!(e.prov.get(Group::Name), Some(ProvCode::Inferred));
        assert!(e.hedge.get(Group::Name) != 0);
    }
}

#[test]
fn a_t3_fact_ranks_below_a_t2_fact_of_the_same_raw_class() {
    // ext.a (T3) at p = 990 is capped to Probable at admission and ranks below ocr (T2, High).
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.default_role = SemRole::Generic;
    let mut case = Case::synthetic("t3".into(), profile, vec![-1], Vec::new());
    case.tables = synthetic_tables();
    let mut wl = Workload {
        case,
        events: Vec::new(),
    };
    wl.events.push(Ev::Life {
        src: PrimarySlot::P0,
        x: 0,
        seq: 1,
        alive: true,
    });
    let mk = |slot: u32, text: &str| FactIn {
        source: SourceId {
            slot,
            generation: 0,
        },
        anchor: AnchorId::Node(NodeIdx(0)),
        group: Group::Name.group_code(),
        seq: 1,
        payload: Payload::Sem(Write::Val(gorca_fuse::Val::Name(Arc::from(text)))),
        p_milli: 990,
        basis: Basis::VisRev(1),
        bind_gen: BindGen(1),
        admitted_at_ms: 0,
        ttl_ms: Some(300_000),
    };
    wl.events.push(Ev::Fact(Box::new(mk(5, "ExtT3"))));
    wl.events.push(Ev::Fact(Box::new(mk(3, "OcrT2"))));
    let out = eval(&wl, &wl.world());
    let e = &out.nodes[0].as_ref().unwrap().fused.entry;
    assert_eq!(e.name.as_deref(), Some("OcrT2"));
    assert_eq!(e.hedge.get(Group::Name), ConfClass::High.code());
    let _ = toy_str(&Slot::Unknown);
}

#[test]
fn an_inferred_fact_on_another_group_is_reported_not_applicable() {
    // S4 fills only name and description; the trail must say so instead of omitting the fact.
    let (mut wl, _) = two_inferred_sources(7, 9);
    wl.events.clear();
    wl.events.push(Ev::Life {
        src: PrimarySlot::P0,
        x: 0,
        seq: 1,
        alive: true,
    });
    wl.events.push(Ev::Fact(Box::new(FactIn {
        source: SourceId {
            slot: 7,
            generation: 0,
        },
        anchor: AnchorId::Node(NodeIdx(0)),
        group: Group::Check.group_code(),
        seq: 1,
        payload: Payload::Sem(write_from_toy(Group::Check, &json!("on"))),
        p_milli: 990,
        basis: Basis::VisRev(1),
        bind_gen: BindGen(1),
        admitted_at_ms: 0,
        ttl_ms: Some(300_000),
    })));
    let out = eval_traced(&wl, &wl.world());
    let ex = out.nodes[0].as_ref().unwrap().explain.as_ref().unwrap();
    let trail = ex.trail(Group::Check);
    assert!(trail.iter().any(|t| matches!(
        t,
        gorca_fuse::TrailEntry::Fact(f) if f.status == gorca_fuse::TrailStatus::NotApplicable
    )));
}

#[test]
fn the_no_quirk_fast_path_equals_the_generic_path() {
    // `NoQuirks` (NOOP) reads the registers directly; `NoQ` goes through per-source records.
    // Both must publish the same entries for every workload.
    let mut nodes = 0usize;
    for seed in 0..trials() {
        let wl = workload(seed + 130_000, size(seed, 38), true);
        let w = wl.world();
        let fast = evaluate_full(&wl.case, &w, &gorca_fuse::NoQuirks, false);
        let slow = evaluate_full(&wl.case, &w, &NoQ, false);
        assert_eq!(fast.entries(), slow.entries(), "seed {seed}");
        nodes += fast.nodes.iter().flatten().count();
    }
    assert!(nodes > 1000);
}
