// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared test support: the toy value domain of the A3 oracle (`tools/research/semantic_fusion.py`)
//! mapped onto the typed model, a from-scratch tree evaluator over `gorca-fuse`, and the
//! comparison against a corpus case.
//!
//! The oracle's non-primary status computation (binding, generation, epoch, `vis_rev`, TTL,
//! floors, S4 ranking) is B1b. B1a feeds `FUSE` the *valid* non-primary candidates read from the
//! expected trails (status `won`, `shadowed`, `not-candidate`, `not-applicable`) in trail order, and
//! tests the strata over them; the registers (observed, lifecycle, invalidation, user, patch)
//! are built from the events under every delivery order.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;

use gorca_fuse::{
    ChildView, FactCand, Fused, Neighbours, NodeCands, NodeRegs, PatchHit, PatchSource,
    PrimarySlot, Profile, Quirks, Record, RoleSet, Rules, S0Node, Scratch, Slot, UserOp, Val,
    Write, fuse_node, s0,
};
use gorca_model::event::Politeness;
use gorca_model::sem::{
    Avail, Check, ConfClass, Expand, Group, GroupMask, Live, Orientation, Relevant, SemEntry,
    SemRole, TrustTier,
};
use serde_json::Value;

pub fn fixture(name: &str) -> Value {
    let path = format!(
        "{}/../../tests/fixtures/fuse/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

// ---- toy value domain -------------------------------------------------------------------------

pub fn toy_role(name: &str) -> SemRole {
    match name {
        "text" => SemRole::StaticText,
        n => SemRole::from_name(n).unwrap_or_else(|| panic!("toy role {n}")),
    }
}

fn live_val(p: Politeness) -> Val {
    Val::Live(Live {
        politeness: p,
        atomic: false,
        relevant: Relevant::empty(),
        root: false,
    })
}

/// Map a toy value of group `g` to a slot (`"?"` Unknown, `"-"` Absent, live `"off"` is Absent).
pub fn slot_from_toy(g: Group, v: &Value) -> Slot {
    if let Some(n) = v.as_u64() {
        assert_eq!(g, Group::Level, "numeric value outside level");
        return Slot::Val(Val::Level(u8::try_from(n).unwrap()));
    }
    let s = v.as_str().unwrap_or_else(|| panic!("toy value {v}"));
    if s == "?" {
        return Slot::Unknown;
    }
    if s == "-" {
        return Slot::Absent;
    }
    match g {
        Group::Role => Slot::Val(Val::Role(toy_role(s))),
        Group::Name => Slot::Val(Val::Name(Arc::from(s))),
        Group::Description => Slot::Val(Val::Description(Arc::from(s))),
        Group::Check => Slot::Val(Val::Check(Check::from_name(s).unwrap())),
        Group::Expand => Slot::Val(Val::Expand(Expand::from_name(s).unwrap())),
        Group::Avail => Slot::Val(Val::Avail(Avail::from_name(s).unwrap())),
        Group::Orientation => Slot::Val(Val::Orientation(Orientation::from_name(s).unwrap())),
        Group::Live => match s {
            "off" => Slot::Absent,
            "polite" => Slot::Val(live_val(Politeness::Polite)),
            "assertive" => Slot::Val(live_val(Politeness::Assertive)),
            other => panic!("toy live {other}"),
        },
        other => panic!("toy group {other:?} is not modelled by the oracle"),
    }
}

pub fn write_from_toy(g: Group, v: &Value) -> Write {
    match slot_from_toy(g, v) {
        Slot::Absent => Write::Absent(g),
        Slot::Val(v) => Write::Val(v),
        Slot::Unknown => panic!("a register is never written Unknown"),
    }
}

/// The toy text of a slot, as the oracle's plain value (`?`, `-`, or the value).
pub fn toy_str(slot: &Slot) -> String {
    match slot {
        Slot::Unknown => "?".into(),
        Slot::Absent => "-".into(),
        Slot::Val(v) => match v {
            Val::Role(r) => toy_role_name(*r),
            Val::Name(s) | Val::Description(s) => s.to_string(),
            Val::Check(c) => c.name().into(),
            Val::Expand(c) => c.name().into(),
            Val::Avail(c) => c.name().into(),
            Val::Orientation(c) => c.name().into(),
            Val::Level(l) => l.to_string(),
            Val::Live(l) => match l.politeness {
                Politeness::Polite => "polite".into(),
                Politeness::Assertive => "assertive".into(),
            },
            other => panic!("toy value {other:?}"),
        },
    }
}

fn toy_role_name(r: SemRole) -> String {
    if r == SemRole::StaticText {
        "text".into()
    } else {
        r.name().into()
    }
}

/// The toy value of group `g` in a finished entry.
pub fn toy_from_entry(e: &SemEntry, g: Group) -> Value {
    match g {
        Group::Role => Value::from(toy_role_name(e.sem.role)),
        Group::Name => Value::from(e.name.as_deref().unwrap_or("")),
        Group::Description => Value::from(e.sem.description().unwrap_or("")),
        Group::Check => Value::from(e.sem.check.name()),
        Group::Expand => Value::from(e.sem.expand.name()),
        Group::Avail => Value::from(e.sem.avail.name()),
        Group::Orientation => Value::from(e.sem.orientation.name()),
        Group::Level => {
            if e.sem.level == 0 {
                Value::from("-")
            } else {
                Value::from(e.sem.level)
            }
        }
        Group::Live => Value::from(match e.sem.live {
            None => "off",
            Some(l) => match l.politeness {
                Politeness::Polite => "polite",
                Politeness::Assertive => "assertive",
            },
        }),
        other => panic!("toy group {other:?}"),
    }
}

/// The oracle's expected `-` (Absent) for a group other than `level` finalises to the default.
pub fn normalise_expected(g: Group, v: &Value) -> Value {
    if v.as_str() == Some("-") && g != Group::Level {
        return Value::from(match g {
            Group::Name | Group::Description => "",
            Group::Live => "off",
            Group::Role => "generic",
            _ => "na",
        });
    }
    v.clone()
}

// ---- profile, rules, quirks ---------------------------------------------------------------------

pub fn toy_rules() -> Rules {
    use SemRole as R;
    Rules {
        checkable: RoleSet::of(&[R::Checkbox]),
        disableable: RoleSet::of(&[R::Button, R::Checkbox, R::Link]),
        expandable: RoleSet::of(&[R::Button, R::Link, R::ListItem]),
        level_roles: RoleSet::of(&[R::Heading]),
        content_named: RoleSet::of(&[R::Button, R::Link, R::Heading, R::ListItem, R::Checkbox]),
        title_named: RoleSet::of(&[R::Button, R::Link, R::Img, R::Checkbox]),
    }
}

pub fn primary_slot(tables: &Value, src: &str) -> PrimarySlot {
    let slot = tables["sources"][src][0].as_u64().unwrap();
    PrimarySlot::new(usize::try_from(slot).unwrap()).unwrap()
}

pub fn group_named(name: &str) -> Group {
    Group::from_name(name).unwrap_or_else(|| panic!("group {name}"))
}

pub fn build_profile(tables: &Value, profile: &str) -> Profile {
    let default_order = [PrimarySlot::P0, PrimarySlot::P1];
    let mut p = Profile::new(&default_order, toy_rules());
    p.default_role = SemRole::Generic;
    for (gname, srcs) in tables["pi"][profile].as_object().unwrap() {
        let order: Vec<PrimarySlot> = srcs
            .as_array()
            .unwrap()
            .iter()
            .map(|s| primary_slot(tables, s.as_str().unwrap()))
            .collect();
        p.set_pi(group_named(gname), &order);
    }
    for g in tables["inexpressible"][profile].as_array().unwrap() {
        p.inexpressible = p.inexpressible.with(group_named(g.as_str().unwrap()));
    }
    p
}

/// Toy quirks Q1 (drop a stale checked state on a non-checkable role) and Q2 (a generic with a
/// level is a heading), applied to the first primary only.
pub struct ToyQuirks;

pub const Q1: u16 = 1;
pub const Q2: u16 = 2;

impl Quirks for ToyQuirks {
    fn apply(&self, src: PrimarySlot, rec: &mut Record) {
        if src != PrimarySlot::P0 {
            return;
        }
        let ix = |g: Group| usize::from(g.slot());
        let role = rec.slots[ix(Group::Role)].role();
        let checked = matches!(
            rec.slots[ix(Group::Check)],
            Slot::Val(Val::Check(Check::On | Check::Mixed))
        );
        if checked && role.is_some_and(|r| r != SemRole::Checkbox) {
            rec.slots[ix(Group::Check)] = Slot::Absent;
            rec.hit[ix(Group::Check)] = Q1;
        }
        let has_level = matches!(rec.slots[ix(Group::Level)], Slot::Val(_));
        if role == Some(SemRole::Generic) && has_level {
            rec.slots[ix(Group::Role)] = Slot::Val(Val::Role(SemRole::Heading));
            rec.hit[ix(Group::Role)] = Q2;
        }
    }
}

// ---- toy patches --------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ToyPatch {
    pub pid: String,
    pub ext_id: String,
    pub spec: i64,
    pub sel_role: String,
    pub empty_name: bool,
    pub sel_parent: Option<String>,
    pub group: Group,
    pub value: Value,
}

impl ToyPatch {
    fn rank(&self) -> (i64, &str, &str) {
        (-self.spec, &self.ext_id, &self.pid)
    }

    fn matches(&self, node: &S0Node, parent: Option<&S0Node>) -> bool {
        if toy_str(node.slot(Group::Role)) != self.sel_role {
            return false;
        }
        if self.empty_name {
            let n = toy_str(node.slot(Group::Name));
            if !n.is_empty() && n != "-" {
                return false;
            }
        }
        if let Some(sp) = &self.sel_parent {
            match parent {
                None => return false,
                Some(p) => {
                    if &toy_str(p.slot(Group::Role)) != sp {
                        return false;
                    }
                }
            }
        }
        true
    }
}

pub struct ToyPatches(pub Vec<ToyPatch>);

impl PatchSource for ToyPatches {
    fn hits(&self, node: &S0Node, nb: &Neighbours<'_>, out: &mut Vec<PatchHit>) {
        for pt in &self.0 {
            if !pt.matches(node, nb.parent) {
                continue;
            }
            if pt.value.as_str() == Some("firstchild") {
                let name = nb
                    .children
                    .first()
                    .and_then(|c| c.s0)
                    .map_or_else(|| "?".to_owned(), |s| toy_str(s.slot(Group::Name)));
                if matches!(name.as_str(), "?" | "-" | "") {
                    continue;
                }
                out.push(PatchHit {
                    write: Write::Val(Val::Name(Arc::from(name.as_str()))),
                });
            } else {
                out.push(PatchHit {
                    write: write_from_toy(pt.group, &pt.value),
                });
            }
        }
    }
}

// ---- case ---------------------------------------------------------------------------------------

pub struct Case {
    pub id: String,
    pub layout: String,
    pub cap: u16,
    pub profile: Profile,
    pub parent: Vec<i64>,
    pub patches: Vec<ToyPatch>,
    pub events: Vec<Value>,
    pub deliveries: Vec<(String, Vec<Vec<usize>>)>,
    pub expected: Vec<Value>,
    pub tables: Value,
}

pub fn load_cases(doc: &Value) -> Vec<Case> {
    let tables = doc["tables"].clone();
    let layout = doc["layout"].as_str().unwrap().to_owned();
    // r2-16 has 16 group slots, x32 has 32; groups with a code at or above the cap are not
    // part of that layout (the oracle never generates events for them).
    let cap_slots: u16 = if layout == "r2-16" { 16 } else { 32 };
    doc["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let profile = build_profile(&tables, c["profile"].as_str().unwrap());
            let patches = c["patches"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| ToyPatch {
                    pid: p["pid"].as_str().unwrap().into(),
                    ext_id: p["ext_id"].as_str().unwrap().into(),
                    spec: p["spec"].as_i64().unwrap(),
                    sel_role: p["sel_role"].as_str().unwrap().into(),
                    empty_name: p["empty_name"].as_bool().unwrap(),
                    sel_parent: p["sel_parent"].as_str().map(str::to_owned),
                    group: group_named(p["group"].as_str().unwrap()),
                    value: p["value"].clone(),
                })
                .collect();
            let deliveries = c["deliveries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| {
                    let batches = d["batches"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|b| {
                            b.as_array()
                                .unwrap()
                                .iter()
                                .map(|i| usize::try_from(i.as_u64().unwrap()).unwrap())
                                .collect()
                        })
                        .collect();
                    (d["name"].as_str().unwrap().to_owned(), batches)
                })
                .collect();
            Case {
                id: c["id"].as_str().unwrap().to_owned(),
                layout: layout.clone(),
                cap: cap_slots,
                profile,
                parent: c["parent"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_i64().unwrap())
                    .collect(),
                patches,
                events: c["events"].as_array().unwrap().clone(),
                deliveries,
                expected: c["expected"]["nodes"].as_array().unwrap().clone(),
                tables: tables.clone(),
            }
        })
        .collect()
}

