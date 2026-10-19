// SPDX-License-Identifier: GPL-3.0-or-later
//! Taint and hedge (ADR-0024 section 2.2 "Taint", FS-12, H-5): every value produced from a
//! non-primary input carries the lowest trust tier and the weakest confidence class among its
//! inputs, and a T3 input caps the class at `Probable`.
//!
//! The `taint` mask holds the highest (least trusted) tier among the winner's inputs, the `hedge`
//! mask the class. Both are written from [`Info`] by the strata; this module owns how the pieces
//! combine. A non-primary winner is never `Asserted` (N-2): the constructors clamp.

use std::sync::Arc;

use gorca_model::sem::{ConfClass, Group, ProvCode, TrustTier};

use crate::profile::Limits;
use crate::side::FactCand;
use crate::strata::ChildView;
use crate::value::{Info, hedge_cap, truncate_str};

/// The class a non-primary winner carries: capped for T3 and never `Asserted`.
pub const fn non_primary_class(conf: ConfClass, tier: TrustTier) -> ConfClass {
    hedge_cap(conf.weaker(ConfClass::High), tier)
}

/// A Secondary winner (S0, inexpressible groups only).
pub fn secondary_info(c: &FactCand) -> Info {
    Info {
        prov: ProvCode::ObservedSecondary,
        conf: non_primary_class(c.conf(), c.tier()),
        tier: c.tier(),
    }
}

/// An Inferred winner (S4).
pub fn inferred_info(c: &FactCand) -> Info {
    Info {
        prov: ProvCode::Inferred,
        conf: non_primary_class(c.conf(), c.tier()),
        tier: c.tier(),
    }
}

/// A name derived from evidence (`title`): always tainted.
pub fn title_info(c: &FactCand) -> Info {
    Info {
        prov: ProvCode::DerivedTainted,
        conf: non_primary_class(c.conf(), c.tier()),
        tier: c.tier(),
    }
}

/// The result of name from content.
#[derive(Clone, Debug)]
pub struct Content {
    pub name: Arc<str>,
    pub info: Info,
    /// The string or the child list was cut at the profile's limits.
    pub truncated: bool,
    /// Writer-local ids of the children read.
    pub from: Vec<u32>,
    /// Which of them were tainted.
    pub tainted_from: Vec<u32>,
}

struct Part<'a> {
    id: u32,
    name: &'a Arc<str>,
    tainted: bool,
    conf: ConfClass,
    tier: TrustTier,
}

