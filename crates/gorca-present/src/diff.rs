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

/// Compare the legacy tokens with the new ones. `allowed` decides which classifiers count.
pub fn diff(legacy: &[LegacyToken], new: &[Token], allowed: &Deviations) -> DiffOutcome {
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
    if only_new.is_empty()
        && !only_legacy.is_empty()
        && only_legacy
            .iter()
            .all(|(k, _)| *k == kind_code(SegmentKind::Value))
    {
        let c = Classifier::ValueNotFused;
        if allowed.allows(c.id()) {
            return DiffOutcome::Allowed(c.id());
        }
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

    #[test]
    fn equal_order_only_and_different() {
        let d = Deviations::builtin();
        let legacy = [l(SegmentKind::Text, "OK"), l(SegmentKind::Role, "button")];
        assert_eq!(
            diff(
                &legacy,
                &[n(SegmentKind::Text, "OK"), n(SegmentKind::Role, "button")],
                &d
            ),
            DiffOutcome::Equal
        );
        assert_eq!(
            diff(
                &legacy,
                &[n(SegmentKind::Role, "button"), n(SegmentKind::Text, "OK")],
                &d
            ),
            DiffOutcome::OrderOnly
        );
        assert_eq!(
            diff(&legacy, &[n(SegmentKind::Text, "OK")], &d),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 0
            }
        );
        // The kind is part of the token: the same words as another kind differ.
        assert_eq!(
            diff(
                &legacy,
                &[n(SegmentKind::Text, "OK"), n(SegmentKind::Text, "button")],
                &d
            ),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 1
            }
        );
    }

    #[test]
    fn a_legacy_only_value_is_an_allowed_deviation() {
        let d = Deviations::builtin();
        let legacy = [l(SegmentKind::Text, "Vol"), l(SegmentKind::Value, "30")];
        assert_eq!(
            diff(&legacy, &[n(SegmentKind::Text, "Vol")], &d),
            DiffOutcome::Allowed("value-not-fused")
        );
        // A deviation list without the row allows nothing.
        let none = Deviations::empty();
        assert_eq!(
            diff(&legacy, &[n(SegmentKind::Text, "Vol")], &none),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 0
            }
        );
    }

    #[test]
    fn duplicates_count_as_a_multiset() {
        let d = Deviations::builtin();
        let legacy = [l(SegmentKind::State, "x"), l(SegmentKind::State, "x")];
        assert_eq!(
            diff(&legacy, &[n(SegmentKind::State, "x")], &d),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 0
            }
        );
    }
}
