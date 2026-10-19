// SPDX-License-Identifier: GPL-3.0-or-later
//! The strata of `FUSE` (ADR-0024 section 2.2): S-1 quirk, S0 observed, S1 patch, S2 user, S3
//! derived, R repair, and the S4 fill rule over a pre-ranked candidate list.
//!
//! `FUSE` is a pure function of the node's registers, its neighbours' already-computed values,
//! the static [`Profile`] and the valid non-primary candidates the side table hands in. It reads
//! no clock, no epoch and no atomic. Evaluate S-1..S2 for all dirty nodes first, then S3 and R
//! bottom-up (children before parents): each value is computed once from final inputs, so the
//! result is unique (Lemma 2).
//!
//! What this module does **not** do (B1b): decide whether a Secondary, Inferred or evidence
//! fact is *valid* (binding, generation, epoch, `vis_rev`, TTL, floor) or rank Inferred facts.
//! Those produce [`NodeCands`], whose `title` and `inferred` lists arrive pre-ranked and
//! pre-filtered. The static `inexpressible` candidacy of Secondaries is decided here (S0).

use std::cmp::Ordering;
use std::sync::Arc;

use gorca_model::sem::{
    Avail, Check, ConfClass, Expand, GROUP_COUNT, Group, GroupMask, Orientation, Press, ProvCode,
    Select, SemEntry, SemFlags, SemRole, Sort, TrustTier,
};

use crate::profile::Profile;
use crate::register::{MAX_PRIMARIES, NodeRegs, PrimarySlot};
use crate::repair::{Eff, ix, repair, unknown_eff};
use crate::value::{Info, Seq, Slot, Val, Write, hedge_cap, write_default};

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
// Non-primary candidates (valid, produced by the B1b side table)
// ------------------------------------------------------------------------------------------------

/// One valid (status `ok`) non-primary candidate value for a group of a node. `conf` is the
/// `ConfClass` after the tier cap; the group is the value's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactCand {
    /// The value (or an explicit `Absent`) the fact proposes; the group is the write's.
    pub write: Write,
    pub conf: ConfClass,
    pub tier: TrustTier,
    /// Stable ordering key of the source (its stable name rank; never a recycled `SourceId`).
    pub src_key: u32,
    pub seq: Seq,
}

impl FactCand {
    pub const fn group(&self) -> Group {
        self.write.group()
    }
}

/// The valid non-primary candidates of one node.
#[derive(Copy, Clone, Debug, Default)]
pub struct NodeCands<'a> {
    /// Secondary facts, any order (S0 ranks them). Only groups in the profile's `inexpressible`
    /// row are candidates; the rest are ignored here.
    pub secondary: &'a [FactCand],
    /// `title` evidence naming an unnamed control (S3), best first. Values are `Val::Name`.
    pub title: &'a [FactCand],
    /// Inferred `name`/`description` facts and rule outputs (S4), best first.
    pub inferred: &'a [FactCand],
}

fn cmp_tier(a: TrustTier, b: TrustTier) -> Ordering {
    if a.is_more_trusted_than(b) {
        Ordering::Less
    } else if b.is_more_trusted_than(a) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn cmp_conf(a: ConfClass, b: ConfClass) -> Ordering {
    if a.is_stronger_than(b) {
        Ordering::Less
    } else if b.is_stronger_than(a) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// Order inside S0 among Secondaries: more trusted tier, stronger class, source key, seq, value.
fn cmp_secondary(a: &FactCand, b: &FactCand) -> Ordering {
    cmp_tier(a.tier, b.tier)
        .then_with(|| cmp_conf(a.conf, b.conf))
        .then_with(|| a.src_key.cmp(&b.src_key))
        .then_with(|| a.seq.cmp(&b.seq))
        .then_with(|| a.write.tie_cmp(&b.write))
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
    secondary: &[FactCand],
    quirks: &Q,
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
        s0_group(profile, regs, &recs, secondary, g)
    });
    Some(S0Node { slots })
}

