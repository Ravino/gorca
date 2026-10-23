// SPDX-License-Identifier: GPL-3.0-or-later
//! The shadow diff (ADR-0022 section 2.11, M-13): token multisets of the legacy presenter and
//! of `gorca-present`, compared as (kind, literal).

use gorca_model::SegmentKind;

use crate::deviations::Deviations;
use crate::render::Token;

/// A token the legacy presenter produced, reduced to what is compared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyToken {
    pub kind: SegmentKind,
    pub text: std::sync::Arc<str>,
}

/// A stable ordinal of a segment kind (the type has no `Ord`).
pub const fn kind_code(k: SegmentKind) -> u8 {
    match k {
        SegmentKind::Text => 0,
        SegmentKind::Role => 1,
        SegmentKind::State => 2,
        SegmentKind::Value => 3,
        SegmentKind::Description => 4,
        SegmentKind::Said => 5,
        SegmentKind::Capital => 6,
    }
}

/// The result of one comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffOutcome {
    /// Same tokens in the same order.
    Equal,
    /// Same multiset, different order.
    OrderOnly,
    /// A difference listed in `deviations.toml`.
    Allowed(&'static str),
    /// A difference nobody allowed: `legacy_only` tokens the new presenter lacks, `new_only` the
    /// ones it added.
    Different { legacy_only: u32, new_only: u32 },
}

/// A classifier of a known, allowed difference. The id is a row of `deviations.toml`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Classifier {
    /// Every differing token is a legacy `Value` the fused entry does not carry yet.
    ValueNotFused,
}

impl Classifier {
    pub const fn id(self) -> &'static str {
        match self {
            Classifier::ValueNotFused => "value-not-fused",
        }
    }
}

type Key<'a> = (u8, &'a str);

fn keys_legacy(t: &[LegacyToken]) -> Vec<Key<'_>> {
    t.iter().map(|x| (kind_code(x.kind), &*x.text)).collect()
}

fn keys_new(t: &[Token]) -> Vec<Key<'_>> {
    t.iter()
        .map(|x| (kind_code(x.kind), x.text.as_str()))
        .collect()
}

/// Multiset difference of two sorted key lists: (only in `a`, only in `b`).
fn difference<'a>(a: &[Key<'a>], b: &[Key<'a>]) -> (Vec<Key<'a>>, Vec<Key<'a>>) {
    let (mut i, mut j) = (0, 0);
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                only_a.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                only_b.push(b[j]);
                j += 1;
            }
        }
    }
    only_a.extend_from_slice(&a[i..]);
    only_b.extend_from_slice(&b[j..]);
    (only_a, only_b)
}

/// What the diff needs to know about the comparison besides the tokens.
#[derive(Copy, Clone, Debug)]
pub struct DiffCtx<'a> {
    /// The surface being compared (`deviations.toml` `surface`).
    pub surface: &'a str,
    /// The fused entry has a value text: a missing value token is then a real bug, not a gap.
    pub entry_has_value_text: bool,
}

impl DiffCtx<'_> {
    /// The native focus surface with no fused value text.
    pub const FOCUS: DiffCtx<'static> = DiffCtx {
        surface: "focus",
        entry_has_value_text: false,
    };
}

/// A legacy `Value` token that is a range's current value (a number), not a field's contents.
fn is_range_value(t: &str) -> bool {
    t.trim().parse::<f64>().is_ok()
}

