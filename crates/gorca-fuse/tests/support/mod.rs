// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared test support: the toy value domain of the A3 oracle (`tools/research/semantic_fusion.py`)
//! mapped onto the typed model, a port of the oracle's pure admission, a toy anchor index, a
//! from-scratch tree evaluator over `gorca-fuse` and the comparison against a corpus case.
//!
//! Non-primary candidates are built from the **raw facts** of the case events: the port of
//! `admit` produces `FactIn`s (class after the tier cap, floor, minimum binding, TTL default),
//! the side table joins them under every delivery order, the toy index resolves anchors from the
//! lifecycle and `attr` registers, and the validity filter, ranking and strata run unchanged.
//! Nothing is read from the expected trails except to compare against them.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gorca_fuse::{
    AnchorId, AnchorIndex, Bound, ChildView, EpochSnapshot, Explain, FactIn, FactLayer, FactTrail,
    FuseCtx, Fused, Grant, Neighbours, NoTrace, NodeCands, NodeIdx, NodeRegs, PatchHit,
    PatchSource, Payload, PrimarySlot, Profile, Quirks, Ranked, Record, Resolved, RoleSet, Rules,
    S0Node, Scratch, SideTable, Slot, SourceInfo, SourceState, Trace, TrailEntry, UserOp, Val,
    Write, fuse_node_with, s0_with,
};
use gorca_model::event::Politeness;
use gorca_model::fact::{Basis, BindGen, EvidenceValue, SourceId};
use gorca_model::sem::{
    Avail, BindQuality, Check, EvidenceKind, Expand, Group, GroupClass, GroupCode, Live,
    Orientation, Relevant, SemEntry, SemRole, SourceClass, TrustTier,
};
use serde_json::{Value, json};

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
    Val::Live(Some(Live {
        politeness: p,
        atomic: false,
        relevant: Relevant::empty(),
        root: false,
    }))
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
            "off" => Slot::Val(Val::Live(None)),
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
            Val::Live(None) => "off".into(),
            Val::Live(Some(l)) => match l.politeness {
                Politeness::Polite => "polite".into(),
                Politeness::Assertive => "assertive".into(),
            },
            other => panic!("toy value {other:?}"),
        },
    }
}

/// The oracle's JSON for a slot: strings, but the level as a number.
pub fn slot_json(g: Group, s: &Slot) -> Value {
    let t = toy_str(s);
    if g == Group::Level && matches!(s, Slot::Val(_)) {
        json!(t.parse::<u64>().unwrap())
    } else {
        json!(t)
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

pub fn quirk_name(id: u16) -> Value {
    match id {
        0 => Value::Null,
        Q1 => json!("quirk:Q1-drop-stale-checked"),
        Q2 => json!("quirk:Q2-fix-xml-role"),
        other => panic!("quirk {other}"),
    }
}

/// Quirk-free evaluation marker for property tests.
pub struct NoQ;

impl Quirks for NoQ {
    fn apply(&self, _src: PrimarySlot, _rec: &mut Record) {}
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
            let mut hit = PatchHit {
                group: pt.group,
                write: None,
                shown: Slot::Unknown,
                spec: u32::try_from(pt.spec).unwrap(),
                ext: Arc::from(pt.ext_id.as_str()),
                rule: Arc::from(pt.pid.as_str()),
            };
            if pt.value.as_str() == Some("firstchild") {
                let shown = nb
                    .children
                    .first()
                    .and_then(|c| c.s0)
                    .map_or(Slot::Unknown, |s| s.slot(Group::Name).clone());
                let name = toy_str(&shown);
                if !matches!(name.as_str(), "?" | "-" | "") {
                    hit.write = Some(Write::Val(Val::Name(Arc::from(name.as_str()))));
                }
                hit.shown = shown;
            } else {
                let w = write_from_toy(pt.group, &pt.value);
                hit.shown = w.slot();
                hit.write = Some(w);
            }
            out.push(hit);
        }
    }
}