fn s0_group(
    profile: &Profile,
    regs: &NodeRegs,
    recs: &[Record; MAX_PRIMARIES],
    secondary: &[FactCand],
    g: Group,
) -> S0Slot {
    let mut fallback = false;
    for &p in profile.pi(g) {
        let rec = &recs[p.index()];
        let v = &rec.slots[ix(g)];
        if v.is_unknown() {
            fallback = true;
            continue;
        }
        let src = regs.src(p);
        return S0Slot {
            slot: v.clone(),
            info: Info::plain(ProvCode::ObservedPrimary),
            src: Some(p),
            seq: src.get(g).map(|r| r.seq),
            fallback,
            stale: src.is_stale(g),
            hit: rec.hit[ix(g)],
        };
    }
    // Every primary is Unknown. A Secondary is a candidate only for a statically inexpressible
    // group (H-3); a dynamic Unknown of an expressible group never admits one.
    if profile.inexpressible.has(g) {
        let best = secondary
            .iter()
            .filter(|c| c.group() == g)
            .min_by(|a, b| cmp_secondary(a, b));
        if let Some(c) = best {
            return S0Slot {
                slot: c.write.slot(),
                info: Info {
                    prov: ProvCode::ObservedSecondary,
                    conf: hedge_cap(c.conf, c.tier),
                    tier: c.tier,
                },
                src: None,
                seq: Some(c.seq),
                fallback: false,
                stale: false,
                hit: 0,
            };
        }
    }
    S0Slot::unknown()
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
    /// The child's S0 (None when the child is not alive).
    pub s0: Option<&'a S0Node>,
    /// The child's final entry (None when not alive); S3 name from content reads it.
    pub fused: Option<&'a SemEntry>,
}

/// A patch edit that applies to the node: the value its group takes.
#[derive(Clone, Debug)]
pub struct PatchHit {
    pub write: Write,
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
        Group::Current => Slot::Val(Val::Current(gorca_model::sem::Current::No)),
        Group::Invalid => Slot::Val(Val::Invalid(gorca_model::sem::Invalid::No)),
        Group::Popup => Slot::Val(Val::Popup(gorca_model::sem::Popup::No)),
        Group::Avail => Slot::Val(Val::Avail(Avail::NotApplicable)),
        Group::Flags => Slot::Val(Val::Flags(SemFlags::empty())),
        Group::Orientation => Slot::Val(Val::Orientation(Orientation::NotApplicable)),
        Group::Sort => Slot::Val(Val::Sort(Sort::None)),
        Group::Level | Group::SetPos | Group::Grid | Group::Live => Slot::Absent,
    }
}

