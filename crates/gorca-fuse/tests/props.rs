// SPDX-License-Identifier: GPL-3.0-or-later
//! Property tests F1-F6, F9 and F10 of `docs/reports/semantic-fusion-math.md` section 5 over seeded
//! random workloads (trees of up to 400 nodes, two primaries, users, patches, quirk-free S0).
//!
//! `proptest` is not vendored in the project-local cargo home, so the generator is a small
//! deterministic xorshift PRNG; every failure message carries the seed and trial. F7 (incremental
//! equals from-scratch) is B2; F6's second half (provenance-only transitions are silent) needs the
//! utterance model of M22b.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::collections::BTreeMap;

use gorca_fuse::{
    FactCand, Fused, Layer, NodeRegs, PrimarySlot, Profile, Slot, UserOp, Write,
    invariant_violations,
};
use gorca_model::sem::{ConfClass, Group, GroupMask, ProvCode, SemEntry, TrustTier};
use serde_json::{Value, json};
use support::{
    Case, OwnedCands, ToyPatch, World, evaluate_with, slot_from_toy, toy_from_entry, toy_rules,
    toy_str, write_from_toy,
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
    }
}

/// Quirk-free evaluation: quirks are covered by the oracle test; here `FUSE` is checked against
/// the raw registers.
fn evaluate(case: &Case, w: &World, cands: &[OwnedCands]) -> Vec<Option<Fused>> {
    evaluate_with(case, w, cands, &gorca_fuse::NoQuirks)
}

fn empty_world(n: usize) -> World {
    World {
        nodes: (0..n).map(|_| NodeRegs::default()).collect(),
        patch: BTreeMap::new(),
    }
}

struct Workload {
    case: Case,
    cands: Vec<OwnedCands>,
    events: Vec<Ev>,
    next_seq: u64,
}

fn rand_cands(r: &mut Rng) -> OwnedCands {
    let mk = |r: &mut Rng, g: Group, key: u32| -> FactCand {
        let mut w = rand_write(r, g);
        if let Write::Absent(_) = w {
            w = write_from_toy(g, &toy_domain(g)[0]);
        }
        FactCand {
            write: w,
            conf: *r.pick(&[ConfClass::High, ConfClass::Probable, ConfClass::Possible]),
            tier: *r.pick(&[
                TrustTier::T2FirstParty,
                TrustTier::T3Untrusted,
                TrustTier::T3Untrusted,
            ]),
            src_key: key,
            seq: r.next() % 50,
        }
    };
    let mut c = OwnedCands::default();
    for _ in 0..r.below(3) {
        let g = *r.pick(&[
            Group::Live,
            Group::Orientation,
            Group::Description,
            Group::Check,
        ]);
        c.secondary.push(mk(r, g, 2));
    }
    for _ in 0..r.below(3) {
        let key = u32::try_from(r.below(3)).unwrap();
        c.title.push(mk(r, Group::Name, key));
    }
    for _ in 0..r.below(3) {
        let g = *r.pick(&[Group::Name, Group::Description]);
        let key = u32::try_from(r.below(3)).unwrap();
        c.inferred.push(mk(r, g, key));
    }
    c
}