// ---- case ---------------------------------------------------------------------------------------

pub struct Case {
    pub id: String,
    pub layout: String,
    pub cap: u16,
    pub scope: String,
    pub profile: Profile,
    pub parent: Vec<i64>,
    pub patches: Vec<ToyPatch>,
    pub events: Vec<Value>,
    pub admission: Vec<String>,
    pub deliveries: Vec<(String, Vec<Vec<usize>>)>,
    pub expected: Vec<Value>,
    pub parked: Vec<Value>,
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
                scope: c["scope"].as_str().unwrap().to_owned(),
                profile,
                parent: c["parent"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_i64().unwrap())
                    .collect(),
                patches,
                events: c["events"].as_array().unwrap().clone(),
                admission: c["admission"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap().to_owned())
                    .collect(),
                deliveries,
                expected: c["expected"]["nodes"].as_array().unwrap().clone(),
                parked: c["expected"]["parked"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
                tables: tables.clone(),
            }
        })
        .collect()
}

impl Case {
    pub fn in_layout(&self, g: Group) -> bool {
        u16::from(g.code()) < self.cap
    }

    /// A synthetic case (property tests): no fixture tables, scope `app1`.
    pub fn synthetic(
        id: String,
        profile: Profile,
        parent: Vec<i64>,
        patches: Vec<ToyPatch>,
    ) -> Case {
        Case {
            id,
            layout: "x32".into(),
            cap: 32,
            scope: "app1".into(),
            profile,
            parent,
            patches,
            events: Vec::new(),
            admission: Vec::new(),
            deliveries: Vec::new(),
            expected: Vec::new(),
            parked: Vec::new(),
            tables: Value::Null,
        }
    }
}

// ---- the world: registers and the side table -----------------------------------------------------

pub struct World {
    pub nodes: Vec<NodeRegs>,
    /// pid -> (seq, installed), joined by max.
    pub patch: BTreeMap<String, (u64, bool)>,
    /// `attr` registers of the DOM-id primary (first primary): node -> (seq, id).
    pub attr: BTreeMap<usize, (u64, Option<String>)>,
    pub vis: Vec<u32>,
    pub clock: u64,
    /// source name -> (n, state).
    pub epoch: BTreeMap<String, (u64, String)>,
    pub side: SideTable,
}

pub fn side_table_for(tables: &Value) -> SideTable {
    let mut side = SideTable::default();
    if let Some(src) = tables["sources"].as_object() {
        for (name, row) in src {
            let slot = u32::try_from(row[0].as_u64().unwrap()).unwrap();
            let generation = u32::try_from(row[1].as_u64().unwrap()).unwrap();
            // Grant data lives in the side table (registered per source), not in the facts.
            let grant = match tables["grants"].get(name) {
                Some(g) => {
                    let floor = |c: &str| u16::try_from(g["floor"][c].as_u64().unwrap()).unwrap();
                    let mb = |c: &str| bind_min(tables, c);
                    Grant::new(
                        [
                            floor("name"),
                            floor("sem"),
                            floor("evidence"),
                            floor("visual"),
                        ],
                        [mb("name"), mb("sem"), mb("evidence"), mb("visual")],
                    )
                }
                None => Grant::standard(),
            };
            side.set_source(SourceInfo {
                id: SourceId { slot, generation },
                name: Arc::from(name.as_str()),
                class: SourceClass::from_name(row[2].as_str().unwrap()).unwrap(),
                tier: TrustTier::from_code(u8::try_from(row[3].as_u64().unwrap()).unwrap())
                    .unwrap(),
                kind_order: row[4].as_u64().map_or(0, |k| u8::try_from(k).unwrap()),
                grant,
            })
            .unwrap();
        }
    }
    side
}

