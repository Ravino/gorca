// SPDX-License-Identifier: GPL-3.0-or-later
//! The strata of `FUSE` (ADR-0024 section 2.2): S-1 quirk, S0 observed, S1 patch, S2 user, S3
//! derived, S4 inferred, R repair.
//!
//! `FUSE` is a pure function of the node's registers, its neighbours' already-computed values,
//! the static [`Profile`] and the node's [`NodeCands`], which only the side table can build and
//! which contain only facts that passed the validity filter. It reads no clock, no epoch and no
//! atomic. Evaluate S-1..S2 for all dirty nodes first, then S3, S4 and R bottom-up (children before
//! parents): each value is computed once from final inputs, so the result is unique (Lemma 2).
//!
//! Every function is generic over a [`Tracer`]: [`NoTrace`] is the hot path, [`crate::Trace`]
//! produces the `explain` trail from the same decisions.

use std::cmp::Ordering;
use std::sync::Arc;

use gorca_model::sem::{
    Avail, Check, Current, Expand, GROUP_COUNT, Group, GroupMask, Invalid, Orientation, Popup,
    Press, ProvCode, Select, SemEntry, SemFlags, SemRole, Sort,
};

use crate::explain::{Conflict, NoTrace, ObsCand, PatchTrail, Tracer};
use crate::profile::{Profile, RoleSet};
use crate::register::{MAX_PRIMARIES, NodeRegs, PrimarySlot};
use crate::repair::{Eff, ix, repair, unknown_eff};
use crate::s4::{inferred_status, title_status};
use crate::side::{Entry, NodeCands, Status, TrailStatus, cmp_secondary_like};
use crate::taint::{inferred_info, name_from_content, secondary_info, title_info};
use crate::value::{Info, Seq, Slot, Val, Write, write_default};

// ------------------------------------------------------------------------------------------------
// S-1 quirks
// ------------------------------------------------------------------------------------------------

/// One primary source's raw record of a node, as the quirk stratum sees it.
#[derive(Clone, Debug)]
pub struct Record {
    pub slots: Eff,
    /// Quirk rule id that rewrote a group (0 = none), for `explain`.
    pub hit: [u16; GROUP_COUNT],
}

impl Record {
    fn new() -> Record {
        Record {
            slots: unknown_eff(),
            hit: [0; GROUP_COUNT],
        }
    }
}

/// Stratum S-1: a pure function of one source's record of one node (`quirk_src`). The closed
/// `QuirkAction` engine and its loader are B5; B1a fixes the seam.
pub trait Quirks {
    fn apply(&self, src: PrimarySlot, rec: &mut Record);
}

/// No quirk rows.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoQuirks;

impl Quirks for NoQuirks {
    fn apply(&self, _src: PrimarySlot, _rec: &mut Record) {}
}

// ------------------------------------------------------------------------------------------------
// S0 observed
// ------------------------------------------------------------------------------------------------

/// The S0 result for one group.
#[derive(Clone, Debug)]
pub struct S0Slot {
    pub slot: Slot,
    pub info: Info,
    /// The primary that won (None for `Unknown`, a Secondary or a default).
    pub src: Option<PrimarySlot>,
    /// The winning register's seq.
    pub seq: Option<Seq>,
    /// A higher-priority primary has not answered (the winner is a fallback).
    pub fallback: bool,
    /// The winner is older than the source's newest invalidation.
    pub stale: bool,
    /// Quirk rule id that produced the winner's value (0 = none).
    pub hit: u16,
}

impl S0Slot {
    fn unknown() -> S0Slot {
        S0Slot {
            slot: Slot::Unknown,
            info: Info::DEFAULT,
            src: None,
            seq: None,
            fallback: false,
            stale: false,
            hit: 0,
        }
    }
}

/// S0 of an alive node: per group the observed winner (or `Unknown`). Patch selectors read only
/// this (the CSS-cascade rule), never S1 output.
#[derive(Clone, Debug)]
pub struct S0Node {
    slots: [S0Slot; GROUP_COUNT],
}

