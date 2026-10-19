// SPDX-License-Identifier: GPL-3.0-or-later
//! The dirty set of the incremental engine: a lazy-deletion binary heap ordered by the fusion
//! key, a stamp table for O(1) membership, and a FIFO of first-dirty ticks for aging.
//!
//! Marking and taking a node are a heap push and a hash probe on flat memory (no tree node
//! allocation, no second ordered structure): that is the per-event cost that matters under a
//! storm. Entries made stale by a re-key or a removal stay in the heap until they reach the top
//! (then they are skipped) or until the heap is rebuilt when stale entries outnumber live ones.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};

use crate::side::NodeIdx;

/// A fast, non-cryptographic hasher for small integer keys (`NodeIdx`). Never used for keys an
/// attacker controls (DOM ids and other page strings keep the default hasher).
#[derive(Default, Clone, Copy)]
pub(crate) struct FxHasher(u64);

impl Hasher for FxHasher {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.write_u64(u64::from(*b));
        }
    }

    fn write_u32(&mut self, i: u32) {
        self.write_u64(u64::from(i));
    }

    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517c_c1b7_2722_0a95);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

pub(crate) type FxMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

/// `(class, deepest first, node index)`: smaller fuses first.
pub(crate) type DirtyKey = (u8, Reverse<u32>, u32);

#[derive(Clone, Copy, Debug)]
struct Live {
    key: DirtyKey,
    /// Validity stamp of the heap entry for `key`.
    hseq: u64,
    /// Validity stamp of the aging entry; assigned when the node first became dirty.
    aseq: u64,
    since: u64,
}

#[derive(Debug, Default)]
pub(crate) struct DirtySet {
    heap: BinaryHeap<Reverse<(DirtyKey, u64)>>,
    live: FxMap<NodeIdx, Live>,
    ages: VecDeque<(u64, u32, u64)>,
    next: u64,
}

impl DirtySet {
    pub(crate) fn len(&self) -> usize {
        self.live.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live.is_empty()
    }

    pub(crate) fn contains(&self, n: NodeIdx) -> bool {
        self.live.contains_key(&n)
    }

    /// Mark `n` dirty under `key` at run `tick`. A node already dirty under the same key costs a
    /// single probe; a different key re-keys it (a new heap entry, the old one goes stale).
    pub(crate) fn mark(&mut self, n: NodeIdx, key: DirtyKey, tick: u64) {
        self.next += 1;
        let seq = self.next;
        match self.live.get_mut(&n) {
            Some(l) if l.key == key => {}
            Some(l) => {
                l.key = key;
                l.hseq = seq;
                self.heap.push(Reverse((key, seq)));
            }
            None => {
                self.live.insert(
                    n,
                    Live {
                        key,
                        hseq: seq,
                        aseq: seq,
                        since: tick,
                    },
                );
                self.heap.push(Reverse((key, seq)));
                self.ages.push_back((tick, n.0, seq));
            }
        }
        if self.heap.len() > 2 * self.live.len() + 1024 {
            self.rebuild();
        }
    }

    /// Forget `n` (its heap and aging entries go stale).
    pub(crate) fn remove(&mut self, n: NodeIdx) {
        self.live.remove(&n);
    }

    fn entry_valid(&self, key: DirtyKey, hseq: u64) -> bool {
        self.live
            .get(&NodeIdx(key.2))
            .is_some_and(|l| l.key == key && l.hseq == hseq)
    }

    /// The first key (without removing it); stale entries on top are dropped.
    pub(crate) fn peek(&mut self) -> Option<DirtyKey> {
        while let Some(&Reverse((key, hseq))) = self.heap.peek() {
            if self.entry_valid(key, hseq) {
                return Some(key);
            }
            self.heap.pop();
        }
        None
    }

    /// Remove the node of `key` (the key returned by `peek` or `oldest`).
    pub(crate) fn take(&mut self, key: DirtyKey) {
        self.live.remove(&NodeIdx(key.2));
    }