pub fn empty_world(n: usize, tables: &Value) -> World {
    World {
        nodes: (0..n).map(|_| NodeRegs::default()).collect(),
        patch: BTreeMap::new(),
        attr: BTreeMap::new(),
        vis: vec![0; n],
        clock: 0,
        epoch: BTreeMap::new(),
        side: side_table_for(tables),
    }
}

// ---- pure admission (port of the oracle's `admit`) -------------------------------------------

const ROLES: &[&str] = &[
    "generic", "button", "checkbox", "link", "img", "heading", "text", "group", "listitem",
];

/// DOMAIN2 of the oracle: the values a semantic fact may carry.
fn domain_ok(g: Group, v: &Value) -> bool {
    let strs = |xs: &[&str]| v.as_str().is_some_and(|s| xs.contains(&s));
    match g {
        Group::Role => strs(ROLES),
        Group::Name => strs(&["", "Close", "Save", "Menu", "Item", "Logo", "Open"]),
        Group::Description => strs(&["", "Opens a dialog", "Required", "-"]),
        Group::Check => strs(&["na", "off", "on", "mixed"]),
        Group::Expand => strs(&["na", "collapsed", "expanded"]),
        Group::Avail => strs(&["na", "enabled", "disabled"]),
        Group::Level => v.as_u64().is_some_and(|n| (1..=3).contains(&n)) || strs(&["-"]),
        Group::Live => strs(&["off", "polite", "assertive"]),
        Group::Orientation => strs(&["na", "horizontal", "vertical"]),
        _ => false,
    }
}

fn group_class(code: GroupCode) -> &'static str {
    match code.classify() {
        GroupClass::Evidence(EvidenceKind::VisualOcrText | EvidenceKind::VisualLabelGuess) => {
            "visual"
        }
        GroupClass::Evidence(_) => "evidence",
        GroupClass::Semantic(Group::Name | Group::Description) => "name",
        GroupClass::Semantic(_) | GroupClass::Unregistered(_) => "sem",
    }
}

fn bind_min(tables: &Value, class: &str) -> BindQuality {
    BindQuality::from_name(tables["min_bind"][class].as_str().unwrap()).unwrap()
}

fn anchor_of(a: &Value) -> AnchorId {
    let kind = a[0].as_str().unwrap();
    match kind {
        "node" => AnchorId::Node(NodeIdx(u32::try_from(a[1].as_u64().unwrap()).unwrap())),
        "dom" => AnchorId::Dom {
            id: Arc::from(a[1].as_str().unwrap()),
            count: u16::try_from(a[2].as_u64().unwrap()).unwrap(),
            frame: 0,
            shadow: 0,
        },
        "region" => AnchorId::Region {
            window: a[1].as_u64().unwrap(),
            rect: (0, 0, 0, 0),
            capture: 0,
        },
        other => panic!("anchor {other}"),
    }
}

/// The oracle anchor tuple of an `AnchorId`.
pub fn anchor_json(a: &AnchorId) -> Value {
    match a {
        AnchorId::Node(n) => json!(["node", n.0]),
        AnchorId::Dom { id, count, .. } => json!(["dom", &**id, count]),
        AnchorId::Region { window, .. } => json!(["region", window]),
        other => panic!("anchor {other:?}"),
    }
}

