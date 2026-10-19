// SPDX-License-Identifier: GPL-3.0-or-later
//! `explain` (FS-9): why does a group have the value it has? `FUSE` runs generic over a
//! [`Tracer`]; [`NoTrace`] compiles to nothing (the hot path), [`Trace`] records the full trail
//! of every group in the order the strata decided it: observed candidates, Secondaries, patches,
//! the user override, derived rules, Inferred facts and the repair mask. The trail is produced by
//! the same code that computes the value, so it cannot drift from it.

use std::sync::Arc;

use gorca_model::sem::{GROUP_COUNT, Group, GroupMask};

use crate::register::PrimarySlot;
use crate::side::{Entry, FactTrail, TrailStatus};
use crate::strata::Layer;
use crate::value::{Seq, Slot};

/// One observed candidate (a primary's value of a group after quirks).
#[derive(Clone, Debug)]
pub struct ObsCand {
    pub src: PrimarySlot,
    pub seq: Option<Seq>,
    pub value: Slot,
    /// Quirk rule id that rewrote the value (0 = none).
    pub rule: u16,
    pub stale: bool,
}

/// One patch edit that matched the node.
#[derive(Clone, Debug)]
pub struct PatchTrail {
    pub ext: Arc<str>,
    pub rule: Arc<str>,
    pub value: Slot,
    pub status: TrailStatus,
}

/// One line of a group's trail.
#[derive(Clone, Debug)]
pub enum TrailEntry {
    Observed {
        cand: ObsCand,
        status: TrailStatus,
    },
    /// A Secondary fact (S0), `hint.title_attr` evidence (S3) or an Inferred fact (S4).
    Fact(Box<FactTrail>),
    Patch(PatchTrail),
    User {
        seq: Seq,
        value: Slot,
    },
    /// Name from content: the children read and which of them were tainted.
    Content {
        value: Arc<str>,
        from: Vec<u32>,
        tainted_from: Vec<u32>,
    },
    /// Repair overwrote the merged value.
    Repair {
        value: Slot,
        masked: Slot,
    },
}

/// A disagreement the static order had to break (a diagnostic, never a different result).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Conflict {
    /// Both primaries know a value and they differ; the winner is chosen by `PI`.
    ObservedDisagree {
        group: Group,
        a: (PrimarySlot, Slot),
        b: (PrimarySlot, Slot),
    },
    /// Two matching patches of equal specificity set different values.
    PatchTie { group: Group, rules: Vec<Arc<str>> },
}

/// Receives the decisions of `FUSE`. All methods default to no-ops.
pub trait Tracer {
    /// False for [`NoTrace`]: callers skip the work of building trace arguments.
    const ON: bool;
    fn observed(&mut self, _g: Group, _c: ObsCand) {}
    fn fact(&mut self, _g: Group, _e: &Entry, _status: TrailStatus) {}
    fn patch(&mut self, _g: Group, _p: PatchTrail) {}
    fn user(&mut self, _g: Group, _seq: Seq, _value: &Slot) {}
    fn content(&mut self, _value: &str, _from: &[u32], _tainted_from: &[u32]) {}
    fn conflict(&mut self, _c: Conflict) {}
    /// Called last: the winner layer of each group, the winning primary of each observed group,
    /// the repair mask with the pre- and post-repair values.
    fn finish(
        &mut self,
        _layer: &[Layer; GROUP_COUNT],
        _winner: &[Option<PrimarySlot>; GROUP_COUNT],
        _masked: GroupMask,
        _before: &[Slot; GROUP_COUNT],
        _after: &[Slot; GROUP_COUNT],
    ) {
    }
}

/// The hot-path tracer: records nothing.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoTrace;

impl Tracer for NoTrace {
    const ON: bool = false;
}

/// The trail of one node.
#[derive(Clone, Debug)]
pub struct Explain {
    pub groups: Vec<Vec<TrailEntry>>,
    pub conflicts: Vec<Conflict>,
}

impl Default for Explain {
    fn default() -> Explain {
        Explain {
            groups: vec![Vec::new(); GROUP_COUNT],
            conflicts: Vec::new(),
        }
    }
}