/// Strata S1, S2, S3, S4 (fill), defaults and R for one alive node.
pub fn fuse_node<P: PatchSource>(
    profile: &Profile,
    regs: &NodeRegs,
    node: &S0Node,
    nb: &Neighbours<'_>,
    cands: &NodeCands<'_>,
    patches: &P,
    scratch: &mut Scratch,
) -> Fused {
    let mut eff = unknown_eff();
    let mut layer = [Layer::Default; GROUP_COUNT];
    let mut info = [Info::DEFAULT; GROUP_COUNT];
    for g in Group::ALL.iter().copied() {
        let s = node.get(g);
        let i = ix(g);
        eff[i] = s.slot.clone();
        info[i] = s.info;
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
    for h in &scratch.hits {
        let g = h.write.group();
        if !patched.has(g) {
            patched = patched.with(g);
            eff[ix(g)] = h.write.slot();
            layer[ix(g)] = Layer::Patch;
            info[ix(g)] = Info::plain(ProvCode::Patch);
        }
    }

    // S2 user override (a Clear register reads as no override).
    for g in Group::ALL.iter().copied() {
        if let Some((_, w)) = regs.user_value(g) {
            eff[ix(g)] = w.slot();
            layer[ix(g)] = Layer::User;
            info[ix(g)] = Info::plain(ProvCode::User);
        }
    }

    // A6 fill rule: only an Unknown, observed-empty or default value is fillable.
    let fillable = |eff: &Eff, layer: &[Layer; GROUP_COUNT], g: Group| {
        eff[ix(g)].is_empty_like() && matches!(layer[ix(g)], Layer::Observed | Layer::Default)
    };
    let role = eff[ix(Group::Role)].role();
    let role_in = |set: crate::profile::RoleSet| role.is_some_and(|r| set.has(r));

    // S3 derived (a): name from content over the children's final names.
    if fillable(&eff, &layer, Group::Name)
        && role_in(profile.rules.content_named)
        && let Some((name, inf)) = name_from_content(nb.children)
    {
        eff[ix(Group::Name)] = Slot::Val(Val::Name(name));
        layer[ix(Group::Name)] = Layer::Derived;
        info[ix(Group::Name)] = inf;
    }
    // S3 derived (b): `title` evidence names an unnamed control; always tainted (evidence).
    if fillable(&eff, &layer, Group::Name) && role_in(profile.rules.title_named) {
        let win = cands
            .title
            .iter()
            .find(|c| c.group() == Group::Name && !c.write.is_empty_like());
        if let Some(c) = win {
            eff[ix(Group::Name)] = c.write.slot();
            layer[ix(Group::Name)] = Layer::Derived;
            info[ix(Group::Name)] = Info {
                prov: ProvCode::DerivedTainted,
                conf: hedge_cap(c.conf, c.tier),
                tier: c.tier,
            };
        }
    }
    // S4 inferred: same fill rule, evaluated after S3, first candidate of the ranked list.
    for g in [Group::Name, Group::Description] {
        if !fillable(&eff, &layer, g) {
            continue;
        }
        let win = cands
            .inferred
            .iter()
            .find(|c| c.group() == g && !c.write.is_empty_like());
        if let Some(c) = win {
            eff[ix(g)] = c.write.slot();
            layer[ix(g)] = Layer::Inferred;
            info[ix(g)] = Info {
                prov: ProvCode::Inferred,
                conf: hedge_cap(c.conf, c.tier),
                tier: c.tier,
            };
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
    let masked = repair(&profile.rules, profile.default_role, &mut eff);
    for g in Group::ALL.iter().copied() {
        if masked.has(g) {
            info[ix(g)] = Info::DEFAULT;
        }
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
    }
}

/// Name from content (S3): the children's final non-empty names joined with a space. Untainted
/// children come first: tainted names (Secondary, Inferred, DerivedTainted) are used only if no
/// untainted part exists, and then the result is `DerivedTainted` with the weakest input class and
/// the least trusted tier (math section 10.4 item 1).
fn name_from_content(children: &[ChildView<'_>]) -> Option<(Arc<str>, Info)> {
    let part = |c: &ChildView<'_>| -> Option<(Arc<str>, bool, ConfClass, TrustTier)> {
        let f = c.fused?;
        let name = f.name.as_ref()?;
        if name.is_empty() {
            return None;
        }
        let tainted = f.prov.get(Group::Name).is_some_and(Info::is_tainted_prov);
        Some((
            Arc::clone(name),
            tainted,
            f.hedge.conf(Group::Name),
            f.taint.tier(Group::Name),
        ))
    };
    let any_clean = children
        .iter()
        .filter_map(part)
        .any(|(_, tainted, _, _)| !tainted);
    let mut out = String::new();
    let mut n = 0usize;
    let mut conf = ConfClass::Asserted;
    let mut tier = TrustTier::T0Core;
    for (name, tainted, c, t) in children.iter().filter_map(part) {
        // With an untainted part present only untainted parts count; otherwise all are tainted.
        if any_clean && tainted {
            continue;
        }
        if n > 0 {
            out.push(' ');
        }
        out.push_str(&name);
        n += 1;
        if tainted {
            conf = conf.weaker(c);
            tier = tier.least_trusted(t);
        }
    }
    if n == 0 {
        return None;
    }
    let info = if any_clean {
        Info::plain(ProvCode::Derived)
    } else {
        Info {
            prov: ProvCode::DerivedTainted,
            conf: hedge_cap(conf, tier),
            tier,
        }
    };
    Some((Arc::from(out), info))
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::Check;

    use super::*;
    use crate::profile::Rules;
    use crate::value::Write;

    fn profile() -> Profile {
        Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria())
    }

    fn alive_regs() -> NodeRegs {
        let mut r = NodeRegs::default();
        r.src_mut(PrimarySlot::P0).apply_life(1, true);
        r
    }

    fn fuse_one(p: &Profile, r: &NodeRegs, c: &NodeCands<'_>) -> Fused {
        let n = s0(p, r, c.secondary, &NoQuirks).expect("alive");
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
        assert!(s0(&p, &NodeRegs::default(), &[], &NoQuirks).is_none());
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0).apply_life(2, false);
        assert!(s0(&p, &r, &[], &NoQuirks).is_none());
    }

    #[test]
    fn primary_priority_and_fallback_flag() {
        let mut p = profile();
        p.default_role = SemRole::Generic;
        let mut r = alive_regs();
        // Only the lower-priority primary answered: its value is used and flagged fallback.
        r.src_mut(PrimarySlot::P1)
            .apply_obs(5, Write::Val(Val::Role(SemRole::Button)));
        let f = fuse_one(&p, &r, &NodeCands::default());
        assert_eq!(f.entry.sem.role, SemRole::Button);
        assert!(f.entry.fallback.has(Group::Role));
        assert_eq!(
            f.entry.prov.get(Group::Role),
            Some(ProvCode::ObservedPrimary)
        );
        // The higher-priority primary answers: no fallback, its value wins.
        r.src_mut(PrimarySlot::P0)
            .apply_obs(1, Write::Val(Val::Role(SemRole::Link)));
        let f = fuse_one(&p, &r, &NodeCands::default());
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
        let f = fuse_one(&p, &r, &NodeCands::default());
        assert!(f.entry.stale.has(Group::Name));
        r.src_mut(PrimarySlot::P0)
            .apply_obs(4, Write::Val(Val::Name(Arc::from("New"))));
        let f = fuse_one(&p, &r, &NodeCands::default());
        assert!(f.entry.stale.is_empty());
        assert_eq!(f.entry.name.as_deref(), Some("New"));
    }

    fn live_cand() -> FactCand {
        FactCand {
            write: Write::Val(Val::Live(gorca_model::sem::Live {
                politeness: gorca_model::event::Politeness::Assertive,
                atomic: false,
                relevant: gorca_model::sem::Relevant::empty(),
                root: false,
            })),
            conf: ConfClass::High,
            tier: TrustTier::T3Untrusted,
            src_key: 1,
            seq: 1,
        }
    }

    #[test]
    fn secondary_wins_only_an_inexpressible_group_and_is_hedged() {
        let mut p = profile();
        let cand = [live_cand()];
        let c = NodeCands {
            secondary: &cand,
            ..NodeCands::default()
        };
        // Expressible group: ignored.
        let f = fuse_one(&p, &alive_regs(), &c);
        assert_eq!(f.entry.sem.live, None);
        assert_eq!(f.entry.prov.get(Group::Live), Some(ProvCode::Default));
        // Inexpressible: wins, hedged Probable (T3 cap) and tainted T3.
        p.inexpressible = GroupMask::EMPTY.with(Group::Live);
        let f = fuse_one(&p, &alive_regs(), &c);
        assert!(f.entry.sem.live.is_some());
        assert_eq!(
            f.entry.prov.get(Group::Live),
            Some(ProvCode::ObservedSecondary)
        );
        assert_eq!(f.entry.hedge.conf(Group::Live), ConfClass::Probable);
        assert_eq!(f.entry.taint.tier(Group::Live), TrustTier::T3Untrusted);
        // A dynamic Unknown is not the same as "inexpressible": a known primary value wins.
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Absent(Group::Live));
        let f = fuse_one(&p, &r, &c);
        assert_eq!(
            f.entry.prov.get(Group::Live),
            Some(ProvCode::ObservedPrimary)
        );
    }

    #[test]
    fn user_and_patch_outrank_observed_and_clear_restores() {
        let p = profile();
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Val(Val::Name(Arc::from("Obs"))));
        let before = fuse_one(&p, &r, &NodeCands::default());
        r.apply_user(
            Group::Name,
            3,
            crate::register::UserOp::Set(Write::Val(Val::Name(Arc::from("Mine")))),
        );
        let f = fuse_one(&p, &r, &NodeCands::default());
        assert_eq!(f.entry.name.as_deref(), Some("Mine"));
        assert_eq!(f.entry.prov.get(Group::Name), Some(ProvCode::User));
        r.apply_user(Group::Name, 4, crate::register::UserOp::Clear);
        let after = fuse_one(&p, &r, &NodeCands::default());
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
        let f = fuse_one(&p, &r, &NodeCands::default());
        assert!(f.masked.has(Group::Check));
        assert_eq!(f.entry.sem.check, Check::NotCheckable);
        assert_eq!(f.entry.prov.get(Group::Check), Some(ProvCode::Default));
        assert!(f.entry.known.has(Group::Check), "known survives masking");
    }

    struct TwoHits;

    impl PatchSource for TwoHits {
        fn hits(&self, _n: &S0Node, _nb: &Neighbours<'_>, out: &mut Vec<PatchHit>) {
            for v in ["Best", "Worse"] {
                out.push(PatchHit {
                    write: Write::Val(Val::Name(Arc::from(v))),
                });
            }
        }
    }

    #[test]
    fn the_highest_ranked_patch_hit_of_a_group_wins() {
        let p = profile();
        let r = alive_regs();
        let n = s0(&p, &r, &[], &NoQuirks).expect("alive");
        let f = fuse_node(
            &p,
            &r,
            &n,
            &Neighbours::NONE,
            &NodeCands::default(),
            &TwoHits,
            &mut Scratch::default(),
        );
        assert_eq!(f.entry.name.as_deref(), Some("Best"));
        assert_eq!(f.entry.prov.get(Group::Name), Some(ProvCode::Patch));
        assert_eq!(f.layer_of(Group::Name), Layer::Patch);
    }

    fn view(e: &SemEntry) -> ChildView<'_> {
        ChildView {
            s0: None,
            fused: Some(e),
        }
    }

    fn child_entry(name: &str, prov: ProvCode, conf: ConfClass, tier: TrustTier) -> SemEntry {
        let mut e = SemEntry {
            name: Some(Arc::from(name)),
            ..SemEntry::default()
        };
        e.prov.set(Group::Name, prov);
        e.hedge.set(Group::Name, conf.code());
        e.taint.set(Group::Name, tier.code());
        e
    }

    #[test]
    fn name_from_content_prefers_untainted_children() {
        let clean = child_entry(
            "Close",
            ProvCode::ObservedPrimary,
            ConfClass::Asserted,
            TrustTier::T0Core,
        );
        let dirty = child_entry(
            "Logo",
            ProvCode::Inferred,
            ConfClass::High,
            TrustTier::T2FirstParty,
        );
        let kids = [view(&clean), view(&dirty)];
        let (name, info) = name_from_content(&kids).expect("name");
        assert_eq!(
            &*name, "Close",
            "the tainted child must not leak into an untainted name"
        );
        assert_eq!(info.prov, ProvCode::Derived);
        // Only tainted parts: used, DerivedTainted with the weakest class and least trusted tier.
        let only = [view(&dirty)];
        let (name, info) = name_from_content(&only).expect("name");
        assert_eq!(&*name, "Logo");
        assert_eq!(info.prov, ProvCode::DerivedTainted);
        assert_eq!(info.conf, ConfClass::High);
        assert_eq!(info.tier, TrustTier::T2FirstParty);
        let t3 = child_entry(
            "X",
            ProvCode::Inferred,
            ConfClass::High,
            TrustTier::T3Untrusted,
        );
        let (_, info) = name_from_content(&[view(&t3)]).expect("name");
        assert_eq!(info.conf, ConfClass::Probable, "T3 caps at Probable");
        assert!(name_from_content(&[]).is_none());
    }

    #[test]
    fn s4_fills_only_a_fillable_group_and_title_beats_it() {
        let mut p = profile();
        p.default_role = SemRole::Generic;
        let mut r = alive_regs();
        r.src_mut(PrimarySlot::P0)
            .apply_obs(2, Write::Val(Val::Role(SemRole::Button)));
        let inf = [FactCand {
            write: Write::Val(Val::Name(Arc::from("Save"))),
            conf: ConfClass::High,
            tier: TrustTier::T2FirstParty,
            src_key: 0,
            seq: 1,
        }];
        let title = [FactCand {
            write: Write::Val(Val::Name(Arc::from("Delete"))),
            conf: ConfClass::Probable,
            tier: TrustTier::T3Untrusted,
            src_key: 0,
            seq: 1,
        }];
        let c = NodeCands {
            inferred: &inf,
            ..NodeCands::default()
        };
        let f = fuse_one(&p, &r, &c);
        assert_eq!(f.entry.name.as_deref(), Some("Save"));
        assert_eq!(f.entry.prov.get(Group::Name), Some(ProvCode::Inferred));
        let c = NodeCands {
            inferred: &inf,
            title: &title,
            ..NodeCands::default()
        };
        let f = fuse_one(&p, &r, &c);
        assert_eq!(f.entry.name.as_deref(), Some("Delete"));
        assert_eq!(
            f.entry.prov.get(Group::Name),
            Some(ProvCode::DerivedTainted)
        );
        // A non-empty primary name is protected (F12).
        r.src_mut(PrimarySlot::P0)
            .apply_obs(3, Write::Val(Val::Name(Arc::from("Real"))));
        let f = fuse_one(&p, &r, &c);
        assert_eq!(f.entry.name.as_deref(), Some("Real"));
        assert_eq!(
            f.entry.prov.get(Group::Name),
            Some(ProvCode::ObservedPrimary)
        );
    }
}