fn workload(seed: u64, n: usize, with_cands: bool) -> Workload {
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
    profile.default_role = gorca_model::sem::SemRole::Generic;
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
    let cands = (0..n)
        .map(|_| {
            if with_cands {
                rand_cands(&mut r)
            } else {
                OwnedCands::default()
            }
        })
        .collect();
    let mut wl = Workload {
        case: Case {
            id: format!("seed{seed}"),
            layout: "x32".into(),
            cap: 32,
            profile,
            parent,
            patches,
            events: Vec::new(),
            deliveries: Vec::new(),
            expected: Vec::new(),
            tables: Value::Null,
        },
        cands,
        events: Vec::new(),
        next_seq: 1,
    };
    // Lifecycle first so most nodes are alive, then a mix of observations and overlays.
    for x in 0..n {
        let seq = wl.next_seq();
        wl.events.push(Ev::Life {
            src: PrimarySlot::P0,
            x,
            seq,
            alive: true,
        });
        if r.chance(60) {
            let seq = wl.next_seq();
            wl.events.push(Ev::Life {
                src: PrimarySlot::P1,
                x,
                seq,
                alive: true,
            });
        }
    }
    for _ in 0..n * 6 {
        let x = r.below(n);
        let seq = wl.next_seq();
        let src = if r.chance(50) {
            PrimarySlot::P0
        } else {
            PrimarySlot::P1
        };
        let roll = r.below(100);
        let ev = if roll < 62 {
            let g = *r.pick(MODELLED);
            Ev::Obs {
                src,
                x,
                seq,
                w: rand_write(&mut r, g),
            }
        } else if roll < 68 {
            Ev::Inval { src, x, seq }
        } else if roll < 74 {
            Ev::Life {
                src,
                x,
                seq,
                alive: r.chance(80),
            }
        } else if roll < 90 {
            let g = *r.pick(MODELLED);
            let op = if r.chance(25) {
                UserOp::Clear
            } else {
                UserOp::Set(rand_write(&mut r, g))
            };
            Ev::User { x, g, seq, op }
        } else if !wl.case.patches.is_empty() {
            let pid = r.pick(&wl.case.patches).pid.clone();
            Ev::Patch {
                pid,
                seq,
                inst: r.chance(70),
            }
        } else {
            Ev::Inval { src, x, seq }
        };
        wl.events.push(ev);
    }
    wl
}

impl Workload {
    fn next_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }
}

fn run_all(wl: &Workload, order: &[usize]) -> (World, Vec<Option<Fused>>) {
    let mut w = empty_world(wl.case.parent.len());
    for &i in order {
        apply(&mut w, &wl.events[i]);
    }
    let finals = evaluate(&wl.case, &w, &wl.cands);
    (w, finals)
}

fn entries(f: &[Option<Fused>]) -> Vec<Option<SemEntry>> {
    f.iter()
        .map(|o| o.as_ref().map(|f| f.entry.clone()))
        .collect()
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

// ---- F1 ---------------------------------------------------------------------------------------

#[test]
fn f1_order_independence_registers_and_entries() {
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 38).unwrap();
        let wl = workload(seed, n, true);
        let canon: Vec<usize> = (0..wl.events.len()).collect();
        let (w0, f0) = run_all(&wl, &canon);
        let mut r = Rng::new(seed ^ 0xF1);
        let rev: Vec<usize> = canon.iter().rev().copied().collect();
        for (label, order) in [
            ("reversed", rev),
            ("shuffled", shuffled(&mut r, canon.len(), false)),
            ("shuffled+dups", shuffled(&mut r, canon.len(), true)),
        ] {
            let (w, f) = run_all(&wl, &order);
            assert_eq!(w.nodes, w0.nodes, "seed {seed} {label}: registers differ");
            assert_eq!(
                w.patch, w0.patch,
                "seed {seed} {label}: patch registers differ"
            );
            assert_eq!(
                entries(&f),
                entries(&f0),
                "seed {seed} {label}: entries differ"
            );
            let layers = |f: &[Option<Fused>]| -> Vec<Option<Vec<Layer>>> {
                f.iter()
                    .map(|o| o.as_ref().map(|f| f.layer.to_vec()))
                    .collect()
            };
            assert_eq!(
                layers(&f),
                layers(&f0),
                "seed {seed} {label}: layers differ"
            );
        }
    }
}

#[test]
fn f1_large_tree_400_nodes() {
    for seed in 1000..1004 {
        let wl = workload(seed, 400, true);
        let canon: Vec<usize> = (0..wl.events.len()).collect();
        let (_, f0) = run_all(&wl, &canon);
        let mut r = Rng::new(seed);
        let (_, f1) = run_all(&wl, &shuffled(&mut r, canon.len(), true));
        assert_eq!(entries(&f1), entries(&f0), "seed {seed}");
    }
}

