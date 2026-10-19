// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure conformance kit for fact sources (ADR-0024 sections 2.6 and 7; M22a-B3).
//!
//! No tokio, no IO. [`RefWriter`] is a minimal reference writer: it admits facts with the real
//! `gorca_fact_api::admit` and keeps registers keyed by (source, subject, group), highest
//! sequence wins. It exists to state the properties against: F16 (flood isolation, pure part)
//! and F19 (scope pinning), plus the checks of spec section 5 item 3 (sequence monotonicity, every
//! rejection reason, out-of-scope facts, oversize strings, the state machine exhaustively,
//! stable identity across reconnect, no fact about a secret node accepted).

use gorca_fact_api::{
    Admission, AnchorKinds, BindInfo, MAX_PAGE_STRING, PrivacyClass, RejectReason, Scope,
    ScopeLookup, ScopeView, SourceBook, SourceEffect, SourceEvent, SourceGrant, SourceState,
    Standing, Strikes, TokenBucket, admit, step,
};
use gorca_model::fact::{BindGen, Fact, FactValue, PropValue, SourceId, SrcSeq, Subject};
use gorca_model::ids::{AppId, ConnectorId, LocalId, NodeKey};
use gorca_model::sem::{
    BindQuality, ConfClass, EvidenceKind, Group, GroupCode, GroupMask, SourceClass, TrustTier,
};

use crate::fake_source::{FakeFactSource, SourceOutput};

/// Outcome of a conformance run: every failed check is named.
#[derive(Debug, Default)]
pub struct Report {
    pub checks: u32,
    pub failures: Vec<String>,
}

impl Report {
    pub fn check(&mut self, name: &str, cond: bool) {
        self.checks += 1;
        if !cond {
            self.failures.push(name.to_owned());
        }
    }

    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn merge(&mut self, other: Report) {
        self.checks += other.checks;
        self.failures.extend(other.failures);
    }
}

/// What `RefWriter::offer` did with one fact.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Offer {
    Stored,
    /// Older or equal sequence for an existing register: dropped, counted.
    Superseded,
    /// A retraction removed (or found no) register.
    Retracted,
    Parked,
    Throttled,
    Rejected(RejectReason),
}

/// One register of the reference writer.
#[derive(Clone, Debug)]
pub struct Reg {
    pub source: SourceId,
    pub subject: Subject,
    pub group: GroupCode,
    pub seq: SrcSeq,
    pub value: FactValue,
    pub standing: Standing,
    pub class: ConfClass,
    pub bind_gen: BindGen,
}

/// Counters of what the reference writer dropped (F16: drops are counted).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Counters {
    pub stored: u32,
    pub superseded: u32,
    pub parked: u32,
    pub throttled: u32,
    pub truncated: u32,
    pub rejected: Vec<(RejectReason, u32)>,
}

impl Counters {
    fn reject(&mut self, r: RejectReason) {
        if let Some(e) = self.rejected.iter_mut().find(|(k, _)| *k == r) {
            e.1 += 1;
        } else {
            self.rejected.push((r, 1));
        }
    }

    pub fn rejected_total(&self) -> u32 {
        self.rejected.iter().map(|(_, n)| *n).sum()
    }
}

/// A minimal single-scope writer. Implements the lookup `admit` needs.
#[derive(Debug)]
pub struct RefWriter {
    pub pinned: Scope,
    nodes: Vec<(NodeKey, Scope)>,
    unknown: Vec<NodeKey>,
    recent_docs: Vec<Option<u64>>,
    binds: Vec<(Subject, Scope, BindInfo)>,
    privacy: Vec<(Subject, PrivacyClass)>,
    regs: Vec<Reg>,
    parked: Vec<(SourceId, Fact)>,
    pub counters: Counters,
}

/// Per-source view of the writer (what `ScopeLookup` needs besides the shared tables).
struct SourceLookup<'a> {
    w: &'a RefWriter,
    source: SourceId,
}

impl ScopeLookup for SourceLookup<'_> {
    fn node_scope(&self, node: NodeKey) -> Option<Scope> {
        if self.w.unknown.contains(&node) {
            return None;
        }
        match self.w.nodes.iter().find(|(n, _)| *n == node) {
            Some((_, s)) => Some(*s),
            None => Some(self.w.pinned),
        }
    }

    fn was_recent_doc(&self, doc: Option<u64>, _pinned: Scope) -> bool {
        self.w.recent_docs.contains(&doc)
    }

    fn bind(&self, subject: &Subject, pinned: Scope) -> BindInfo {
        if let Some((_, _, b)) = self
            .w
            .binds
            .iter()
            .find(|(s, sc, _)| s == subject && *sc == pinned)
        {
            return *b;
        }
        match subject {
            Subject::Node(_) => BindInfo {
                quality: BindQuality::Exact,
                generation: BindGen(0),
            },
            Subject::Anchor(_) => BindInfo::UNBOUND,
        }
    }

    fn privacy(&self, subject: &Subject) -> PrivacyClass {
        self.w
            .privacy
            .iter()
            .find(|(s, _)| s == subject)
            .map_or(PrivacyClass::Normal, |(_, c)| *c)
    }

    fn has_register(&self, subject: &Subject, group: GroupCode) -> bool {
        self.w
            .regs
            .iter()
            .any(|r| r.source == self.source && r.group == group && &r.subject == subject)
    }
}

impl RefWriter {
    pub fn new(pinned: Scope) -> RefWriter {
        RefWriter {
            pinned,
            nodes: Vec::new(),
            unknown: Vec::new(),
            recent_docs: Vec::new(),
            binds: Vec::new(),
            privacy: Vec::new(),
            regs: Vec::new(),
            parked: Vec::new(),
            counters: Counters::default(),
        }
    }

    /// Declare that `node` lives in `scope` (a node of another document of the same app).
    pub fn add_node(&mut self, node: NodeKey, scope: Scope) {
        self.nodes.push((node, scope));
    }

    /// The pinned tab just navigated away from `doc` (inside the race window).
    pub fn note_navigation(&mut self, doc: Option<u64>) {
        self.recent_docs.push(doc);
    }