/// Admit one raw `fact` event: the admission label of the oracle and the `FactIn` to store.
pub fn admit(case: &Case, ev: &[Value]) -> (String, Option<FactIn>) {
    let t = &case.tables;
    let src = ev[1].as_str().unwrap();
    let Some(row) = t["sources"].get(src) else {
        return ("reject:class".into(), None);
    };
    let class = row[2].as_str().unwrap();
    if class == "Primary" || class == "Annotation" {
        return ("reject:class".into(), None);
    }
    if ev[2].as_str().unwrap() != case.scope {
        return ("reject:scope".into(), None);
    }
    let gname = ev[4].as_str().unwrap();
    let grant = &t["grants"][src];
    let contains = |list: &Value, s: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .any(|x| x.as_str() == Some(s))
    };
    let code = if let Some(k) = EvidenceKind::from_name(gname) {
        if !contains(&grant["evidence"], gname) {
            return ("reject:group".into(), None);
        }
        k.group_code()
    } else {
        let g = group_named(gname);
        if !contains(&grant["semantic"], gname) || !case.in_layout(g) {
            return ("reject:group".into(), None);
        }
        g.group_code()
    };
    let anchor = &ev[3];
    if !contains(&grant["anchors"], anchor[0].as_str().unwrap()) {
        return ("reject:anchor".into(), None);
    }
    let p = ev[7].as_i64().unwrap();
    if !(0..=1000).contains(&p) {
        return ("reject:value".into(), None);
    }
    let mut label = "accept";
    let value = &ev[6];
    let payload = if value.as_str() == Some("<retract>") {
        Payload::Retract
    } else if let GroupClass::Evidence(kind) = code.classify() {
        let Some(s) = value.as_str() else {
            return ("reject:value".into(), None);
        };
        let max = usize::try_from(t["max_str"].as_u64().unwrap()).unwrap();
        let s: String = if s.chars().count() > max {
            label = "accept:truncated";
            s.chars().take(max).collect()
        } else {
            s.to_owned()
        };
        match kind {
            EvidenceKind::LayoutCssHidden => Payload::Evidence(EvidenceValue::Flag(s == "true")),
            _ => Payload::Evidence(EvidenceValue::Text(Arc::from(s.as_str()))),
        }
    } else {
        let g = group_named(gname);
        if !domain_ok(g, value) {
            return ("reject:value".into(), None);
        }
        Payload::Sem(write_from_toy(g, value))
    };
    let p16 = u16::try_from(p).unwrap();
    let ttl = ev[11]
        .as_u64()
        .or_else(|| (class == "Inferred").then(|| t["default_ttl_ms"].as_u64().unwrap()));
    let basis = ev[8].as_i64().unwrap();
    let fact = FactIn {
        source: SourceId {
            slot: u32::try_from(row[0].as_u64().unwrap()).unwrap(),
            generation: u32::try_from(row[1].as_u64().unwrap()).unwrap(),
        },
        anchor: anchor_of(anchor),
        group: code,
        seq: ev[5].as_u64().unwrap(),
        payload,
        p_milli: p16,
        // The oracle's basis can be -1 (stamped stale at vis 0): shift both sides by one.
        basis: Basis::VisRev(u32::try_from(basis + 1).unwrap()),
        bind_gen: BindGen(u32::try_from(ev[9].as_u64().unwrap()).unwrap()),
        admitted_at_ms: ev[10].as_u64().unwrap(),
        ttl_ms: ttl,
    };
    (label.to_owned(), Some(fact))
}

// ---- registers from events ----------------------------------------------------------------------

/// Apply the events of `order` (indices into `case.events`) in that order. Returns the admission
/// label of every fact event (`-` for the others), indexed like the events.
pub fn ingest(case: &Case, order: &[usize]) -> (World, Vec<String>) {
    let mut w = empty_world(case.parent.len(), &case.tables);
    let mut labels = vec!["-".to_owned(); case.events.len()];
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
            "attr" => {
                if e[1].as_str() == Some("atspi") {
                    let x = node(&e[2]);
                    let new = (e[3].as_u64().unwrap(), e[4].as_str().map(str::to_owned));
                    // Max by (seq, vkey): None sorts as the string "None" in the oracle.
                    let key = |a: &(u64, Option<String>)| {
                        (a.0, a.1.clone().unwrap_or_else(|| "None".into()))
                    };
                    match w.attr.get(&x) {
                        Some(cur) if key(&new) <= key(cur) => {}
                        _ => {
                            w.attr.insert(x, new);
                        }
                    }
                }
            }
            "vis" => {
                let x = node(&e[1]);
                let rev = u32::try_from(e[2].as_u64().unwrap()).unwrap();
                if rev > w.vis[x] {
                    w.vis[x] = rev;
                }
            }
            "tick" => w.clock = w.clock.max(e[1].as_u64().unwrap()),
            "epoch" => {
                let src = e[1].as_str().unwrap().to_owned();
                let new = (e[2].as_u64().unwrap(), e[3].as_str().unwrap().to_owned());
                match w.epoch.get(&src) {
                    Some(cur) if new <= *cur => {}
                    _ => {
                        w.epoch.insert(src, new);
                    }
                }
            }
            "fact" => {
                let (label, fact) = admit(case, e);
                labels[i] = label;
                if let Some(f) = fact {
                    w.side.insert(f).unwrap();
                }
            }
            other => panic!("event {other}"),
        }
    }
    (w, labels)
}