impl S0Node {
    pub fn get(&self, g: Group) -> &S0Slot {
        &self.slots[ix(g)]
    }

    /// The S0 value of `g` (`Unknown` included).
    pub fn slot(&self, g: Group) -> &Slot {
        &self.get(g).slot
    }

    /// The S0 role, if any source reported one.
    pub fn role(&self) -> Option<SemRole> {
        self.slot(Group::Role).role()
    }
}

/// Existence is the lifecycle of the highest-priority source that has an opinion
/// (`PI[profile][role]`); the secondary cannot keep a node alive (FS-3).
pub fn is_alive(profile: &Profile, regs: &NodeRegs) -> bool {
    for &p in profile.pi(Group::Role) {
        if let Some((_, alive)) = regs.src(p).life() {
            return alive;
        }
    }
    false
}

/// Stratum S-1 and S0 for one node. `None` when the node is not alive.
pub fn s0<Q: Quirks>(
    profile: &Profile,
    regs: &NodeRegs,
    cands: NodeCands<'_>,
    quirks: &Q,
) -> Option<S0Node> {
    s0_with(profile, regs, cands, quirks, &mut NoTrace)
}

/// As [`s0`], reporting every candidate and decision to `tr`.
pub fn s0_with<Q: Quirks, T: Tracer>(
    profile: &Profile,
    regs: &NodeRegs,
    cands: NodeCands<'_>,
    quirks: &Q,
    tr: &mut T,
) -> Option<S0Node> {
    if !is_alive(profile, regs) {
        return None;
    }
    let mut recs: [Record; MAX_PRIMARIES] = std::array::from_fn(|_| Record::new());
    for (i, rec) in recs.iter_mut().enumerate() {
        let Some(p) = PrimarySlot::new(i) else {
            continue;
        };
        for r in regs.src(p).regs() {
            rec.slots[ix(r.write.group())] = r.write.slot();
        }
        quirks.apply(p, rec);
    }
    let slots = std::array::from_fn(|i| {
        let Some(g) = Group::ALL.get(i).copied() else {
            return S0Slot::unknown();
        };
        s0_group(profile, regs, &recs, cands, g, tr)
    });
    Some(S0Node { slots })
}

fn s0_group<T: Tracer>(
    profile: &Profile,
    regs: &NodeRegs,
    recs: &[Record; MAX_PRIMARIES],
    cands: NodeCands<'_>,
    g: Group,
    tr: &mut T,
) -> S0Slot {
    let mut fallback = false;
    let mut win: Option<S0Slot> = None;
    for &p in profile.pi(g) {
        let rec = &recs[p.index()];
        let v = &rec.slots[ix(g)];
        let src = regs.src(p);
        if T::ON {
            tr.observed(
                g,
                ObsCand {
                    src: p,
                    seq: src.get(g).map(|r| r.seq),
                    value: v.clone(),
                    rule: rec.hit[ix(g)],
                    stale: src.is_stale(g),
                },
            );
        }
        if v.is_unknown() {
            if win.is_none() {
                fallback = true;
            }
            continue;
        }
        if win.is_none() {
            win = Some(S0Slot {
                slot: v.clone(),
                info: Info::plain(ProvCode::ObservedPrimary),
                src: Some(p),
                seq: src.get(g).map(|r| r.seq),
                fallback,
                stale: src.is_stale(g),
                hit: rec.hit[ix(g)],
            });
            if !T::ON {
                break;
            }
        }
    }
    if T::ON {
        trace_disagreement(profile, recs, g, tr);
    }
    let inexpressible = profile.inexpressible.has(g);
    // A Secondary is a candidate only for a statically inexpressible group (H-3); a dynamic
    // Unknown of an expressible group never admits one.
    let mut sec_win: Option<S0Slot> = None;
    if T::ON {
        let mut list: Vec<&Entry> = cands
            .secondary()
            .iter()
            .filter(|e| e.target() == g)
            .collect();
        let ok = |e: &Entry| e.status() == Status::Ok && inexpressible;
        list.sort_by(|a, b| cmp_secondary_like(a, b, ok(a), ok(b)));
        let mut won = win.is_some();
        for e in list {
            let st = match e.cand() {
                Some(_) if !inexpressible => TrailStatus::NotCandidate,
                Some(c) if !won => {
                    won = true;
                    sec_win = Some(secondary_slot(c));
                    TrailStatus::Won
                }
                Some(_) => TrailStatus::Shadowed,
                None => e.trail().status,
            };
            tr.fact(g, e, st);
        }
    } else if win.is_none() && inexpressible {
        let best = cands
            .secondary()
            .iter()
            .filter(|e| e.target() == g)
            .filter_map(Entry::cand)
            .min_by(|a, b| cmp_cand(a, b));
        sec_win = best.map(secondary_slot);
    }
    win.or(sec_win).unwrap_or_else(S0Slot::unknown)
}