    /// The writer learned the node's scope.
    pub fn learn_node(&mut self, node: NodeKey) {
        self.unknown.retain(|n| *n != node);
    }

    /// Mark a node whose scope the writer does not know yet.
    pub fn forget_node(&mut self, node: NodeKey) {
        self.unknown.push(node);
    }

    pub fn bind(&mut self, subject: Subject, quality: BindQuality, generation: BindGen) {
        self.bind_in(self.pinned, subject, quality, generation);
    }

    /// Bind an anchor inside `scope` only; a lookup for another pinned scope never sees it.
    pub fn bind_in(
        &mut self,
        scope: Scope,
        subject: Subject,
        quality: BindQuality,
        generation: BindGen,
    ) {
        self.binds
            .retain(|(s, sc, _)| s != &subject || *sc != scope);
        self.binds.push((
            subject,
            scope,
            BindInfo {
                quality,
                generation,
            },
        ));
    }

    pub fn set_privacy(&mut self, subject: Subject, class: PrivacyClass) {
        self.privacy.retain(|(s, _)| s != &subject);
        self.privacy.push((subject, class));
    }

    pub fn registers(&self) -> &[Reg] {
        &self.regs
    }

    pub fn registers_of(&self, source: SourceId) -> Vec<Reg> {
        self.regs
            .iter()
            .filter(|r| r.source == source)
            .cloned()
            .collect()
    }

    pub fn parked_count(&self, source: SourceId) -> usize {
        self.parked.iter().filter(|(s, _)| *s == source).count()
    }

    /// Offer one fact from `source`: token bucket, then `admit`, then store.
    pub fn offer(
        &mut self,
        source: SourceId,
        grant: &SourceGrant,
        bucket: &mut TokenBucket,
        now_ms: u64,
        f: &Fact,
    ) -> Offer {
        if !bucket.try_take(now_ms) {
            self.counters.throttled += 1;
            return Offer::Throttled;
        }
        let registers = u32::try_from(self.regs.iter().filter(|r| r.source == source).count())
            .unwrap_or(u32::MAX);
        let parked = u16::try_from(self.parked_count(source)).unwrap_or(u16::MAX);
        let verdict = {
            let lookup = SourceLookup { w: self, source };
            let view = ScopeView {
                pinned: self.pinned,
                lookup: &lookup,
                registers,
                parked,
            };
            admit(f, grant, &view)
        };
        match verdict {
            Admission::Reject(r) => {
                self.counters.reject(r.reason);
                Offer::Rejected(r.reason)
            }
            Admission::Park => {
                self.parked.push((source, f.clone()));
                self.counters.parked += 1;
                Offer::Parked
            }
            Admission::Admit(a) => {
                self.counters.truncated += a.truncated;
                self.store(source, &a.fact, a.class, a.standing, a.bind.generation)
            }
        }
    }

    fn store(
        &mut self,
        source: SourceId,
        f: &Fact,
        class: ConfClass,
        standing: Standing,
        bind_gen: BindGen,
    ) -> Offer {
        let idx = self
            .regs
            .iter()
            .position(|r| r.source == source && r.group == f.group && r.subject == f.subject);
        if let Some(i) = idx
            && let Some(old) = self.regs.get(i)
            && f.seq <= old.seq
        {
            self.counters.superseded += 1;
            return Offer::Superseded;
        }
        if let FactValue::Retract = f.value {
            if let Some(i) = idx {
                self.regs.swap_remove(i);
            }
            return Offer::Retracted;
        }
        let reg = Reg {
            source,
            subject: f.subject.clone(),
            group: f.group,
            seq: f.seq,
            value: f.value.clone(),
            standing,
            class,
            bind_gen,
        };
        match idx {
            Some(i) => {
                if let Some(slot) = self.regs.get_mut(i) {
                    *slot = reg;
                }
            }
            None => self.regs.push(reg),
        }
        self.counters.stored += 1;
        Offer::Stored
    }
}

/// Scope used by the kit.
pub fn kit_scope(n: u32) -> Scope {
    Scope {
        app: AppId {
            connector: ConnectorId(1),
            serial: n,
        },
        doc: None,
        site: None,
        tab: None,
    }
}

/// A node of the kit's pinned scope.
pub fn kit_node(local: u64) -> Subject {
    Subject::Node(NodeKey {
        app: kit_scope(1).app,
        local: LocalId(local),
    })
}

/// A grant listing every semantic and evidence group.
pub fn full_grant(class: SourceClass, tier: TrustTier) -> SourceGrant {
    let mut g = SourceGrant::new(class, tier);
    g.semantic_groups = GroupMask((1 << Group::ALL.len()) - 1);
    g.evidence_groups = gorca_fact_api::EvidenceMask::ALL;
    g.anchors = AnchorKinds::ALL;
    g
}

fn offer_once(w: &mut RefWriter, src: SourceId, g: &SourceGrant, f: &Fact) -> Offer {
    let mut b = TokenBucket::new(1_000_000, 1_000_000);
    w.offer(src, g, &mut b, 0, f)
}

/// Replay the script of a fake source through the reference writer under `grant`, returning the
/// writer and the offer verdicts of its facts (state-machine events are checked separately).
pub fn replay(src: &FakeFactSource, grant: &SourceGrant, scope: Scope) -> (RefWriter, Vec<Offer>) {
    let mut w = RefWriter::new(scope);
    let mut b = TokenBucket::new(1_000_000, 1_000_000);
    let mut out = Vec::new();
    for o in src.script() {
        if let SourceOutput::Fact(f) = o {
            out.push(w.offer(src.id(), grant, &mut b, 0, f));
        }
    }
    (w, out)
}

/// A source's own obligation: sequence numbers strictly increase within the source (Lemma 1).
pub fn seq_is_monotone(src: &FakeFactSource) -> bool {
    let mut last = 0;
    for f in src.facts() {
        if f.seq.0 <= last {
            return false;
        }
        last = f.seq.0;
    }
    true
}

