// SPDX-License-Identifier: GPL-3.0-or-later
//! Code registries and slot-packed masks (ADR-0024 section 2.12, layout x32).
//!
//! `gorca-model` is the source of truth. `crates/gorca-ext-api/src/codes.rs` holds a copy
//! (the extension SDK cannot depend on this crate) and an equality test plus an append-only
//! snapshot. `tests/fixtures/fuse/fuse-x32.json` is the oracle the tables are checked against.

use super::coded::coded_enum;

/// Number of group slots in the entry (`known`/`stale`/`fallback` are `u32` masks).
pub const GROUP_CAP: usize = 32;
/// Registered semantic groups (append-only; a new group up to `GROUP_CAP` is a registry append).
pub const GROUP_COUNT: usize = Group::ALL.len();
/// Bit 15 of a group code marks an evidence kind (W-3).
pub const EVIDENCE_BIT: u16 = 0x8000;
/// `p_milli` at or above which a fact is `High` (ADR-0024 section 2.5).
pub const HIGH_MILLI: u16 = 970;
/// `p_milli` at or above which a fact is `Probable`.
pub const PROBABLE_MILLI: u16 = 900;

// Layout gates (ADR-0024 section 2.12).
const _: () = assert!(GROUP_COUNT <= GROUP_CAP);
const _: () = assert!(GROUP_CAP * 4 <= 128);
const _: () = assert!(GROUP_CAP * 2 <= 64);

coded_enum! {
    /// The fused semantic groups, in code order (= slot index).
    pub enum Group: u8 {
        Role = 0 => "role",
        Name = 1 => "name",
        Description = 2 => "description",
        Check = 3 => "check",
        Press = 4 => "press",
        Expand = 5 => "expand",
        Select = 6 => "select",
        Current = 7 => "current",
        Invalid = 8 => "invalid",
        Popup = 9 => "popup",
        Avail = 10 => "avail",
        Flags = 11 => "flags",
        Level = 12 => "level",
        SetPos = 13 => "setpos",
        Grid = 14 => "grid",
        Live = 15 => "live",
        Orientation = 16 => "orientation",
        Sort = 17 => "sort",
        // Appended after the oracle corpus (M22a-C3, review W-6): the rare text of `SemText`
        // and the name source, one register each. Primary-only: no fact may target them.
        RoleDescription = 18 => "roledescription",
        Placeholder = 19 => "placeholder",
        KeyShortcuts = 20 => "keyshortcuts",
        ValueText = 21 => "valuetext",
        NameFrom = 22 => "namefrom",
        // The error message of an invalid node (`aria-errormessage` target text): a rare text
        // register like the others, primary-only.
        ErrorText = 23 => "errortext",
    }
}

coded_enum! {
    /// Evidence kinds: never presented directly, read only by S3/S4. Codes have bit 15 set.
    pub enum EvidenceKind: u16 {
        HintIconClass = 0x8000 => "hint.icon_class",
        HintSrcName = 0x8001 => "hint.src_name",
        HintTitleAttr = 0x8002 => "hint.title_attr",
        VisualOcrText = 0x8003 => "visual.ocr_text",
        VisualLabelGuess = 0x8004 => "visual.label_guess",
        LayoutBox = 0x8005 => "layout.box",
        LayoutCssHidden = 0x8006 => "layout.css_hidden",
    }
}

impl Group {
    /// True for the groups only a primary source writes (the rare text and the name source,
    /// appended in M22a-C3): no fact shape exists for them, so a secondary or inferred fact that
    /// names one is malformed.
    #[inline]
    pub const fn is_primary_only(self) -> bool {
        matches!(
            self,
            Group::RoleDescription
                | Group::Placeholder
                | Group::KeyShortcuts
                | Group::ValueText
                | Group::NameFrom
                | Group::ErrorText
        )
    }

    /// The slot index of this group in every mask.
    #[inline]
    pub const fn slot(self) -> u8 {
        self as u8
    }

    /// The wire group code.
    #[inline]
    pub const fn group_code(self) -> GroupCode {
        GroupCode(self as u8 as u16)
    }
}

impl EvidenceKind {
    /// The wire group code.
    #[inline]
    pub const fn group_code(self) -> GroupCode {
        GroupCode(self as u16)
    }
}

/// A wire group code (`u16`; bit 15 = evidence). Unregistered codes are representable on
/// purpose: admission rejects them as malformed rather than the decoder panicking.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct GroupCode(pub u16);