fn secondary_slot(c: &crate::side::FactCand) -> S0Slot {
    S0Slot {
        slot: c.write().slot(),
        info: secondary_info(c),
        src: None,
        seq: Some(c.seq()),
        fallback: false,
        stale: false,
        hit: 0,
    }
}

/// Order among valid Secondaries (the hot path needs no entry list): more trusted tier, stronger
/// class, stable source name, seq, value.
fn cmp_cand(a: &crate::side::FactCand, b: &crate::side::FactCand) -> Ordering {
    let tier = if a.tier().is_more_trusted_than(b.tier()) {
        Ordering::Less
    } else if b.tier().is_more_trusted_than(a.tier()) {
        Ordering::Greater
    } else {
        Ordering::Equal
    };
    let conf = if a.conf().is_stronger_than(b.conf()) {
        Ordering::Less
    } else if b.conf().is_stronger_than(a.conf()) {
        Ordering::Greater
    } else {
        Ordering::Equal
    };
    tier.then(conf)
        .then_with(|| a.source().cmp(b.source()))
        .then_with(|| a.seq().cmp(&b.seq()))
        .then_with(|| a.write().tie_cmp(b.write()))
}

fn trace_disagreement<T: Tracer>(
    profile: &Profile,
    recs: &[Record; MAX_PRIMARIES],
    g: Group,
    tr: &mut T,
) {
    let known: Vec<(PrimarySlot, &Slot)> = profile
        .pi(g)
        .iter()
        .map(|&p| (p, &recs[p.index()].slots[ix(g)]))
        .filter(|(_, s)| !s.is_unknown())
        .collect();
    if let [(pa, a), (pb, b)] = known.as_slice()
        && a != b
    {
        tr.conflict(Conflict::ObservedDisagree {
            group: g,
            a: (*pa, (*a).clone()),
            b: (*pb, (*b).clone()),
        });
    }
}

// ------------------------------------------------------------------------------------------------
// S1 patches
// ------------------------------------------------------------------------------------------------

/// The neighbourhood a patch selector or a derived rule may read.
#[derive(Copy, Clone, Debug)]
pub struct Neighbours<'a> {
    pub parent: Option<&'a S0Node>,
    /// All children in a stable order (dead ones included, with `None` views).
    pub children: &'a [ChildView<'a>],
}

impl Neighbours<'_> {
    /// No parent, no children.
    pub const NONE: Neighbours<'static> = Neighbours {
        parent: None,
        children: &[],
    };
}

/// One child as the parent sees it.
#[derive(Copy, Clone, Debug)]
pub struct ChildView<'a> {
    /// The child's writer-local id (reported by `explain`).
    pub id: u32,
    /// The child's S0 (None when the child is not alive).
    pub s0: Option<&'a S0Node>,
    /// The child's final entry (None when not alive); S3 name from content reads it.
    pub fused: Option<&'a SemEntry>,
}