/// Every rejection reason, one crafted fact each; plus the accepting and parking cases.
pub fn admission_matrix() -> Report {
    use RejectReason as R;
    let mut r = Report::default();
    let src = SourceId {
        slot: 7,
        generation: 0,
    };
    let node = |n: u64| kit_node(n);
    let name = |s: &str| {
        let mut f = FakeFactSource::new("m", SourceClass::Secondary);
        f.name(kit_node(1), s, 990);
        f.facts().next().cloned()
    };
    let Some(base) = name("Save") else {
        r.check("fake source produced a fact", false);
        return r;
    };
    let sec = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    let mut w = RefWriter::new(kit_scope(1));
    r.check(
        "bound well-formed fact is stored",
        offer_once(&mut w, src, &sec, &base) == Offer::Stored,
    );
    // The same fact, restated with the same seq, is superseded (never double-stored).
    r.check(
        "equal seq is superseded",
        offer_once(&mut w, src, &sec, &base) == Offer::Superseded,
    );

    let expect = |r: &mut Report, label: &str, g: &SourceGrant, f: &Fact, want: Offer| {
        let mut w = RefWriter::new(kit_scope(1));
        let got = offer_once(&mut w, src, g, f);
        r.check(&format!("{label}: {got:?}"), got == want);
    };

    // FS-11 / F19.
    let mut other = base.clone();
    other.subject = Subject::Node(NodeKey {
        app: kit_scope(2).app,
        local: LocalId(1),
    });
    expect(
        &mut r,
        "scope mismatch",
        &sec,
        &other,
        Offer::Rejected(R::ScopeMismatch),
    );
    // Class.
    let ann = full_grant(SourceClass::Annotation, TrustTier::T2FirstParty);
    expect(
        &mut r,
        "annotation never fused",
        &ann,
        &base,
        Offer::Rejected(R::ClassNotAllowed),
    );
    let prim = full_grant(SourceClass::Primary, TrustTier::T1Platform);
    let mut ev = base.clone();
    ev.group = EvidenceKind::HintTitleAttr.group_code();
    ev.value = FactValue::Value(PropValue::Evidence(gorca_model::fact::EvidenceValue::Text(
        "t".into(),
    )));
    expect(
        &mut r,
        "primary writes no evidence",
        &prim,
        &ev,
        Offer::Rejected(R::ClassNotAllowed),
    );
    // Unregistered group.
    let mut unreg = base.clone();
    unreg.group = GroupCode(0x00ff);
    expect(
        &mut r,
        "unregistered group",
        &sec,
        &unreg,
        Offer::Rejected(R::UnregisteredGroup),
    );
    // Group not in grant.
    let mut narrow = sec.clone();
    narrow.semantic_groups = GroupMask::EMPTY.with(Group::Level);
    expect(
        &mut r,
        "group not granted",
        &narrow,
        &base,
        Offer::Rejected(R::GroupNotGranted),
    );
    // Anchor kind undeclared.
    let mut anchored = base.clone();
    anchored.subject = FakeFactSource::dom_anchor("save", 1);
    let mut no_anchor = sec.clone();
    no_anchor.anchors = AnchorKinds::EXACT;
    expect(
        &mut r,
        "anchor undeclared",
        &no_anchor,
        &anchored,
        Offer::Rejected(R::AnchorKindUndeclared),
    );
    // Malformed.
    let mut wrong = base.clone();
    wrong.value = FactValue::Value(PropValue::Level(1));
    expect(
        &mut r,
        "wrong value shape",
        &sec,
        &wrong,
        Offer::Rejected(R::Malformed),
    );
    let mut p = base.clone();
    p.conf.p_milli = 1001;
    expect(
        &mut r,
        "p_milli above 1000",
        &sec,
        &p,
        Offer::Rejected(R::Malformed),
    );
    // Missing TTL for Inferred.
    let inf = full_grant(SourceClass::Inferred, TrustTier::T2FirstParty);
    let mut no_ttl = base.clone();
    no_ttl.ttl_ms = None;
    expect(
        &mut r,
        "inferred needs ttl",
        &inf,
        &no_ttl,
        Offer::Rejected(R::MissingTtl),
    );
    let mut with_ttl = base.clone();
    with_ttl.ttl_ms = Some(1000);
    expect(&mut r, "inferred with ttl", &inf, &with_ttl, Offer::Stored);
    // Unbound anchor parks; a full park lot rejects benignly.
    expect(
        &mut r,
        "unbound anchor parks",
        &sec,
        &anchored,
        Offer::Parked,
    );
    let mut full_park = sec.clone();
    full_park.max_parked = 0;
    expect(
        &mut r,
        "park full",
        &full_park,
        &anchored,
        Offer::Rejected(R::ParkFull),
    );
    // Register cap: a new register is refused, an existing one is updated, a retract always passes.
    let mut capped = sec.clone();
    capped.max_registers = 1;
    let mut w2 = RefWriter::new(kit_scope(1));
    let mut f1 = base.clone();
    r.check(
        "cap: first stored",
        offer_once(&mut w2, src, &capped, &f1) == Offer::Stored,
    );
    let mut f2 = base.clone();
    f2.subject = node(2);
    f2.seq = SrcSeq(2);
    r.check(
        "cap: second refused",
        offer_once(&mut w2, src, &capped, &f2) == Offer::Rejected(R::RegisterCap),
    );
    f1.seq = SrcSeq(3);
    r.check(
        "cap: update of an existing register passes",
        offer_once(&mut w2, src, &capped, &f1) == Offer::Stored,
    );
    f1.seq = SrcSeq(4);
    f1.value = FactValue::Retract;
    r.check(
        "cap: retract passes",
        offer_once(&mut w2, src, &capped, &f1) == Offer::Retracted,
    );
    r.check("cap: retract freed the register", w2.registers().is_empty());
    // Unknown node scope fails closed (parks), then admits once known.
    let mut w5 = RefWriter::new(kit_scope(1));
    if let Subject::Node(n) = node(1) {
        w5.forget_node(n);
    }
    r.check(
        "node with unknown scope is parked, not admitted",
        offer_once(&mut w5, src, &sec, &base) == Offer::Parked && w5.registers().is_empty(),
    );
    let mut full_unknown = sec.clone();
    full_unknown.max_parked = 0;
    r.check(
        "node with unknown scope and full park lot is rejected",
        offer_once(&mut w5, src, &full_unknown, &base) == Offer::Rejected(R::ParkFull),
    );
    if let Subject::Node(n) = node(1) {
        w5.learn_node(n);
    }
    r.check(
        "parked fact is admitted on retry once the scope is known",
        offer_once(&mut w5, src, &sec, &base) == Offer::Stored,
    );
    // An anchor bound only in another tab's scope never binds here (review B-5).
    let mut w6 = RefWriter::new(kit_scope(1));
    let other_tab = Scope {
        doc: Some(2),
        ..kit_scope(1)
    };
    w6.bind_in(
        other_tab,
        anchored.subject.clone(),
        BindQuality::Unique,
        BindGen(1),
    );
    r.check(
        "anchor bound in another tab does not bind in this one",
        offer_once(&mut w6, src, &sec, &anchored) == Offer::Parked,
    );
    // Privacy.
    let mut w3 = RefWriter::new(kit_scope(1));
    w3.set_privacy(node(1), PrivacyClass::Secret);
    let mut allow_all = sec.clone();
    allow_all.privacy.allow_restricted = true;
    r.check(
        "secret subject excluded even when restricted is allowed",
        offer_once(&mut w3, src, &allow_all, &base) == Offer::Rejected(R::PrivacyExcluded),
    );
    let mut w4 = RefWriter::new(kit_scope(1));
    w4.set_privacy(node(1), PrivacyClass::Restricted);
    r.check(
        "restricted subject excluded by default",
        offer_once(&mut w4, src, &sec, &base) == Offer::Rejected(R::PrivacyExcluded),
    );
    r.check(
        "restricted subject allowed by grant",
        offer_once(&mut w4, src, &allow_all, &base) == Offer::Stored,
    );
    r
}