impl Case {
    fn in_layout(&self, g: Group) -> bool {
        u16::from(g.code()) < self.cap
    }
}

// ---- registers from events ----------------------------------------------------------------------

pub struct World {
    pub nodes: Vec<NodeRegs>,
    /// pid -> (seq, installed), joined by max.
    pub patch: BTreeMap<String, (u64, bool)>,
}

/// Apply the register events of `order` (indices into `case.events`) in that order.
pub fn ingest(case: &Case, order: &[usize]) -> World {
    let mut w = World {
        nodes: (0..case.parent.len())
            .map(|_| NodeRegs::default())
            .collect(),
        patch: BTreeMap::new(),
    };
    for &i in order {
        let e = case.events[i].as_array().unwrap();
        let kind = e[0].as_str().unwrap();
        let node = |v: &Value| usize::try_from(v.as_u64().unwrap()).unwrap();
        match kind {
            "obs" => {
                let src = primary_slot(&case.tables, e[1].as_str().unwrap());
                let g = group_named(e[3].as_str().unwrap());
                if !case.in_layout(g) {
                    continue;
                }
                w.nodes[node(&e[2])]
                    .src_mut(src)
                    .apply_obs(e[4].as_u64().unwrap(), write_from_toy(g, &e[5]));
            }
            "life" => {
                let src = primary_slot(&case.tables, e[1].as_str().unwrap());
                w.nodes[node(&e[2])]
                    .src_mut(src)
                    .apply_life(e[3].as_u64().unwrap(), e[4].as_bool().unwrap());
            }
            "inval" => {
                let src = primary_slot(&case.tables, e[1].as_str().unwrap());
                w.nodes[node(&e[2])]
                    .src_mut(src)
                    .apply_inval(e[3].as_u64().unwrap());
            }
            "user" => {
                let g = group_named(e[2].as_str().unwrap());
                if !case.in_layout(g) {
                    continue;
                }
                let op = if e[4].as_str() == Some("<clear>") {
                    UserOp::Clear
                } else {
                    UserOp::Set(write_from_toy(g, &e[4]))
                };
                w.nodes[node(&e[1])].apply_user(g, e[3].as_u64().unwrap(), op);
            }
            "patch" => {
                let pid = e[1].as_str().unwrap().to_owned();
                let new = (e[2].as_u64().unwrap(), e[3].as_bool().unwrap());
                match w.patch.get(&pid) {
                    Some(&cur) if new <= cur => {}
                    _ => {
                        w.patch.insert(pid, new);
                    }
                }
            }
            // Fact-side events (attr, vis, tick, epoch, fact) feed the B1b side table.
            _ => {}
        }
    }
    w
}