/// Compare the legacy tokens with the new ones. `allowed` decides which classifiers count on
/// `ctx.surface`.
pub fn diff(
    legacy: &[LegacyToken],
    new: &[Token],
    allowed: &Deviations,
    ctx: DiffCtx<'_>,
) -> DiffOutcome {
    let (la, lb) = (keys_legacy(legacy), keys_new(new));
    if la == lb {
        return DiffOutcome::Equal;
    }
    let (mut sa, mut sb) = (la.clone(), lb.clone());
    sa.sort_unstable();
    sb.sort_unstable();
    if sa == sb {
        return DiffOutcome::OrderOnly;
    }
    let (only_legacy, only_new) = difference(&sa, &sb);
    // value-not-fused: exactly one legacy-only token, a numeric Value, while the entry has no
    // value text and the new side has no value token at all.
    let c = Classifier::ValueNotFused;
    if only_new.is_empty()
        && !ctx.entry_has_value_text
        && only_legacy.len() == 1
        && only_legacy[0].0 == kind_code(SegmentKind::Value)
        && is_range_value(only_legacy[0].1)
        && !lb.iter().any(|(k, _)| *k == kind_code(SegmentKind::Value))
        && allowed.allows(c.id(), ctx.surface)
    {
        return DiffOutcome::Allowed(c.id());
    }
    DiffOutcome::Different {
        legacy_only: u32::try_from(only_legacy.len()).unwrap_or(u32::MAX),
        new_only: u32::try_from(only_new.len()).unwrap_or(u32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::render::TokenText;

    fn l(kind: SegmentKind, s: &str) -> LegacyToken {
        LegacyToken {
            kind,
            text: Arc::from(s),
        }
    }

    fn n(kind: SegmentKind, s: &str) -> Token {
        Token {
            kind,
            text: TokenText::Shared(Arc::from(s)),
        }
    }

    fn run(legacy: &[LegacyToken], new: &[Token]) -> DiffOutcome {
        diff(legacy, new, &Deviations::builtin(), DiffCtx::FOCUS)
    }

    #[test]
    fn equal_order_only_and_different() {
        let legacy = [l(SegmentKind::Text, "OK"), l(SegmentKind::Role, "button")];
        assert_eq!(
            run(
                &legacy,
                &[n(SegmentKind::Text, "OK"), n(SegmentKind::Role, "button")]
            ),
            DiffOutcome::Equal
        );
        assert_eq!(
            run(
                &legacy,
                &[n(SegmentKind::Role, "button"), n(SegmentKind::Text, "OK")]
            ),
            DiffOutcome::OrderOnly
        );
        assert_eq!(
            run(&legacy, &[n(SegmentKind::Text, "OK")]),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 0
            }
        );
        // The kind is part of the token: the same words as another kind differ.
        assert_eq!(
            run(
                &legacy,
                &[n(SegmentKind::Text, "OK"), n(SegmentKind::Text, "button")]
            ),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 1
            }
        );
    }

    #[test]
    fn only_a_numeric_legacy_value_with_no_fused_value_is_allowed() {
        let legacy = [l(SegmentKind::Text, "Vol"), l(SegmentKind::Value, "30")];
        let new = [n(SegmentKind::Text, "Vol")];
        assert_eq!(run(&legacy, &new), DiffOutcome::Allowed("value-not-fused"));
        // A list without the row allows nothing.
        assert!(matches!(
            diff(&legacy, &new, &Deviations::empty(), DiffCtx::FOCUS),
            DiffOutcome::Different { .. }
        ));
        // Another surface is not covered by the focus row.
        let browse = DiffCtx {
            surface: "browse",
            ..DiffCtx::FOCUS
        };
        assert!(matches!(
            diff(&legacy, &new, &Deviations::builtin(), browse),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn a_missing_value_is_a_real_difference_when_the_entry_has_a_value_text() {
        // The slider has "50 percent" in the entry: compose dropping it must not hide as a gap.
        let legacy = [l(SegmentKind::Text, "Vol"), l(SegmentKind::Value, "50")];
        let ctx = DiffCtx {
            entry_has_value_text: true,
            ..DiffCtx::FOCUS
        };
        assert!(matches!(
            diff(
                &legacy,
                &[n(SegmentKind::Text, "Vol")],
                &Deviations::builtin(),
                ctx
            ),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn a_text_entrys_contents_are_not_a_range_value() {
        let legacy = [l(SegmentKind::Text, "Name"), l(SegmentKind::Value, "Alice")];
        assert!(matches!(
            run(&legacy, &[n(SegmentKind::Text, "Name")]),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn a_different_value_is_not_hidden_by_the_row() {
        let legacy = [l(SegmentKind::Value, "30")];
        assert!(matches!(
            run(&legacy, &[n(SegmentKind::Value, "40")]),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn duplicates_count_as_a_multiset() {
        let legacy = [l(SegmentKind::State, "x"), l(SegmentKind::State, "x")];
        assert_eq!(
            run(&legacy, &[n(SegmentKind::State, "x")]),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 0
            }
        );
    }
}
