// SPDX-License-Identifier: GPL-3.0-or-later
//! The grant: what the core lets one source do (ADR-0024 section 2.6). Computed by the core
//! from the descriptor, the manifest and the settings; never taken from the source.

use crate::descriptor::AnchorKinds;
use gorca_model::sem::{
    BindQuality, EvidenceKind, GroupClass, GroupCode, GroupMask, SourceClass, TrustTier,
};

/// Number of group classes that carry their own binding minimum and confidence floor.
pub const GROUP_CLASSES: usize = 3;

/// Class of a group for `min_bind` and `conf_floor` (ADR-0024 section 2.3.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum GroupBindClass {
    /// Every semantic group (name and description included): needs `Unique`.
    Semantic,
    /// `hint.*` and `layout.*` evidence: accepts `Heuristic`.
    Evidence,
    /// `visual.*` evidence: accepts `Geometric`.
    Visual,
}

impl GroupBindClass {
    pub const ALL: [GroupBindClass; GROUP_CLASSES] = [
        GroupBindClass::Semantic,
        GroupBindClass::Evidence,
        GroupBindClass::Visual,
    ];

    /// Index into `min_bind` / `conf_floor`.
    pub const fn index(self) -> usize {
        match self {
            GroupBindClass::Semantic => 0,
            GroupBindClass::Evidence => 1,
            GroupBindClass::Visual => 2,
        }
    }

    /// The class of a registered group code, `None` for an unregistered code.
    pub const fn of(code: GroupCode) -> Option<GroupBindClass> {
        match code.classify() {
            GroupClass::Semantic(_) => Some(GroupBindClass::Semantic),
            GroupClass::Evidence(k) => Some(match k {
                EvidenceKind::VisualOcrText | EvidenceKind::VisualLabelGuess => {
                    GroupBindClass::Visual
                }
                EvidenceKind::HintIconClass
                | EvidenceKind::HintSrcName
                | EvidenceKind::HintTitleAttr
                | EvidenceKind::LayoutBox
                | EvidenceKind::LayoutCssHidden => GroupBindClass::Evidence,
            }),
            GroupClass::Unregistered(_) => None,
        }
    }
}

/// A bit set over the registered evidence kinds.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct EvidenceMask(pub u16);

impl EvidenceMask {
    pub const EMPTY: EvidenceMask = EvidenceMask(0);
    pub const ALL: EvidenceMask = EvidenceMask(0x7f);

    const fn bit(k: EvidenceKind) -> u16 {
        1 << (k.code() & 0x7fff)
    }

    pub const fn has(self, k: EvidenceKind) -> bool {
        self.0 & Self::bit(k) != 0
    }

    #[must_use]
    pub const fn with(self, k: EvidenceKind) -> EvidenceMask {
        EvidenceMask(self.0 | Self::bit(k))
    }
}

/// Token bucket applied per (source, scope) inside the port's `offer` (M23-0). Pure: the
/// caller supplies the time. Tokens are kept in milli-tokens so slow rates do not round to 0.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct TokenBucket {
    pub capacity: u32,
    pub per_sec: u32,
    milli: u64,
    last_ms: u64,
}

impl TokenBucket {
    /// A full bucket.
    #[allow(clippy::cast_lossless)]
    pub const fn new(capacity: u32, per_sec: u32) -> TokenBucket {
        TokenBucket {
            capacity,
            per_sec,
            milli: capacity as u64 * 1000,
            last_ms: 0,
        }
    }

    /// Take one token at `now_ms`; false when the bucket is empty (the offer is `Throttled`).
    pub fn try_take(&mut self, now_ms: u64) -> bool {
        let dt = now_ms.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now_ms);
        let cap = u64::from(self.capacity) * 1000;
        self.milli = self
            .milli
            .saturating_add(dt.saturating_mul(u64::from(self.per_sec)))
            .min(cap);
        if self.milli >= 1000 {
            self.milli -= 1000;
            true
        } else {
            false
        }
    }
}

/// What privacy classes of subject this source may see (FS-8). `Secret` subjects (ADR-0002
/// X-1..X-11) are never allowed, whatever the grant says.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PrivacyGrant {
    pub allow_restricted: bool,
}

