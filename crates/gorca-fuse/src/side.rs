// SPDX-License-Identifier: GPL-3.0-or-later
//! The evidence side table (ADR-0024 sections 2.3, 2.4, 2.12): Secondary, Inferred and evidence
//! registers keyed by `(source, anchor, group)`, the binding pass and the validity filter.
//!
//! This module is the **only** place that can build a [`FactCand`]: its fields are private and the
//! constructor runs the whole validity filter (retraction, epoch snapshot, binding quality and
//! generation, `vis_rev` and document staleness, TTL, confidence floor). Strata S0, S3 and S4 get
//! candidates only through [`NodeCands`], so a fact that is stale, masked, rebound, expired, below
//! its floor or bound too weakly cannot win by construction (FS-5, F11, F13, F15, F18).
//!
//! Every non-primary register is a join-semilattice under `max` over `(seq, tie)` (Lemma 1); the
//! binding is recomputed from the anchor index at `FUSE` time and never cached in the register,
//! the generation the writer stamped at admission travels with the register and is compared with
//! the current one (stale when different).

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use gorca_model::fact::{Basis, BindGen, SourceId};
use gorca_model::sem::{
    BindQuality, ConfClass, EvidenceKind, Group, GroupClass, GroupCode, SourceClass, TrustTier,
};

use crate::value::{MAX_TEXT_BYTES, Seq, Val, Write};

/// A node of the writer: its local index. The anchor index maps keys to nodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct NodeIdx(pub u32);

/// How a non-primary source names a node, as a hashable, totally ordered key. Variants are in
/// alphabetical order of their kind (`content`, `dom`, `exact`, `node`, `region`, `structural`):
/// the S4 tie-break compares anchors, so the order is part of the contract.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum AnchorId {
    Content(u128),
    /// An HTML `id`; `count` is the add-on's count of that id in its frame and shadow root.
    Dom {
        id: Arc<str>,
        count: u16,
        frame: u64,
        shadow: u64,
    },
    Exact {
        space: u16,
        id: u128,
    },
    /// Minted by a Primary of this scope.
    Node(NodeIdx),
    Region {
        window: u64,
        rect: (i32, i32, u32, u32),
        capture: u64,
    },
    Structural {
        path: u64,
        role: u16,
        name: u64,
    },
}

/// The current binding of an anchor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    pub node: Option<NodeIdx>,
    pub quality: BindQuality,
    pub generation: BindGen,
}

impl Resolved {
    pub const UNBOUND: Resolved = Resolved {
        node: None,
        quality: BindQuality::Unbound,
        generation: BindGen(0),
    };
}

/// Resolves anchors against the writer's live nodes. Pure in the registers it reads: the binding
/// is recomputed at `FUSE` time, never cached in a fact register (math section 10.2).
pub trait AnchorIndex {
    fn resolve(&self, anchor: &AnchorId) -> Resolved;
}

/// Lifecycle state of a source in the epoch snapshot (ADR-0024 section 2.6).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SourceState {
    Active,
    Throttled,
    Detached,
    Paused,
    Quarantined,
    Revoked,
}

/// The epoch snapshot the writer passes into `FUSE` (M-10: no atomic is read inside `FUSE`).
#[derive(Clone, Debug, Default)]
pub struct EpochSnapshot {
    states: HashMap<SourceId, SourceState>,
}

impl EpochSnapshot {
    pub fn set(&mut self, source: SourceId, state: SourceState) {
        self.states.insert(source, state);
    }

    /// The state of `source`, or `None` when the snapshot has no entry for it. The validity
    /// filter fails closed: a source missing from the snapshot is masked, never assumed active.
    pub fn state(&self, source: SourceId) -> Option<SourceState> {
        self.states.get(&source).copied()
    }
}

/// Static facts about a registered non-primary source. `name` is the stable name that orders
/// ties: slots are recycled, so `SourceId` order depends on allocation order and must never
/// decide a ranking (review F-6).
#[derive(Clone, Debug)]
pub struct SourceInfo {
    pub id: SourceId,
    pub name: Arc<str>,
    pub class: SourceClass,
    pub tier: TrustTier,
    /// Position of the source's kind inside S4 ranking (static data).
    pub kind_order: u8,
    /// The source's grant data: confidence floors and minimum binding per group class. Owned by
    /// the side table so a writer cannot assert a class, a floor or a binding minimum per fact.
    pub grant: Grant,
}

/// Group class of a grant: floors and binding minima are per class (ADR-0024 section 2.5).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GrantClass {
    /// `name` and `description`.
    Name,
    /// Every other semantic group.
    Sem,
    /// Evidence other than `visual.*`.
    Evidence,
    /// `visual.*` evidence.
    Visual,
}

/// The grant data of one source.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Grant {
    floor: [u16; 4],
    min_bind: [BindQuality; 4],
}

impl Grant {
    /// Floors and minima per class `[name, sem, evidence, visual]`.
    pub const fn new(floor: [u16; 4], min_bind: [BindQuality; 4]) -> Grant {
        Grant { floor, min_bind }
    }

    /// Floor 800 for every class; name and semantic facts need a `Unique` binding, evidence a
    /// `Heuristic` one and visual evidence a `Geometric` one.
    pub const fn standard() -> Grant {
        Grant::new(
            [800; 4],
            [
                BindQuality::Unique,
                BindQuality::Unique,
                BindQuality::Heuristic,
                BindQuality::Geometric,
            ],
        )
    }

    const fn idx(c: GrantClass) -> usize {
        match c {
            GrantClass::Name => 0,
            GrantClass::Sem => 1,
            GrantClass::Evidence => 2,
            GrantClass::Visual => 3,
        }
    }

    /// The `p_milli` floor of a class.
    pub fn floor(&self, c: GrantClass) -> u16 {
        self.floor.get(Self::idx(c)).copied().unwrap_or(u16::MAX)
    }

    /// The minimum binding quality of a class.
    pub fn min_bind(&self, c: GrantClass) -> BindQuality {
        self.min_bind
            .get(Self::idx(c))
            .copied()
            .unwrap_or(BindQuality::Exact)
    }
}