// ---- candidates from the expected trails ----------------------------------------------------------

#[derive(Default, Clone)]
pub struct OwnedCands {
    pub secondary: Vec<FactCand>,
    pub title: Vec<FactCand>,
    pub inferred: Vec<FactCand>,
}

fn conf_named(n: &str) -> ConfClass {
    ConfClass::from_name(n).unwrap_or_else(|| panic!("class {n}"))
}

fn tier_of(tables: &Value, src: &str) -> TrustTier {
    TrustTier::from_code(u8::try_from(tables["sources"][src][3].as_u64().unwrap()).unwrap())
        .unwrap()
}

pub fn cands_from_expected(case: &Case) -> Vec<OwnedCands> {
    case.expected
        .iter()
        .map(|n| {
            let mut out = OwnedCands::default();
            if n.is_null() {
                return out;
            }
            for (gname, trail) in n["trail"].as_object().unwrap() {
                let g = group_named(gname);
                for t in trail.as_array().unwrap() {
                    let layer = t["layer"].as_str().unwrap();
                    let status = t["status"].as_str().unwrap_or("");
                    let mk = |g: Group| -> FactCand {
                        let src = t["src"].as_str().unwrap();
                        let write = match slot_from_toy(g, &t["value"]) {
                            Slot::Absent => Write::Absent(g),
                            Slot::Val(v) => Write::Val(v),
                            Slot::Unknown => panic!("fact value unknown"),
                        };
                        FactCand {
                            write,
                            conf: conf_named(t["class"].as_str().unwrap()),
                            tier: tier_of(&case.tables, src),
                            src_key: u32::try_from(
                                case.tables["sources"][src][0].as_u64().unwrap(),
                            )
                            .unwrap(),
                            seq: t["seq"].as_u64().unwrap(),
                        }
                    };
                    match (layer, status) {
                        ("secondary", "won" | "shadowed" | "not-candidate") => {
                            out.secondary.push(mk(g));
                        }
                        ("derived:title", "won" | "shadowed" | "not-applicable") => {
                            out.title.push(mk(Group::Name));
                        }
                        ("inferred", "won" | "shadowed") => out.inferred.push(mk(g)),
                        _ => {}
                    }
                }
            }
            out
        })
        .collect()
}

