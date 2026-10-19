// SPDX-License-Identifier: GPL-3.0-or-later
//! Inline primary registers (ADR-0024 section 2.12, math section 1.2): the only mutable state of
//! `FUSE`. Every register is a join-semilattice under `max` over the total order `(seq, tie)`,
//! so the state after any multiset of writes, in any order, with any duplication, is a function
//! of the set of writes only (Lemma 1).
//!
//! Registers are sparse (sorted by group slot): a node pays only for the groups a source has
//! actually reported. Secondary, Inferred and evidence registers live in the side table (B1b).

use std::cmp::Ordering;

use gorca_model::sem::Group;

use crate::value::{Seq, Slot, Write};

/// Primary sources a node can have registers for (AT-SPI and Newton today).
pub const MAX_PRIMARIES: usize = 2;

/// Index of a primary source in the node's inline register file. The mapping from a source's
/// identity to the slot is the writer's (it comes from the profile, not from `SourceId`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct PrimarySlot(u8);

impl PrimarySlot {
    pub const P0: PrimarySlot = PrimarySlot(0);
    pub const P1: PrimarySlot = PrimarySlot(1);

    /// `None` when `i >= MAX_PRIMARIES`.
    pub const fn new(i: usize) -> Option<PrimarySlot> {
        if i < MAX_PRIMARIES {
            // `i < 2`, so the truncation cannot lose bits.
            Some(PrimarySlot(i.to_le_bytes()[0]))
        } else {
            None
        }
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// One observed register: the winner of all writes seen for `(source, group)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reg {
    pub seq: Seq,
    pub write: Write,
}

impl Reg {
    fn cmp_key(&self, other: &Reg) -> Ordering {
        self.seq
            .cmp(&other.seq)
            .then_with(|| self.write.tie_cmp(&other.write))
    }
}

/// The registers of one primary source for one node.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SrcRegs {
    /// Lifecycle: `(seq, alive)`; at an equal seq `alive` wins (a total order, deterministic).
    life: Option<(Seq, bool)>,
    /// Invalidation: the newest seq; a value older than this reads as stale.
    inval: Option<Seq>,
    /// Observed values, sorted by group slot, at most one per group.
    regs: Vec<Reg>,
}

impl SrcRegs {
    pub fn life(&self) -> Option<(Seq, bool)> {
        self.life
    }

    pub fn inval(&self) -> Option<Seq> {
        self.inval
    }

    pub fn regs(&self) -> &[Reg] {
        &self.regs
    }

    /// The register of `g`, if the source reported it.
    pub fn get(&self, g: Group) -> Option<&Reg> {
        self.regs
            .binary_search_by_key(&g.slot(), |r| r.write.group().slot())
            .ok()
            .and_then(|i| self.regs.get(i))
    }

    /// Join an observed value. Returns true iff the register changed.
    pub fn apply_obs(&mut self, seq: Seq, write: Write) -> bool {
        let slot = write.group().slot();
        let new = Reg { seq, write };
        match self
            .regs
            .binary_search_by_key(&slot, |r| r.write.group().slot())
        {
            Ok(i) => match self.regs.get_mut(i) {
                Some(cur) if new.cmp_key(cur) == Ordering::Greater => {
                    *cur = new;
                    true
                }
                _ => false,
            },
            Err(i) => {
                self.regs.insert(i, new);
                true
            }
        }
    }

    /// Join a lifecycle fact. Returns true iff the register changed.
    pub fn apply_life(&mut self, seq: Seq, alive: bool) -> bool {
        match self.life {
            Some(cur) if (seq, alive) <= cur => false,
            _ => {
                self.life = Some((seq, alive));
                true
            }
        }
    }

    /// Join an invalidation marker. Returns true iff the register changed.
    pub fn apply_inval(&mut self, seq: Seq) -> bool {
        match self.inval {
            Some(cur) if seq <= cur => false,
            _ => {
                self.inval = Some(seq);
                true
            }
        }
    }

    /// True iff `g`'s value is older than the newest invalidation (stale, ADR-0022 LR rules).
    pub fn is_stale(&self, g: Group) -> bool {
        match (self.get(g), self.inval) {
            (Some(r), Some(i)) => i > r.seq,
            _ => false,
        }
    }
}

/// A user override: a value or `Clear` (which wins as a register, then reads as "no override").
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UserOp {
    Clear,
    Set(Write),
}

/// One user override register of `(node, group)`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserReg {
    pub group: Group,
    pub seq: Seq,
    pub op: UserOp,
}

impl UserReg {
    fn cmp_key(&self, other: &UserReg) -> Ordering {
        // "<clear>" is a string in the oracle's vkey, so it sorts with the other strings.
        let key = |op: &UserOp| match op {
            UserOp::Clear => (1, "<clear>".to_owned()),
            UserOp::Set(w) => w.tie_key(),
        };
        self.seq
            .cmp(&other.seq)
            .then_with(|| key(&self.op).cmp(&key(&other.op)))
    }
}

/// All inline registers of one node.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NodeRegs {
    src: [SrcRegs; MAX_PRIMARIES],
    /// User overrides, sorted by group slot.
    user: Vec<UserReg>,
}