/// The grant class of a group code.
pub fn grant_class(code: GroupCode) -> GrantClass {
    match code.classify() {
        GroupClass::Evidence(EvidenceKind::VisualOcrText | EvidenceKind::VisualLabelGuess) => {
            GrantClass::Visual
        }
        GroupClass::Evidence(_) => GrantClass::Evidence,
        GroupClass::Semantic(Group::Name | Group::Description) => GrantClass::Name,
        GroupClass::Semantic(_) | GroupClass::Unregistered(_) => GrantClass::Sem,
    }
}

/// The payload of a non-primary register.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Payload {
    /// Back to Unknown.
    Retract,
    /// A value (or `Absent`) for a semantic group.
    Sem(Write),
    /// An evidence payload (never presented directly).
    Evidence(gorca_model::fact::EvidenceValue),
}

impl Payload {
    fn tie_key(&self) -> (u8, String) {
        match self {
            Payload::Retract => (1, "<retract>".to_owned()),
            Payload::Sem(w) => w.tie_key(),
            Payload::Evidence(gorca_model::fact::EvidenceValue::Text(s)) => (1, s.to_string()),
            Payload::Evidence(e) => (1, format!("{e:?}")),
        }
    }

    fn text(&self) -> Option<&Arc<str>> {
        match self {
            Payload::Evidence(gorca_model::fact::EvidenceValue::Text(s))
            | Payload::Sem(Write::Val(Val::Name(s) | Val::Description(s))) => Some(s),
            _ => None,
        }
    }

    fn is_empty_like(&self) -> bool {
        match self {
            Payload::Sem(w) => w.is_empty_like(),
            Payload::Evidence(gorca_model::fact::EvidenceValue::Text(s)) => s.is_empty(),
            Payload::Retract | Payload::Evidence(_) => false,
        }
    }
}