/// A patch edit that matched the node.
#[derive(Clone, Debug)]
pub struct PatchHit {
    pub group: Group,
    /// The value to set; `None` when the patch matched but its source had no value (for example
    /// "name of the first child" with an unnamed first child): reported, not applied.
    pub write: Option<Write>,
    /// What the rule read (for `explain`).
    pub shown: Slot,
    /// Specificity: equal-specificity hits that disagree are a diagnostic.
    pub spec: u32,
    pub ext: Arc<str>,
    pub rule: Arc<str>,
}

impl PatchHit {
    /// A hit that sets `write`, with no origin labels.
    pub fn set(write: Write) -> PatchHit {
        PatchHit {
            group: write.group(),
            shown: write.slot(),
            write: Some(write),
            spec: 0,
            ext: Arc::from(""),
            rule: Arc::from(""),
        }
    }
}

/// The installed patches. Selectors read S0 of the node and bounded neighbours only. `hits`
/// appends the node's matching edits in rank order (highest first): the first hit of a group wins.
pub trait PatchSource {
    fn hits(&self, node: &S0Node, nb: &Neighbours<'_>, out: &mut Vec<PatchHit>);
}

/// No patches installed.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoPatches;

impl PatchSource for NoPatches {
    fn hits(&self, _node: &S0Node, _nb: &Neighbours<'_>, _out: &mut Vec<PatchHit>) {}
}

// ------------------------------------------------------------------------------------------------
// FUSE for one node
// ------------------------------------------------------------------------------------------------

/// The layer that produced a group's effective value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Layer {
    Observed,
    Secondary,
    Patch,
    User,
    Derived,
    Inferred,
    Default,
}

/// The result of fusing one node.
#[derive(Clone, Debug)]
pub struct Fused {
    pub entry: SemEntry,
    pub layer: [Layer; GROUP_COUNT],
    /// Groups repair `R` overwrote (their provenance is reset to `Default`).
    pub masked: GroupMask,
    /// Groups whose derived string was cut at the profile's text limit.
    pub truncated: GroupMask,
}

impl Fused {
    pub fn layer_of(&self, g: Group) -> Layer {
        self.layer[ix(g)]
    }
}

/// Scratch space reused across calls (allocation-aware hot path).
#[derive(Debug, Default)]
pub struct Scratch {
    hits: Vec<PatchHit>,
}

/// The value a group takes when nothing set it. The role default is profile data; everything else
/// is the type's default. Live regions, set position, grid and level default to `Absent`.
pub fn default_slot(g: Group, default_role: SemRole) -> Slot {
    match g {
        Group::Role => Slot::Val(Val::Role(default_role)),
        Group::Name => Slot::Val(Val::Name(Arc::from(""))),
        Group::Description => Slot::Val(Val::Description(Arc::from(""))),
        Group::Check => Slot::Val(Val::Check(Check::NotCheckable)),
        Group::Press => Slot::Val(Val::Press(Press::NotToggle)),
        Group::Expand => Slot::Val(Val::Expand(Expand::NotExpandable)),
        Group::Select => Slot::Val(Val::Select(Select::NotSelectable)),
        Group::Current => Slot::Val(Val::Current(Current::No)),
        Group::Invalid => Slot::Val(Val::Invalid(Invalid::No)),
        Group::Popup => Slot::Val(Val::Popup(Popup::No)),
        Group::Avail => Slot::Val(Val::Avail(Avail::NotApplicable)),
        Group::Flags => Slot::Val(Val::Flags(SemFlags::empty())),
        Group::Orientation => Slot::Val(Val::Orientation(Orientation::NotApplicable)),
        Group::Sort => Slot::Val(Val::Sort(Sort::None)),
        Group::Level | Group::SetPos | Group::Grid | Group::Live => Slot::Absent,
    }
}

/// Strata S1, S2, S3, S4, defaults and R for one alive node.
pub fn fuse_node<P: PatchSource>(
    profile: &Profile,
    regs: &NodeRegs,
    node: &S0Node,
    nb: &Neighbours<'_>,
    cands: NodeCands<'_>,
    patches: &P,
    scratch: &mut Scratch,
) -> Fused {
    fuse_node_with(
        profile,
        regs,
        node,
        nb,
        cands,
        patches,
        scratch,
        &mut NoTrace,
    )
}

/// As [`fuse_node`], reporting every decision to `tr`.
#[allow(clippy::too_many_arguments)]
pub fn fuse_node_with<P: PatchSource, T: Tracer>(
    profile: &Profile,
    regs: &NodeRegs,
    node: &S0Node,
    nb: &Neighbours<'_>,
    cands: NodeCands<'_>,
    patches: &P,
    scratch: &mut Scratch,
    tr: &mut T,
) -> Fused {
    let mut eff = unknown_eff();
    let mut layer = [Layer::Default; GROUP_COUNT];
    let mut info = [Info::DEFAULT; GROUP_COUNT];
    let mut winner: [Option<PrimarySlot>; GROUP_COUNT] = [None; GROUP_COUNT];
    for g in Group::ALL.iter().copied() {
        let s = node.get(g);
        let i = ix(g);
        eff[i] = s.slot.clone();
        info[i] = s.info;
        winner[i] = s.src;
        layer[i] = match (&s.slot, s.info.prov) {
            (Slot::Unknown, _) => Layer::Default,
            (_, ProvCode::ObservedSecondary) => Layer::Secondary,
            _ => Layer::Observed,
        };
    }

    // S1 patch: the first (highest-ranked) hit of a group wins; selectors read S0 only.
    scratch.hits.clear();
    patches.hits(node, nb, &mut scratch.hits);
    let mut patched = GroupMask::EMPTY;
    if T::ON {
        for h in scratch.hits.iter().filter(|h| h.write.is_none()) {
            tr.patch(
                h.group,
                PatchTrail {
                    ext: Arc::clone(&h.ext),
                    rule: Arc::clone(&h.rule),
                    value: h.shown.clone(),
                    status: TrailStatus::NoValue,
                },
            );
        }
    }
    for h in &scratch.hits {
        let Some(w) = &h.write else { continue };
        let g = h.group;
        let first = !patched.has(g);
        if first {
            patched = patched.with(g);
            eff[ix(g)] = w.slot();
            layer[ix(g)] = Layer::Patch;
            info[ix(g)] = Info::plain(ProvCode::Patch);
        }
        if T::ON {
            tr.patch(
                g,
                PatchTrail {
                    ext: Arc::clone(&h.ext),
                    rule: Arc::clone(&h.rule),
                    value: w.slot(),
                    status: if first {
                        TrailStatus::Won
                    } else {
                        TrailStatus::Shadowed
                    },
                },
            );
        }
    }
    if T::ON {
        trace_patch_ties(&scratch.hits, tr);
    }

    // S2 user override (a Clear register reads as no override).
    for g in Group::ALL.iter().copied() {
        if let Some((seq, w)) = regs.user_value(g) {
            eff[ix(g)] = w.slot();
            layer[ix(g)] = Layer::User;
            info[ix(g)] = Info::plain(ProvCode::User);
            if T::ON {
                tr.user(g, seq, &eff[ix(g)]);
            }
        }
    }

    // A6 fill rule: only an Unknown, observed-empty or default value is fillable.
    let fillable = |eff: &Eff, layer: &[Layer; GROUP_COUNT], g: Group| {
        eff[ix(g)].is_empty_like() && matches!(layer[ix(g)], Layer::Observed | Layer::Default)
    };
    let role = eff[ix(Group::Role)].role();
    let role_in = |set: RoleSet| role.is_some_and(|r| set.has(r));
    let mut truncated = GroupMask::EMPTY;

    // S3 derived (a): name from content over the children's final names.
    if fillable(&eff, &layer, Group::Name)
        && role_in(profile.rules.content_named)
        && let Some(c) = name_from_content(nb.children, &profile.limits)
    {
        truncated.set(Group::Name, c.truncated);
        if T::ON {
            tr.content(&c.name, &c.from, &c.tainted_from);
        }
        eff[ix(Group::Name)] = Slot::Val(Val::Name(c.name));
        layer[ix(Group::Name)] = Layer::Derived;
        info[ix(Group::Name)] = c.info;
    }
    // S3 derived (b): `title` evidence names an unnamed control; always tainted.
    {
        let title_ok = fillable(&eff, &layer, Group::Name) && role_in(profile.rules.title_named);
        let mut won = false;
        for e in cands.title() {
            let st = title_status(e, title_ok, won);
            if st == TrailStatus::Won
                && let Some(c) = e.cand()
            {
                won = true;
                eff[ix(Group::Name)] = c.write().slot();
                layer[ix(Group::Name)] = Layer::Derived;
                info[ix(Group::Name)] = title_info(c);
            }
            if T::ON {
                tr.fact(Group::Name, e, st);
            }
        }
    }
    // S4 inferred: same fill rule, evaluated after S3, first candidate of the ranked list.
    for g in [Group::Name, Group::Description] {
        let can_fill = fillable(&eff, &layer, g);
        let mut won = false;
        for e in cands.inferred().iter().filter(|e| e.target() == g) {
            let st = inferred_status(e, can_fill, won);
            if st == TrailStatus::Won
                && let Some(c) = e.cand()
            {
                won = true;
                eff[ix(g)] = c.write().slot();
                layer[ix(g)] = Layer::Inferred;
                info[ix(g)] = inferred_info(c);
            }
            if T::ON {
                tr.fact(g, e, st);
            }
        }
    }

    // Defaults for what is still Unknown; `known` is set by any layer before defaults.
    let mut known = GroupMask::EMPTY;
    for g in Group::ALL.iter().copied() {
        let i = ix(g);
        if eff[i].is_unknown() {
            eff[i] = default_slot(g, profile.default_role);
            layer[i] = Layer::Default;
            info[i] = Info::DEFAULT;
        } else {
            known = known.with(g);
        }
    }

    // R: canonical form. A masked group is recorded and becomes a plain default.
    let before = if T::ON { Some(eff.clone()) } else { None };
    let masked = repair(&profile.rules, profile.default_role, &mut eff);
    for g in Group::ALL.iter().copied() {
        if masked.has(g) {
            info[ix(g)] = Info::DEFAULT;
        }
    }
    if T::ON
        && let Some(before) = &before
    {
        tr.finish(&layer, &winner, masked, before, &eff);
    }

    // Freshness flags are the observed winner's own (F9); a masked or overridden group has none.
    let mut stale = GroupMask::EMPTY;
    let mut fallback = GroupMask::EMPTY;
    for g in Group::ALL.iter().copied() {
        if matches!(layer[ix(g)], Layer::Observed) && !masked.has(g) {
            let s = node.get(g);
            stale.set(g, s.stale);
            fallback.set(g, s.fallback);
        }
    }

    let mut entry = SemEntry::default();
    for g in Group::ALL.iter().copied() {
        let i = ix(g);
        match &eff[i] {
            Slot::Val(v) => v.write_to(&mut entry),
            Slot::Absent | Slot::Unknown => write_default(&mut entry, g, profile.default_role),
        }
        entry.prov.set(g, info[i].prov);
        entry.hedge.set(g, info[i].conf.code());
        entry.taint.set(g, info[i].tier.code());
    }
    entry.known = known;
    entry.stale = stale;
    entry.fallback = fallback;
    Fused {
        entry,
        layer,
        masked,
        truncated,
    }
}

fn trace_patch_ties<T: Tracer>(hits: &[PatchHit], tr: &mut T) {
    for g in Group::ALL.iter().copied() {
        let mine: Vec<&PatchHit> = hits
            .iter()
            .filter(|h| h.group == g && h.write.is_some())
            .collect();
        let Some(top) = mine.first().map(|h| h.spec) else {
            continue;
        };
        let ties: Vec<&&PatchHit> = mine.iter().filter(|h| h.spec == top).collect();
        let differs = ties
            .iter()
            .any(|h| h.write != ties.first().and_then(|f| f.write.clone()));
        if differs {
            tr.conflict(Conflict::PatchTie {
                group: g,
                rules: ties.iter().map(|h| Arc::clone(&h.rule)).collect(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Rules;
    use crate::side::NodeCands;
    use crate::value::Write;
    use gorca_model::sem::{Check, ProvCode};

    fn profile() -> Profile {
        Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria())
    }

    fn alive_regs() -> NodeRegs {
        let mut r = NodeRegs::default();
        r.src_mut(PrimarySlot::P0).apply_life(1, true);
        r
    }

    fn fuse_one(p: &Profile, r: &NodeRegs) -> Fused {
        let c = NodeCands::none();
        let n = s0(p, r, c, &NoQuirks).expect("alive");
        fuse_node(
            p,
            r,
            &n,
            &Neighbours::NONE,
            c,
            &NoPatches,
            &mut Scratch::default(),
        )
    }

    #[test]
    fn dead_node_has_no_s0() {
        let p = profile();
        let c = NodeCands::none();
        assert!(s0(&p, &NodeRegs::default(), c, &NoQuirks).is_none());
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0).apply_life(2, false);
        assert!(s0(&p, &r, c, &NoQuirks).is_none());
    }

    #[test]
    fn primary_priority_and_fallback_flag() {
        let mut p = profile();
        p.default_role = SemRole::Generic;
        let mut r = alive_regs();
        // Only the lower-priority primary answered: its value is used and flagged fallback.
        r.src_mut(PrimarySlot::P1)
            .apply_obs(5, Write::Val(Val::Role(SemRole::Button)));
        let f = fuse_one(&p, &r);
        assert_eq!(f.entry.sem.role, SemRole::Button);
        assert!(f.entry.fallback.has(Group::Role));
        assert_eq!(
            f.entry.prov.get(Group::Role),
            Some(ProvCode::ObservedPrimary)
        );
        // The higher-priority primary answers: no fallback, its value wins.
        r.src_mut(PrimarySlot::P0)
            .apply_obs(1, Write::Val(Val::Role(SemRole::Link)));
        let f = fuse_one(&p, &r);
        assert_eq!(f.entry.sem.role, SemRole::Link);
        assert!(!f.entry.fallback.has(Group::Role));
    }

    #[test]
    fn stale_flag_follows_the_winner() {
        let p = profile();
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Val(Val::Name(Arc::from("Old"))));
        r.src_mut(PrimarySlot::P0).apply_inval(3);
        let f = fuse_one(&p, &r);
        assert!(f.entry.stale.has(Group::Name));
        r.src_mut(PrimarySlot::P0)
            .apply_obs(4, Write::Val(Val::Name(Arc::from("New"))));
        let f = fuse_one(&p, &r);
        assert!(f.entry.stale.is_empty());
        assert_eq!(f.entry.name.as_deref(), Some("New"));
    }

    #[test]
    fn user_outranks_observed_and_clear_restores() {
        let p = profile();
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Val(Val::Name(Arc::from("Obs"))));
        let before = fuse_one(&p, &r);
        r.apply_user(
            Group::Name,
            3,
            crate::register::UserOp::Set(Write::Val(Val::Name(Arc::from("Mine")))),
        );
        let f = fuse_one(&p, &r);
        assert_eq!(f.entry.name.as_deref(), Some("Mine"));
        assert_eq!(f.entry.prov.get(Group::Name), Some(ProvCode::User));
        r.apply_user(Group::Name, 4, crate::register::UserOp::Clear);
        let after = fuse_one(&p, &r);
        assert_eq!(
            after.entry, before.entry,
            "override then Clear restores (F6)"
        );
    }

    #[test]
    fn repair_masks_and_resets_provenance() {
        let mut p = profile();
        p.default_role = SemRole::Generic;
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Val(Val::Role(SemRole::Paragraph)));
        r.src_mut(PrimarySlot::P0)
            .apply_obs(3, Write::Val(Val::Check(Check::On)));
        let f = fuse_one(&p, &r);
        assert!(f.masked.has(Group::Check));
        assert_eq!(f.entry.sem.check, Check::NotCheckable);
        assert_eq!(f.entry.prov.get(Group::Check), Some(ProvCode::Default));
        assert!(f.entry.known.has(Group::Check), "known survives masking");
    }

    struct TwoHits;

    impl PatchSource for TwoHits {
        fn hits(&self, _n: &S0Node, _nb: &Neighbours<'_>, out: &mut Vec<PatchHit>) {
            for v in ["Best", "Worse"] {
                out.push(PatchHit::set(Write::Val(Val::Name(Arc::from(v)))));
            }
        }
    }

    #[test]
    fn the_highest_ranked_patch_hit_of_a_group_wins() {
        let p = profile();
        let r = alive_regs();
        let c = NodeCands::none();
        let n = s0(&p, &r, c, &NoQuirks).expect("alive");
        let f = fuse_node(
            &p,
            &r,
            &n,
            &Neighbours::NONE,
            c,
            &TwoHits,
            &mut Scratch::default(),
        );
        assert_eq!(f.entry.name.as_deref(), Some("Best"));
        assert_eq!(f.entry.prov.get(Group::Name), Some(ProvCode::Patch));
        assert_eq!(f.layer_of(Group::Name), Layer::Patch);
    }

    #[test]
    fn registers_cap_name_and_description_at_ingestion() {
        let mut r = alive_regs();
        let long: Arc<str> = Arc::from("é".repeat(3000));
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Val(Val::Name(Arc::clone(&long))));
        r.src_mut(PrimarySlot::P0)
            .apply_obs(3, Write::Val(Val::Description(Arc::clone(&long))));
        r.apply_user(
            Group::Name,
            4,
            crate::register::UserOp::Set(Write::Val(Val::Name(long))),
        );
        let f = fuse_one(&profile(), &r);
        assert!(f.entry.name.as_deref().is_some_and(|n| n.len() <= 1024));
        assert!(
            f.entry
                .sem
                .description()
                .is_some_and(|n| n.len() <= 1024 && !n.is_empty())
        );
    }

