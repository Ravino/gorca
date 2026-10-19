// SPDX-License-Identifier: GPL-3.0-or-later
//! Inline primary registers (ADR-0024 section 2.12, math section 1.2): the only mutable state of
//! `FUSE`. Every register is a join-semilattice under `max` over the total order `(seq, tie)`,
//! so the state after any multiset of writes, in any order, with any duplication, is a function
//! of the set of writes only (Lemma 1).
//!
//! Registers are sparse (sorted by group slot): a node pays only for the groups a source has
//! actually reported. Secondary, Inferred and evidence registers live in the side table (B1b).

use std::cmp::Ordering;

use gorca_model::sem::{Group, GroupMask};

use crate::value::{MAX_TEXT_BYTES, Seq, Slot, Write};

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

/// A register as read: a value register, or the `Absent` of a group (stored in a run).
#[derive(Copy, Clone, Debug)]
pub enum RegRef<'a> {
    Val(&'a Reg),
    Absent(Seq, Group),
}

impl RegRef<'_> {
    pub fn seq(&self) -> Seq {
        match self {
            RegRef::Val(r) => r.seq,
            RegRef::Absent(s, _) => *s,
        }
    }

    pub fn slot(&self) -> Slot {
        match self {
            RegRef::Val(r) => r.write.slot(),
            RegRef::Absent(..) => Slot::Absent,
        }
    }

    pub fn is_absent(&self) -> bool {
        matches!(self, RegRef::Absent(..))
    }
}

/// `(seq, write)` order shared by every register join: newest seq first, then the oracle's value
/// order (`Absent` is the string "-").
fn cmp_join(seq_a: Seq, a: &Write, seq_b: Seq, b: &Write) -> Ordering {
    seq_a.cmp(&seq_b).then_with(|| a.tie_cmp(b))
}

/// The registers of one primary source for one node.
///
/// A full state read writes most groups as `Absent` with one seq, so `Absent` registers are kept
/// as **runs** `(seq, groups)` instead of one register each; the join is still per group, and the
/// stored form is canonical (value registers sorted by group, runs sorted by seq with distinct
/// seqs, every group in at most one of them), so equal winners mean equal stores.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SrcRegs {
    /// Lifecycle: `(seq, alive)`; at an equal seq `alive` wins (a total order, deterministic).
    life: Option<(Seq, bool)>,
    /// Invalidation: the newest seq; a value older than this reads as stale.
    inval: Option<Seq>,
    /// Observed **values**, sorted by group slot, at most one per group; never `Absent`.
    vals: Vec<Reg>,
    /// Observed `Absent` runs, sorted by seq.
    absent: Vec<(Seq, GroupMask)>,
}

impl SrcRegs {
    pub fn life(&self) -> Option<(Seq, bool)> {
        self.life
    }

    pub fn inval(&self) -> Option<Seq> {
        self.inval
    }

    /// The value registers (sorted by group); `Absent` groups are in [`SrcRegs::absent_runs`].
    pub fn regs(&self) -> &[Reg] {
        &self.vals
    }

    /// The `Absent` runs, sorted by seq.
    pub fn absent_runs(&self) -> &[(Seq, GroupMask)] {
        &self.absent
    }