// ---- the toy anchor index -------------------------------------------------------------------------

pub struct ToyIndex {
    n: usize,
    /// Node -> life seq of the highest-priority source that has an opinion, when alive.
    alive: Vec<Option<u64>>,
    dom: HashMap<String, Vec<(usize, u64)>>,
}

impl ToyIndex {
    pub fn new(profile: &Profile, w: &World) -> ToyIndex {
        let alive: Vec<Option<u64>> = w
            .nodes
            .iter()
            .map(|regs| {
                profile
                    .pi(Group::Role)
                    .iter()
                    .find_map(|p| regs.src(*p).life())
                    .and_then(|(seq, a)| a.then_some(seq))
            })
            .collect();
        let mut dom: HashMap<String, Vec<(usize, u64)>> = HashMap::new();
        for (x, (seq, id)) in &w.attr {
            if let Some(id) = id
                && alive.get(*x).copied().flatten().is_some()
            {
                dom.entry(id.clone()).or_default().push((*x, *seq));
            }
        }
        for v in dom.values_mut() {
            v.sort_unstable();
        }
        ToyIndex {
            n: w.nodes.len(),
            alive,
            dom,
        }
    }
}

fn gen_of(seq: u64) -> BindGen {
    BindGen(u32::try_from(seq).unwrap())
}

impl AnchorIndex for ToyIndex {
    fn resolve(&self, anchor: &AnchorId) -> Resolved {
        match anchor {
            AnchorId::Node(NodeIdx(x)) => self.by_node(*x as usize, BindQuality::Exact),
            AnchorId::Region { window, .. } => {
                self.by_node(usize::try_from(*window).unwrap(), BindQuality::Geometric)
            }
            AnchorId::Dom { id, count, .. } => match self.dom.get(&**id) {
                Some(c) if !c.is_empty() => {
                    let (x, seq) = c[0];
                    Resolved {
                        node: Some(NodeIdx(u32::try_from(x).unwrap())),
                        quality: if *count == 1 && c.len() == 1 {
                            BindQuality::Unique
                        } else {
                            BindQuality::Heuristic
                        },
                        generation: gen_of(seq),
                    }
                }
                _ => Resolved::UNBOUND,
            },
            _ => Resolved::UNBOUND,
        }
    }
}

impl ToyIndex {
    fn by_node(&self, x: usize, q: BindQuality) -> Resolved {
        match self.alive.get(x).copied().flatten() {
            Some(seq) if x < self.n => Resolved {
                node: Some(NodeIdx(u32::try_from(x).unwrap())),
                quality: q,
                generation: gen_of(seq),
            },
            _ => Resolved::UNBOUND,
        }
    }
}

// ---- evaluation ---------------------------------------------------------------------------------