    /// The key of the longest-dirty node, if it has been dirty for at least `min_age` runs.
    pub(crate) fn oldest(&mut self, tick: u64, min_age: u64) -> Option<DirtyKey> {
        while let Some(&(since, idx, aseq)) = self.ages.front() {
            match self.live.get(&NodeIdx(idx)) {
                Some(l) if l.aseq == aseq => {
                    return (tick.saturating_sub(since) >= min_age).then_some(l.key);
                }
                _ => {
                    self.ages.pop_front();
                }
            }
        }
        None
    }

    fn rebuild(&mut self) {
        self.heap = self
            .live
            .values()
            .map(|l| Reverse((l.key, l.hseq)))
            .collect();
        let mut by_age: Vec<(u64, u32, u64)> = self
            .live
            .iter()
            .map(|(n, l)| (l.since, n.0, l.aseq))
            .collect();
        by_age.sort_unstable();
        self.ages = by_age.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(i: u32) -> NodeIdx {
        NodeIdx(i)
    }

    fn k(class: u8, depth: u32, i: u32) -> DirtyKey {
        (class, Reverse(depth), i)
    }

    #[test]
    fn orders_by_class_then_depth_then_index_and_rekeys() {
        let mut d = DirtySet::default();
        d.mark(n(5), k(2, 1, 5), 0);
        d.mark(n(6), k(2, 4, 6), 0);
        d.mark(n(7), k(0, 0, 7), 0);
        assert_eq!(d.len(), 3);
        let first = d.peek().expect("first");
        assert_eq!(first, k(0, 0, 7));
        d.take(first);
        // Re-key 5 to the front class: it now beats the deeper node 6.
        d.mark(n(5), k(0, 1, 5), 0);
        assert_eq!(d.peek(), Some(k(0, 1, 5)));
        d.take(k(0, 1, 5));
        assert_eq!(d.peek(), Some(k(2, 4, 6)));
        d.take(k(2, 4, 6));
        assert!(d.peek().is_none() && d.is_empty());
    }

    #[test]
    fn a_removed_node_never_comes_back_and_marking_twice_is_one_entry() {
        let mut d = DirtySet::default();
        d.mark(n(1), k(2, 0, 1), 0);
        d.mark(n(1), k(2, 0, 1), 0);
        assert_eq!(d.len(), 1);
        d.remove(n(1));
        assert!(d.peek().is_none());
        d.mark(n(1), k(2, 0, 1), 3);
        assert_eq!(d.peek(), Some(k(2, 0, 1)));
    }

    #[test]
    fn aging_returns_the_longest_dirty_node_once_old_enough() {
        let mut d = DirtySet::default();
        d.mark(n(1), k(2, 5, 1), 1);
        d.mark(n(2), k(2, 0, 2), 2);
        assert_eq!(d.oldest(3, 3), None, "node 1 is only 2 runs old");
        assert_eq!(d.oldest(4, 3), Some(k(2, 5, 1)));
        d.take(k(2, 5, 1));
        assert_eq!(d.oldest(4, 3), None);
        assert_eq!(d.oldest(5, 3), Some(k(2, 0, 2)));
    }

    #[test]
    fn stale_entries_are_compacted() {
        let mut d = DirtySet::default();
        for round in 0..5000u32 {
            d.mark(n(1), k(2, round, 1), 0);
        }
        assert!(d.heap.len() < 2 * d.len() + 1100, "{}", d.heap.len());
        assert_eq!(d.peek().map(|key| key.2), Some(1));
    }

    #[test]
    fn fx_hasher_spreads_small_integers() {
        use std::hash::BuildHasher;
        let b = BuildHasherDefault::<FxHasher>::default();
        let h: Vec<u64> = (0..64u32).map(|i| b.hash_one(i)).collect();
        let mut s = h.clone();
        s.sort_unstable();
        s.dedup();
        assert_eq!(s.len(), 64);
    }
}