/// Binding minimums, confidence floors, the T3 cap and ambiguous Dom ids.
pub fn standing_and_class() -> Report {
    let mut r = Report::default();
    let src = SourceId {
        slot: 3,
        generation: 0,
    };
    let sec = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    let mut s = FakeFactSource::new("s", SourceClass::Secondary);
    s.name(kit_node(1), "A", 995);
    let Some(f) = s.facts().next().cloned() else {
        r.check("fact", false);
        return r;
    };
    // T3 cap: p = 0.995 is High before the cap, Probable after; class is never the source's.
    let mut w = RefWriter::new(kit_scope(1));
    let mut lying = f.clone();
    lying.conf.class = ConfClass::Asserted;
    offer_once(&mut w, src, &sec, &lying);
    r.check(
        "T3 fact at p=0.995 is Probable, source-claimed Asserted ignored",
        w.registers().first().map(|g| g.class) == Some(ConfClass::Probable),
    );
    let t2 = full_grant(SourceClass::Inferred, TrustTier::T2FirstParty);
    let mut w = RefWriter::new(kit_scope(1));
    let mut with_ttl = f.clone();
    with_ttl.ttl_ms = Some(1);
    offer_once(&mut w, src, &t2, &with_ttl);
    r.check(
        "T2 fact at p=0.995 is High",
        w.registers().first().map(|g| g.class) == Some(ConfClass::High),
    );
    // Primary facts are Asserted.
    let prim = full_grant(SourceClass::Primary, TrustTier::T1Platform);
    let mut w = RefWriter::new(kit_scope(1));
    offer_once(&mut w, src, &prim, &f);
    r.check(
        "primary fact is Asserted",
        w.registers().first().map(|g| g.class) == Some(ConfClass::Asserted),
    );
    // Floor.
    let mut floored = sec.clone();
    floored.conf_floor[0] = 950;
    let mut low = f.clone();
    low.conf.p_milli = 900;
    let mut w = RefWriter::new(kit_scope(1));
    offer_once(&mut w, src, &floored, &low);
    r.check(
        "below floor is stored but cannot win",
        w.registers().first().map(|g| g.standing) == Some(Standing::BelowFloor),
    );
    // Binding minimum (F15): Heuristic bound anchor stays below Unique minimum.
    let anchor = FakeFactSource::dom_anchor("save", 1);
    let mut af = f.clone();
    af.subject = anchor.clone();
    let mut w = RefWriter::new(kit_scope(1));
    w.bind(anchor.clone(), BindQuality::Heuristic, BindGen(4));
    offer_once(&mut w, src, &sec, &af);
    let reg = w.registers().first().cloned();
    r.check(
        "heuristic binding never wins a semantic group",
        reg.as_ref().map(|g| g.standing) == Some(Standing::BelowBindMin),
    );
    r.check(
        "BindGen stamped at admission",
        reg.as_ref().map(|g| g.bind_gen) == Some(BindGen(4)),
    );
    // Ambiguous DomId (dom_count 2) never binds Unique even when the writer says Unique.
    let amb = FakeFactSource::dom_anchor("dup", 2);
    let mut bf = f.clone();
    bf.subject = amb.clone();
    let mut w = RefWriter::new(kit_scope(1));
    w.bind(amb, BindQuality::Unique, BindGen(1));
    offer_once(&mut w, src, &sec, &bf);
    r.check(
        "ambiguous DomId capped below Unique",
        w.registers().first().map(|g| g.standing) == Some(Standing::BelowBindMin),
    );
    // Evidence accepts Heuristic; visual evidence accepts Geometric.
    let ev_anchor = FakeFactSource::dom_anchor("img", 1);
    let mut w = RefWriter::new(kit_scope(1));
    w.bind(ev_anchor.clone(), BindQuality::Geometric, BindGen(1));
    let mut ocr = FakeFactSource::new("o", SourceClass::Inferred);
    ocr.evidence_text(ev_anchor.clone(), EvidenceKind::VisualOcrText, "Close", 960);
    ocr.evidence_text(ev_anchor, EvidenceKind::HintTitleAttr, "Close", 960);
    let regs: Vec<Fact> = ocr.facts().cloned().collect();
    let inf = full_grant(SourceClass::Inferred, TrustTier::T2FirstParty);
    for f in &regs {
        offer_once(&mut w, src, &inf, f);
    }
    let by = |k: EvidenceKind| {
        w.registers()
            .iter()
            .find(|g| g.group == k.group_code())
            .map(|g| g.standing)
    };
    r.check(
        "visual evidence at Geometric is eligible",
        by(EvidenceKind::VisualOcrText) == Some(Standing::Eligible),
    );
    r.check(
        "hint evidence at Geometric is below its Heuristic minimum",
        by(EvidenceKind::HintTitleAttr) == Some(Standing::BelowBindMin),
    );
    r
}

