// SPDX-License-Identifier: GPL-3.0-or-later
//! Pure admission (ADR-0024 section 2.6). Runs in the node's writer before a register is
//! touched. No clock, no IO, no allocation unless a page string must be truncated or the fact
//! is admitted (the admitted fact is a clone: `Arc` bumps only).

use std::sync::Arc;

use gorca_model::fact::{AnchorKey, BindGen, EvidenceValue, Fact, FactValue, PropValue, Subject};
use gorca_model::ids::{AppId, NodeKey};
use gorca_model::sem::{BindQuality, ConfClass, GroupClass, GroupCode, SourceClass, TrustTier};

use crate::descriptor::AnchorKinds;
use crate::grant::{GroupBindClass, SourceGrant, class_allows};

/// Page-derived strings longer than this are truncated and counted, not violations (M-6).
pub const MAX_PAGE_STRING: usize = 1024;

/// A scope: an app, or one document of an app. Pinned by the transport (FS-11).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct Scope {
    pub app: AppId,
    pub doc: Option<u64>,
    /// The site (document host hash) of a web document. The source-wide quarantine counts
    /// distinct sites, not tabs or reloads (review B-1).
    pub site: Option<u64>,
    /// The browser tab, derived by the core from the connection (never declared by the
    /// source). Stable across reloads and navigations of one tab, unlike `doc`.
    pub tab: Option<u64>,
}

impl Scope {
    /// What the source-wide quarantine counts: the site within an app, not the document. Without
    /// a site the key falls back to the tab (stable across reloads), never to the per-navigation
    /// document id, so a site-less page reloading itself stays one key.
    pub const fn site_key(&self) -> (AppId, Option<u64>, Option<u64>) {
        match self.site {
            Some(_) => (self.app, self.site, None),
            None => (self.app, None, self.tab),
        }
    }
}

/// Privacy class of a subject (ADR-0002 X-1..X-11, FS-8).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PrivacyClass {
    Normal,
    /// Allowed only when the grant says so.
    Restricted,
    /// Passwords, card numbers, ...: never visible to a non-primary source.
    Secret,
}

/// What the writer knows about the target of a subject.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BindInfo {
    pub quality: BindQuality,
    pub generation: BindGen,
}

impl BindInfo {
    pub const UNBOUND: BindInfo = BindInfo {
        quality: BindQuality::Unbound,
        generation: BindGen(0),
    };
}

/// Read-only view of the writer's state that admission needs. Implemented by the writer (and by
/// the test kit); keeps `admit` a pure function of its arguments.
pub trait ScopeLookup {
    /// The scope a node of this app lives in, `None` when the node is not (yet) known.
    fn node_scope(&self, node: NodeKey) -> Option<Scope>;
    /// True when `doc` is the pinned tab's **previous** document and the tab navigated within
    /// the race window (about 2 s). Evidence of a benign navigation race, nothing else.
    fn was_recent_doc(&self, doc: Option<u64>, pinned: Scope) -> bool;
    /// How well the subject is bound right now, and the current generation. `pinned` is the
    /// transport-pinned scope: an implementation must resolve anchors only inside it, so a
    /// per-app id index can never bind across tabs (review B-5).
    fn bind(&self, subject: &Subject, pinned: Scope) -> BindInfo;
    fn privacy(&self, subject: &Subject) -> PrivacyClass;
    /// True when a register for (this source, subject, group) already exists.
    fn has_register(&self, subject: &Subject, group: GroupCode) -> bool;
}

/// Everything admission reads besides the fact and the grant.
pub struct ScopeView<'a> {
    /// The scope the transport pinned for this connection.
    pub pinned: Scope,
    pub lookup: &'a dyn ScopeLookup,
    /// Registers this source already holds in this scope.
    pub registers: u32,
    /// Facts this source already has parked in this scope.
    pub parked: u16,
}

impl std::fmt::Debug for ScopeView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScopeView")
            .field("pinned", &self.pinned)
            .field("registers", &self.registers)
            .field("parked", &self.parked)
            .finish_non_exhaustive()
    }
}