/// One admitted fact as the side table stores it. The writer builds this from
/// `gorca_fact_api::Admitted`; the only writer-stamped judgement is `bind_gen`. The class, the
/// floor and the binding minimum are the side table's (registered per source), and the binding is
/// recomputed at `FUSE` time.
#[derive(Clone, Debug)]
pub struct FactIn {
    pub source: SourceId,
    pub anchor: AnchorId,
    pub group: GroupCode,
    pub seq: Seq,
    pub payload: Payload,
    /// The source's calibrated precision estimate. The class, the floor check and the binding
    /// minimum are computed by the side table from this and the source's [`Grant`].
    pub p_milli: u16,
    /// `Capture` and `Snapshot` bases are rejected at insert until their owners exist.
    pub basis: Basis,
    /// The `BindGen` the writer stamped at admission.
    pub bind_gen: BindGen,
    pub admitted_at_ms: u64,
    /// Required for Inferred (the admission default applies before this point).
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum SideError {
    #[error("unknown source")]
    UnknownSource,
    #[error("source class is never fused")]
    NotFusable,
    #[error("evidence side table is full")]
    Full,
    #[error("capture and snapshot bases are not supported yet")]
    UnsupportedBasis,
    #[error("another source already uses this stable name")]
    DuplicateName,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct SideKey {
    source: SourceId,
    anchor: AnchorId,
    group: GroupCode,
}

#[derive(Clone, Debug)]
struct FactReg {
    seq: Seq,
    fact: FactIn,
}

impl FactReg {
    fn cmp_key(&self, other: &FactReg) -> Ordering {
        self.seq.cmp(&other.seq).then_with(|| {
            self.fact
                .payload
                .tie_key()
                .cmp(&other.fact.payload.tie_key())
        })
    }
}

/// The per-writer side table.
#[derive(Debug)]
pub struct SideTable {
    sources: BTreeMap<SourceId, SourceInfo>,
    regs: HashMap<SideKey, FactReg>,
    /// Reverse index: the registers of each anchor (incremental maintenance reads it).
    by_anchor: HashMap<AnchorId, Vec<SideKey>>,
    cap: usize,
}

impl Default for SideTable {
    fn default() -> SideTable {
        SideTable::with_cap(1 << 20)
    }
}

impl SideTable {
    /// A table that refuses a new register once `cap` registers exist (FS-10 bound).
    pub fn with_cap(cap: usize) -> SideTable {
        SideTable {
            sources: BTreeMap::new(),
            regs: HashMap::new(),
            by_anchor: HashMap::new(),
            cap,
        }
    }

    /// Register a source. Stable names are unique: they order ranking ties, so two sources must
    /// never share one (the tie-break would otherwise fall to allocation order).
    pub fn set_source(&mut self, info: SourceInfo) -> Result<(), SideError> {
        if self
            .sources
            .values()
            .any(|s| s.id != info.id && s.name == info.name)
        {
            return Err(SideError::DuplicateName);
        }
        self.sources.insert(info.id, info);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.regs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regs.is_empty()
    }

    /// Every register rendered and sorted: a stable fingerprint of the table for equality checks
    /// (the table is a function of the set of facts, Lemma 1).
    pub fn dump(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .regs
            .iter()
            .map(|(k, r)| format!("{k:?} => {:?}", r.fact))
            .collect();
        v.sort();
        v
    }

    /// Join a fact into its register. Returns true iff the register changed. Registers are
    /// joined by `max (seq, tie)`, so the table is a function of the set of facts.
    pub fn insert(&mut self, mut f: FactIn) -> Result<bool, SideError> {
        let info = self
            .sources
            .get(&f.source)
            .ok_or(SideError::UnknownSource)?;
        match info.class {
            SourceClass::Secondary | SourceClass::Inferred => {}
            SourceClass::Primary | SourceClass::Annotation => return Err(SideError::NotFusable),
        }
        if matches!(f.basis, Basis::Capture(..) | Basis::Snapshot(_)) {
            return Err(SideError::UnsupportedBasis);
        }
        cap_payload(&mut f.payload);
        let key = SideKey {
            source: f.source,
            anchor: f.anchor.clone(),
            group: f.group,
        };
        let new = FactReg {
            seq: f.seq,
            fact: f,
        };
        if let Some(cur) = self.regs.get_mut(&key) {
            if new.cmp_key(cur) == Ordering::Greater {
                *cur = new;
                return Ok(true);
            }
            return Ok(false);
        }
        if self.regs.len() >= self.cap {
            return Err(SideError::Full);
        }
        self.by_anchor
            .entry(key.anchor.clone())
            .or_default()
            .push(key.clone());
        self.regs.insert(key, new);
        Ok(true)
    }

    /// Release a source: forget its registration **and every register it owns**. A source that
    /// reconnects under the same stable name must call this (or [`SideTable::reconnect`]) before
    /// registering its new id, because stable names are unique. Returns the anchors that lost
    /// registers (their nodes must be re-fused).
    pub fn release_source(&mut self, id: SourceId) -> Vec<AnchorId> {
        self.sources.remove(&id);
        let keys: Vec<SideKey> = self
            .regs
            .keys()
            .filter(|k| k.source == id)
            .cloned()
            .collect();
        let mut anchors = Vec::new();
        for k in keys {
            self.regs.remove(&k);
            if let Some(list) = self.by_anchor.get_mut(&k.anchor) {
                list.retain(|x| x != &k);
                if list.is_empty() {
                    self.by_anchor.remove(&k.anchor);
                }
            }
            anchors.push(k.anchor);
        }
        anchors.sort();
        anchors.dedup();
        anchors
    }

    /// A source reconnects: the old registration and its registers go first, then the new one is
    /// registered (the explicit "release before `set_source`" path). Returns the anchors whose
    /// registers were dropped. On error nothing was released.
    pub fn reconnect(
        &mut self,
        old: SourceId,
        info: SourceInfo,
    ) -> Result<Vec<AnchorId>, SideError> {
        // Validate against every *other* source first, so a failure leaves the table untouched.
        if self
            .sources
            .values()
            .any(|s| s.id != old && s.id != info.id && s.name == info.name)
        {
            return Err(SideError::DuplicateName);
        }
        let anchors = self.release_source(old);
        self.set_source(info)?;
        Ok(anchors)
    }

    /// The registered sources.
    pub fn source_ids(&self) -> impl Iterator<Item = SourceId> + '_ {
        self.sources.keys().copied()
    }

    /// True when `a` still has at least one register.
    pub fn has_anchor(&self, a: &AnchorId) -> bool {
        self.by_anchor.contains_key(a)
    }

    /// Anchors with at least one register of `id`.
    pub fn anchors_of_source(&self, id: SourceId) -> Vec<AnchorId> {
        let mut v: Vec<AnchorId> = self
            .regs
            .keys()
            .filter(|k| k.source == id)
            .map(|k| k.anchor.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Anchors with a register whose TTL expires in `(from_ms, to_ms]`.
    pub fn expiring(&self, from_ms: u64, to_ms: u64) -> Vec<AnchorId> {
        let mut v: Vec<AnchorId> = self
            .regs
            .iter()
            .filter(|(_, r)| {
                r.fact.ttl_ms.is_some_and(|t| {
                    let at = r.fact.admitted_at_ms.saturating_add(t);
                    at > from_ms && at <= to_ms
                })
            })
            .map(|(k, _)| k.anchor.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Every anchor that has registers.
    pub fn anchors(&self) -> impl Iterator<Item = &AnchorId> {
        self.by_anchor.keys()
    }

    /// The ranked entries of `node`, given the anchors currently bound to it and the current
    /// resolution of each. Same result as `bind(..).node(node, ctx)`, computed from the anchors
    /// only (incremental maintenance).
    pub fn ranked_for<'x>(
        &self,
        node: NodeIdx,
        anchors: impl IntoIterator<Item = &'x AnchorId>,
        resolve: &dyn Fn(&AnchorId) -> Resolved,
        ctx: &FuseCtx<'_>,
    ) -> Ranked {
        let mut pairs: Vec<(&SideKey, &FactReg)> = Vec::new();
        for a in anchors {
            if let Some(keys) = self.by_anchor.get(a) {
                for k in keys {
                    if let Some(r) = self.regs.get(k) {
                        pairs.push((k, r));
                    }
                }
            }
        }
        pairs.sort_by(|a, b| self.key_order(a.0, b.0));
        let mut r = Ranked::default();
        for (k, reg) in pairs {
            let Some(info) = self.sources.get(&k.source) else {
                continue;
            };
            let bf = Bf {
                info,
                key: k,
                reg,
                res: resolve(&k.anchor),
            };
            r.push(build_entry(&bf, node, ctx));
        }
        r.sort();
        r
    }

    /// Resolve every register against `index`: nodes with facts, and the parked ones.
    pub fn bind<'a, I: AnchorIndex>(&'a self, index: &I) -> Bound<'a> {
        let mut keys: Vec<(&SideKey, &FactReg)> = self.regs.iter().collect();
        // Deterministic order independent of hash-map iteration: stable source name, anchor, group.
        keys.sort_by(|a, b| self.key_order(a.0, b.0));
        let mut by_node: HashMap<NodeIdx, Vec<Bf<'a>>> = HashMap::new();
        let mut parked = Vec::new();
        for (k, r) in keys {
            let Some(info) = self.sources.get(&k.source) else {
                continue;
            };
            let res = index.resolve(&k.anchor);
            let bf = Bf {
                info,
                key: k,
                reg: r,
                res,
            };
            match res.node {
                Some(n) => by_node.entry(n).or_default().push(bf),
                None => parked.push(bf),
            }
        }
        Bound { by_node, parked }
    }

    fn key_order(&self, a: &SideKey, b: &SideKey) -> Ordering {
        let name = |k: &SideKey| self.sources.get(&k.source).map(|s| Arc::clone(&s.name));
        name(a)
            .cmp(&name(b))
            .then_with(|| a.anchor.cmp(&b.anchor))
            .then_with(|| a.group.cmp(&b.group))
    }
}

fn cap_payload(p: &mut Payload) {
    use gorca_model::fact::EvidenceValue;
    match p {
        Payload::Sem(w) => {
            let taken = std::mem::replace(w, Write::Absent(Group::Role));
            *w = taken.capped(MAX_TEXT_BYTES).0;
        }
        Payload::Evidence(EvidenceValue::Text(s)) if s.len() > MAX_TEXT_BYTES => {
            *s = Arc::from(crate::value::truncate_str(s, MAX_TEXT_BYTES));
        }
        Payload::Retract | Payload::Evidence(_) => {}
    }
}

#[derive(Debug)]
struct Bf<'a> {
    info: &'a SourceInfo,
    key: &'a SideKey,
    reg: &'a FactReg,
    res: Resolved,
}

/// The binding pass of one `FUSE` evaluation.
#[derive(Debug)]
pub struct Bound<'a> {
    by_node: HashMap<NodeIdx, Vec<Bf<'a>>>,
    parked: Vec<Bf<'a>>,
}