/// The grant for one source.
#[derive(Clone, Debug)]
pub struct SourceGrant {
    pub class: SourceClass,
    /// Anchor kinds the source declared in its descriptor.
    pub anchors: AnchorKinds,
    pub tier: TrustTier,
    pub semantic_groups: GroupMask,
    pub evidence_groups: EvidenceMask,
    pub min_bind: [BindQuality; GROUP_CLASSES],
    pub conf_floor: [u16; GROUP_CLASSES],
    pub rate: TokenBucket,
    pub max_registers: u32,
    pub max_port_entries: u32,
    pub max_parked: u16,
    pub synthetic: Option<u32>,
    pub privacy: PrivacyGrant,
}

impl SourceGrant {
    /// A grant with the ADR defaults and **no** groups: semantic groups need `Unique`,
    /// evidence `Heuristic`, visual evidence `Geometric`; floors 0; bounded registers.
    pub fn new(class: SourceClass, tier: TrustTier) -> SourceGrant {
        SourceGrant {
            class,
            anchors: AnchorKinds::NONE,
            tier,
            semantic_groups: GroupMask::EMPTY,
            evidence_groups: EvidenceMask::EMPTY,
            min_bind: [
                BindQuality::Unique,
                BindQuality::Heuristic,
                BindQuality::Geometric,
            ],
            conf_floor: [0; GROUP_CLASSES],
            rate: TokenBucket::new(200, 1000),
            max_registers: 4096,
            max_port_entries: 1024,
            max_parked: 256,
            synthetic: None,
            privacy: PrivacyGrant::default(),
        }
    }

    pub fn min_bind_for(&self, c: GroupBindClass) -> BindQuality {
        self.min_bind[c.index()]
    }

    pub fn floor_for(&self, c: GroupBindClass) -> u16 {
        self.conf_floor[c.index()]
    }

    /// True when the grant lists this registered group.
    pub fn grants(&self, code: GroupCode) -> bool {
        match code.classify() {
            GroupClass::Semantic(g) => self.semantic_groups.has(g),
            GroupClass::Evidence(k) => self.evidence_groups.has(k),
            GroupClass::Unregistered(_) => false,
        }
    }
}

/// True when a source of `class` may write this kind of group at all (ADR-0024 section 2.1):
/// Primary writes semantic groups; Secondary and Inferred write semantic and evidence;
/// Annotation is never fused.
pub const fn class_allows(class: SourceClass, group: GroupClass) -> bool {
    match (class, group) {
        (SourceClass::Primary, GroupClass::Semantic(_))
        | (
            SourceClass::Secondary | SourceClass::Inferred,
            GroupClass::Semantic(_) | GroupClass::Evidence(_),
        ) => true,
        (SourceClass::Primary, GroupClass::Evidence(_) | GroupClass::Unregistered(_))
        | (SourceClass::Secondary | SourceClass::Inferred, GroupClass::Unregistered(_))
        | (SourceClass::Annotation, _) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_refills_and_caps() {
        let mut b = TokenBucket::new(2, 1);
        assert!(b.try_take(0));
        assert!(b.try_take(0));
        assert!(!b.try_take(0));
        assert!(!b.try_take(500));
        assert!(b.try_take(1000));
        // Long idle never exceeds capacity.
        assert!(b.try_take(1_000_000));
        assert!(b.try_take(1_000_000));
        assert!(!b.try_take(1_000_000));
        // Time going backwards never refills.
        assert!(!b.try_take(5));
    }

    #[test]
    fn every_evidence_kind_fits_the_mask() {
        let mut m = EvidenceMask::EMPTY;
        for k in EvidenceKind::ALL {
            assert!(!m.has(*k));
            m = m.with(*k);
        }
        assert_eq!(m, EvidenceMask::ALL);
    }

    #[test]
    fn bind_class_of_every_registered_code() {
        for k in EvidenceKind::ALL {
            assert!(GroupBindClass::of(k.group_code()).is_some());
        }
        assert_eq!(GroupBindClass::of(GroupCode(0x1234)), None);
        assert_eq!(
            GroupBindClass::of(EvidenceKind::VisualOcrText.group_code()),
            Some(GroupBindClass::Visual)
        );
    }
}