impl NodeRegs {
    pub fn src(&self, s: PrimarySlot) -> &SrcRegs {
        &self.src[s.index()]
    }

    pub fn src_mut(&mut self, s: PrimarySlot) -> &mut SrcRegs {
        &mut self.src[s.index()]
    }

    pub fn user(&self) -> &[UserReg] {
        &self.user
    }

    /// The user override of `g`, unless it is a `Clear`.
    pub fn user_value(&self, g: Group) -> Option<(Seq, &Write)> {
        self.user
            .binary_search_by_key(&g.slot(), |r| r.group.slot())
            .ok()
            .and_then(|i| self.user.get(i))
            .and_then(|r| match &r.op {
                UserOp::Clear => None,
                UserOp::Set(w) => Some((r.seq, w)),
            })
    }

    /// Join a user override or clear. Returns true iff the register changed.
    pub fn apply_user(&mut self, group: Group, seq: Seq, op: UserOp) -> bool {
        let new = UserReg { group, seq, op };
        match self
            .user
            .binary_search_by_key(&group.slot(), |r| r.group.slot())
        {
            Ok(i) => match self.user.get_mut(i) {
                Some(cur) if new.cmp_key(cur) == Ordering::Greater => {
                    *cur = new;
                    true
                }
                _ => false,
            },
            Err(i) => {
                self.user.insert(i, new);
                true
            }
        }
    }
}

/// Convenience: the effective slot of a register file entry (`Unknown` when not reported).
pub fn slot_of(regs: &SrcRegs, g: Group) -> Slot {
    regs.get(g).map_or(Slot::Unknown, |r| r.write.slot())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_model::sem::Check;

    use super::*;
    use crate::value::Val;

    fn name(s: &str) -> Write {
        Write::Val(Val::Name(Arc::from(s)))
    }

    #[test]
    fn newest_seq_wins_regardless_of_order() {
        let mut a = SrcRegs::default();
        assert!(a.apply_obs(2, name("new")));
        assert!(!a.apply_obs(1, name("old")));
        let mut b = SrcRegs::default();
        assert!(b.apply_obs(1, name("old")));
        assert!(b.apply_obs(2, name("new")));
        assert_eq!(a, b);
        assert!(!a.apply_obs(2, name("new")), "duplicate is a no-op");
    }

    #[test]
    fn seq_collision_is_broken_by_the_value_order() {
        let mut a = SrcRegs::default();
        a.apply_obs(7, name("Opens a dialog"));
        a.apply_obs(7, name("Required"));
        let mut b = SrcRegs::default();
        b.apply_obs(7, name("Required"));
        b.apply_obs(7, name("Opens a dialog"));
        assert_eq!(a, b);
        assert_eq!(
            a.get(Group::Name).map(|r| r.write.clone()),
            Some(name("Required"))
        );
    }

    #[test]
    fn regs_stay_sorted_and_unique_per_group() {
        let mut r = SrcRegs::default();
        r.apply_obs(1, Write::Val(Val::Check(Check::On)));
        r.apply_obs(1, name("x"));
        r.apply_obs(2, Write::Absent(Group::Role));
        let slots: Vec<u8> = r.regs().iter().map(|g| g.write.group().slot()).collect();
        assert_eq!(slots, vec![0, 1, 3]);
    }

    #[test]
    fn life_and_inval_are_monotone() {
        let mut r = SrcRegs::default();
        assert!(r.apply_life(3, false));
        assert!(!r.apply_life(2, true), "older life fact cannot resurrect");
        assert!(r.apply_life(4, true));
        assert!(r.apply_inval(5));
        assert!(!r.apply_inval(5));
        r.apply_obs(4, name("x"));
        assert!(r.is_stale(Group::Name));
        r.apply_obs(6, name("y"));
        assert!(!r.is_stale(Group::Name));
    }

    #[test]
    fn user_clear_wins_as_register_and_reads_as_no_override() {
        let mut n = NodeRegs::default();
        n.apply_user(Group::Name, 1, UserOp::Set(name("Save")));
        assert!(n.user_value(Group::Name).is_some());
        n.apply_user(Group::Name, 2, UserOp::Clear);
        assert!(n.user_value(Group::Name).is_none());
        assert!(!n.apply_user(Group::Name, 1, UserOp::Set(name("Save"))));
        assert!(n.user_value(Group::Name).is_none());
    }

    #[test]
    fn equal_life_fact_is_a_no_op_and_alive_wins_a_tie() {
        let mut r = SrcRegs::default();
        assert!(r.apply_life(16, true));
        assert!(!r.apply_life(16, true));
        assert!(!r.apply_life(16, false), "alive wins an equal seq");
        let mut q = SrcRegs::default();
        assert!(q.apply_life(16, false));
        assert!(q.apply_life(16, true));
        assert_eq!(q, r);
    }

    #[test]
    fn primary_slot_bounds() {
        assert_eq!(PrimarySlot::new(1), Some(PrimarySlot::P1));
        assert_eq!(PrimarySlot::new(MAX_PRIMARIES), None);
    }
}