/// Inputs of the validity filter besides the registers.
pub struct FuseCtx<'a> {
    pub epoch: &'a EpochSnapshot,
    /// The logical clock: TTL is a lifecycle input, no timer runs inside `FUSE`.
    pub clock_ms: u64,
    /// The node's `vis_rev` (bumps on bounds, visibility, children and primary name changes).
    pub vis_rev: &'a dyn Fn(NodeIdx) -> u32,
    /// The node's document revision (for `Basis::DocRev`).
    pub doc_rev: &'a dyn Fn(NodeIdx) -> u64,
}

impl std::fmt::Debug for FuseCtx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FuseCtx")
            .field("clock_ms", &self.clock_ms)
            .finish_non_exhaustive()
    }
}

impl Bound<'_> {
    /// The ranked non-primary entries of `node`.
    pub fn node(&self, node: NodeIdx, ctx: &FuseCtx<'_>) -> Ranked {
        let mut r = Ranked::default();
        if let Some(list) = self.by_node.get(&node) {
            for bf in list {
                r.push(build_entry(bf, node, ctx));
            }
        }
        r.sort();
        r
    }

    /// Facts whose anchor resolves to no live node, for `explain`.
    pub fn parked(&self) -> Vec<FactTrail> {
        self.parked
            .iter()
            .map(|bf| trail_of(bf, FactLayer::Parked, TrailStatus::Unbound, None))
            .collect()
    }
}

// ---- validity filter -----------------------------------------------------------------------------

/// Why a fact can or cannot win, in the order the filter checks (the order is part of the
/// `explain` contract).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Status {
    Retracted,
    Masked,
    Detached,
    Unbound,
    WeakBinding,
    Rebound,
    Stale,
    Expired,
    LowConfidence,
    Ok,
}

/// True for `visual.*` evidence (always subject to `vis_rev` staleness).
fn is_visual(code: GroupCode) -> bool {
    matches!(
        code.classify(),
        GroupClass::Evidence(EvidenceKind::VisualOcrText | EvidenceKind::VisualLabelGuess)
    )
}

fn status_of(bf: &Bf<'_>, node: NodeIdx, ctx: &FuseCtx<'_>) -> Status {
    let f = &bf.reg.fact;
    if matches!(f.payload, Payload::Retract) {
        return Status::Retracted;
    }
    match ctx.epoch.state(f.source) {
        None | Some(SourceState::Paused | SourceState::Quarantined | SourceState::Revoked) => {
            return Status::Masked;
        }
        Some(SourceState::Detached) => return Status::Detached,
        Some(SourceState::Active | SourceState::Throttled) => {}
    }
    let gc = grant_class(f.group);
    if !bf.res.quality.at_least(bf.info.grant.min_bind(gc)) {
        return match bf.res.quality {
            BindQuality::Unbound => Status::Unbound,
            BindQuality::Geometric
            | BindQuality::Heuristic
            | BindQuality::Unique
            | BindQuality::Exact => Status::WeakBinding,
        };
    }
    if f.bind_gen != bf.res.generation {
        return Status::Rebound;
    }
    let visual_or_inferred = bf.info.class == SourceClass::Inferred || is_visual(f.group);
    let stale = match f.basis {
        Basis::VisRev(n) => visual_or_inferred && n < (ctx.vis_rev)(node),
        Basis::DocRev(n) => n < (ctx.doc_rev)(node),
        Basis::None => false,
        // Rejected at insert; if one is ever stored it fails closed (stale), never "never stale".
        Basis::Capture(..) | Basis::Snapshot(_) => true,
    };
    if stale {
        return Status::Stale;
    }
    if let Some(ttl) = f.ttl_ms
        && f.admitted_at_ms.saturating_add(ttl) <= ctx.clock_ms
    {
        return Status::Expired;
    }
    if f.p_milli < bf.info.grant.floor(gc) {
        return Status::LowConfidence;
    }
    Status::Ok
}

/// The class a fact gets, computed from `p_milli`, the source's tier and the **side table's own**
/// grant data (never supplied by the writer): thresholds by `p_milli`, capped for T3 and never
/// `Asserted`.
fn class_of(info: &SourceInfo, p_milli: u16) -> ConfClass {
    crate::taint::non_primary_class(ConfClass::from_p_milli(p_milli), info.tier)
}

// ---- entries -------------------------------------------------------------------------------------

/// Which rule consumes a fact.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FactLayer {
    /// A Secondary semantic fact (S0, only for inexpressible groups).
    Secondary,
    /// `hint.title_attr` evidence (S3).
    DerivedTitle,
    /// An Inferred semantic fact or evidence rule output (S4).
    Inferred,
    /// Other evidence, listed by `explain` only.
    Evidence,
    /// Anchor bound to no node.
    Parked,
}

/// The fused-away status names of the `explain` trail.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TrailStatus {
    Won,
    Shadowed,
    NotCandidate,
    NotApplicable,
    NoValue,
    Unknown,
    Ok,
    Retracted,
    Masked,
    Detached,
    Unbound,
    WeakBinding,
    Rebound,
    Stale,
    Expired,
    LowConfidence,
}