/// How a rejection feeds the source state machine.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Severity {
    /// A strike (`SourceEvent::Violation`).
    Violation,
    /// A resource cap was hit (`SourceEvent::RateExceeded`).
    Overload,
    /// Dropped and counted, no consequence.
    Benign,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RejectReason {
    /// FS-11 / F19: the target is outside the transport-pinned scope.
    ScopeMismatch,
    /// The node lives in the pinned tab's previous document and the tab navigated within the
    /// race window: rejected and counted, never stored. Benign up to a small rate, then a
    /// strike (`SourceEvent::DocRace`). A node of any other document is `ScopeMismatch`.
    DocChanged,
    /// The class may not write this kind of group (for example Annotation facts are never fused).
    ClassNotAllowed,
    /// The group code is not registered.
    UnregisteredGroup,
    GroupNotGranted,
    AnchorKindUndeclared,
    /// Value shape does not match the group code, or `p_milli` > 1000.
    Malformed,
    /// An Inferred fact without a TTL (required, section 2.4).
    MissingTtl,
    /// The parking lot of this (source, scope) is full.
    ParkFull,
    RegisterCap,
    /// The subject is excluded for non-primary sources (FS-8).
    PrivacyExcluded,
}

impl RejectReason {
    pub const fn severity(self) -> Severity {
        match self {
            RejectReason::ScopeMismatch
            | RejectReason::ClassNotAllowed
            | RejectReason::UnregisteredGroup
            | RejectReason::GroupNotGranted
            | RejectReason::AnchorKindUndeclared
            | RejectReason::Malformed
            | RejectReason::MissingTtl => Severity::Violation,
            RejectReason::RegisterCap => Severity::Overload,
            // Page-caused or timing races: a page can flip `type=password` between the
            // add-on's send and admission, so these never strike (review B-1).
            RejectReason::ParkFull | RejectReason::PrivacyExcluded | RejectReason::DocChanged => {
                Severity::Benign
            }
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Rejection {
    pub reason: RejectReason,
}

impl Rejection {
    pub const fn severity(self) -> Severity {
        self.reason.severity()
    }
}

/// Whether an admitted fact may win. A fact that cannot win is still stored for `explain`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Standing {
    Eligible,
    /// Binding below the grant's minimum for the group class (IR-3 analogue).
    BelowBindMin,
    /// `p_milli` below the grant's floor for the group class.
    BelowFloor,
}

#[derive(Clone, Debug)]
pub struct Admitted {
    /// The fact with page strings truncated.
    pub fact: Fact,
    /// Recomputed from `p_milli` and the grant (never the source's own class), after the tier cap.
    pub class: ConfClass,
    /// Current binding at admission; `generation` is the `BindGen` to stamp on the register.
    pub bind: BindInfo,
    pub standing: Standing,
    /// Number of strings truncated to `MAX_PAGE_STRING`.
    pub truncated: u32,
}

#[derive(Clone, Debug)]
pub enum Admission {
    Admit(Admitted),
    /// The anchor is not bound yet: park it (the caller owns the fact), retry on index change.
    Park,
    Reject(Rejection),
}

const fn reject(reason: RejectReason) -> Admission {
    Admission::Reject(Rejection { reason })
}

/// The weaker binding of two.
fn worse_bind(a: BindQuality, b: BindQuality) -> BindQuality {
    if a.at_least(b) { b } else { a }
}

/// The class a fact gets: Primary facts are `Asserted`; everything else comes from `p_milli`
/// and is capped at `Probable` for T3 (math report section 10.4 item 3). Never `Asserted`.
fn class_of(grant: &SourceGrant, p_milli: u16) -> ConfClass {
    let c = match grant.class {
        SourceClass::Primary => return ConfClass::Asserted,
        SourceClass::Secondary | SourceClass::Inferred | SourceClass::Annotation => {
            ConfClass::from_p_milli(p_milli)
        }
    };
    match grant.tier {
        TrustTier::T3Untrusted => c.weaker(ConfClass::Probable),
        TrustTier::T0Core | TrustTier::T1Platform | TrustTier::T2FirstParty => c,
    }
}

fn cut(s: &Arc<str>, n: &mut u32) -> Option<Arc<str>> {
    if s.len() <= MAX_PAGE_STRING {
        return None;
    }
    let mut end = MAX_PAGE_STRING;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    *n += 1;
    s.get(..end).map(Arc::from)
}

fn needs_truncation(f: &Fact) -> bool {
    let long = |s: &Arc<str>| s.len() > MAX_PAGE_STRING;
    let anchor = match &f.subject {
        Subject::Anchor(AnchorKey::DomId { id, .. }) => long(id),
        Subject::Anchor(_) | Subject::Node(_) => false,
    };
    anchor
        || match &f.value {
            FactValue::Value(
                PropValue::Name(s)
                | PropValue::Description(s)
                | PropValue::Role {
                    role_description: Some(s),
                    ..
                }
                | PropValue::Evidence(EvidenceValue::Text(s)),
            ) => long(s),
            FactValue::Value(_) | FactValue::Absent | FactValue::Retract => false,
        }
}

/// Truncate every page-derived string; returns how many were cut.
fn truncate(f: &mut Fact) -> u32 {
    let mut n = 0;
    if let Subject::Anchor(AnchorKey::DomId { id, .. }) = &mut f.subject
        && let Some(c) = cut(id, &mut n)
    {
        *id = c;
    }
    if let FactValue::Value(v) = &mut f.value {
        match v {
            PropValue::Name(s)
            | PropValue::Description(s)
            | PropValue::Evidence(EvidenceValue::Text(s))
            | PropValue::Role {
                role_description: Some(s),
                ..
            } => {
                if let Some(c) = cut(s, &mut n) {
                    *s = c;
                }
            }
            PropValue::Role {
                role_description: None,
                ..
            }
            | PropValue::Check(_)
            | PropValue::Press(_)
            | PropValue::Expand(_)
            | PropValue::Select(_)
            | PropValue::Current(_)
            | PropValue::Invalid(_)
            | PropValue::Popup(_)
            | PropValue::Avail(_)
            | PropValue::Flags(_)
            | PropValue::Level(_)
            | PropValue::SetPos(_)
            | PropValue::Grid(_)
            | PropValue::Live(_)
            | PropValue::Orientation(_)
            | PropValue::Sort(_)
            | PropValue::Evidence(EvidenceValue::Box(_) | EvidenceValue::Flag(_)) => {}
        }
    }
    n
}

/// The ADR-0024 section 2.6 admission checks, in order: scope pinned and matching (FS-11);
/// class allows the group; group in grant; anchor kind declared; value well formed; binding
/// quality (parked when unbound, stored but never winning below the minimum); register cap;
/// privacy (FS-8). Well-formedness runs before binding so a parked fact is always well formed.
///
/// The declared anchor kinds are `g.anchors` (computed from the descriptor).
pub fn admit(f: &Fact, g: &SourceGrant, scope: &ScopeView<'_>) -> Admission {
    // 1. Scope (FS-11, F19). A subject's scope is never taken from the fact. A node whose
    // scope is not known yet fails closed: it is parked, never admitted (re-run on retry).
    let mut scope_unknown = false;
    if let Subject::Node(n) = &f.subject {
        if n.app != scope.pinned.app {
            return reject(RejectReason::ScopeMismatch);
        }
        match scope.lookup.node_scope(*n) {
            Some(s) if s != scope.pinned => {
                return reject(
                    if s.app == scope.pinned.app && scope.lookup.was_recent_doc(s.doc, scope.pinned)
                    {
                        RejectReason::DocChanged
                    } else {
                        RejectReason::ScopeMismatch
                    },
                );
            }
            Some(_) => {}
            None => scope_unknown = true,
        }
    }
    // 2. Class allows this kind of group.
    let gc = f.group.classify();
    if let GroupClass::Unregistered(_) = gc {
        return reject(RejectReason::UnregisteredGroup);
    }
    if !class_allows(g.class, gc) {
        return reject(RejectReason::ClassNotAllowed);
    }
    // 3. Group in grant.
    if !g.grants(f.group) {
        return reject(RejectReason::GroupNotGranted);
    }
    // 4. Anchor kind declared.
    if let Subject::Anchor(a) = &f.subject
        && !g.anchors.contains(AnchorKinds::of(a))
    {
        return reject(RejectReason::AnchorKindUndeclared);
    }
    // 5. Well formed (after truncation of page strings, which is not a violation).
    let mut truncated = 0;
    let owned;
    let fact = if needs_truncation(f) {
        let mut c = f.clone();
        truncated = truncate(&mut c);
        owned = c;
        &owned
    } else {
        f
    };
    if f.conf.p_milli > 1000 || !fact.is_well_formed() {
        return reject(RejectReason::Malformed);
    }
    if let SourceClass::Inferred = g.class
        && fact.ttl_ms.is_none()
        && !matches!(fact.value, FactValue::Retract)
    {
        return reject(RejectReason::MissingTtl);
    }
    // 6. Binding.
    let Some(bind_class) = GroupBindClass::of(f.group) else {
        return reject(RejectReason::UnregisteredGroup);
    };
    let mut bind = scope.lookup.bind(&fact.subject, scope.pinned);
    if let Subject::Anchor(AnchorKey::DomId { dom_count, .. }) = &fact.subject
        && *dom_count != 1
    {
        // An ambiguous id never binds `Unique` (F15), whatever the writer's index says.
        bind.quality = worse_bind(bind.quality, BindQuality::Heuristic);
    }
    if scope_unknown || matches!(bind.quality, BindQuality::Unbound) {
        return if scope.parked >= g.max_parked {
            reject(RejectReason::ParkFull)
        } else {
            Admission::Park
        };
    }
    // 7. Register cap: only a new register is refused; retractions always pass.
    if !matches!(fact.value, FactValue::Retract)
        && scope.registers >= g.max_registers
        && !scope.lookup.has_register(&fact.subject, fact.group)
    {
        return reject(RejectReason::RegisterCap);
    }
    // 8. Privacy (FS-8).
    match scope.lookup.privacy(&fact.subject) {
        PrivacyClass::Normal => {}
        PrivacyClass::Restricted => {
            if !g.privacy.allow_restricted {
                return reject(RejectReason::PrivacyExcluded);
            }
        }
        PrivacyClass::Secret => return reject(RejectReason::PrivacyExcluded),
    }
    let standing = if !bind.quality.at_least(g.min_bind_for(bind_class)) {
        Standing::BelowBindMin
    } else if f.conf.p_milli < g.floor_for(bind_class) {
        Standing::BelowFloor
    } else {
        Standing::Eligible
    };
    Admission::Admit(Admitted {
        fact: fact.clone(),
        class: class_of(g, f.conf.p_milli),
        bind,
        standing,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_respects_char_boundaries_and_counts() {
        let mut n = 0;
        let ok: Arc<str> = Arc::from("a".repeat(MAX_PAGE_STRING));
        assert!(cut(&ok, &mut n).is_none());
        assert_eq!(n, 0);
        let mut s = "a".repeat(MAX_PAGE_STRING - 1);
        s.push('\u{e9}');
        let long: Arc<str> = Arc::from(s);
        let c = cut(&long, &mut n);
        assert_eq!(n, 1);
        let c = c.unwrap_or_else(|| Arc::from(""));
        assert_eq!(c.len(), MAX_PAGE_STRING - 1);
    }

    struct Spy {
        seen: std::cell::Cell<Option<Scope>>,
    }

    impl ScopeLookup for Spy {
        fn node_scope(&self, _: NodeKey) -> Option<Scope> {
            None
        }
        fn was_recent_doc(&self, _: Option<u64>, _: Scope) -> bool {
            false
        }
        fn bind(&self, _: &Subject, pinned: Scope) -> BindInfo {
            self.seen.set(Some(pinned));
            BindInfo::UNBOUND
        }
        fn privacy(&self, _: &Subject) -> PrivacyClass {
            PrivacyClass::Normal
        }
        fn has_register(&self, _: &Subject, _: GroupCode) -> bool {
            false
        }
    }

    #[test]
    fn bind_receives_the_pinned_scope() {
        use gorca_model::fact::{Basis, SrcSeq};
        use gorca_model::ids::{ConnectorId, LocalId};
        use gorca_model::sem::{Group, TrustTier};
        let pinned = Scope {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            doc: Some(7),
            site: Some(1),
            tab: None,
        };
        let spy = Spy {
            seen: std::cell::Cell::new(None),
        };
        let view = ScopeView {
            pinned,
            lookup: &spy,
            registers: 0,
            parked: 0,
        };
        let mut g = SourceGrant::new(SourceClass::Secondary, TrustTier::T3Untrusted);
        g.semantic_groups = gorca_model::sem::GroupMask::EMPTY.with(Group::Name);
        let f = Fact {
            subject: Subject::Node(NodeKey {
                app: pinned.app,
                local: LocalId(1),
            }),
            group: Group::Name.group_code(),
            value: FactValue::Value(PropValue::Name(Arc::from("x"))),
            seq: SrcSeq(1),
            conf: gorca_model::fact::Confidence::from_p_milli(990),
            basis: Basis::None,
            ttl_ms: None,
        };
        // Unknown node scope fails closed: parked, never admitted, and bind saw the pinned scope.
        assert!(matches!(admit(&f, &g, &view), Admission::Park));
        assert_eq!(spy.seen.get(), Some(pinned));
    }

    #[test]
    fn reject_severity_table_is_total() {
        for r in [
            RejectReason::ScopeMismatch,
            RejectReason::ClassNotAllowed,
            RejectReason::UnregisteredGroup,
            RejectReason::GroupNotGranted,
            RejectReason::AnchorKindUndeclared,
            RejectReason::Malformed,
            RejectReason::MissingTtl,
        ] {
            assert_eq!(r.severity(), Severity::Violation);
        }
        for r in [RejectReason::PrivacyExcluded, RejectReason::DocChanged] {
            assert_eq!(r.severity(), Severity::Benign, "{r:?}");
        }
        assert_eq!(RejectReason::RegisterCap.severity(), Severity::Overload);
        assert_eq!(RejectReason::ParkFull.severity(), Severity::Benign);
    }
}
