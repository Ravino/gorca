//! Incremental update operations (ADR-0003 section 9.6) and the identity hooks for SPA churn
//! (variant C, section 5): removal detaches a subtree instead of destroying it, so a later
//! re-match can bring the same `DocNodeId`s back (IL-R3, IL-R4).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::cats::{PropsPatch, SubtreeSpec};
use super::doc::{DocNodeId, NIL, WebDoc};
use super::error::DocError;

/// One desired child in a [`DocOp::SyncChildren`] list.
#[derive(Clone, Debug)]
pub enum ChildSpec {
    /// Keep (or move, or re-attach from the defunct set) this node, with its subtree.
    Existing(DocNodeId),
    /// Create a new subtree.
    New(Arc<SubtreeSpec>),
}

/// A mutation of a [`WebDoc`]. Applied identically to both publication copies (PUB-1).
#[derive(Clone, Debug)]
pub enum DocOp {
    /// Insert a new subtree under `parent` before `before` (`None`: last). `parent == None`
    /// creates the document root and is only valid on an empty document.
    InsertSubtree {
        parent: Option<DocNodeId>,
        before: Option<DocNodeId>,
        subtree: Arc<SubtreeSpec>,
    },
    /// Detach a subtree. Its nodes stay allocated and re-matchable until [`DocOp::Reap`].
    RemoveSubtree {
        id: DocNodeId,
    },
    /// Re-attach a defunct subtree root (identity-preserving re-match).
    AttachSubtree {
        id: DocNodeId,
        parent: DocNodeId,
        before: Option<DocNodeId>,
    },
    /// Move an attached subtree to a new place; ids are preserved.
    MoveSubtree {
        id: DocNodeId,
        parent: DocNodeId,
        before: Option<DocNodeId>,
    },
    /// Reorder children: `order` must be a permutation of the current children.
    ReorderChildren {
        parent: DocNodeId,
        order: Vec<DocNodeId>,
    },
    /// Make `parent`'s children equal `desired`: keep/move/re-attach existing nodes with a
    /// minimal number of moves (|C| - LIS), insert new subtrees, detach the rest.
    SyncChildren {
        parent: DocNodeId,
        desired: Vec<ChildSpec>,
    },
    SetText {
        id: DocNodeId,
        text: Option<Arc<str>>,
    },
    SetProps {
        id: DocNodeId,
        patch: PropsPatch,
    },
    /// Free a defunct subtree (`Some`) or every defunct subtree (`None`).
    Reap {
        id: Option<DocNodeId>,
    },
}

/// What an applied op produced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OpOutput {
    /// Ids of newly created nodes, in the preorder of the created subtree(s) (for
    /// `SyncChildren`: in desired-child order).
    pub created: Vec<DocNodeId>,
}

/// Longest strictly increasing subsequence; returns a keep-mask over `seq`.
fn lis_mask(seq: &[usize]) -> Vec<bool> {
    let n = seq.len();
    let mut tails: Vec<usize> = Vec::new(); // indices into seq
    let mut pred: Vec<usize> = vec![usize::MAX; n];
    for i in 0..n {
        let pos = tails.partition_point(|&t| seq[t] < seq[i]);
        if pos > 0 {
            pred[i] = tails[pos - 1];
        }
        if pos == tails.len() {
            tails.push(i);
        } else {
            tails[pos] = i;
        }
    }
    let mut keep = vec![false; n];
    let mut cur = tails.last().copied();
    while let Some(i) = cur {
        keep[i] = true;
        cur = (pred[i] != usize::MAX).then_some(pred[i]);
    }
    keep
}