    /// The register of `g`, if the source reported it.
    pub fn get(&self, g: Group) -> Option<RegRef<'_>> {
        if let Ok(i) = self
            .vals
            .binary_search_by_key(&g.slot(), |r| r.write.group().slot())
        {
            return self.vals.get(i).map(RegRef::Val);
        }
        self.absent
            .iter()
            .find(|(_, m)| m.has(g))
            .map(|(s, _)| RegRef::Absent(*s, g))
    }

    /// Visit every reported group with its slot (value or `Absent`).
    pub fn for_each_slot(&self, mut f: impl FnMut(Group, Slot)) {
        for r in &self.vals {
            f(r.write.group(), r.write.slot());
        }
        for (_, m) in &self.absent {
            for g in Group::ALL.iter().copied().filter(|g| m.has(*g)) {
                f(g, Slot::Absent);
            }
        }
    }

    /// Release unused capacity (a node's registers are written once from a state read and rarely
    /// grow afterwards).
    pub fn shrink_to_fit(&mut self) {
        self.vals.shrink_to_fit();
        self.absent.shrink_to_fit();
    }

    /// Join an observed value. Returns true iff the register changed.
    pub fn apply_obs(&mut self, seq: Seq, write: Write) -> bool {
        self.apply_obs_capped(seq, write, MAX_TEXT_BYTES)
    }

    fn add_absent(&mut self, seq: Seq, g: Group) {
        match self.absent.binary_search_by_key(&seq, |(s, _)| *s) {
            Ok(k) => {
                if let Some(run) = self.absent.get_mut(k) {
                    run.1 = run.1.with(g);
                }
            }
            Err(k) => self.absent.insert(k, (seq, GroupMask::EMPTY.with(g))),
        }
    }

    fn remove_absent(&mut self, k: usize, g: Group) {
        if let Some(run) = self.absent.get_mut(k) {
            run.1 = run.1.without(g);
            if run.1.is_empty() {
                self.absent.remove(k);
            }
        }
    }

    /// As [`SrcRegs::apply_obs`] with the text cap of the profile's `Limits`. The cap is applied
    /// before the join, so the result stays a function of the set of writes.
    pub fn apply_obs_capped(&mut self, seq: Seq, write: Write, max_text: usize) -> bool {
        let write = write.capped(max_text).0;
        let g = write.group();
        let pos = self
            .vals
            .binary_search_by_key(&g.slot(), |r| r.write.group().slot());
        let run = self.absent.iter().position(|(_, m)| m.has(g));
        match (pos, run) {
            (Ok(i), _) => {
                let Some(cur) = self.vals.get(i) else {
                    return false;
                };
                if cmp_join(seq, &write, cur.seq, &cur.write) != Ordering::Greater {
                    return false;
                }
                if matches!(write, Write::Absent(_)) {
                    self.vals.remove(i);
                    self.add_absent(seq, g);
                } else if let Some(slot) = self.vals.get_mut(i) {
                    *slot = Reg { seq, write };
                }
                true
            }
            (Err(i), Some(k)) => {
                let cur_seq = self.absent.get(k).map_or(0, |(s, _)| *s);
                if cmp_join(seq, &write, cur_seq, &Write::Absent(g)) != Ordering::Greater {
                    return false;
                }
                self.remove_absent(k, g);
                if matches!(write, Write::Absent(_)) {
                    self.add_absent(seq, g);
                } else {
                    self.vals.insert(i, Reg { seq, write });
                }
                true
            }
            (Err(i), None) => {
                if matches!(write, Write::Absent(_)) {
                    self.add_absent(seq, g);
                } else {
                    self.vals.insert(i, Reg { seq, write });
                }
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
            (Some(r), Some(i)) => i > r.seq(),
            _ => false,
        }
    }

    /// Number of reported groups (values plus `Absent`).
    pub fn len(&self) -> usize {
        self.vals.len()
            + self
                .absent
                .iter()
                .map(|(_, m)| m.0.count_ones() as usize)
                .sum::<usize>()
    }

    pub fn is_empty(&self) -> bool {
        self.vals.is_empty()
            && self.absent.is_empty()
            && self.life.is_none()
            && self.inval.is_none()
    }
}

/// An empty register file (what a source that never wrote reads as).
static EMPTY_SRC: SrcRegs = SrcRegs {
    life: None,
    inval: None,
    vals: Vec::new(),
    absent: Vec::new(),
};

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

/// All inline registers of one node. The second primary's registers exist only when that source
/// wrote something (most nodes have one source), so an unused source costs one pointer.
#[derive(Clone, Debug, Default)]
pub struct NodeRegs {
    p0: SrcRegs,
    p1: Option<Box<SrcRegs>>,
    /// User overrides, sorted by group slot (almost no node has any).
    user: Option<Box<Vec<UserReg>>>,
}