// ---- evaluation ---------------------------------------------------------------------------------

/// From-scratch FUSE over the whole tree: S0 everywhere, then S1..R children before parents.
pub fn evaluate(case: &Case, world: &World, cands: &[OwnedCands]) -> Vec<Option<Fused>> {
    evaluate_with(case, world, cands, &ToyQuirks)
}

pub fn evaluate_with<Q: Quirks>(
    case: &Case,
    world: &World,
    cands: &[OwnedCands],
    quirks: &Q,
) -> Vec<Option<Fused>> {
    let n = case.parent.len();
    let mut installed: Vec<ToyPatch> = case
        .patches
        .iter()
        .filter(|p| world.patch.get(&p.pid).is_some_and(|&(_, inst)| inst))
        .filter(|p| case.in_layout(p.group))
        .cloned()
        .collect();
    installed.sort_by(|a, b| a.rank().cmp(&b.rank()));
    let patches = ToyPatches(installed);

    let s0s: Vec<Option<S0Node>> = (0..n)
        .map(|x| s0(&case.profile, &world.nodes[x], &cands[x].secondary, quirks))
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (x, &p) in case.parent.iter().enumerate() {
        if p >= 0 {
            children[usize::try_from(p).unwrap()].push(x);
        }
    }
    let depth = |mut x: usize| {
        let mut d = 0;
        while case.parent[x] >= 0 {
            x = usize::try_from(case.parent[x]).unwrap();
            d += 1;
        }
        d
    };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&x| std::cmp::Reverse(depth(x)));

    let mut finals: Vec<Option<Fused>> = (0..n).map(|_| None).collect();
    let mut scratch = Scratch::default();
    for x in order {
        let Some(node) = &s0s[x] else { continue };
        let views: Vec<ChildView<'_>> = children[x]
            .iter()
            .map(|&c| ChildView {
                s0: s0s[c].as_ref(),
                fused: finals[c].as_ref().map(|f| &f.entry),
            })
            .collect();
        let parent = if case.parent[x] >= 0 {
            s0s[usize::try_from(case.parent[x]).unwrap()].as_ref()
        } else {
            None
        };
        let nb = Neighbours {
            parent,
            children: &views,
        };
        let c = NodeCands {
            secondary: &cands[x].secondary,
            title: &cands[x].title,
            inferred: &cands[x].inferred,
        };
        let f = fuse_node(
            &case.profile,
            &world.nodes[x],
            node,
            &nb,
            &c,
            &patches,
            &mut scratch,
        );
        drop(views);
        finals[x] = Some(f);
    }
    finals
}