impl TrailStatus {
    pub const fn name(self) -> &'static str {
        match self {
            TrailStatus::Won => "won",
            TrailStatus::Shadowed => "shadowed",
            TrailStatus::NotCandidate => "not-candidate",
            TrailStatus::NotApplicable => "not-applicable",
            TrailStatus::NoValue => "no-value",
            TrailStatus::Unknown => "unknown",
            TrailStatus::Ok => "ok",
            TrailStatus::Retracted => "retracted",
            TrailStatus::Masked => "masked",
            TrailStatus::Detached => "detached",
            TrailStatus::Unbound => "unbound",
            TrailStatus::WeakBinding => "weak-binding",
            TrailStatus::Rebound => "rebound",
            TrailStatus::Stale => "stale",
            TrailStatus::Expired => "expired",
            TrailStatus::LowConfidence => "low-confidence",
        }
    }
}

impl From<Status> for TrailStatus {
    fn from(s: Status) -> TrailStatus {
        match s {
            Status::Retracted => TrailStatus::Retracted,
            Status::Masked => TrailStatus::Masked,
            Status::Detached => TrailStatus::Detached,
            Status::Unbound => TrailStatus::Unbound,
            Status::WeakBinding => TrailStatus::WeakBinding,
            Status::Rebound => TrailStatus::Rebound,
            Status::Stale => TrailStatus::Stale,
            Status::Expired => TrailStatus::Expired,
            Status::LowConfidence => TrailStatus::LowConfidence,
            Status::Ok => TrailStatus::Ok,
        }
    }
}

/// One fact in an `explain` trail (FS-9: source, class, tier, seq, confidence class, basis status,
/// binding quality and generation).
#[derive(Clone, Debug)]
pub struct FactTrail {
    pub layer: FactLayer,
    pub source: Arc<str>,
    pub tier: TrustTier,
    pub seq: Seq,
    pub anchor: AnchorId,
    pub group: GroupCode,
    pub payload: Payload,
    pub p_milli: u16,
    /// Present only for a fact that passed the filter.
    pub class: Option<ConfClass>,
    pub bind: BindQuality,
    pub generation: BindGen,
    pub fact_generation: BindGen,
    pub status: TrailStatus,
}

fn trail_of(
    bf: &Bf<'_>,
    layer: FactLayer,
    status: TrailStatus,
    class: Option<ConfClass>,
) -> FactTrail {
    let f = &bf.reg.fact;
    FactTrail {
        layer,
        source: Arc::clone(&bf.info.name),
        tier: bf.info.tier,
        seq: bf.reg.seq,
        anchor: bf.key.anchor.clone(),
        group: f.group,
        payload: f.payload.clone(),
        p_milli: f.p_milli,
        class,
        bind: bf.res.quality,
        generation: bf.res.generation,
        fact_generation: f.bind_gen,
        status,
    }
}

/// A valid non-primary candidate. **Only this module can construct one**, and only for a fact that
/// passed [`status_of`]; the strata take it as proof of validity. The fields are private, so
/// outside this module a candidate cannot be forged (N-1):
///
/// ```compile_fail,E0451
/// use gorca_fuse::{FactCand, Write};
/// use gorca_model::sem::{ConfClass, Group, TrustTier};
/// let _forged = FactCand {
///     write: Write::Absent(Group::Name),
///     conf: ConfClass::Asserted,
///     tier: TrustTier::T0Core,
///     source: "x".into(),
///     seq: 0,
/// };
/// ```
///
/// There is no public constructor either:
///
/// ```compile_fail,E0599
/// let _ = gorca_fuse::FactCand::new();
/// ```
#[derive(Clone, Debug)]
pub struct FactCand {
    write: Write,
    conf: ConfClass,
    tier: TrustTier,
    source: Arc<str>,
    seq: Seq,
}

impl FactCand {
    /// The value (or `Absent`) the fact proposes; the group is the write's.
    pub fn write(&self) -> &Write {
        &self.write
    }

    pub fn group(&self) -> Group {
        self.write.group()
    }

    /// The class after the tier cap, never `Asserted`: a non-primary winner is always hedged (N-2).
    pub const fn conf(&self) -> ConfClass {
        self.conf
    }

    pub const fn tier(&self) -> TrustTier {
        self.tier
    }

    /// The stable source name.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub const fn seq(&self) -> Seq {
        self.seq
    }
}

/// One non-primary fact on a node with its filter verdict and, if it passed, its candidate.
#[derive(Clone, Debug)]
pub struct Entry {
    trail: FactTrail,
    status: Status,
    cand: Option<FactCand>,
    /// Target group (Name for title evidence and the OCR rule).
    target: Group,
    kind_order: u8,
    /// 0 = a fact for the group itself, 1 = rule output (S4 tie-break: direct before rule).
    rule: u8,
}

impl Entry {
    pub fn trail(&self) -> &FactTrail {
        &self.trail
    }

    pub const fn status(&self) -> Status {
        self.status
    }

    /// The candidate, present iff the fact passed the filter.
    pub fn cand(&self) -> Option<&FactCand> {
        self.cand.as_ref()
    }

    pub const fn target(&self) -> Group {
        self.target
    }

    /// The class of a fact that passed the filter.
    pub fn class(&self) -> Option<ConfClass> {
        self.trail.class
    }
}

/// S4 rule table: evidence kinds whose text names an unnamed node when no better source did
/// (toy rule of the oracle; real rules arrive with M24).
pub const S4_RULES: &[(EvidenceKind, Group)] = &[(EvidenceKind::VisualOcrText, Group::Name)];

/// S3 evidence rules: the evidence kind that names an unnamed control (`title` attribute).
pub const S3_TITLE: EvidenceKind = EvidenceKind::HintTitleAttr;