/// Name from content (S3): the children's final non-empty names joined with a space. Untainted
/// children come first: tainted names (Secondary, Inferred, DerivedTainted) are used only if no
/// untainted part exists, and then the result is `DerivedTainted` with the weakest input class and
/// the least trusted tier (math section 10.4 item 1).
///
/// Bounded work and output: at most `limits.max_children` children are read and the string stops
/// at `limits.max_text` bytes on a char boundary. Every child name is itself at most `max_text`,
/// so a parent's name is too, however deep the tree.
pub fn name_from_content(children: &[ChildView<'_>], limits: &Limits) -> Option<Content> {
    let full_len = children.len();
    let children = children.get(..limits.max_children).unwrap_or(children);
    let parts = children.iter().filter_map(|c| {
        let f = c.fused?;
        let name = f.name.as_ref()?;
        if name.is_empty() {
            return None;
        }
        Some(Part {
            id: c.id,
            name,
            tainted: f.prov.get(Group::Name).is_some_and(Info::is_tainted_prov),
            conf: f.hedge.conf(Group::Name),
            tier: f.taint.tier(Group::Name),
        })
    });
    let any_clean = parts.clone().any(|p| !p.tainted);
    let mut out = String::new();
    let mut n = 0usize;
    let mut cut = children.len() < full_len;
    let mut conf = ConfClass::Asserted;
    let mut tier = TrustTier::T0Core;
    let mut from = Vec::new();
    let mut tainted_from = Vec::new();
    for p in parts {
        // With an untainted part present only untainted parts count; otherwise all are tainted.
        if any_clean && p.tainted {
            continue;
        }
        if out.len() >= limits.max_text {
            cut = true;
            break;
        }
        if n > 0 {
            out.push(' ');
        }
        out.push_str(p.name);
        n += 1;
        from.push(p.id);
        if p.tainted {
            tainted_from.push(p.id);
            conf = conf.weaker(p.conf);
            tier = tier.least_trusted(p.tier);
        }
    }
    if n == 0 {
        return None;
    }
    if out.len() > limits.max_text {
        cut = true;
        let keep = truncate_str(&out, limits.max_text).len();
        out.truncate(keep);
    }
    let info = if any_clean {
        Info::plain(ProvCode::Derived)
    } else {
        Info {
            prov: ProvCode::DerivedTainted,
            conf: non_primary_class(conf, tier),
            tier,
        }
    };
    Some(Content {
        name: Arc::from(out),
        info,
        truncated: cut,
        from,
        tainted_from,
    })
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::SemEntry;

    use super::*;

    fn view(id: u32, e: &SemEntry) -> ChildView<'_> {
        ChildView {
            id,
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
        let lim = Limits::default();
        let kids = [view(0, &clean), view(1, &dirty)];
        let c = name_from_content(&kids, &lim).expect("name");
        assert_eq!(
            &*c.name, "Close",
            "a tainted child must not leak into an untainted name"
        );
        assert_eq!(c.info.prov, ProvCode::Derived);
        assert_eq!(c.from, vec![0]);
        // Only tainted parts: used, DerivedTainted with the weakest class and least trusted tier.
        let only = [view(7, &dirty)];
        let c = name_from_content(&only, &lim).expect("name");
        assert_eq!(&*c.name, "Logo");
        assert_eq!(c.info.prov, ProvCode::DerivedTainted);
        assert_eq!(c.info.conf, ConfClass::High);
        assert_eq!(c.info.tier, TrustTier::T2FirstParty);
        assert_eq!(c.tainted_from, vec![7]);
        let t3 = child_entry(
            "X",
            ProvCode::Inferred,
            ConfClass::High,
            TrustTier::T3Untrusted,
        );
        let c = name_from_content(&[view(0, &t3)], &lim).expect("name");
        assert_eq!(c.info.conf, ConfClass::Probable, "T3 caps at Probable");
        assert!(name_from_content(&[], &lim).is_none());
    }

    #[test]
    fn derived_from_asserted_tainted_input_is_still_hedged() {
        // A tainted child with an (impossible) Asserted class: the derived class is clamped.
        let dirty = child_entry(
            "Logo",
            ProvCode::ObservedSecondary,
            ConfClass::Asserted,
            TrustTier::T1Platform,
        );
        let c = name_from_content(&[view(0, &dirty)], &Limits::default()).expect("name");
        assert_eq!(c.info.conf, ConfClass::High);
        assert_eq!(
            non_primary_class(ConfClass::Asserted, TrustTier::T0Core),
            ConfClass::High
        );
    }

    #[test]
    fn wide_subtree_name_is_bounded() {
        let e = child_entry(
            "word",
            ProvCode::ObservedPrimary,
            ConfClass::Asserted,
            TrustTier::T0Core,
        );
        let kids: Vec<ChildView<'_>> = (0..10_000).map(|i| view(i, &e)).collect();
        let lim = Limits::default();
        let c = name_from_content(&kids, &lim).expect("name");
        assert!(
            c.truncated,
            "children beyond max_children are reported as truncation"
        );
        assert!(c.name.len() <= lim.max_text, "{} bytes", c.name.len());
        assert!(c.from.len() <= lim.max_children);
        // Few, huge children: cut at the text limit on a char boundary.
        let big = child_entry(
            &"é".repeat(2000),
            ProvCode::ObservedPrimary,
            ConfClass::Asserted,
            TrustTier::T0Core,
        );
        let kids = [view(0, &big), view(1, &big)];
        let c = name_from_content(&kids, &lim).expect("name");
        assert!(c.truncated && c.name.len() <= lim.max_text);
        assert!(
            c.name.chars().all(|ch| ch == 'é' || ch == ' '),
            "char boundary"
        );
    }

    #[test]
    fn deep_chain_does_not_amplify() {
        // Each level derives its name from two children of the level below; the string never
        // exceeds the limit however deep the chain is, and the work per level is bounded.
        let lim = Limits {
            max_text: 64,
            ..Limits::default()
        };
        let mut below = child_entry(
            &"x".repeat(60),
            ProvCode::ObservedPrimary,
            ConfClass::Asserted,
            TrustTier::T0Core,
        );
        for _ in 0..5_000 {
            let kids = [view(0, &below), view(1, &below)];
            let c = name_from_content(&kids, &lim).expect("name");
            assert!(c.name.len() <= 64);
            below = child_entry(&c.name, c.info.prov, c.info.conf, c.info.tier);
        }
        assert_eq!(below.name.as_deref().map(str::len), Some(64));
    }
}