// ---- F2 ---------------------------------------------------------------------------------------

#[test]
fn f2_replaying_any_suffix_changes_nothing() {
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 30).unwrap();
        let wl = workload(seed + 5000, n, true);
        let canon: Vec<usize> = (0..wl.events.len()).collect();
        let (mut w, f0) = run_all(&wl, &canon);
        let before = w.nodes.clone();
        let mut r = Rng::new(seed);
        let start = r.below(canon.len());
        for ev in &wl.events[start..] {
            let was = w.nodes.clone();
            let changed = apply(&mut w, ev);
            let now = w.nodes.clone();
            assert!(
                !changed,
                "seed {seed}: replay of {ev:?} changed a register (eq: {})",
                was == now
            );
        }
        assert_eq!(w.nodes, before);
        let f1 = evaluate(&wl.case, &w, &wl.cands);
        assert_eq!(entries(&f1), entries(&f0), "seed {seed}");
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

#[test]
fn f3_provenance_is_complete_and_sound() {
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 38).unwrap();
        let wl = workload(seed + 9000, n, true);
        let canon: Vec<usize> = (0..wl.events.len()).collect();
        let (w, finals) = run_all(&wl, &canon);
        for (x, f) in finals.iter().enumerate() {
            let Some(f) = f else { continue };
            let e = &f.entry;
            for g in Group::ALL.iter().copied() {
                let prov = e
                    .prov
                    .get(g)
                    .unwrap_or_else(|| panic!("seed {seed} {x} {g:?}: spare prov code"));
                let masked = f.masked.has(g);
                assert!(
                    prov_matches_layer(f.layer_of(g), prov, masked),
                    "seed {seed} node {x} {g:?}: {prov:?} vs {:?} masked={masked}",
                    f.layer_of(g)
                );
                // Taint and hedge are zero exactly for the untainted provenances.
                let tainted = matches!(
                    prov,
                    ProvCode::ObservedSecondary | ProvCode::Inferred | ProvCode::DerivedTainted
                );
                if tainted {
                    assert!(
                        e.taint.get(g) != 0,
                        "seed {seed} node {x} {g:?}: tainted without a tier"
                    );
                    assert!(
                        e.hedge.get(g) != 0 || prov == ProvCode::Inferred,
                        "seed {seed} node {x} {g:?}"
                    );
                } else {
                    assert_eq!(e.hedge.get(g), 0, "seed {seed} node {x} {g:?}");
                    assert_eq!(e.taint.get(g), 0, "seed {seed} node {x} {g:?}");
                }
                // A non-default winner means the group is known.
                if prov != ProvCode::Default {
                    assert!(e.known.has(g), "seed {seed} node {x} {g:?}");
                }
                // Soundness against the registers: a User winner has a live user register with
                // that value; an Observed winner is the first primary in PI with a record.
                match prov {
                    ProvCode::User => {
                        let (_, wr) = w.nodes[x].user_value(g).unwrap_or_else(|| {
                            panic!("seed {seed} node {x} {g:?}: User without register")
                        });
                        assert_eq!(
                            toy_value(&wr.slot(), g),
                            toy_from_entry(e, g),
                            "seed {seed} node {x} {g:?}"
                        );
                    }
                    ProvCode::ObservedPrimary => {
                        let first = wl
                            .case
                            .profile
                            .pi(g)
                            .iter()
                            .find_map(|p| w.nodes[x].src(*p).get(g));
                        let r = first.unwrap_or_else(|| {
                            panic!("seed {seed} node {x} {g:?}: observed without register")
                        });
                        assert_eq!(
                            toy_value(&r.write.slot(), g),
                            toy_from_entry(e, g),
                            "seed {seed} node {x} {g:?}"
                        );
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The finalised toy value of a slot (Absent -> the group default), for comparisons.
fn toy_value(s: &Slot, g: Group) -> Value {
    support::normalise_expected(
        g,
        &match s {
            Slot::Absent => json!("-"),
            Slot::Unknown => panic!("unknown"),
            Slot::Val(_) => slot_to_json(s, g),
        },
    )
}

fn slot_to_json(s: &Slot, g: Group) -> Value {
    let t = toy_str(s);
    if g == Group::Level {
        json!(t.parse::<u64>().unwrap())
    } else {
        json!(t)
    }
}

// ---- F4, F5, F6 -------------------------------------------------------------------------------

fn base_world(seed: u64, n: usize) -> (Workload, World, Vec<Option<Fused>>) {
    let mut wl = workload(seed, n, false);
    // Overlay-free base: only primary observations decide the starting point.
    wl.events.retain(|e| !matches!(e, Ev::User { .. }));
    let canon: Vec<usize> = (0..wl.events.len()).collect();
    let (w, f) = run_all(&wl, &canon);
    (wl, w, f)
}

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
    Group::ALL
        .iter()
        .filter(|g| !skip.has(**g))
        .copied()
        .filter(|g| MODELLED.contains(g))
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
        let n = 3 + usize::try_from(seed % 30).unwrap();
        let (wl, mut w, base) = base_world(seed + 20_000, n);
        let mut r = Rng::new(seed);
        let x = r.below(n);
        if base[x].is_none() {
            continue;
        }
        let g = *r.pick(MODELLED);
        let wr = rand_write(&mut r, g);
        let seq = wl.next_seq + 10;
        w.nodes[x].apply_user(g, seq, UserOp::Set(wr.clone()));
        let after = evaluate(&wl.case, &w, &wl.cands);

        // F4: the effective value is the override, or repair masked it and the entry says so.
        let f = after[x].as_ref().unwrap();
        let want = toy_value(&wr.slot(), g);
        if f.masked.has(g) {
            masked_cases += 1;
            assert_eq!(
                f.entry.prov.get(g),
                Some(ProvCode::Default),
                "seed {seed}: masked group not reset"
            );
        } else {
            dominated += 1;
            assert_eq!(
                toy_from_entry(&f.entry, g),
                want,
                "seed {seed}: override lost"
            );
            assert_eq!(f.entry.prov.get(g), Some(ProvCode::User), "seed {seed}");
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
            let (Some(b), Some(a)) = (&base[y], &after[y]) else {
                assert_eq!(
                    base[y].is_some(),
                    after[y].is_some(),
                    "seed {seed}: liveness changed at {y}"
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
                project(&b.entry, skip),
                project(&a.entry, skip),
                "seed {seed}: override on ({x},{g:?}) leaked into node {y}"
            );
        }

        // F6: an override followed by Clear restores the previous state exactly.
        w.nodes[x].apply_user(g, seq + 1, UserOp::Clear);
        let restored = evaluate(&wl.case, &w, &wl.cands);
        assert_eq!(
            entries(&restored),
            entries(&base),
            "seed {seed}: Clear did not restore ({x},{g:?})"
        );
    }
    assert!(
        dominated > 20 && masked_cases > 0,
        "weak coverage: {dominated} dominant, {masked_cases} masked"
    );
}

// ---- F9 ---------------------------------------------------------------------------------------

#[test]
fn f9_flags_follow_the_winner_and_priority_beats_freshness() {
    let mut checked = 0usize;
    let mut stale_seen = 0usize;
    let mut fb_seen = 0usize;
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 30).unwrap();
        let wl = workload(seed + 40_000, n, true);
        let canon: Vec<usize> = (0..wl.events.len()).collect();
        let (w, finals) = run_all(&wl, &canon);
        for (x, f) in finals.iter().enumerate() {
            let Some(f) = f else { continue };
            for g in Group::ALL.iter().copied() {
                let flagged = f.entry.stale.has(g) || f.entry.fallback.has(g);
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
                let want_fb = pos > 0;
                assert_eq!(
                    f.entry.stale.has(g),
                    want_stale,
                    "seed {seed} node {x} {g:?}: stale"
                );
                assert_eq!(
                    f.entry.fallback.has(g),
                    want_fb,
                    "seed {seed} node {x} {g:?}: fallback"
                );
                checked += 1;
                stale_seen += usize::from(want_stale);
                fb_seen += usize::from(want_fb);
            }
        }
    }
    assert!(
        checked > 500 && stale_seen > 0 && fb_seen > 0,
        "{checked} {stale_seen} {fb_seen}"
    );
}

#[test]
fn f9_stale_primary_beats_a_fresh_secondary() {
    // `live` is inexpressible, but the primary reported it (an old value, later invalidated):
    // priority picks the primary, flagged stale; the secondary does not win.
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], toy_rules());
    profile.inexpressible = GroupMask::EMPTY.with(Group::Live);
    let mut regs = NodeRegs::default();
    regs.src_mut(PrimarySlot::P0).apply_life(1, true);
    regs.src_mut(PrimarySlot::P0)
        .apply_obs(2, write_from_toy(Group::Live, &json!("polite")));
    regs.src_mut(PrimarySlot::P0).apply_inval(3);
    let sec = [FactCand {
        write: write_from_toy(Group::Live, &json!("assertive")),
        conf: ConfClass::High,
        tier: TrustTier::T3Untrusted,
        src_key: 0,
        seq: 99,
    }];
    let n = gorca_fuse::s0(&profile, &regs, &sec, &gorca_fuse::NoQuirks).unwrap();
    let f = gorca_fuse::fuse_node(
        &profile,
        &regs,
        &n,
        &gorca_fuse::Neighbours::NONE,
        &gorca_fuse::NodeCands {
            secondary: &sec,
            ..Default::default()
        },
        &gorca_fuse::NoPatches,
        &mut gorca_fuse::Scratch::default(),
    );
    assert_eq!(toy_from_entry(&f.entry, Group::Live), json!("polite"));
    assert!(f.entry.stale.has(Group::Live));
    assert_eq!(
        f.entry.prov.get(Group::Live),
        Some(ProvCode::ObservedPrimary)
    );
}