fn build_entry(bf: &Bf<'_>, node: NodeIdx, ctx: &FuseCtx<'_>) -> (FactLayer, Entry) {
    let f = &bf.reg.fact;
    let status = status_of(bf, node, ctx);
    // N-2: a non-primary winner is never Asserted.
    let class = (status == Status::Ok).then(|| class_of(bf.info, f.p_milli));
    let (layer, target, rule) = match f.group.classify() {
        GroupClass::Semantic(g) => match bf.info.class {
            SourceClass::Secondary => (FactLayer::Secondary, g, 0),
            SourceClass::Inferred => (FactLayer::Inferred, g, 0),
            SourceClass::Primary | SourceClass::Annotation => (FactLayer::Evidence, g, 0),
        },
        GroupClass::Evidence(k) if k == S3_TITLE => (FactLayer::DerivedTitle, Group::Name, 0),
        GroupClass::Evidence(k) => match S4_RULES.iter().find(|(kind, _)| *kind == k) {
            Some((_, g)) if bf.info.class == SourceClass::Inferred => (FactLayer::Inferred, *g, 1),
            _ => (FactLayer::Evidence, Group::Name, 0),
        },
        GroupClass::Unregistered(_) => (FactLayer::Evidence, Group::Name, 0),
    };
    // S4 treats an empty value as "no value" before ranking; the title rule does it after.
    let empty = f.payload.is_empty_like();
    let mut st = TrailStatus::from(status);
    if layer == FactLayer::Inferred && status == Status::Ok && empty {
        st = TrailStatus::NoValue;
    }
    let cand = (status == Status::Ok).then(|| FactCand {
        write: match (&f.payload, layer) {
            (Payload::Sem(w), _) => w.clone(),
            (p, _) => match p.text() {
                Some(t) => Write::Val(Val::Name(Arc::clone(t))),
                None => Write::Absent(target),
            },
        },
        conf: class.unwrap_or(ConfClass::High),
        tier: bf.info.tier,
        source: Arc::clone(&bf.info.name),
        seq: f.seq,
    });
    let mut trail = trail_of(bf, layer, st, class);
    trail.layer = layer;
    (
        layer,
        Entry {
            trail,
            status,
            cand,
            target,
            kind_order: bf.info.kind_order,
            rule,
        },
    )
}

// ---- ranking -------------------------------------------------------------------------------------

fn cmp_tier(a: TrustTier, b: TrustTier) -> Ordering {
    if a.is_more_trusted_than(b) {
        Ordering::Less
    } else if b.is_more_trusted_than(a) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

/// Stronger class first; a fact without a class (it did not pass the filter) sorts last.
pub fn cmp_class(a: Option<ConfClass>, b: Option<ConfClass>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => {
            if x.is_stronger_than(y) {
                Ordering::Less
            } else if y.is_stronger_than(x) {
                Ordering::Greater
            } else {
                Ordering::Equal
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn payload_cmp(a: &Payload, b: &Payload) -> Ordering {
    a.tie_key().cmp(&b.tie_key())
}

/// Order of Secondary and title facts: passed the filter first, then the more trusted tier, the
/// stronger class, the stable source name, the seq and the value. Never by `SourceId`.
pub fn cmp_secondary_like(a: &Entry, b: &Entry, ok_a: bool, ok_b: bool) -> Ordering {
    (!ok_a)
        .cmp(&!ok_b)
        .then_with(|| cmp_tier(a.trail.tier, b.trail.tier))
        .then_with(|| cmp_class(a.trail.class, b.trail.class))
        .then_with(|| a.trail.source.cmp(&b.trail.source))
        .then_with(|| a.trail.seq.cmp(&b.trail.seq))
        .then_with(|| payload_cmp(&a.trail.payload, &b.trail.payload))
}

/// S4 order: passed the filter first, then the class **after the tier cap** (stronger first), the
/// static kind order, the stable source name, direct fact before rule output, value, anchor, seq.
/// Raw `p_milli` is never compared (FS-4) and `SourceId` never decides (F-6).
fn cmp_s4(a: &Entry, b: &Entry) -> Ordering {
    let ok = |e: &Entry| e.status == Status::Ok && e.trail.status != TrailStatus::NoValue;
    (!ok(a))
        .cmp(&!ok(b))
        .then_with(|| cmp_class(a.trail.class, b.trail.class))
        .then_with(|| a.kind_order.cmp(&b.kind_order))
        .then_with(|| a.trail.source.cmp(&b.trail.source))
        .then_with(|| a.rule.cmp(&b.rule))
        .then_with(|| payload_cmp(&a.trail.payload, &b.trail.payload))
        .then_with(|| a.trail.anchor.cmp(&b.trail.anchor))
        .then_with(|| a.trail.seq.cmp(&b.trail.seq))
}

/// The non-primary entries of one node, ranked. Constructible only by the side table.
#[derive(Clone, Debug, Default)]
pub struct Ranked {
    secondary: Vec<Entry>,
    title: Vec<Entry>,
    inferred: Vec<Entry>,
    evidence: Vec<Entry>,
}

impl Ranked {
    fn push(&mut self, (layer, e): (FactLayer, Entry)) {
        match layer {
            FactLayer::Secondary => self.secondary.push(e),
            FactLayer::DerivedTitle => self.title.push(e),
            FactLayer::Inferred => self.inferred.push(e),
            FactLayer::Evidence | FactLayer::Parked => self.evidence.push(e),
        }
    }

    fn sort(&mut self) {
        let ok = |e: &Entry| e.status == Status::Ok;
        self.title
            .sort_by(|a, b| cmp_secondary_like(a, b, ok(a), ok(b)));
        self.inferred.sort_by(cmp_s4);
        // Secondaries are ranked in S0, which knows the inexpressible row; keep a stable order.
        self.secondary
            .sort_by(|a, b| cmp_secondary_like(a, b, ok(a), ok(b)));
    }
}

/// A read-only view of a node's ranked entries. [`NodeCands::none`] is the empty view.
#[derive(Copy, Clone, Debug, Default)]
pub struct NodeCands<'a> {
    ranked: Option<&'a Ranked>,
}

impl<'a> NodeCands<'a> {
    pub const fn none() -> NodeCands<'static> {
        NodeCands { ranked: None }
    }

    pub const fn new(r: &'a Ranked) -> NodeCands<'a> {
        NodeCands { ranked: Some(r) }
    }

    pub fn secondary(&self) -> &'a [Entry] {
        self.ranked.map_or(&[], |r| r.secondary.as_slice())
    }

    pub fn title(&self) -> &'a [Entry] {
        self.ranked.map_or(&[], |r| r.title.as_slice())
    }

    pub fn inferred(&self) -> &'a [Entry] {
        self.ranked.map_or(&[], |r| r.inferred.as_slice())
    }

    pub fn evidence(&self) -> &'a [Entry] {
        self.ranked.map_or(&[], |r| r.evidence.as_slice())
    }
}

