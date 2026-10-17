// SPDX-License-Identifier: GPL-3.0-or-later
//! Left-right publication (ADR-0003 section 4.8), built on `arc-swap` so it needs no `unsafe`.
//!
//! Two copies of the document exist. Readers load the *live* one through an `ArcSwap` and never
//! wait for the writer. The writer mutates the other (*back*) copy, logs each op, and
//! `publish()` swaps the two with one atomic pointer exchange. The old live copy becomes the
//! new back copy; the logged ops are replayed on it before the next write, as soon as its last
//! reader has left (PUB-3). If a reader holds it longer than the wait budget, the writer clones
//! the live copy instead of waiting, so a stuck reader costs memory and one O(n) copy, never a
//! stall (CLAUDE.md "never block").

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use super::doc::WebDoc;
use super::error::DocError;
use super::ops::{DocOp, OpOutput};

/// How long the writer waits for readers of the old copy before cloning instead.
pub const DEFAULT_WAIT_BUDGET: Duration = Duration::from_millis(2);
/// Waits above this are counted as slow (PUB-3 watchdog).
pub const SLOW_WAIT: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PublishStats {
    pub publishes: u64,
    /// Replays that had to wait for a reader to leave the old copy.
    pub waits: u64,
    pub slow_waits: u64,
    pub max_wait_ns: u64,
    /// Waits that exceeded the budget and fell back to cloning the live copy.
    pub fallback_clones: u64,
}

/// Owned by the document actor. Not `Clone`.
#[derive(Debug)]
pub struct DocWriter {
    live: Arc<ArcSwap<WebDoc>>,
    back: Arc<WebDoc>,
    /// Ops the live copy has and the back copy lacks.
    replay: Vec<DocOp>,
    /// Ops applied to the back copy since the last publish.
    pending: Vec<DocOp>,
    wait_budget: Duration,
    stats: PublishStats,
}

/// Cheap, cloneable reader handle: one per reading thread.
#[derive(Clone, Debug)]
pub struct DocRead {
    live: Arc<ArcSwap<WebDoc>>,
}

/// Create a writer/reader pair around an empty document with the given priority seed.
pub fn channel(seed: u64) -> (DocWriter, DocRead) {
    channel_from(WebDoc::new(seed))
}

/// Create a pair around an existing document.
pub fn channel_from(doc: WebDoc) -> (DocWriter, DocRead) {
    let live = Arc::new(ArcSwap::from_pointee(doc.clone()));
    let w = DocWriter {
        live: Arc::clone(&live),
        back: Arc::new(doc),
        replay: Vec::new(),
        pending: Vec::new(),
        wait_budget: DEFAULT_WAIT_BUDGET,
        stats: PublishStats::default(),
    };
    (w, DocRead { live })
}

impl DocRead {
    /// Run `f` on the current published snapshot. Wait-free for the caller; keep `f` short
    /// (hot queries take 1 to 5 us, PUB-3 allows 100 us) because the writer cannot reuse this
    /// copy until `f` returns.
    pub fn with<R>(&self, f: impl FnOnce(&WebDoc) -> R) -> R {
        let guard = self.live.load();
        f(&guard)
    }

    /// An owned handle to the current snapshot. Holding it long forces the writer onto its
    /// clone fallback; prefer [`DocRead::with`].
    pub fn snapshot(&self) -> Arc<WebDoc> {
        self.live.load_full()
    }
}

impl DocWriter {
    pub fn set_wait_budget(&mut self, budget: Duration) {
        self.wait_budget = budget;
    }

    pub fn stats(&self) -> PublishStats {
        self.stats
    }

    /// The writer's working copy (includes unpublished changes). Brings the copy up to date
    /// first, so it may wait (bounded) for readers of the previously live copy.
    pub fn doc(&mut self) -> &WebDoc {
        self.catch_up();
        &self.back
    }

    /// Number of ops applied since the last publish.
    pub fn pending_ops(&self) -> usize {
        self.pending.len()
    }

    /// Bring the back copy up to date with the live copy, waiting (bounded) for old readers.
    fn catch_up(&mut self) {
        if self.replay.is_empty() {
            return;
        }
        let start = Instant::now();
        let mut waited = false;
        loop {
            if let Some(doc) = Arc::get_mut(&mut self.back) {
                for op in self.replay.drain(..) {
                    // Deterministic replay of an op that succeeded on the other copy.
                    let applied = doc.apply(&op);
                    debug_assert!(applied.is_ok(), "replay diverged: {applied:?}");
                }
                break;
            }
            waited = true;
            if start.elapsed() >= self.wait_budget {
                let cur = self.live.load_full();
                self.back = Arc::new(WebDoc::clone(&cur));
                self.replay.clear();
                self.stats.fallback_clones += 1;
                break;
            }
            std::thread::yield_now();
        }
        if waited {
            let ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
            self.stats.waits += 1;
            self.stats.max_wait_ns = self.stats.max_wait_ns.max(ns);
            if ns >= u64::try_from(SLOW_WAIT.as_nanos()).unwrap_or(u64::MAX) {
                self.stats.slow_waits += 1;
            }
        }
    }

