// SPDX-License-Identifier: GPL-3.0-or-later
//! Stratum R: canonicalise the role-dependent groups of the merged record (SM-1, SM-2, SM-5).
//!
//! `R` is a projection: every rule maps a record into the set where that rule's precondition is
//! false, and no rule re-enables another's precondition (rules read `role` only and `R` never
//! changes `role`), so `R` after `R` is `R` (Lemma 3). Masking is recorded in the returned mask,
//! never silent; the caller resets the group's provenance to `Default`.

use gorca_model::sem::{Avail, Check, Expand, GROUP_COUNT, Group, GroupMask, SemEntry, SemRole};

use crate::profile::Rules;
use crate::value::{Slot, Val};

/// The effective view of one node: one slot per registered group, indexed by group slot.
pub type Eff = [Slot; GROUP_COUNT];

/// A fresh effective view with every group `Unknown`.
pub fn unknown_eff() -> Eff {
    std::array::from_fn(|_| Slot::Unknown)
}

/// Index of `g` in an [`Eff`].
#[inline]
pub fn ix(g: Group) -> usize {
    usize::from(g.slot())
}

fn is_check_na(s: &Slot) -> bool {
    matches!(s, Slot::Val(Val::Check(Check::NotCheckable)))
}

fn is_avail_na(s: &Slot) -> bool {
    matches!(s, Slot::Val(Val::Avail(Avail::NotApplicable)))
}

fn is_expand_na(s: &Slot) -> bool {
    matches!(s, Slot::Val(Val::Expand(Expand::NotExpandable)))
}

/// Repair `eff` in place and return the groups that were masked. Precondition: no group is
/// `Unknown` (defaults are filled first). `Absent` is **not** the not-applicable value: an
/// `Absent` state on a role that cannot carry it is masked, as in the oracle.
pub fn repair(rules: &Rules, default_role: SemRole, eff: &mut Eff) -> GroupMask {
    let role = eff
        .get(ix(Group::Role))
        .and_then(Slot::role)
        .unwrap_or(default_role);
    let mut masked = GroupMask::EMPTY;

    let mut mask = |eff: &mut Eff, g: Group, to: Slot| {
        if let Some(s) = eff.get_mut(ix(g)) {
            *s = to;
            masked = masked.with(g);
        }
    };

    let check = eff.get(ix(Group::Check)).cloned().unwrap_or_default();
    if !rules.checkable.has(role) && !is_check_na(&check) {
        mask(
            eff,
            Group::Check,
            Slot::Val(Val::Check(Check::NotCheckable)),
        );
    } else if rules.checkable.has(role) && is_check_na(&check) {
        mask(eff, Group::Check, Slot::Val(Val::Check(Check::Off)));
    }

    let avail = eff.get(ix(Group::Avail)).cloned().unwrap_or_default();
    if !rules.disableable.has(role) && !is_avail_na(&avail) {
        mask(
            eff,
            Group::Avail,
            Slot::Val(Val::Avail(Avail::NotApplicable)),
        );
    } else if rules.disableable.has(role) && is_avail_na(&avail) {
        mask(eff, Group::Avail, Slot::Val(Val::Avail(Avail::Enabled)));
    }

    let expand = eff.get(ix(Group::Expand)).cloned().unwrap_or_default();
    if !rules.expandable.has(role) && !is_expand_na(&expand) {
        mask(
            eff,
            Group::Expand,
            Slot::Val(Val::Expand(Expand::NotExpandable)),
        );
    }

    let level = eff.get(ix(Group::Level)).cloned().unwrap_or_default();
    if !rules.level_roles.has(role) && !level.is_empty_like() {
        mask(eff, Group::Level, Slot::Absent);
    }
    masked
}