pub fn epoch_snapshot(case: &Case, w: &World) -> EpochSnapshot {
    let mut e = EpochSnapshot::default();
    let side = &case.tables["sources"];
    // The filter fails closed on a source missing from the snapshot, so list them all.
    if let Some(all) = side.as_object() {
        for row in all.values() {
            e.set(
                SourceId {
                    slot: u32::try_from(row[0].as_u64().unwrap()).unwrap(),
                    generation: u32::try_from(row[1].as_u64().unwrap()).unwrap(),
                },
                SourceState::Active,
            );
        }
    }
    for (name, (_, state)) in &w.epoch {
        let row = &side[name.as_str()];
        let id = SourceId {
            slot: u32::try_from(row[0].as_u64().unwrap()).unwrap(),
            generation: u32::try_from(row[1].as_u64().unwrap()).unwrap(),
        };
        e.set(
            id,
            match state.as_str() {
                "Active" => SourceState::Active,
                "Throttled" => SourceState::Throttled,
                "Detached" => SourceState::Detached,
                "Paused" => SourceState::Paused,
                "Quarantined" => SourceState::Quarantined,
                "Revoked" => SourceState::Revoked,
                other => panic!("epoch state {other}"),
            },
        );
    }
    e
}

/// One evaluated node.
pub struct NodeOut {
    pub fused: Fused,
    pub explain: Option<Explain>,
    pub evidence: Vec<FactTrail>,
}

pub struct TreeOut {
    pub nodes: Vec<Option<NodeOut>>,
    pub parked: Vec<FactTrail>,
}

impl TreeOut {
    pub fn entries(&self) -> Vec<Option<SemEntry>> {
        self.nodes
            .iter()
            .map(|o| o.as_ref().map(|n| n.fused.entry.clone()))
            .collect()
    }
}

/// From-scratch FUSE over the whole tree: bind facts, S0 everywhere, then S1..R children before
/// parents. `quirks` is the S-1 table; `trace` records the explain trail.
pub fn evaluate_full<Q: Quirks>(case: &Case, world: &World, quirks: &Q, trace: bool) -> TreeOut {
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

    let index = ToyIndex::new(&case.profile, world);
    let bound: Bound<'_> = world.side.bind(&index);
    let epoch = epoch_snapshot(case, world);
    let vis: Vec<u32> = world.vis.iter().map(|v| v + 1).collect();
    let vis_fn = |x: NodeIdx| vis[x.0 as usize];
    let doc_fn = |_: NodeIdx| 0u64;
    let ctx = FuseCtx {
        epoch: &epoch,
        clock_ms: world.clock,
        vis_rev: &vis_fn,
        doc_rev: &doc_fn,
    };
    let ranked: Vec<Ranked> = (0..n)
        .map(|x| bound.node(NodeIdx(u32::try_from(x).unwrap()), &ctx))
        .collect();

    let mut traces: Vec<Trace> = (0..n).map(|_| Trace::default()).collect();
    let s0s: Vec<Option<S0Node>> = (0..n)
        .map(|x| {
            if trace {
                s0_with(
                    &case.profile,
                    &world.nodes[x],
                    ranked[x].view(),
                    quirks,
                    &mut traces[x],
                )
            } else {
                s0_with(
                    &case.profile,
                    &world.nodes[x],
                    ranked[x].view(),
                    quirks,
                    &mut NoTrace,
                )
            }
        })
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

    let mut outs: Vec<Option<NodeOut>> = (0..n).map(|_| None).collect();
    let mut scratch = Scratch::default();
    for x in order {
        let Some(node) = &s0s[x] else { continue };
        let views: Vec<ChildView<'_>> = children[x]
            .iter()
            .map(|&c| ChildView {
                id: u32::try_from(c).unwrap(),
                s0: s0s[c].as_ref(),
                fused: outs[c].as_ref().map(|o| &o.fused.entry),
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
            more_children: false,
        };
        let cands: NodeCands<'_> = ranked[x].view();
        let fused = if trace {
            fuse_node_with(
                &case.profile,
                &world.nodes[x],
                node,
                &nb,
                cands,
                &patches,
                &mut scratch,
                &mut traces[x],
            )
        } else {
            fuse_node_with(
                &case.profile,
                &world.nodes[x],
                node,
                &nb,
                cands,
                &patches,
                &mut scratch,
                &mut NoTrace,
            )
        };
        drop(views);
        let evidence = ranked[x]
            .view()
            .evidence()
            .iter()
            .map(|e| e.trail().clone())
            .collect();
        outs[x] = Some(NodeOut {
            fused,
            explain: None,
            evidence,
        });
    }
    if trace {
        for (x, t) in traces.into_iter().enumerate() {
            if let Some(o) = outs[x].as_mut() {
                o.explain = Some(t.explain);
            }
        }
    }
    TreeOut {
        nodes: outs,
        parked: bound.parked(),
    }
}

// ---- trail rendering (oracle JSON) ------------------------------------------------------------------

fn layer_name(l: FactLayer) -> &'static str {
    match l {
        FactLayer::Secondary => "secondary",
        FactLayer::DerivedTitle => "derived:title",
        FactLayer::Inferred => "inferred",
        FactLayer::Evidence => "evidence",
        FactLayer::Parked => "parked",
    }
}