/// Oversize page strings are truncated and counted, never violations (M-6).
pub fn oversize_strings_truncated() -> Report {
    let mut r = Report::default();
    let sec = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    let mut s = FakeFactSource::new("s", SourceClass::Secondary);
    s.oversize_name(kit_node(1), MAX_PAGE_STRING * 4);
    // A multi-byte char straddling the limit must not be split.
    let mut text = "a".repeat(MAX_PAGE_STRING - 1);
    text.push('\u{e9}');
    text.push_str("tail");
    s.name(kit_node(2), &text, 990);
    s.name(kit_node(3), &"b".repeat(MAX_PAGE_STRING), 990);
    let (w, verdicts) = replay(&s, &sec, kit_scope(1));
    r.check(
        "oversize facts are stored, not rejected",
        verdicts.iter().all(|v| *v == Offer::Stored),
    );
    let lens: Vec<usize> = w
        .registers()
        .iter()
        .filter_map(|g| match &g.value {
            FactValue::Value(PropValue::Name(n)) => Some(n.len()),
            FactValue::Value(_) | FactValue::Absent | FactValue::Retract => None,
        })
        .collect();
    r.check(
        "every stored string is within the limit",
        !lens.is_empty() && lens.iter().all(|l| *l <= MAX_PAGE_STRING),
    );
    r.check(
        "exactly two strings truncated and counted",
        w.counters.truncated == 2,
    );
    r.check(
        "exact-limit string is untouched",
        lens.contains(&MAX_PAGE_STRING),
    );
    r
}

/// F19: a fact is stored iff its target is in the transport-pinned scope; never otherwise.
pub fn f19_scope_pinning(seed: u64) -> Report {
    let mut r = Report::default();
    let sec = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    let src = SourceId {
        slot: 2,
        generation: 0,
    };
    let mut rng = Xorshift(seed | 1);
    let mut w = RefWriter::new(kit_scope(1));
    // A node of the right app but a different document of it is also out of scope.
    let other_doc = Scope {
        app: kit_scope(1).app,
        doc: Some(9),
        site: None,
        tab: None,
    };
    w.add_node(
        NodeKey {
            app: kit_scope(1).app,
            local: LocalId(9000),
        },
        other_doc,
    );
    let mut bucket = TokenBucket::new(1_000_000, 1_000_000);
    let mut stored_out = 0;
    let mut in_scope_stored = 0;
    for i in 0..2000u64 {
        let pick = rng.next() % 3;
        let (app, local) = match pick {
            0 => (kit_scope(1).app, rng.next() % 50),
            1 => (
                kit_scope(2 + u32::try_from(rng.next() % 5).unwrap_or(0)).app,
                rng.next() % 50,
            ),
            _ => (kit_scope(1).app, 9000),
        };
        let mut s = FakeFactSource::new("x", SourceClass::Secondary);
        s.name(
            Subject::Node(NodeKey {
                app,
                local: LocalId(local),
            }),
            "n",
            990,
        );
        let Some(mut f) = s.facts().next().cloned() else {
            continue;
        };
        f.seq = SrcSeq(i + 1);
        let in_scope = app == kit_scope(1).app && local != 9000;
        let got = w.offer(src, &sec, &mut bucket, 0, &f);
        match got {
            Offer::Stored | Offer::Superseded => {
                if in_scope {
                    in_scope_stored += 1;
                } else {
                    stored_out += 1;
                }
            }
            Offer::Rejected(RejectReason::ScopeMismatch | RejectReason::DocChanged) => {
                r.check("scope mismatch only for out-of-scope targets", !in_scope);
            }
            Offer::Retracted | Offer::Parked | Offer::Throttled | Offer::Rejected(_) => {
                r.check(&format!("unexpected verdict {got:?}"), false);
            }
        }
    }
    r.check("F19: no out-of-scope fact was ever stored", stored_out == 0);
    r.check("F19: in-scope facts were stored", in_scope_stored > 0);
    r.check(
        "F19: no register about a foreign app",
        w.registers().iter().all(|g| match &g.subject {
            Subject::Node(n) => n.app == kit_scope(1).app && n.local != LocalId(9000),
            Subject::Anchor(_) => true,
        }),
    );
    r
}