impl WebDoc {
    /// Apply one operation. A rejected op leaves the document unchanged.
    pub fn apply(&mut self, op: &DocOp) -> Result<OpOutput, DocError> {
        let out = match op {
            DocOp::InsertSubtree {
                parent,
                before,
                subtree,
            } => self.op_insert(*parent, *before, subtree)?,
            DocOp::RemoveSubtree { id } => {
                self.op_remove(*id)?;
                OpOutput::default()
            }
            DocOp::AttachSubtree { id, parent, before } => {
                self.op_attach(*id, *parent, *before)?;
                OpOutput::default()
            }
            DocOp::MoveSubtree { id, parent, before } => {
                self.op_move(*id, *parent, *before)?;
                OpOutput::default()
            }
            DocOp::ReorderChildren { parent, order } => {
                self.op_reorder(*parent, order)?;
                OpOutput::default()
            }
            DocOp::SyncChildren { parent, desired } => self.op_sync(*parent, desired)?,
            DocOp::SetText { id, text } => {
                let s = self.resolve(*id)?;
                self.set_text_slot(s, text.clone());
                OpOutput::default()
            }
            DocOp::SetProps { id, patch } => {
                let s = self.resolve(*id)?;
                self.patch_slot(s, patch);
                OpOutput::default()
            }
            DocOp::Reap { id } => {
                match id {
                    Some(id) => {
                        let s = self.resolve(*id)?;
                        if !self.is_detached_root(s) {
                            return Err(DocError::NotDetached(*id));
                        }
                        self.reap_slot(s);
                    }
                    None => {
                        for s in self.detached_slots() {
                            self.reap_slot(s);
                        }
                    }
                }
                OpOutput::default()
            }
        };
        self.touch();
        Ok(out)
    }

    fn before_slot(&self, p: u32, before: Option<DocNodeId>) -> Result<u32, DocError> {
        match before {
            None => Ok(NIL),
            Some(b) => {
                let bs = self.resolve(b)?;
                if self.parent_slot(bs) == p {
                    Ok(bs)
                } else {
                    Err(DocError::NotChild(b))
                }
            }
        }
    }

    fn op_insert(
        &mut self,
        parent: Option<DocNodeId>,
        before: Option<DocNodeId>,
        subtree: &SubtreeSpec,
    ) -> Result<OpOutput, DocError> {
        let (p, b) = match parent {
            None => {
                if self.tree_root_exists() {
                    return Err(DocError::RootExists);
                }
                (NIL, NIL)
            }
            Some(pid) => {
                if !self.tree_root_exists() {
                    return Err(DocError::NoRoot);
                }
                let p = self.resolve_attached(pid)?;
                (p, self.before_slot(p, before)?)
            }
        };
        let nodes = self.insert_spec(p, b, subtree)?;
        Ok(OpOutput {
            created: nodes.iter().map(|s| self.slot_id(*s)).collect(),
        })
    }

    fn op_remove(&mut self, id: DocNodeId) -> Result<(), DocError> {
        let s = self.resolve_attached(id)?;
        if s == self.root_slot() {
            return Err(DocError::RootImmutable);
        }
        self.detach_core(s);
        self.mark_detached(s);
        Ok(())
    }

    fn op_attach(
        &mut self,
        id: DocNodeId,
        parent: DocNodeId,
        before: Option<DocNodeId>,
    ) -> Result<(), DocError> {
        let s = self.resolve(id)?;
        if !self.is_detached_root(s) {
            return Err(DocError::NotDetached(id));
        }
        let p = self.resolve_attached(parent)?;
        let b = self.before_slot(p, before)?;
        self.unmark_detached(s);
        self.attach_core(s, p, b);
        Ok(())
    }

    fn op_move(
        &mut self,
        id: DocNodeId,
        parent: DocNodeId,
        before: Option<DocNodeId>,
    ) -> Result<(), DocError> {
        let s = self.resolve_attached(id)?;
        if s == self.root_slot() {
            return Err(DocError::RootImmutable);
        }
        let p = self.resolve_attached(parent)?;
        if self.is_ancestor_or_self(s, p) {
            return Err(DocError::Cycle(id));
        }
        let b = self.before_slot(p, before)?;
        if b == s {
            return Ok(());
        }
        self.detach_core(s);
        self.attach_core(s, p, b);
        Ok(())
    }