impl Explain {
    pub fn trail(&self, g: Group) -> &[TrailEntry] {
        self.groups
            .get(usize::from(g.slot()))
            .map_or(&[], Vec::as_slice)
    }

    fn push(&mut self, g: Group, e: TrailEntry) {
        if let Some(v) = self.groups.get_mut(usize::from(g.slot())) {
            v.push(e);
        }
    }
}

/// The recording tracer (trace mode).
#[derive(Debug, Default)]
pub struct Trace {
    pub explain: Explain,
}

impl Tracer for Trace {
    const ON: bool = true;

    fn observed(&mut self, g: Group, c: ObsCand) {
        self.explain.push(
            g,
            TrailEntry::Observed {
                cand: c,
                status: TrailStatus::Unknown,
            },
        );
    }

    fn fact(&mut self, g: Group, e: &Entry, status: TrailStatus) {
        let mut t = e.trail().clone();
        t.status = status;
        self.explain.push(g, TrailEntry::Fact(Box::new(t)));
    }

    fn patch(&mut self, g: Group, p: PatchTrail) {
        self.explain.push(g, TrailEntry::Patch(p));
    }

    fn user(&mut self, g: Group, seq: Seq, value: &Slot) {
        self.explain.push(
            g,
            TrailEntry::User {
                seq,
                value: value.clone(),
            },
        );
    }

    fn content(&mut self, value: &str, from: &[u32], tainted_from: &[u32]) {
        self.explain.push(
            Group::Name,
            TrailEntry::Content {
                value: Arc::from(value),
                from: from.to_vec(),
                tainted_from: tainted_from.to_vec(),
            },
        );
    }

    fn conflict(&mut self, c: Conflict) {
        self.explain.conflicts.push(c);
    }

    fn finish(
        &mut self,
        layer: &[Layer; GROUP_COUNT],
        winner: &[Option<PrimarySlot>; GROUP_COUNT],
        masked: GroupMask,
        before: &[Slot; GROUP_COUNT],
        after: &[Slot; GROUP_COUNT],
    ) {
        for g in Group::ALL.iter().copied() {
            let i = usize::from(g.slot());
            let (l, w) = (layer.get(i).copied(), winner.get(i).copied().flatten());
            if let Some(list) = self.explain.groups.get_mut(i) {
                for e in list.iter_mut() {
                    if let TrailEntry::Observed { cand, status } = e {
                        *status = if cand.value.is_unknown() {
                            TrailStatus::Unknown
                        } else if l == Some(Layer::Observed) && w == Some(cand.src) {
                            TrailStatus::Won
                        } else {
                            TrailStatus::Shadowed
                        };
                    }
                }
                if masked.has(g)
                    && let (Some(b), Some(a)) = (before.get(i), after.get(i))
                {
                    list.push(TrailEntry::Repair {
                        value: a.clone(),
                        masked: b.clone(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notrace_is_off_and_trace_is_on() {
        const { assert!(!NoTrace::ON) };
        const { assert!(Trace::ON) };
    }

    #[test]
    fn finish_marks_the_observed_winner() {
        let mut t = Trace::default();
        let c = |p: PrimarySlot, v: Slot| ObsCand {
            src: p,
            seq: Some(1),
            value: v,
            rule: 0,
            stale: false,
        };
        t.observed(Group::Name, c(PrimarySlot::P0, Slot::Unknown));
        t.observed(Group::Name, c(PrimarySlot::P1, Slot::Absent));
        let mut layer = [Layer::Default; GROUP_COUNT];
        layer[usize::from(Group::Name.slot())] = Layer::Observed;
        let mut winner = [None; GROUP_COUNT];
        winner[usize::from(Group::Name.slot())] = Some(PrimarySlot::P1);
        let s: [Slot; GROUP_COUNT] = std::array::from_fn(|_| Slot::Unknown);
        t.finish(&layer, &winner, GroupMask::EMPTY, &s, &s);
        let st: Vec<TrailStatus> = t
            .explain
            .trail(Group::Name)
            .iter()
            .filter_map(|e| match e {
                TrailEntry::Observed { status, .. } => Some(*status),
                _ => None,
            })
            .collect();
        assert_eq!(st, vec![TrailStatus::Unknown, TrailStatus::Won]);
    }
}