/// What a [`GroupCode`] denotes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GroupClass {
    Semantic(Group),
    Evidence(EvidenceKind),
    Unregistered(u16),
}

impl GroupCode {
    /// Classify the code against the registries.
    pub const fn classify(self) -> GroupClass {
        let c = self.0;
        if c & EVIDENCE_BIT != 0 {
            return match EvidenceKind::from_code(c) {
                Some(k) => GroupClass::Evidence(k),
                None => GroupClass::Unregistered(c),
            };
        }
        if c > u8::MAX as u16 {
            return GroupClass::Unregistered(c);
        }
        match Group::from_code(c.to_le_bytes()[0]) {
            Some(g) => GroupClass::Semantic(g),
            None => GroupClass::Unregistered(c),
        }
    }

    /// True for a registered semantic group or evidence kind.
    pub const fn is_registered(self) -> bool {
        match self.classify() {
            GroupClass::Semantic(_) | GroupClass::Evidence(_) => true,
            GroupClass::Unregistered(_) => false,
        }
    }

    /// True when bit 15 is set (evidence namespace, registered or not).
    pub const fn is_evidence(self) -> bool {
        self.0 & EVIDENCE_BIT != 0
    }

    /// Mask slot of a registered semantic group.
    pub const fn slot(self) -> Option<u8> {
        match self.classify() {
            GroupClass::Semantic(g) => Some(g.slot()),
            GroupClass::Evidence(_) | GroupClass::Unregistered(_) => None,
        }
    }
}

impl From<Group> for GroupCode {
    fn from(g: Group) -> Self {
        g.group_code()
    }
}

impl From<EvidenceKind> for GroupCode {
    fn from(k: EvidenceKind) -> Self {
        k.group_code()
    }
}

coded_enum! {
    /// Provenance of a group's winner: 4 bits per slot in [`ProvTag`], 7 codes spare.
    ///
    /// There is **no `Fallback` code**: a primary value used because a higher-ranked primary is
    /// `Unknown` is `ObservedPrimary` plus the `fallback` mask bit (math section 10.4 item 4).
    pub enum ProvCode: u8 {
        Default = 0 => "Default",
        Derived = 1 => "Derived",
        DerivedTainted = 2 => "DerivedTainted",
        Inferred = 3 => "Inferred",
        ObservedPrimary = 4 => "ObservedPrimary",
        ObservedSecondary = 5 => "ObservedSecondary",
        Patch = 6 => "Patch",
        User = 7 => "User",
        /// Made at once from the source record while the node waits for its fusion (PV-1,
        /// ADR-0022 section 8.3): the lowest stratum, below `Default` only in that it is replaced
        /// by the first real fusion. Never saved, never read by patch selectors.
        Provisional = 8 => "Provisional",
    }
}

coded_enum! {
    /// Confidence class (2 bits in the hedge mask; 0 = plain so a zeroed mask means "all plain").
    pub enum ConfClass: u8 {
        Asserted = 0 => "Asserted",
        High = 1 => "High",
        Probable = 2 => "Probable",
        Possible = 3 => "Possible",
    }
}

impl ConfClass {
    /// The class for a calibrated precision estimate, by the default thresholds. Admission
    /// floors are grant data and are applied by `admit`, not here.
    pub const fn from_p_milli(p: u16) -> ConfClass {
        if p >= HIGH_MILLI {
            ConfClass::High
        } else if p >= PROBABLE_MILLI {
            ConfClass::Probable
        } else {
            ConfClass::Possible
        }
    }

    /// True when `self` is strictly more certain than `other` (code 0 = most certain).
    pub const fn is_stronger_than(self, other: ConfClass) -> bool {
        self.code() < other.code()
    }

    /// The weaker (less certain) of two classes.
    #[must_use]
    pub const fn weaker(self, other: ConfClass) -> ConfClass {
        if self.code() >= other.code() {
            self
        } else {
            other
        }
    }
}

coded_enum! {
    /// How well an anchor is bound to a node (ADR-0024 section 2.3.2).
    pub enum BindQuality: u8 {
        Unbound = 0 => "Unbound",
        Geometric = 1 => "Geometric",
        Heuristic = 2 => "Heuristic",
        Unique = 3 => "Unique",
        Exact = 4 => "Exact",
    }
}

