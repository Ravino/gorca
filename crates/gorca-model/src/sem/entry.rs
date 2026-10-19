// SPDX-License-Identifier: GPL-3.0-or-later
//! `Semantics` and `SemEntry` (ADR-0022 section 2.1, ADR-0024 section 2.12, layout x32).

use std::mem::size_of;
use std::sync::Arc;

use super::codes::{GroupMask, ProvTag, SlotBits2};
use super::role::SemRole;
use super::states::{
    Avail, Check, Current, Expand, GridPos, Invalid, Live, NameFrom, Orientation, Popup, Press,
    Select, SemFlags, SemText, SetPos, Sort,
};

/// Normalised semantics of one node: owned, immutable per snapshot (SM-7), no provenance field
/// (that lives in [`SemEntry`]). Rare strings, including `description` and `roledescription`,
/// share one `Arc<SemText>`; `Option<Arc<SemText>>` compares by contents.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Semantics {
    pub role: SemRole,
    pub check: Check,
    pub press: Press,
    pub expand: Expand,
    pub select: Select,
    pub current: Current,
    pub invalid: Invalid,
    pub popup: Popup,
    pub avail: Avail,
    pub name_from: NameFrom,
    pub orientation: Orientation,
    pub sort: Sort,
    pub flags: SemFlags,
    /// Heading or tree level; 0 = none.
    pub level: u8,
    pub pos: Option<SetPos>,
    pub grid: Option<GridPos>,
    /// `None` = live region off.
    pub live: Option<Live>,
    pub text: Option<Arc<SemText>>,
}

impl Semantics {
    /// The accessible description, if any.
    pub fn description(&self) -> Option<&str> {
        self.text.as_deref().and_then(|t| t.description.as_deref())
    }

    /// The author's `aria-roledescription`, if any.
    pub fn role_description(&self) -> Option<&str> {
        self.text
            .as_deref()
            .and_then(|t| t.role_description.as_deref())
    }
}

/// The fused result for one node: values plus per-group provenance, confidence, taint and the
/// `known`/`stale`/`fallback` masks. Unknown is a clear `known` bit, never an enum variant.
///
/// `name` stays outside `Semantics` (it changes with content and is shared with the snapshot).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SemEntry {
    pub sem: Semantics,
    pub name: Option<Arc<str>>,
    /// 32 slots x 4 bits, [`super::ProvCode`] index.
    pub prov: ProvTag,
    /// 32 x 2 bits: the winner's `ConfClass` after the T3 cap.
    pub hedge: SlotBits2,
    /// 32 x 2 bits: highest trust tier among the winner's inputs (FS-12); 0 = untainted core.
    pub taint: SlotBits2,
    pub known: GroupMask,
    pub stale: GroupMask,
    pub fallback: GroupMask,
    /// Bumped on every change of this entry (cache key, act-on-what-was-spoken guard).
    pub sem_rev: u32,
}

// Layout gates (ADR-0024 section 2.12). Measured: Semantics 64 B, SemEntry 128 B (x86_64).
const _: () = assert!(size_of::<Semantics>() <= 80);
const _: () = assert!(size_of::<SemEntry>() <= 160);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sem::codes::{Group, ProvCode};

    #[test]
    fn measured_sizes_on_64_bit() {
        eprintln!(
            "size_of Semantics = {} B, SemEntry = {} B",
            size_of::<Semantics>(),
            size_of::<SemEntry>()
        );
        if cfg!(target_pointer_width = "64") {
            assert!(size_of::<Semantics>() <= 64);
            assert_eq!(size_of::<SemEntry>(), 128);
        }
    }

    #[test]
    fn default_entry_is_all_unknown_and_plain() {
        let e = SemEntry::default();
        assert!(e.known.is_empty() && e.stale.is_empty() && e.fallback.is_empty());
        assert_eq!(e.prov.get(Group::Role), Some(ProvCode::Default));
        assert_eq!(e.sem.role, SemRole::Unknown);
    }

    #[test]
    fn description_equality_compares_contents_not_pointers() {
        let mk = |d: &str| Semantics {
            text: Some(Arc::new(SemText {
                description: Some(Arc::from(d)),
                ..SemText::default()
            })),
            ..Semantics::default()
        };
        let a = mk("Opens a dialog");
        let b = mk("Opens a dialog");
        let c = mk("Required");
        let (Some(ta), Some(tb)) = (&a.text, &b.text) else {
            panic!("text set");
        };
        assert!(!Arc::ptr_eq(ta, tb), "distinct allocations");
        assert_eq!(a, b, "equal contents, different Arcs");
        assert_ne!(a, c);
        assert_eq!(a.description(), Some("Opens a dialog"));
        assert_ne!(a, Semantics::default(), "text vs no text");
        // roledescription lives in the same Arc and takes part in equality.
        let mut d = b.clone();
        let mut t = SemText::clone(ta);
        t.role_description = Some(Arc::from("slide"));
        d.text = Some(Arc::new(t));
        assert_ne!(a, d);
        assert_eq!(d.role_description(), Some("slide"));
    }

    #[test]
    fn entry_equality_includes_masks_and_name() {
        let mut a = SemEntry::default();
        let b = a.clone();
        assert_eq!(a, b);
        a.name = Some(Arc::from("Close"));
        assert_ne!(a, b);
        let mut c = b.clone();
        c.hedge.set(Group::Name, 2);
        assert_ne!(c, b);
    }
}