impl PartialEq for NodeRegs {
    fn eq(&self, other: &NodeRegs) -> bool {
        // A boxed-but-empty second source equals an absent one.
        self.p0 == other.p0
            && self.user() == other.user()
            && self.p1.as_deref().unwrap_or(&EMPTY_SRC) == other.p1.as_deref().unwrap_or(&EMPTY_SRC)
    }
}

impl Eq for NodeRegs {}

impl NodeRegs {
    pub fn src(&self, s: PrimarySlot) -> &SrcRegs {
        match s.index() {
            0 => &self.p0,
            _ => self.p1.as_deref().unwrap_or(&EMPTY_SRC),
        }
    }

    pub fn src_mut(&mut self, s: PrimarySlot) -> &mut SrcRegs {
        match s.index() {
            0 => &mut self.p0,
            _ => self.p1.get_or_insert_with(Box::default),
        }
    }

    /// Release unused capacity of every register list.
    pub fn shrink_to_fit(&mut self) {
        self.p0.shrink_to_fit();
        if let Some(p1) = self.p1.as_mut() {
            p1.shrink_to_fit();
        }
        if let Some(u) = self.user.as_mut() {
            u.shrink_to_fit();
        }
    }

    pub fn user(&self) -> &[UserReg] {
        self.user.as_deref().map_or(&[], Vec::as_slice)
    }

    /// The user override of `g`, unless it is a `Clear`.
    pub fn user_value(&self, g: Group) -> Option<(Seq, &Write)> {
        let user = self.user();
        user.binary_search_by_key(&g.slot(), |r| r.group.slot())
            .ok()
            .and_then(|i| user.get(i))
            .and_then(|r| match &r.op {
                UserOp::Clear => None,
                UserOp::Set(w) => Some((r.seq, w)),
            })
    }

    /// Join a user override or clear. Returns true iff the register changed.
    pub fn apply_user(&mut self, group: Group, seq: Seq, op: UserOp) -> bool {
        let op = match op {
            UserOp::Set(w) => UserOp::Set(w.capped(MAX_TEXT_BYTES).0),
            UserOp::Clear => UserOp::Clear,
        };
        let new = UserReg { group, seq, op };
        let user = self.user.get_or_insert_with(Box::default);
        match user.binary_search_by_key(&group.slot(), |r| r.group.slot()) {
            Ok(i) => match user.get_mut(i) {
                Some(cur) if new.cmp_key(cur) == Ordering::Greater => {
                    *cur = new;
                    true
                }
                _ => false,
            },
            Err(i) => {
                user.insert(i, new);
                true
            }
        }
    }
}