fn payload_json(p: &Payload, g: Group) -> Value {
    match p {
        Payload::Retract => json!("<retract>"),
        Payload::Sem(w) => slot_json(w.group(), &w.slot()),
        Payload::Evidence(EvidenceValue::Text(s)) => json!(&**s),
        Payload::Evidence(EvidenceValue::Flag(b)) => json!(if *b { "true" } else { "false" }),
        Payload::Evidence(other) => panic!("{other:?} {g:?}"),
    }
}

pub fn fact_json(t: &FactTrail, class_none: bool) -> Value {
    let class = if class_none {
        Value::Null
    } else {
        t.class.map_or(Value::Null, |c| json!(c.name()))
    };
    json!({
        "layer": layer_name(t.layer),
        "src": &*t.source,
        "seq": t.seq,
        "anchor": anchor_json(&t.anchor),
        "group": match t.group.classify() {
            GroupClass::Semantic(g) => g.name(),
            GroupClass::Evidence(k) => k.name(),
            GroupClass::Unregistered(_) => "?",
        },
        "value": payload_json(&t.payload, Group::Role),
        "p_milli": t.p_milli,
        "class": class,
        "bind": t.bind.name(),
        "gen": t.generation.0,
        "fact_gen": t.fact_generation.0,
        "status": t.status.name(),
    })
}

fn entry_json(g: Group, e: &TrailEntry) -> Value {
    match e {
        TrailEntry::Observed { cand, status } => {
            let name = |p: PrimarySlot| {
                if p == PrimarySlot::P0 {
                    "atspi"
                } else {
                    "newton"
                }
            };
            json!({
                "layer": "observed",
                "rule": quirk_name(cand.rule),
                "seq": cand.seq,
                "src": name(cand.src),
                "stale": cand.stale,
                "status": status.name(),
                "value": slot_json(g, &cand.value),
            })
        }
        TrailEntry::Fact(t) => fact_json(t, false),
        TrailEntry::Patch(p) => json!({
            "layer": "patch",
            "src": &*p.ext,
            "rule": &*p.rule,
            "value": slot_json(g, &p.value),
            "status": p.status.name(),
        }),
        TrailEntry::User { seq, value } => json!({
            "layer": "user", "src": "user", "seq": seq, "value": slot_json(g, value), "status": "won",
        }),
        TrailEntry::Content {
            value,
            from,
            tainted_from,
        } => json!({
            "layer": "derived", "src": "content", "value": &**value, "status": "won",
            "from": from, "tainted_from": tainted_from,
        }),
        TrailEntry::Repair { value, masked } => json!({
            "layer": "repair", "value": slot_json(g, value), "masked": slot_json(g, masked), "status": "won",
        }),
    }
}