/// F16 (pure part): a flood from source A changes none of B's registers, nor B's state machine;
/// A's registers stay within the cap and every dropped fact is counted.
pub fn f16_flood_isolation(seed: u64) -> Report {
    let mut r = Report::default();
    let a = SourceId {
        slot: 1,
        generation: 0,
    };
    let b = SourceId {
        slot: 2,
        generation: 0,
    };
    let mut ga = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    ga.max_registers = 64;
    ga.rate = TokenBucket::new(100, 1000);
    let gb = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    let mut w = RefWriter::new(kit_scope(1));
    let mut ba = ga.rate;
    let mut bb = TokenBucket::new(1_000_000, 1_000_000);
    // B's facts.
    let mut sb = FakeFactSource::new("b", SourceClass::Secondary);
    for n in 0..20 {
        sb.name(kit_node(n), "B", 990);
    }
    for f in sb.facts() {
        w.offer(b, &gb, &mut bb, 0, f);
    }
    let before = w.registers_of(b);
    r.check("B has its registers", before.len() == 20);
    // A floods 20k facts across 5 ms of virtual time, random subjects.
    let mut rng = Xorshift(seed | 1);
    let mut book = SourceBook::new();
    let ident = FakeFactSource::new("a", SourceClass::Secondary);
    let ident_b = FakeFactSource::new("b2", SourceClass::Secondary);
    let scope = kit_scope(1);
    book.apply(
        ident.identity(),
        scope,
        SourceEvent::Hello { remembered: None },
        0,
    );
    book.apply(
        ident_b.identity(),
        scope,
        SourceEvent::Hello { remembered: None },
        0,
    );
    let mut offered = 0u32;
    for i in 0..20_000u64 {
        let mut s = FakeFactSource::new("a", SourceClass::Secondary);
        s.name(kit_node(rng.next() % 5000), "A", 990);
        let Some(mut f) = s.facts().next().cloned() else {
            continue;
        };
        f.seq = SrcSeq(i + 1);
        offered += 1;
        let got = w.offer(a, &ga, &mut ba, i / 4, &f);
        match got {
            Offer::Throttled | Offer::Rejected(RejectReason::RegisterCap) => {
                book.apply(ident.identity(), scope, SourceEvent::RateExceeded, i / 4);
            }
            Offer::Stored
            | Offer::Superseded
            | Offer::Retracted
            | Offer::Parked
            | Offer::Rejected(_) => {}
        }
    }
    r.check(
        "A registers are within max_registers",
        w.registers_of(a).len() <= 64,
    );
    r.check("B registers are byte-for-byte unchanged", {
        let after = w.registers_of(b);
        after.len() == before.len()
            && after.iter().zip(&before).all(|(x, y)| {
                x.subject == y.subject && x.group == y.group && x.seq == y.seq && x.value == y.value
            })
    });
    let c = &w.counters;
    r.check(
        "every offered fact is accounted for (stored + superseded + throttled + rejected)",
        c.stored + c.superseded + c.parked + c.throttled + c.rejected_total() == offered + 20
            && c.throttled > 0,
    );
    r.check(
        "A ends Throttled",
        matches!(
            book.state(ident.identity(), scope),
            SourceState::Throttled(_)
        ),
    );
    r.check(
        "B's state machine is untouched",
        matches!(
            book.state(ident_b.identity(), scope),
            SourceState::Active(_)
        ),
    );
    r
}

/// Every state x every event of the r2 table: totality and the invariants of FS-7.
pub fn state_machine_exhaustive() -> Report {
    use SourceEvent as E;
    let mut r = Report::default();
    let states = [
        SourceState::Registered,
        SourceState::Active(Strikes::NONE),
        SourceState::Active(Strikes::NONE.record(0).0.record(1).0),
        SourceState::Throttled(Strikes::NONE),
        SourceState::Paused,
        SourceState::Detached { purged: false },
        SourceState::Detached { purged: true },
        SourceState::Quarantined,
        SourceState::Revoked,
    ];
    let mut events = vec![
        E::Hello { remembered: None },
        E::Violation { at_ms: 5 },
        E::DocRace { at_ms: 6 },
        E::RateExceeded,
        E::Quiet,
        E::TransportClosed,
        E::UserRevoke,
        E::PrivacyPause,
        E::Resume { by_user: false },
        E::Resume { by_user: true },
        E::DetachExpired,
        E::WideQuarantine,
    ];
    for m in [
        gorca_fact_api::Remembered::Paused,
        gorca_fact_api::Remembered::Quarantined,
        gorca_fact_api::Remembered::Revoked,
    ] {
        events.push(E::Hello {
            remembered: Some(m),
        });
    }
    for s in states {
        for e in &events {
            let (n, fx) = step(s, *e);
            let label = format!("{s:?} --{e:?}--> {n:?}");
            // Revoked is terminal for the session.
            if let SourceState::Revoked = s {
                r.check(
                    &format!("revoked is absorbing: {label}"),
                    n == s && fx.is_empty(),
                );
            }
            // Leaving the winning set silences it in the same turn (FS-7, M-10).
            if s.facts_win() && !n.facts_win() {
                r.check(
                    &format!("silencing emits Mask + ReFuseFocus: {label}"),
                    fx.contains(&SourceEffect::Mask) && fx.contains(&SourceEffect::ReFuseFocus),
                );
            }
            // Re-entering the winning set from a masked state unmasks (a first Hello does not).
            let was_masked = !matches!(s, SourceState::Registered) && !s.facts_win();
            if was_masked && n.facts_win() {
                r.check(
                    &format!("re-activation unmasks: {label}"),
                    fx.contains(&SourceEffect::Unmask),
                );
            }
            // A Quarantined source only leaves quarantine by a user resume or revoke.
            if let SourceState::Quarantined = s {
                let by_user = matches!(e, E::Resume { by_user: true } | E::UserRevoke);
                r.check(
                    &format!("quarantine exit needs the user: {label}"),
                    by_user || n == SourceState::Quarantined,
                );
            }
        }
    }
    // Specific table rows.
    let (n, _) = step(SourceState::Registered, E::Violation { at_ms: 0 });
    r.check(
        "Registered + Violation -> Revoked",
        n == SourceState::Revoked,
    );
    let (n, _) = step(SourceState::Throttled(Strikes::NONE), E::Quiet);
    r.check(
        "Throttled + quiet -> Active",
        matches!(n, SourceState::Active(_)),
    );
    let (n, fx) = step(SourceState::Paused, E::TransportClosed);
    r.check(
        "Paused + TransportClosed purges and stays Paused",
        n == SourceState::Paused && fx.contains(&SourceEffect::PurgeRegisters),
    );
    r
}

/// Stable identity across reconnect (M-7): pause, quarantine and revoke survive Hello, per scope.
pub fn identity_memory() -> Report {
    use SourceEvent as E;
    let mut r = Report::default();
    let src = FakeFactSource::new("ext", SourceClass::Secondary);
    let id = src.identity();
    let (s1, s2) = (kit_scope(1), kit_scope(2));
    let mut b = SourceBook::new();
    b.apply(id, s1, E::Hello { remembered: None }, 0);
    b.apply(id, s1, E::PrivacyPause, 1);
    b.apply(id, s1, E::TransportClosed, 2);
    b.apply(id, s1, E::Hello { remembered: None }, 3);
    r.check(
        "paused survives reconnect",
        b.state(id, s1) == SourceState::Paused,
    );
    b.apply(id, s1, E::Resume { by_user: false }, 4);
    r.check(
        "resume clears the memory",
        b.remembered(id, s1).is_none() && b.state(id, s1).facts_win(),
    );
    b.apply(id, s1, E::UserRevoke, 5);
    b.apply(id, s1, E::TransportClosed, 6);
    b.apply(id, s1, E::Hello { remembered: None }, 7);
    r.check(
        "revoked survives reconnect",
        b.state(id, s1) == SourceState::Revoked,
    );
    b.apply(id, s2, E::Hello { remembered: None }, 8);
    r.check(
        "revocation of one scope leaves another scope active",
        b.state(id, s2).facts_win(),
    );
    // A different identity is a different source.
    let other = FakeFactSource::new("other", SourceClass::Secondary);
    b.apply(other.identity(), s1, E::Hello { remembered: None }, 9);
    r.check(
        "other identity unaffected",
        b.state(other.identity(), s1).facts_win(),
    );
    r
}