impl BindQuality {
    /// True when the binding is at least as good as `min` (higher code = better binding).
    pub const fn at_least(self, min: BindQuality) -> bool {
        self.code() >= min.code()
    }
}

coded_enum! {
    /// Source class, stored as a `u8` code.
    pub enum SourceClass: u8 {
        Primary = 0 => "Primary",
        Secondary = 1 => "Secondary",
        Inferred = 2 => "Inferred",
        Annotation = 3 => "Annotation",
    }
}

coded_enum! {
    /// Trust tier, stored as a `u8` code (also the 2-bit value in the taint mask; a lower code
    /// is more trusted, so the "highest tier among inputs" is the numerically largest).
    pub enum TrustTier: u8 {
        T0Core = 0 => "T0",
        T1Platform = 1 => "T1",
        T2FirstParty = 2 => "T2",
        T3Untrusted = 3 => "T3",
    }
}

impl TrustTier {
    /// True when `self` is strictly more trusted than `other` (code 0 = most trusted).
    pub const fn is_more_trusted_than(self, other: TrustTier) -> bool {
        self.code() < other.code()
    }

    /// The less trusted of two tiers: the taint of a value derived from both.
    #[must_use]
    pub const fn least_trusted(self, other: TrustTier) -> TrustTier {
        if self.code() >= other.code() {
            self
        } else {
            other
        }
    }
}

/// A bit set over the group slots (`known`, `stale`, `fallback`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct GroupMask(pub u32);

impl GroupMask {
    pub const EMPTY: GroupMask = GroupMask(0);

    #[inline]
    pub const fn has(self, g: Group) -> bool {
        self.0 & (1 << g.slot()) != 0
    }

    #[inline]
    #[must_use]
    pub const fn with(self, g: Group) -> GroupMask {
        GroupMask(self.0 | (1 << g.slot()))
    }

    #[inline]
    #[must_use]
    pub const fn without(self, g: Group) -> GroupMask {
        GroupMask(self.0 & !(1 << g.slot()))
    }