// ---- F10 --------------------------------------------------------------------------------------

#[test]
fn f10_invariants_hold_on_every_published_entry() {
    let mut nodes = 0usize;
    for seed in 0..trials() {
        let n = 3 + usize::try_from(seed % 38).unwrap();
        let wl = workload(seed + 60_000, n, true);
        let canon: Vec<usize> = (0..wl.events.len()).collect();
        let (_, finals) = run_all(&wl, &canon);
        for (x, f) in finals.iter().enumerate() {
            let Some(f) = f else { continue };
            nodes += 1;
            let bad = invariant_violations(&wl.case.profile.rules, &f.entry);
            assert!(
                bad.is_empty(),
                "seed {seed} node {x}: violations {bad:?} in {:?}",
                f.entry.sem
            );
            // The published value of a masked group is the not-applicable default.
            for g in Group::ALL.iter().copied().filter(|g| f.masked.has(*g)) {
                assert_eq!(f.entry.prov.get(g), Some(ProvCode::Default));
            }
        }
    }
    assert!(nodes > 1000);
}

#[test]
fn fuse_is_deterministic_and_pure() {
    let wl = workload(77, 40, true);
    let canon: Vec<usize> = (0..wl.events.len()).collect();
    let (_, a) = run_all(&wl, &canon);
    let (_, b) = run_all(&wl, &canon);
    assert_eq!(entries(&a), entries(&b));
}

#[test]
fn slot_roundtrip_helper_sanity() {
    assert_eq!(slot_from_toy(Group::Live, &json!("off")), Slot::Absent);
}