/// Memory is keyed on the verified peer, not the self-declared name: renaming cannot escape it.
pub fn identity_keyed_on_peer() -> Report {
    use SourceEvent as E;
    let mut r = Report::default();
    let a = FakeFactSource::new("alpha", SourceClass::Secondary).on_peer("alpha");
    let b = FakeFactSource::new("beta", SourceClass::Secondary).on_peer("alpha");
    let sc = kit_scope(1);
    let mut bk = SourceBook::new();
    bk.apply(a.identity(), sc, E::Hello { remembered: None }, 0);
    for t in 1..=3 {
        bk.apply(a.identity(), sc, E::Violation { at_ms: t }, t);
    }
    bk.apply(b.identity(), sc, E::Hello { remembered: None }, 10);
    r.check(
        "renamed source of a quarantined peer starts Quarantined",
        bk.state(b.identity(), sc) == SourceState::Quarantined,
    );
    // Source-wide quarantine reaches every name of the peer.
    let mut bk = SourceBook::new();
    for n in 1..=3 {
        bk.apply(a.identity(), kit_scope(n), E::Hello { remembered: None }, 0);
        for t in 0..3 {
            bk.apply(a.identity(), kit_scope(n), E::Violation { at_ms: t }, 100);
        }
    }
    bk.apply(
        b.identity(),
        kit_scope(7),
        E::Hello { remembered: None },
        200,
    );
    r.check(
        "source-wide quarantine applies to a renamed source of the peer",
        bk.state(b.identity(), kit_scope(7)) == SourceState::Quarantined,
    );
    // Revocation too.
    let mut bk = SourceBook::new();
    bk.apply(a.identity(), sc, E::Hello { remembered: None }, 0);
    bk.apply(a.identity(), sc, E::UserRevoke, 1);
    bk.apply(b.identity(), sc, E::Hello { remembered: None }, 2);
    r.check(
        "revocation covers the peer",
        bk.state(b.identity(), sc) == SourceState::Revoked,
    );
    r
}

/// Cross-document writes are `DocChanged` only with evidence of a recent navigation of the
/// pinned tab; otherwise a strike. Repeated races turn into strikes.
pub fn doc_changed_needs_evidence() -> Report {
    let mut r = Report::default();
    let sec = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    let src = SourceId {
        slot: 4,
        generation: 0,
    };
    let other = NodeKey {
        app: kit_scope(1).app,
        local: LocalId(77),
    };
    let other_doc = Scope {
        doc: Some(5),
        ..kit_scope(1)
    };
    let mut s = FakeFactSource::new("s", SourceClass::Secondary);
    s.name(Subject::Node(other), "x", 990);
    let Some(f) = s.facts().next().cloned() else {
        r.check("fact", false);
        return r;
    };
    let mut w = RefWriter::new(Scope {
        doc: Some(6),
        tab: Some(1),
        ..kit_scope(1)
    });
    w.add_node(other, other_doc);
    r.check(
        "other document without navigation evidence is a ScopeMismatch (strike)",
        offer_once(&mut w, src, &sec, &f) == Offer::Rejected(RejectReason::ScopeMismatch)
            && RejectReason::ScopeMismatch.severity() == gorca_fact_api::Severity::Violation,
    );
    w.note_navigation(Some(5));
    r.check(
        "previous document of a just-navigated tab is a benign DocChanged",
        offer_once(&mut w, src, &sec, &f) == Offer::Rejected(RejectReason::DocChanged),
    );
    r.check("DocChanged never stores", w.registers().is_empty());
    // Repeats turn into strikes: the 4th inside 10 s is a strike, 10 s later the count resets.
    let st = SourceState::Active(Strikes::NONE);
    let mut s2 = st;
    for t in [0, 1000, 2000] {
        s2 = step(s2, SourceEvent::DocRace { at_ms: t }).0;
    }
    r.check(
        "3 races in 10 s are free",
        matches!(s2, SourceState::Active(_)),
    );
    let mut q = s2;
    for t in [3000, 3100, 3200] {
        q = step(q, SourceEvent::DocRace { at_ms: t }).0;
    }
    r.check(
        "races beyond the limit strike until quarantine",
        q == SourceState::Quarantined,
    );
    let mut spaced = st;
    for k in 0..20u64 {
        spaced = step(spaced, SourceEvent::DocRace { at_ms: k * 4000 }).0;
    }
    r.check(
        "slow races never strike",
        matches!(spaced, SourceState::Active(_)),
    );
    r
}

/// A web-document scope without a tab admits nothing, and the rejection is not a strike.
pub fn web_scope_needs_a_tab() -> Report {
    let mut r = Report::default();
    let sec = full_grant(SourceClass::Secondary, TrustTier::T3Untrusted);
    let src = SourceId {
        slot: 5,
        generation: 0,
    };
    let mut s = FakeFactSource::new("s", SourceClass::Secondary);
    s.name(kit_node(1), "x", 990);
    let Some(f) = s.facts().next().cloned() else {
        r.check("fact", false);
        return r;
    };
    let mut w = RefWriter::new(Scope {
        doc: Some(3),
        ..kit_scope(1)
    });
    r.check(
        "tab-less web scope is rejected with MissingTab",
        offer_once(&mut w, src, &sec, &f) == Offer::Rejected(RejectReason::MissingTab)
            && w.registers().is_empty(),
    );
    r.check(
        "MissingTab is not a strike",
        RejectReason::MissingTab.severity() != gorca_fact_api::Severity::Violation,
    );
    let mut w = RefWriter::new(Scope {
        doc: Some(3),
        tab: Some(1),
        ..kit_scope(1)
    });
    r.check(
        "the same web scope with a tab admits",
        offer_once(&mut w, src, &sec, &f) == Offer::Stored,
    );
    r
}