    /// Apply an op to the working copy. Not visible to readers until [`DocWriter::publish`].
    pub fn apply(&mut self, op: DocOp) -> Result<OpOutput, DocError> {
        self.catch_up();
        // `make_mut` clones only if the back copy is somehow shared; correctness never
        // depends on the uniqueness invariant.
        let out = Arc::make_mut(&mut self.back).apply(&op)?;
        self.pending.push(op);
        Ok(out)
    }

    /// Apply a batch; stops at the first rejected op (earlier ops stay applied).
    pub fn apply_all(&mut self, ops: impl IntoIterator<Item = DocOp>) -> Result<(), DocError> {
        for op in ops {
            self.apply(op)?;
        }
        Ok(())
    }

    /// Make everything applied so far visible to readers, atomically (PUB-2: readers never
    /// see half a batch). Returns `false` when there was nothing to publish.
    pub fn publish(&mut self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        let new_live = Arc::clone(&self.back);
        let old = self.live.swap(new_live);
        self.back = old;
        self.replay = std::mem::take(&mut self.pending);
        self.stats.publishes += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_model::Role;

    use super::*;
    use crate::web::cats::{NodeSpec, SubtreeSpec};

    fn root_op() -> DocOp {
        DocOp::InsertSubtree {
            parent: None,
            before: None,
            subtree: Arc::new(SubtreeSpec::leaf(NodeSpec::new(Role::Document))),
        }
    }

    #[test]
    fn unpublished_changes_are_invisible() {
        let (mut w, r) = channel(1);
        assert_eq!(r.with(WebDoc::len), 0);
        assert!(w.apply(root_op()).is_ok());
        assert_eq!(r.with(WebDoc::len), 0);
        assert_eq!(w.doc().len(), 1);
        assert!(w.publish());
        assert_eq!(r.with(WebDoc::len), 1);
        assert!(!w.publish());
    }

    #[test]
    fn both_copies_converge_after_replay() {
        let (mut w, r) = channel(7);
        let out = w.apply(root_op()).map(|o| o.created);
        let root = out.ok().and_then(|c| c.first().copied());
        let Some(root) = root else {
            panic!("root not created");
        };
        w.publish();
        for i in 0..20 {
            let sub = Arc::new(SubtreeSpec::leaf(
                NodeSpec::new(Role::Heading).level(1 + (i % 6)),
            ));
            assert!(
                w.apply(DocOp::InsertSubtree {
                    parent: Some(root),
                    before: None,
                    subtree: sub
                })
                .is_ok()
            );
            w.publish();
            assert_eq!(r.with(WebDoc::len), w.doc().len());
        }
        let a: Vec<_> = r.with(|d| d.preorder().collect());
        let b: Vec<_> = w.doc().preorder().collect();
        assert_eq!(a, b);
        assert!(r.with(WebDoc::validate).is_ok());
        assert!(w.doc().validate().is_ok());
    }

    #[test]
    fn stuck_reader_triggers_clone_fallback_not_a_stall() {
        let (mut w, r) = channel(3);
        w.set_wait_budget(Duration::from_millis(1));
        assert!(w.apply(root_op()).is_ok());
        w.publish();
        let held = r.snapshot(); // stuck reader pins the live copy
        assert!(w.apply(root_op()).is_err()); // root exists; catch-up already ran on first apply
        let sub = Arc::new(SubtreeSpec::leaf(NodeSpec::new(Role::Paragraph)));
        let root = w.doc().root();
        assert!(
            w.apply(DocOp::InsertSubtree {
                parent: root,
                before: None,
                subtree: Arc::clone(&sub)
            })
            .is_ok()
        );
        w.publish(); // back (= held's copy) goes stale
        let t = Instant::now();
        assert!(
            w.apply(DocOp::InsertSubtree {
                parent: root,
                before: None,
                subtree: sub
            })
            .is_ok()
        );
        assert!(t.elapsed() < Duration::from_millis(500));
        assert!(w.stats().fallback_clones >= 1);
        assert_eq!(held.len(), 1);
    }
}