impl Ranked {
    pub fn view(&self) -> NodeCands<'_> {
        NodeCands::new(self)
    }
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::Orientation;

    use super::*;

    /// Nodes bind `Exact`; a DOM id binds `Heuristic` (ambiguous); everything else is unbound.
    struct Idx;
    impl AnchorIndex for Idx {
        fn resolve(&self, a: &AnchorId) -> Resolved {
            match a {
                AnchorId::Node(n) => Resolved {
                    node: Some(*n),
                    quality: BindQuality::Exact,
                    generation: BindGen(1),
                },
                AnchorId::Dom { .. } => Resolved {
                    node: Some(NodeIdx(0)),
                    quality: BindQuality::Heuristic,
                    generation: BindGen(1),
                },
                _ => Resolved::UNBOUND,
            }
        }
    }

    fn id(slot: u32) -> SourceId {
        SourceId {
            slot,
            generation: 0,
        }
    }

    fn src(slot: u32, name: &str, class: SourceClass, tier: TrustTier) -> SourceInfo {
        SourceInfo {
            id: id(slot),
            name: Arc::from(name),
            class,
            tier,
            kind_order: 0,
            grant: Grant::standard(),
        }
    }

    fn fact(source: u32, node: u32, g: Group, seq: u64, w: Write) -> FactIn {
        FactIn {
            source: id(source),
            anchor: AnchorId::Node(NodeIdx(node)),
            group: g.group_code(),
            seq,
            payload: Payload::Sem(w),
            p_milli: 990,
            basis: Basis::None,
            bind_gen: BindGen(1),
            admitted_at_ms: 0,
            ttl_ms: None,
        }
    }

    /// A snapshot with every listed source `Active` (a missing source is masked).
    fn active(slots: &[u32]) -> EpochSnapshot {
        let mut e = EpochSnapshot::default();
        for s in slots {
            e.set(id(*s), SourceState::Active);
        }
        e
    }

    fn ctx(e: &EpochSnapshot, clock: u64) -> FuseCtx<'_> {
        FuseCtx {
            epoch: e,
            clock_ms: clock,
            vis_rev: &|_| 0,
            doc_rev: &|_| 0,
        }
    }

    fn table() -> SideTable {
        let mut t = SideTable::default();
        t.set_source(src(
            1,
            "bus",
            SourceClass::Secondary,
            TrustTier::T3Untrusted,
        ))
        .expect("bus");
        t.set_source(src(
            2,
            "ocr",
            SourceClass::Inferred,
            TrustTier::T2FirstParty,
        ))
        .expect("ocr");
        t
    }

    fn vertical() -> Write {
        Write::Val(Val::Orientation(Orientation::Vertical))
    }

    fn status_of_only(t: &SideTable, e: &EpochSnapshot, clock: u64) -> Status {
        let b = t.bind(&Idx);
        let r = b.node(NodeIdx(0), &ctx(e, clock));
        r.view().secondary()[0].status()
    }

    #[test]
    fn a_valid_fact_becomes_a_candidate_with_a_side_table_class() {
        let mut t = table();
        // p = 990 from a T3 source: High capped to Probable by the side table itself.
        t.insert(fact(1, 0, Group::Orientation, 1, vertical()))
            .expect("insert");
        let b = t.bind(&Idx);
        let e = active(&[1, 2]);
        let r = b.node(NodeIdx(0), &ctx(&e, 0));
        let c = r.view().secondary()[0].cand().expect("valid");
        assert_eq!(c.conf(), ConfClass::Probable);
    }

    #[test]
    fn a_writer_cannot_assert_class_floor_or_binding_minimum() {
        // The fact type carries no class, floor flag or minimum: a low-precision fact is below the
        // floor, and a fact on an ambiguous anchor is below the minimum, whatever else it says.
        let e = active(&[1, 2]);
        let mut low = table();
        let mut f = fact(1, 0, Group::Orientation, 1, vertical());
        f.p_milli = 500;
        low.insert(f).expect("insert");
        assert_eq!(status_of_only(&low, &e, 0), Status::LowConfidence);

        let mut amb = table();
        let mut f = fact(1, 0, Group::Orientation, 1, vertical());
        f.anchor = AnchorId::Dom {
            id: Arc::from("x"),
            count: 2,
            frame: 0,
            shadow: 0,
        };
        amb.insert(f).expect("insert");
        assert_eq!(status_of_only(&amb, &e, 0), Status::WeakBinding);

        // A source registered with a stricter grant is held to it.
        let mut strict = SideTable::default();
        let mut info = src(1, "bus", SourceClass::Secondary, TrustTier::T2FirstParty);
        info.grant = Grant::new([999; 4], [BindQuality::Exact; 4]);
        strict.set_source(info).expect("bus");
        strict
            .insert(fact(1, 0, Group::Orientation, 1, vertical()))
            .expect("insert");
        assert_eq!(status_of_only(&strict, &e, 0), Status::LowConfidence);
    }

    #[test]
    fn every_filter_step_blocks_the_candidate() {
        let e = active(&[1, 2]);
        let cases: Vec<(&str, FactIn, Status)> = vec![
            (
                "retracted",
                {
                    let mut f = fact(1, 0, Group::Orientation, 1, vertical());
                    f.payload = Payload::Retract;
                    f
                },
                Status::Retracted,
            ),
            (
                "weak binding",
                {
                    let mut f = fact(1, 0, Group::Orientation, 1, vertical());
                    f.anchor = AnchorId::Dom {
                        id: Arc::from("x"),
                        count: 1,
                        frame: 0,
                        shadow: 0,
                    };
                    f
                },
                Status::WeakBinding,
            ),
            (
                "rebound",
                {
                    let mut f = fact(1, 0, Group::Orientation, 1, vertical());
                    f.bind_gen = BindGen(0);
                    f
                },
                Status::Rebound,
            ),
            (
                "expired",
                {
                    let mut f = fact(1, 0, Group::Orientation, 1, vertical());
                    f.ttl_ms = Some(10);
                    f
                },
                Status::Expired,
            ),
            (
                "floor",
                {
                    let mut f = fact(1, 0, Group::Orientation, 1, vertical());
                    f.p_milli = 100;
                    f
                },
                Status::LowConfidence,
            ),
        ];
        for (name, f, want) in cases {
            let mut t = table();
            t.insert(f).expect("insert");
            let b = t.bind(&Idx);
            let r = b.node(NodeIdx(0), &ctx(&e, 10));
            let ent = &r.view().secondary()[0];
            assert_eq!(ent.status(), want, "{name}");
            assert!(ent.cand().is_none(), "{name}");
        }
    }

    #[test]
    fn unbound_facts_are_parked() {
        let mut t = table();
        let mut f = fact(1, 0, Group::Orientation, 1, vertical());
        f.anchor = AnchorId::Content(1);
        t.insert(f).expect("insert");
        let b = t.bind(&Idx);
        let e = active(&[1, 2]);
        assert!(
            b.node(NodeIdx(0), &ctx(&e, 0))
                .view()
                .secondary()
                .is_empty()
        );
        assert_eq!(b.parked().len(), 1);
    }

    #[test]
    fn masked_and_detached_sources_and_stale_basis() {
        let mut t = table();
        let mut f = fact(
            2,
            0,
            Group::Name,
            1,
            Write::Val(Val::Name(Arc::from("Save"))),
        );
        f.basis = Basis::VisRev(1);
        t.insert(f).expect("insert");
        let b = t.bind(&Idx);
        let mut e = active(&[1, 2]);
        let status = |e: &EpochSnapshot, vis: u32| {
            let c = FuseCtx {
                epoch: e,
                clock_ms: 0,
                vis_rev: &move |_| vis,
                doc_rev: &|_| 0,
            };
            b.node(NodeIdx(0), &c).view().inferred()[0].status()
        };
        assert_eq!(status(&e, 1), Status::Ok);
        assert_eq!(status(&e, 2), Status::Stale);
        e.set(id(2), SourceState::Revoked);
        assert_eq!(status(&e, 1), Status::Masked);
        e.set(id(2), SourceState::Detached);
        assert_eq!(status(&e, 1), Status::Detached);
        e.set(id(2), SourceState::Throttled);
        assert_eq!(status(&e, 1), Status::Ok);
    }

    #[test]
    fn a_source_missing_from_the_epoch_snapshot_fails_closed() {
        let mut t = table();
        t.insert(fact(1, 0, Group::Orientation, 1, vertical()))
            .expect("insert");
        // Source 1 has no entry: masked, never assumed active.
        let e = active(&[2]);
        assert_eq!(status_of_only(&t, &e, 0), Status::Masked);
        assert_eq!(
            status_of_only(&t, &EpochSnapshot::default(), 0),
            Status::Masked
        );
    }

    #[test]
    fn capture_and_snapshot_bases_are_rejected_at_insert() {
        use gorca_model::fact::{CaptureId, SnapshotToken};
        let mut t = table();
        let mut f = fact(1, 0, Group::Orientation, 1, vertical());
        f.basis = Basis::Capture(CaptureId(1), 7);
        assert_eq!(t.insert(f.clone()), Err(SideError::UnsupportedBasis));
        f.basis = Basis::Snapshot(SnapshotToken(1));
        assert_eq!(t.insert(f), Err(SideError::UnsupportedBasis));
        assert!(t.is_empty());
    }

    #[test]
    fn stable_names_are_unique() {
        let mut t = table();
        assert_eq!(
            t.set_source(src(
                9,
                "bus",
                SourceClass::Inferred,
                TrustTier::T2FirstParty
            )),
            Err(SideError::DuplicateName)
        );
        // Re-registering the same id under its name is an update, not a duplicate.
        assert_eq!(
            t.set_source(src(
                1,
                "bus",
                SourceClass::Secondary,
                TrustTier::T2FirstParty
            )),
            Ok(())
        );
    }

    #[test]
    fn register_join_is_order_independent_and_capped() {
        let mut a = table();
        let mut b = table();
        let f1 = fact(1, 0, Group::Orientation, 1, vertical());
        let f2 = fact(
            1,
            0,
            Group::Orientation,
            2,
            Write::Val(Val::Orientation(Orientation::Horizontal)),
        );
        a.insert(f1.clone()).expect("a1");
        a.insert(f2.clone()).expect("a2");
        b.insert(f2).expect("b2");
        assert_eq!(b.insert(f1), Ok(false));
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        let mut small = SideTable::with_cap(1);
        small
            .set_source(src(
                1,
                "bus",
                SourceClass::Secondary,
                TrustTier::T3Untrusted,
            ))
            .expect("bus");
        small
            .insert(fact(1, 0, Group::Orientation, 1, vertical()))
            .expect("first");
        assert_eq!(
            small.insert(fact(1, 1, Group::Orientation, 1, vertical())),
            Err(SideError::Full)
        );
        assert_eq!(
            small.insert(fact(9, 0, Group::Orientation, 1, vertical())),
            Err(SideError::UnknownSource)
        );
    }

    #[test]
    fn primary_and_annotation_sources_are_not_fusable() {
        let mut t = SideTable::default();
        t.set_source(src(5, "atspi", SourceClass::Primary, TrustTier::T1Platform))
            .expect("atspi");
        t.set_source(src(
            6,
            "note",
            SourceClass::Annotation,
            TrustTier::T2FirstParty,
        ))
        .expect("note");
        for s in [5, 6] {
            assert_eq!(
                t.insert(fact(s, 0, Group::Name, 1, vertical())),
                Err(SideError::NotFusable)
            );
        }
    }
}