    fn op_reorder(&mut self, parent: DocNodeId, order: &[DocNodeId]) -> Result<(), DocError> {
        let p = self.resolve_attached(parent)?;
        let cur = self.child_slots(p);
        if cur.len() != order.len() {
            return Err(DocError::NotPermutation);
        }
        let curset: HashSet<u32> = cur.iter().copied().collect();
        let mut seen = HashSet::with_capacity(order.len());
        for id in order {
            let s = self.resolve(*id)?;
            if !curset.contains(&s) {
                return Err(DocError::NotPermutation);
            }
            if !seen.insert(s) {
                return Err(DocError::Duplicate(*id));
            }
        }
        let desired: Vec<ChildSpec> = order.iter().map(|id| ChildSpec::Existing(*id)).collect();
        self.op_sync(parent, &desired).map(|_| ())
    }

    fn op_sync(&mut self, parent: DocNodeId, desired: &[ChildSpec]) -> Result<OpOutput, DocError> {
        let p = self.resolve_attached(parent)?;
        let cur = self.child_slots(p);
        let cur_index: HashMap<u32, usize> = cur.iter().enumerate().map(|(i, s)| (*s, i)).collect();

        // Validate everything before mutating.
        let mut seen: HashSet<u32> = HashSet::with_capacity(desired.len());
        let mut slots: Vec<Option<u32>> = Vec::with_capacity(desired.len());
        let mut hoist: Vec<u32> = Vec::new();
        for d in desired {
            match d {
                ChildSpec::New(_) => slots.push(None),
                ChildSpec::Existing(id) => {
                    let s = self.resolve(*id)?;
                    if !seen.insert(s) {
                        return Err(DocError::Duplicate(*id));
                    }
                    if self.parent_slot(s) == p {
                        // current child
                    } else if self.is_detached_root(s) {
                        // re-attach
                    } else if self.resolve_attached(*id).is_ok() {
                        if s == self.root_slot() {
                            return Err(DocError::RootImmutable);
                        }
                        if self.is_ancestor_or_self(s, p) {
                            return Err(DocError::Cycle(*id));
                        }
                        hoist.push(s);
                    } else {
                        return Err(DocError::NotAttached(*id));
                    }
                    slots.push(Some(s));
                }
            }
        }

        // Nodes coming from elsewhere in the tree are cut out first, deepest first, so that
        // removing an undesired wrapper below cannot strand them (hoisting children out of a
        // removed wrapper is the common SPA case).
        hoist.sort_by_key(|s| std::cmp::Reverse(self.depth_of(*s)));
        for &s in &hoist {
            self.detach_core(s);
        }
        let hoisted: HashSet<u32> = hoist.into_iter().collect();

        // Children that are not desired become defunct (re-matchable).
        for &c in &cur {
            if !seen.contains(&c) {
                self.detach_core(c);
                self.mark_detached(c);
            }
        }

        // Keep the longest already-ordered run of current children in place.
        let existing_cur: Vec<(usize, usize)> = slots
            .iter()
            .enumerate()
            .filter_map(|(di, s)| {
                let s = (*s)?;
                cur_index.get(&s).map(|ci| (di, *ci))
            })
            .collect();
        let seq: Vec<usize> = existing_cur.iter().map(|(_, ci)| *ci).collect();
        let keep = lis_mask(&seq);
        let mut stays = vec![false; desired.len()];
        for (k, (di, _)) in existing_cur.iter().enumerate() {
            stays[*di] = keep[k];
        }

        // Right to left: each item goes immediately before its successor.
        let mut created_rev: Vec<Vec<DocNodeId>> = Vec::new();
        let mut anchor = NIL;
        for di in (0..desired.len()).rev() {
            match &desired[di] {
                ChildSpec::New(spec) => {
                    let nodes = self.insert_spec(p, anchor, spec)?;
                    anchor = nodes[0];
                    created_rev.push(nodes.iter().map(|s| self.slot_id(*s)).collect());
                }
                ChildSpec::Existing(_) => {
                    let Some(s) = slots[di] else { continue };
                    if !stays[di] {
                        if self.is_detached_root(s) {
                            self.unmark_detached(s);
                            self.attach_core(s, p, anchor);
                        } else if hoisted.contains(&s) {
                            self.attach_core(s, p, anchor);
                        } else {
                            self.detach_core(s);
                            self.attach_core(s, p, anchor);
                        }
                    }
                    anchor = s;
                }
            }
        }
        let created = created_rev.into_iter().rev().flatten().collect();
        Ok(OpOutput { created })
    }
}