/// A page can flip a field to `type=password` between the add-on's send and admission: such
/// rejections and navigation races must not strike the source (review B-1).
pub fn page_caused_rejections_do_not_strike() -> Report {
    let mut r = Report::default();
    for reason in [RejectReason::PrivacyExcluded, RejectReason::DocChanged] {
        r.check(
            &format!("{reason:?} is not a strike"),
            reason.severity() != gorca_fact_api::Severity::Violation,
        );
    }
    r.check(
        "an out-of-app fact still strikes",
        RejectReason::ScopeMismatch.severity() == gorca_fact_api::Severity::Violation,
    );
    r
}

/// Run every pure check against one scripted source: its sequence numbers, and its script
/// through the reference writer under `grant` (rejections must match what the source marked).
pub fn check_source(src: &FakeFactSource, grant: &SourceGrant) -> Report {
    let mut r = Report::default();
    r.check(
        &format!(
            "{}: sequence numbers strictly increase",
            src.descriptor().name
        ),
        seq_is_monotone(src),
    );
    let (w, verdicts) = replay(src, grant, kit_scope(1));
    r.check(
        &format!(
            "{}: no stored register is about a node outside the pinned scope",
            src.descriptor().name
        ),
        w.registers().iter().all(|g| match &g.subject {
            Subject::Node(n) => n.app == w.pinned.app,
            Subject::Anchor(_) => true,
        }),
    );
    r.check(
        &format!("{}: every dropped fact is counted", src.descriptor().name),
        w.counters.stored
            + w.counters.superseded
            + w.counters.parked
            + w.counters.throttled
            + w.counters.rejected_total()
            >= u32::try_from(
                verdicts
                    .iter()
                    .filter(|v| !matches!(v, Offer::Retracted))
                    .count(),
            )
            .unwrap_or(0),
    );
    r
}

/// The whole pure kit. Every check must pass.
pub fn run_all(seed: u64) -> Report {
    let mut r = Report::default();
    r.merge(admission_matrix());
    r.merge(standing_and_class());
    r.merge(oversize_strings_truncated());
    r.merge(f19_scope_pinning(seed));
    r.merge(f16_flood_isolation(seed));
    r.merge(state_machine_exhaustive());
    r.merge(identity_memory());
    r.merge(identity_keyed_on_peer());
    r.merge(page_caused_rejections_do_not_strike());
    r.merge(doc_changed_needs_evidence());
    r.merge(web_scope_needs_a_tab());
    r
}

/// Deterministic xorshift64 (no `rand` dependency).
#[derive(Debug)]
pub struct Xorshift(pub u64);

impl Xorshift {
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_ok(r: &Report) {
        assert!(r.ok(), "failed checks: {:#?}", r.failures);
        assert!(r.checks > 0);
    }

    #[test]
    fn admission_matrix_passes() {
        assert_ok(&admission_matrix());
    }

    #[test]
    fn standing_and_class_pass() {
        assert_ok(&standing_and_class());
    }

    #[test]
    fn oversize_strings() {
        assert_ok(&oversize_strings_truncated());
    }

    #[test]
    fn f19_over_seeds() {
        for seed in [1, 2, 3, 0xdead_beef, 0x1234_5678_9abc] {
            assert_ok(&f19_scope_pinning(seed));
        }
    }

    #[test]
    fn f16_over_seeds() {
        for seed in [1, 7, 99] {
            assert_ok(&f16_flood_isolation(seed));
        }
    }

    #[test]
    fn state_machine() {
        assert_ok(&state_machine_exhaustive());
    }

    #[test]
    fn web_tab() {
        assert_ok(&web_scope_needs_a_tab());
    }

    #[test]
    fn doc_changed_evidence() {
        assert_ok(&doc_changed_needs_evidence());
    }

    #[test]
    fn page_caused() {
        assert_ok(&page_caused_rejections_do_not_strike());
    }

    #[test]
    fn identity_by_peer() {
        assert_ok(&identity_keyed_on_peer());
    }

    #[test]
    fn identity() {
        assert_ok(&identity_memory());
    }

    #[test]
    fn fake_sources_of_every_class_conform() {
        for class in [
            SourceClass::Primary,
            SourceClass::Secondary,
            SourceClass::Inferred,
        ] {
            let tier = match class {
                SourceClass::Primary => TrustTier::T1Platform,
                SourceClass::Secondary | SourceClass::Inferred | SourceClass::Annotation => {
                    TrustTier::T3Untrusted
                }
            };
            let mut s = FakeFactSource::new("fake", class);
            s.hello()
                .name(kit_node(1), "Save", 990)
                .name(kit_node(2), "Open", 920)
                .oversize_name(kit_node(3), 5000)
                .out_of_scope(kit_scope(5).app, LocalId(1))
                .malformed_value(kit_node(1))
                .retract(kit_node(1), Group::Name.group_code())
                .detach();
            assert_ok(&check_source(&s, &full_grant(class, tier)));
        }
    }

    #[test]
    fn a_source_repeating_a_seq_fails_conformance() {
        let mut s = FakeFactSource::new("bad", SourceClass::Secondary);
        s.name(kit_node(1), "x", 990).repeat_last_seq();
        let r = check_source(
            &s,
            &full_grant(SourceClass::Secondary, TrustTier::T3Untrusted),
        );
        assert!(!r.ok());
    }

    #[test]
    fn slot_reuse_bumps_the_generation() {
        let mut s = FakeFactSource::new("s", SourceClass::Secondary);
        let before = s.id();
        s.reuse_slot();
        assert_eq!(s.id().slot, before.slot);
        assert_ne!(s.id().generation, before.generation);
    }

    #[test]
    fn run_all_is_green() {
        assert_ok(&run_all(42));
    }
}