    #[inline]
    pub const fn set(&mut self, g: Group, on: bool) {
        *self = if on { self.with(g) } else { self.without(g) };
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Provenance of every slot: 32 slots x 4 bits in two `u64` words (not `u128`: alignment).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ProvTag(pub [u64; 2]);

impl ProvTag {
    #[inline]
    const fn locate(slot: u8) -> (usize, u32) {
        ((slot >> 4) as usize & 1, (slot as u32 & 15) * 4)
    }

    /// The raw 4-bit code of a slot.
    #[inline]
    pub const fn raw(self, g: Group) -> u8 {
        let (w, sh) = Self::locate(g.slot());
        ((self.0[w] >> sh) & 0xF) as u8
    }

    /// The provenance of a slot; `None` if the 4-bit code is one of the 8 spare values.
    #[inline]
    pub const fn get(self, g: Group) -> Option<ProvCode> {
        ProvCode::from_code(self.raw(g))
    }

    #[inline]
    pub const fn set(&mut self, g: Group, p: ProvCode) {
        let (w, sh) = Self::locate(g.slot());
        self.0[w] = (self.0[w] & !(0xF << sh)) | ((p as u64) << sh);
    }
}

/// A 2-bit value per slot in one `u64` (`hedge` and `taint`).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct SlotBits2(pub u64);

impl SlotBits2 {
    /// The 2-bit value of a slot (0..=3).
    #[inline]
    pub const fn get(self, g: Group) -> u8 {
        ((self.0 >> (g.slot() as u32 * 2)) & 0x3) as u8
    }

    /// Store the low two bits of `v` into a slot.
    #[inline]
    pub const fn set(&mut self, g: Group, v: u8) {
        let sh = g.slot() as u32 * 2;
        self.0 = (self.0 & !(0x3 << sh)) | (((v & 0x3) as u64) << sh);
    }

    /// The hedge view: the winner's confidence class.
    #[inline]
    pub const fn conf(self, g: Group) -> ConfClass {
        match ConfClass::from_code(self.get(g)) {
            Some(c) => c,
            // Unreachable: two bits always decode; kept total without a panic path.
            None => ConfClass::Possible,
        }
    }

    /// The taint view: the highest trust tier among the winner's inputs.
    #[inline]
    pub const fn tier(self, g: Group) -> TrustTier {
        match TrustTier::from_code(self.get(g)) {
            Some(t) => t,
            None => TrustTier::T3Untrusted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_are_dense_in_slot_order() {
        assert_eq!(GROUP_COUNT, 24);
        for (i, g) in Group::ALL.iter().enumerate() {
            assert_eq!(usize::from(g.slot()), i);
            assert_eq!(g.group_code().classify(), GroupClass::Semantic(*g));
        }
        assert_eq!(Group::Orientation.code(), 16);
        assert_eq!(Group::Sort.code(), 17);
        assert_eq!(Group::NameFrom.code(), 22);
        assert_eq!(Group::ErrorText.code(), 23);
        assert_eq!(Group::ALL.iter().filter(|g| g.is_primary_only()).count(), 6);
    }

    #[test]
    fn evidence_codes_have_bit_15() {
        for k in EvidenceKind::ALL {
            assert!(k.group_code().is_evidence());
            assert_eq!(k.group_code().classify(), GroupClass::Evidence(*k));
            assert_eq!(k.group_code().slot(), None);
        }
        assert!(GroupCode(0x8007).is_evidence());
        assert!(!GroupCode(0x8007).is_registered());
        assert!(GroupCode(23).is_registered());
        assert!(!GroupCode(24).is_registered());
        assert!(!GroupCode(0x0100).is_registered());
    }

    #[test]
    fn prov_tag_round_trips_every_slot_and_code() {
        for g in Group::ALL {
            for p in ProvCode::ALL {
                let mut t = ProvTag::default();
                t.set(*g, *p);
                assert_eq!(t.get(*g), Some(*p));
                for other in Group::ALL.iter().filter(|o| *o != g) {
                    assert_eq!(t.get(*other), Some(ProvCode::Default), "{g:?} leaks");
                }
            }
        }
        let mut t = ProvTag::default();
        t.0[0] = 0xF;
        assert_eq!(t.get(Group::Role), None, "spare code decodes to None");
    }

    #[test]
    fn two_bit_masks_are_isolated_per_slot() {
        let mut m = SlotBits2::default();
        m.set(Group::Sort, 3);
        m.set(Group::Orientation, 2);
        assert_eq!(m.conf(Group::Sort), ConfClass::Possible);
        assert_eq!(m.get(Group::Orientation), 2);
        assert_eq!(m.get(Group::Live), 0);
        m.set(Group::Sort, 0);
        assert_eq!(m.get(Group::Sort), 0);
        assert_eq!(m.get(Group::Orientation), 2);
        assert_eq!(m.tier(Group::Orientation), TrustTier::T2FirstParty);
    }

    #[test]
    fn group_mask_ops() {
        let mut m = GroupMask::EMPTY;
        m.set(Group::Sort, true);
        assert!(m.has(Group::Sort) && !m.has(Group::Orientation));
        assert_eq!(m.0, 1 << 17);
        m.set(Group::Sort, false);
        assert!(m.is_empty());
    }

    #[test]
    fn conf_class_thresholds() {
        assert_eq!(ConfClass::from_p_milli(970), ConfClass::High);
        assert_eq!(ConfClass::from_p_milli(969), ConfClass::Probable);
        assert_eq!(ConfClass::from_p_milli(900), ConfClass::Probable);
        assert_eq!(ConfClass::from_p_milli(899), ConfClass::Possible);
        assert_eq!(
            ConfClass::High.weaker(ConfClass::Probable),
            ConfClass::Probable
        );
    }

    #[test]
    fn ordering_directions_are_explicit() {
        assert!(ConfClass::High.is_stronger_than(ConfClass::Possible));
        assert!(!ConfClass::Possible.is_stronger_than(ConfClass::High));
        assert!(TrustTier::T0Core.is_more_trusted_than(TrustTier::T3Untrusted));
        assert_eq!(
            TrustTier::T1Platform.least_trusted(TrustTier::T3Untrusted),
            TrustTier::T3Untrusted
        );
        assert!(BindQuality::Exact.at_least(BindQuality::Unique));
        assert!(!BindQuality::Heuristic.at_least(BindQuality::Unique));
    }

    #[test]
    fn prov_code_has_no_fallback() {
        assert_eq!(ProvCode::from_name("Fallback"), None);
        assert_eq!(ProvCode::from_code(9), None);
        assert_eq!(
            ProvCode::ALL.len(),
            9,
            "eight observed codes and Provisional (PV-1)"
        );
        assert_eq!(
            ProvCode::from_name("Provisional"),
            Some(ProvCode::Provisional)
        );
    }
}