/// Align old children to new ones by signature (IL-3): returns, for each new key, the index of
/// the old child it keeps identity with. Exact LCS on the trimmed middle (bounded), then a
/// greedy first-fit by key for the leftovers, so duplicates and reorders still match.
pub fn align_by_key(old: &[u64], new: &[u64]) -> Vec<Option<usize>> {
    const LCS_CELLS: usize = 1 << 20;
    let mut out: Vec<Option<usize>> = vec![None; new.len()];
    let mut lo = 0;
    while lo < old.len() && lo < new.len() && old[lo] == new[lo] {
        out[lo] = Some(lo);
        lo += 1;
    }
    let (mut oh, mut nh) = (old.len(), new.len());
    while oh > lo && nh > lo && old[oh - 1] == new[nh - 1] {
        oh -= 1;
        nh -= 1;
        out[nh] = Some(oh);
    }
    let (om, nm) = (&old[lo..oh], &new[lo..nh]);
    let mut used = vec![false; old.len()];
    for o in out.iter().flatten() {
        used[*o] = true;
    }
    if !om.is_empty() && !nm.is_empty() && om.len() * nm.len() <= LCS_CELLS {
        let (w, h) = (nm.len() + 1, om.len() + 1);
        let mut dp = vec![0u32; w * h];
        for i in (0..om.len()).rev() {
            for j in (0..nm.len()).rev() {
                dp[i * w + j] = if om[i] == nm[j] {
                    dp[(i + 1) * w + j + 1] + 1
                } else {
                    dp[(i + 1) * w + j].max(dp[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < om.len() && j < nm.len() {
            if om[i] == nm[j] {
                out[lo + j] = Some(lo + i);
                used[lo + i] = true;
                i += 1;
                j += 1;
            } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
                i += 1;
            } else {
                j += 1;
            }
        }
    }
    // Leftovers: first unused old child with the same key (keyed move / unkeyed shuffle).
    let mut by_key: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, k) in old.iter().enumerate() {
        if !used[i] {
            by_key.entry(*k).or_default().push(i);
        }
    }
    for v in by_key.values_mut() {
        v.reverse();
    }
    for (j, k) in new.iter().enumerate() {
        if out[j].is_none()
            && let Some(i) = by_key.get_mut(k).and_then(Vec::pop)
        {
            out[j] = Some(i);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lis_marks_a_longest_run() {
        let m = lis_mask(&[3, 1, 2, 5, 4, 6]);
        let kept: Vec<usize> = [3, 1, 2, 5, 4, 6]
            .iter()
            .zip(&m)
            .filter(|(_, k)| **k)
            .map(|(v, _)| *v)
            .collect();
        assert_eq!(kept.len(), 4);
        assert!(kept.windows(2).all(|w| w[0] < w[1]));
        assert!(lis_mask(&[]).is_empty());
    }

    #[test]
    fn align_exact_and_shuffled() {
        assert_eq!(
            align_by_key(&[1, 2, 3], &[1, 2, 3]),
            vec![Some(0), Some(1), Some(2)]
        );
        let a = align_by_key(&[1, 2, 3, 4], &[4, 3, 2, 1]);
        assert!(a.iter().all(Option::is_some));
        let mut idx: Vec<usize> = a.into_iter().flatten().collect();
        idx.sort_unstable();
        assert_eq!(idx, vec![0, 1, 2, 3]);
        let b = align_by_key(&[1, 2, 3], &[1, 9, 3]);
        assert_eq!(b, vec![Some(0), None, Some(2)]);
    }

    #[test]
    fn align_duplicates_are_used_once() {
        let a = align_by_key(&[7, 7], &[7, 7, 7]);
        assert_eq!(a.iter().flatten().count(), 2);
    }
}