/// The full rendered explain trails of a tree (for order-independence checks).
pub fn trails_json(out: &TreeOut) -> Vec<Value> {
    out.nodes
        .iter()
        .map(|o| match o {
            None => Value::Null,
            Some(n) => {
                let ex = n.explain.as_ref().unwrap();
                let groups: Vec<Value> = Group::ALL
                    .iter()
                    .map(|g| Value::Array(ex.trail(*g).iter().map(|t| entry_json(*g, t)).collect()))
                    .collect();
                json!({"groups": groups, "evidence": n.evidence.iter().map(|t| fact_json(t, true)).collect::<Vec<_>>()})
            }
        })
        .collect()
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
pub fn compare(case: &Case, out: &TreeOut, labels: &[String], with_trails: bool) -> Vec<String> {
    let mut diffs = Vec::new();
    if labels != case.admission.as_slice() {
        diffs.push(format!(
            "{} admission want {:?} have {:?}",
            case.id, case.admission, labels
        ));
    }
    for (x, exp) in case.expected.iter().enumerate() {
        let got = &out.nodes[x];
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
        let e = &got.fused.entry;
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
            if with_trails {
                let want_trail = &exp["trail"][gname];
                let have_trail: Vec<Value> = got
                    .explain
                    .as_ref()
                    .unwrap()
                    .trail(g)
                    .iter()
                    .map(|t| entry_json(g, t))
                    .collect();
                if want_trail != &Value::Array(have_trail.clone()) {
                    diffs.push(format!(
                        "{} node {x} {gname}: trail\n  want {want_trail}\n  have {}",
                        case.id,
                        Value::Array(have_trail)
                    ));
                }
            }
        }
        for (key, have) in [
            ("known", e.known),
            ("stale", e.stale),
            ("fallback", e.fallback),
        ] {
            let mut have_names: Vec<String> = Group::ALL
                .iter()
                .filter(|g| have.has(**g) && case.in_layout(**g))
                .map(|g| g.name().to_owned())
                .collect();
            have_names.sort();
            let mut want_sorted = names(&exp[key]);
            want_sorted.sort();
            if have_names != want_sorted {
                diffs.push(format!(
                    "{} node {x}: {key} want {want_sorted:?} have {have_names:?}",
                    case.id
                ));
            }
        }
        if with_trails {
            let canon = |mut v: Vec<Value>| {
                v.sort_by_key(ToString::to_string);
                v
            };
            let want = canon(exp["evidence"].as_array().unwrap().clone());
            let have = canon(
                got.evidence
                    .iter()
                    .filter(|t| {
                        !matches!(
                            t.group.classify(),
                            GroupClass::Evidence(
                                EvidenceKind::HintTitleAttr | EvidenceKind::VisualOcrText
                            )
                        )
                    })
                    .map(|t| fact_json(t, true))
                    .collect(),
            );
            // The oracle reports the raw filter status of an evidence fact (`ok` included).
            let have: Vec<Value> = have
                .into_iter()
                .map(|mut v| {
                    if v["status"] == "ok" {
                        v["status"] = json!("ok");
                    }
                    v
                })
                .collect();
            if want != have {
                diffs.push(format!(
                    "{} node {x}: evidence\n  want {}\n  have {}",
                    case.id,
                    Value::Array(want),
                    Value::Array(have)
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
                if enc[key].as_u64().unwrap() != u64::from(have.0) {
                    diffs.push(format!(
                        "{} node {x}: {key} enc want {} have {}",
                        case.id, enc[key], have.0
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
                if enc[key].as_u64().unwrap() != u64::from(have.0 & 0xFFFF) {
                    diffs.push(format!(
                        "{} node {x}: r2 {key} enc want {}",
                        case.id, enc[key]
                    ));
                }
            }
        }
    }
    if with_trails {
        let canon = |mut v: Vec<Value>| {
            v.sort_by_key(ToString::to_string);
            v
        };
        let want = canon(case.parked.clone());
        let have = canon(out.parked.iter().map(|t| fact_json(t, true)).collect());
        if want != have {
            diffs.push(format!(
                "{} parked\n  want {}\n  have {}",
                case.id,
                Value::Array(want),
                Value::Array(have)
            ));
        }
    }
    diffs
}