    #[test]
    fn conflicts_are_diagnostics_not_results() {
        use crate::explain::Trace;
        let p = profile();
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Val(Val::Name(Arc::from("A"))));
        r.src_mut(PrimarySlot::P1)
            .apply_obs(3, Write::Val(Val::Name(Arc::from("B"))));
        let c = NodeCands::none();
        let mut tr = Trace::default();
        let n = s0_with(&p, &r, c, &NoQuirks, &mut tr).expect("alive");
        let f = fuse_node_with(
            &p,
            &r,
            &n,
            &Neighbours::NONE,
            c,
            &TwoHits,
            &mut Scratch::default(),
            &mut tr,
        );
        // The patch (rank order) wins; the two equal-specificity patches disagree.
        assert_eq!(f.entry.name.as_deref(), Some("Best"));
        let kinds: Vec<&Conflict> = tr.explain.conflicts.iter().collect();
        assert!(kinds.iter().any(|c| matches!(
            c,
            Conflict::ObservedDisagree {
                group: Group::Name,
                ..
            }
        )));
        assert!(kinds.iter().any(|c| matches!(
            c,
            Conflict::PatchTie {
                group: Group::Name,
                ..
            }
        )));
        // Same inputs without a tracer give the same entry.
        let n2 = s0(&p, &r, c, &NoQuirks).expect("alive");
        let g = fuse_node(
            &p,
            &r,
            &n2,
            &Neighbours::NONE,
            c,
            &TwoHits,
            &mut Scratch::default(),
        );
        assert_eq!(g.entry, f.entry);
    }
}