// ---- comparison -----------------------------------------------------------------------------------

fn hex(v: &Value) -> u128 {
    u128::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_owned())
        .collect()
}

/// Compare the evaluated tree with the expected nodes. Returns one line per mismatch.
pub fn compare(case: &Case, finals: &[Option<Fused>]) -> Vec<String> {
    let mut diffs = Vec::new();
    let masks = |m: GroupMask| m.0;
    for (x, exp) in case.expected.iter().enumerate() {
        let got = &finals[x];
        if exp.is_null() {
            if got.is_some() {
                diffs.push(format!("{} node {x}: expected dead", case.id));
            }
            continue;
        }
        let Some(got) = got else {
            diffs.push(format!("{} node {x}: expected alive", case.id));
            continue;
        };
        let e = &got.entry;
        for (gname, ev) in exp["values"].as_object().unwrap() {
            let g = group_named(gname);
            let want = normalise_expected(g, ev);
            let have = toy_from_entry(e, g);
            if want != have {
                diffs.push(format!(
                    "{} node {x} {gname}: value want {want} have {have}",
                    case.id
                ));
            }
            let want_prov = exp["prov"][gname].as_str().unwrap();
            let have_prov = e.prov.get(g).map_or("?", |p| p.name());
            if want_prov != have_prov {
                diffs.push(format!(
                    "{} node {x} {gname}: prov want {want_prov} have {have_prov}",
                    case.id
                ));
            }
            let want_h = exp["hedge"][gname].as_u64().unwrap();
            if want_h != u64::from(e.hedge.get(g)) {
                diffs.push(format!(
                    "{} node {x} {gname}: hedge want {want_h} have {}",
                    case.id,
                    e.hedge.get(g)
                ));
            }
            let want_t = exp["taint"][gname].as_u64().unwrap();
            if want_t != u64::from(e.taint.get(g)) {
                diffs.push(format!(
                    "{} node {x} {gname}: taint want {want_t} have {}",
                    case.id,
                    e.taint.get(g)
                ));
            }
        }
        // known / stale / fallback as group-name sets, over the layout's groups.
        for (key, have) in [
            ("known", e.known),
            ("stale", e.stale),
            ("fallback", e.fallback),
        ] {
            let want = names(&exp[key]);
            let mut have_names: Vec<String> = Group::ALL
                .iter()
                .filter(|g| have.has(**g) && case.in_layout(**g))
                .map(|g| g.name().to_owned())
                .collect();
            have_names.sort();
            let mut want_sorted = want.clone();
            want_sorted.sort();
            if have_names != want_sorted {
                diffs.push(format!(
                    "{} node {x}: {key} want {want_sorted:?} have {have_names:?}",
                    case.id
                ));
            }
        }
        // Layout bit encodings. x32 compares the full words; r2-16 the low words and masks.
        let enc = &exp["enc"];
        let prov = u128::from(e.prov.0[0]) | (u128::from(e.prov.0[1]) << 64);
        if case.layout == "x32" {
            if hex(&enc["prov_tag"]) != prov {
                diffs.push(format!(
                    "{} node {x}: prov_tag want {} have {prov:#034x}",
                    case.id, enc["prov_tag"]
                ));
            }
            if hex(&enc["hedge"]) != u128::from(e.hedge.0) {
                diffs.push(format!(
                    "{} node {x}: hedge enc want {} have {:#x}",
                    case.id, enc["hedge"], e.hedge.0
                ));
            }
            if hex(&enc["taint"]) != u128::from(e.taint.0) {
                diffs.push(format!(
                    "{} node {x}: taint enc want {} have {:#x}",
                    case.id, enc["taint"], e.taint.0
                ));
            }
            for (key, have) in [
                ("known", e.known),
                ("stale", e.stale),
                ("fallback", e.fallback),
            ] {
                if enc[key].as_u64().unwrap() != u64::from(masks(have)) {
                    diffs.push(format!(
                        "{} node {x}: {key} enc want {} have {}",
                        case.id,
                        enc[key],
                        masks(have)
                    ));
                }
            }
        } else {
            if hex(&enc["prov_tag"]) != u128::from(e.prov.0[0]) {
                diffs.push(format!(
                    "{} node {x}: r2 prov_tag want {} have {:#x}",
                    case.id, enc["prov_tag"], e.prov.0[0]
                ));
            }
            if hex(&enc["hedge"]) != u128::from(e.hedge.0 & 0xFFFF_FFFF) {
                diffs.push(format!(
                    "{} node {x}: r2 hedge want {}",
                    case.id, enc["hedge"]
                ));
            }
            for (key, have) in [
                ("known", e.known),
                ("stale", e.stale),
                ("fallback", e.fallback),
            ] {
                if enc[key].as_u64().unwrap() != u64::from(masks(have) & 0xFFFF) {
                    diffs.push(format!(
                        "{} node {x}: r2 {key} enc want {}",
                        case.id, enc[key]
                    ));
                }
            }
        }
    }
    diffs
}