/// The groups of `entry` that violate an applicability invariant (F10): a state on a role that
/// cannot carry it. Empty for every entry `FUSE` publishes.
///
/// Only the "not applicable therefore default" direction is checked. The converse (an applicable
/// role never has the not-applicable value) is the oracle's `R` rule for an explicit
/// `NotCheckable`; an `Absent` state on an applicable role finalises to the default and is
/// outside this predicate (see the B1a report).
pub fn invariant_violations(rules: &Rules, entry: &SemEntry) -> GroupMask {
    let role = entry.sem.role;
    let mut bad = GroupMask::EMPTY;
    if !rules.checkable.has(role) && entry.sem.check != Check::NotCheckable {
        bad = bad.with(Group::Check);
    }
    if !rules.disableable.has(role) && entry.sem.avail != Avail::NotApplicable {
        bad = bad.with(Group::Avail);
    }
    if !rules.expandable.has(role) && entry.sem.expand != Expand::NotExpandable {
        bad = bad.with(Group::Expand);
    }
    if !rules.level_roles.has(role) && entry.sem.level != 0 {
        bad = bad.with(Group::Level);
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eff_with(role: SemRole, check: Slot, avail: Slot, expand: Slot, level: Slot) -> Eff {
        let mut e = unknown_eff();
        e[ix(Group::Role)] = Slot::Val(Val::Role(role));
        e[ix(Group::Check)] = check;
        e[ix(Group::Avail)] = avail;
        e[ix(Group::Expand)] = expand;
        e[ix(Group::Level)] = level;
        e
    }

    fn na() -> (Slot, Slot, Slot, Slot) {
        (
            Slot::Val(Val::Check(Check::NotCheckable)),
            Slot::Val(Val::Avail(Avail::NotApplicable)),
            Slot::Val(Val::Expand(Expand::NotExpandable)),
            Slot::Absent,
        )
    }

    #[test]
    fn non_applicable_states_are_masked_and_reported() {
        let rules = Rules::aria();
        let mut e = eff_with(
            SemRole::Paragraph,
            Slot::Val(Val::Check(Check::On)),
            Slot::Val(Val::Avail(Avail::Disabled)),
            Slot::Val(Val::Expand(Expand::Expanded)),
            Slot::Val(Val::Level(2)),
        );
        let m = repair(&rules, SemRole::Generic, &mut e);
        for g in [Group::Check, Group::Avail, Group::Expand, Group::Level] {
            assert!(m.has(g), "{g:?}");
        }
        let (c, a, x, l) = na();
        assert_eq!(e[ix(Group::Check)], c);
        assert_eq!(e[ix(Group::Avail)], a);
        assert_eq!(e[ix(Group::Expand)], x);
        assert_eq!(e[ix(Group::Level)], l);
    }

    #[test]
    fn applicable_roles_get_their_state_defaults() {
        let rules = Rules::aria();
        let (c, a, x, l) = na();
        let mut e = eff_with(SemRole::Checkbox, c, a, x, l);
        let m = repair(&rules, SemRole::Generic, &mut e);
        assert!(m.has(Group::Check) && m.has(Group::Avail));
        assert_eq!(e[ix(Group::Check)], Slot::Val(Val::Check(Check::Off)));
        assert_eq!(e[ix(Group::Avail)], Slot::Val(Val::Avail(Avail::Enabled)));
    }

    #[test]
    fn absent_is_not_the_not_applicable_value() {
        let rules = Rules::aria();
        let (_, a, x, l) = na();
        let mut e = eff_with(SemRole::Paragraph, Slot::Absent, a, x, l);
        let m = repair(&rules, SemRole::Generic, &mut e);
        assert!(
            m.has(Group::Check),
            "Absent on a non-checkable role is masked"
        );
        assert_eq!(
            e[ix(Group::Check)],
            Slot::Val(Val::Check(Check::NotCheckable))
        );
    }

    #[test]
    fn repair_is_idempotent() {
        let rules = Rules::aria();
        let roles = [
            SemRole::Generic,
            SemRole::Button,
            SemRole::Checkbox,
            SemRole::Heading,
        ];
        let checks = [Slot::Absent, Slot::Val(Val::Check(Check::On)), na().0];
        let levels = [Slot::Absent, Slot::Val(Val::Level(3))];
        for r in roles {
            for c in &checks {
                for l in &levels {
                    let (_, a, x, _) = na();
                    let mut e = eff_with(r, c.clone(), a, x, l.clone());
                    repair(&rules, SemRole::Generic, &mut e);
                    let once = e.clone();
                    let m2 = repair(&rules, SemRole::Generic, &mut e);
                    assert!(m2.is_empty(), "{r:?}");
                    assert_eq!(e, once);
                }
            }
        }
    }
}