/// Convenience: the effective slot of a register file entry (`Unknown` when not reported).
pub fn slot_of(regs: &SrcRegs, g: Group) -> Slot {
    regs.get(g).map_or(Slot::Unknown, |r| r.slot())
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
            a.get(Group::Name).map(|r| r.slot()),
            Some(name("Required").slot())
        );
    }

    #[test]
    fn values_stay_sorted_and_absent_groups_share_a_run() {
        let mut r = SrcRegs::default();
        r.apply_obs(1, Write::Val(Val::Check(Check::On)));
        r.apply_obs(1, name("x"));
        r.apply_obs(2, Write::Absent(Group::Role));
        r.apply_obs(2, Write::Absent(Group::Level));
        r.apply_obs(2, Write::Absent(Group::Live));
        let slots: Vec<u8> = r.regs().iter().map(|g| g.write.group().slot()).collect();
        assert_eq!(slots, vec![1, 3], "only the values are registers");
        assert_eq!(r.absent_runs().len(), 1, "three Absent groups, one run");
        assert_eq!(r.len(), 5);
        assert!(
            r.get(Group::Role)
                .is_some_and(|x| x.is_absent() && x.seq() == 2)
        );
        // A newer value takes a group out of its run; an older one cannot.
        assert!(r.apply_obs(3, name("late")));
        assert!(r.apply_obs(3, Write::Val(Val::Level(2))));
        assert!(!r.apply_obs(1, Write::Absent(Group::Name)));
        assert_eq!(
            r.absent_runs()
                .iter()
                .map(|(_, m)| m.0.count_ones())
                .sum::<u32>(),
            2
        );
        assert_eq!(
            r.get(Group::Level).map(|x| x.slot()),
            Some(Write::Val(Val::Level(2)).slot())
        );
    }

    /// The reference join: one `(seq, write)` per group, `max` under `(seq, tie)`.
    fn reference(writes: &[(Seq, Write)]) -> std::collections::BTreeMap<u8, (Seq, Write)> {
        let mut m: std::collections::BTreeMap<u8, (Seq, Write)> = std::collections::BTreeMap::new();
        for (seq, w) in writes {
            let k = w.group().slot();
            match m.get(&k) {
                Some((cs, cw)) if cmp_join(*seq, w, *cs, cw) != Ordering::Greater => {}
                _ => {
                    m.insert(k, (*seq, w.clone()));
                }
            }
        }
        m
    }

    #[test]
    fn the_run_store_equals_the_per_group_join_under_every_order_including_equal_seq_ties() {
        // Equal seqs on purpose: `Absent` ("-") against values and against other values at the
        // same seq exercise the tie order of the oracle's `vkey`.
        let groups = [
            Group::Role,
            Group::Name,
            Group::Level,
            Group::Live,
            Group::Check,
            Group::Avail,
        ];
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: usize| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            usize::try_from(x % n as u64).unwrap_or(0)
        };
        for _ in 0..400 {
            let mut writes: Vec<(Seq, Write)> = Vec::new();
            for _ in 0..(5 + next(30)) {
                let g = groups[next(groups.len())];
                let seq = 1 + next(4) as u64;
                let w = match (g, next(5)) {
                    (_, 0 | 1) => Write::Absent(g),
                    (Group::Level, _) => Write::Val(Val::Level(1 + next(3) as u8)),
                    (Group::Check, _) => Write::Val(Val::Check(Check::On)),
                    (Group::Avail, _) => Write::Val(Val::Avail(gorca_model::sem::Avail::Enabled)),
                    (Group::Live, _) => Write::Val(Val::Live(None)),
                    (Group::Role, _) => Write::Val(Val::Role(gorca_model::sem::SemRole::Button)),
                    _ => name(["", "Close", "Save"][next(3)]),
                };
                writes.push((seq, w));
            }
            let want = reference(&writes);
            let mut forward = SrcRegs::default();
            for (s, w) in &writes {
                forward.apply_obs(*s, w.clone());
            }
            let mut reversed = SrcRegs::default();
            for (s, w) in writes.iter().rev() {
                reversed.apply_obs(*s, w.clone());
            }
            let mut shuffled = SrcRegs::default();
            let mut order: Vec<usize> = (0..writes.len()).collect();
            for i in (1..order.len()).rev() {
                order.swap(i, next(i + 1));
            }
            for i in order.iter().chain(order.iter().take(3)) {
                shuffled.apply_obs(writes[*i].0, writes[*i].1.clone());
            }
            assert_eq!(forward, reversed, "canonical form is order independent");
            assert_eq!(forward, shuffled, "duplicates and shuffling change nothing");
            for g in Group::ALL {
                let got = forward.get(*g);
                match (want.get(&g.slot()), got) {
                    (None, None) => {}
                    (Some((s, w)), Some(r)) => {
                        assert_eq!(r.seq(), *s, "{g:?}");
                        assert_eq!(r.slot(), w.slot(), "{g:?}");
                    }
                    (w, r) => panic!("{g:?}: want {w:?} got {:?}", r.map(|r| r.slot())),
                }
            }
        }
    }

    #[test]
    fn an_unused_second_source_costs_nothing_and_compares_equal_to_an_empty_one() {
        let mut a = NodeRegs::default();
        let b = NodeRegs::default();
        a.src_mut(PrimarySlot::P1); // boxed but empty
        assert_eq!(a, b);
        a.src_mut(PrimarySlot::P1).apply_life(1, true);
        assert_ne!(a, b);
        assert_eq!(a.src(PrimarySlot::P1).life(), Some((1, true)));
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
