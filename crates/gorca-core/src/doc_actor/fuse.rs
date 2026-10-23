// SPDX-License-Identifier: GPL-3.0-or-later
//! The document actor's fusion stage (M22a-C2, ADR-0022 SM-8, ADR-0024 H-7).
//!
//! The document actor is the single writer of its nodes, so it is where `FUSE` runs. The source
//! delivers normalised records ([`gorca_model::SrcNode::rec`], built by the normaliser in the
//! source crate, never here); this stage mirrors the source's tree into a [`gorca_fuse::Ivm`],
//! feeds every record as the `atspi` primary's registers, and answers "what is the fused entry
//! of this source node". The translator asks it while it builds the mirror, so the entry
//! (`Arc<SemEntry>`) is part of the node's [`gorca_doc::web::NodeMeta`] when the batch is
//! published: fusion lands in the left-right back copy, never after the swap.
//!
//! # Timeline
//!
//! Registers keep the newest write per source (Lemma 1), so every write carries a sequence number
//! on the actor timeline: a batch is `2n` for the `n`-th flush; a fact issued while `t` batches
//! had been flushed (a focus fetch of a web node forwarded by the app actor, SM-8) is `2t + 1`,
//! newer than the batch that produced revision `t` and older than the next one.
//!
//! # Structure
//!
//! The Ivm follows the source's complete child lists (SL-4), not the mirror: placeholders, caps
//! and the translator's orphan handling stay the translator's business. A node listed before its
//! own upsert exists in the engine but is not alive (no life register) until the upsert arrives.
//! Everything the source sends is untrusted (SL-3): the node count is capped and every engine
//! error is counted, never propagated.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc_source_api::FuseSetup;
use gorca_doc_source_api::limits::{MAX_CHILDREN, MAX_DOC_NODES};
use gorca_fuse::{Budget, Input, Ivm, NoPatches, NodeIdx, PrimarySlot, QuirkSet, Val, Write};
use gorca_model::sem::{Group, SemEntry, SemRole, SrcRecord};
use gorca_model::{DocDelta, SrcNode, SrcNodeId};

/// Work one run of the engine may do before the actor goes back to its queues.
const BATCH_BUDGET: Duration = Duration::from_millis(2);

/// The AT-SPI primary's register slot.
const ATSPI: PrimarySlot = PrimarySlot::P0;

/// Where the translator reads fused entries from.
pub trait SemLookup {
    /// The published entry of a source node, `None` when it is unknown or not alive.
    fn sem(&self, src: SrcNodeId) -> Option<Arc<SemEntry>>;
}

/// No fusion: every node has no entry.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoSem;

impl SemLookup for NoSem {
    fn sem(&self, _src: SrcNodeId) -> Option<Arc<SemEntry>> {
        None
    }
}

/// Counters of the stage (process-independent: one per document).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FuseStats {
    /// Batches and facts applied.
    pub applies: u64,
    /// Node fusions the engine performed.
    pub fusions: u64,
    /// Engine refusals (unknown node, cycle, duplicate): a hostile or buggy list, never fatal.
    pub errors: u64,
    /// Nodes not added because the document cap was reached.
    pub capped: u64,
}

/// The fusion stage of one document.
#[derive(Debug)]
pub struct FuseStage {
    setup: FuseSetup,
    ivm: Ivm<QuirkSet, NoPatches>,
    ids: HashMap<SrcNodeId, NodeIdx>,
    /// Source id of each engine slot (`None`: free).
    src_of: Vec<Option<SrcNodeId>>,
    free: Vec<u32>,
    root: Option<SrcNodeId>,
    batches: u64,
    stats: FuseStats,
}

fn engine(setup: &FuseSetup) -> Ivm<QuirkSet, NoPatches> {
    Ivm::new(setup.profile.clone(), setup.quirks.clone(), NoPatches)
}

impl FuseStage {
    pub fn new(setup: FuseSetup) -> FuseStage {
        FuseStage {
            ivm: engine(&setup),
            setup,
            ids: HashMap::new(),
            src_of: Vec::new(),
            free: Vec::new(),
            root: None,
            batches: 0,
            stats: FuseStats::default(),
        }
    }

    pub fn stats(&self) -> FuseStats {
        self.stats
    }

    /// Name of the profile the tables came from (logs).
    pub fn profile_name(&self) -> &str {
        &self.setup.name
    }

    /// Source nodes the engine holds.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The timeline value a fact issued now would carry: the number of batches applied.
    pub fn timeline(&self) -> u64 {
        self.batches
    }

    fn reset(&mut self) {
        self.ivm = engine(&self.setup);
        self.ids.clear();
        self.src_of.clear();
        self.free.clear();
        self.root = None;
    }

    fn alloc(&mut self, src: SrcNodeId) -> Option<NodeIdx> {
        if let Some(n) = self.ids.get(&src) {
            return Some(*n);
        }
        if self.ids.len() >= MAX_DOC_NODES {
            self.stats.capped += 1;
            return None;
        }
        let ix = if let Some(i) = self.free.pop() {
            i
        } else {
            let i = u32::try_from(self.src_of.len()).ok()?;
            self.src_of.push(None);
            i
        };
        let n = NodeIdx(ix);
        if self.ivm.add_node(n, None).is_err() {
            self.stats.errors += 1;
            self.free.push(ix);
            return None;
        }
        if let Some(slot) = self.src_of.get_mut(ix as usize) {
            *slot = Some(src);
        }
        self.ids.insert(src, n);
        Some(n)
    }

    /// Forget the engine slot (the engine already dropped the node).
    fn release(&mut self, n: NodeIdx) {
        if let Some(slot) = self.src_of.get_mut(n.0 as usize)
            && let Some(src) = slot.take()
        {
            self.ids.remove(&src);
            self.free.push(n.0);
        }
    }

    /// Remove `n` and its subtree from the engine and the id maps.
    fn remove_subtree(&mut self, n: NodeIdx) {
        let mut all = vec![n];
        let mut i = 0;
        while let Some(&cur) = all.get(i) {
            all.extend_from_slice(self.ivm.children(cur));
            i += 1;
        }
        if self.ivm.remove_subtree(n).is_err() {
            self.stats.errors += 1;
            return;
        }
        for x in all {
            self.release(x);
        }
    }

    /// Write a record's known groups as the `atspi` primary's registers at `seq`.
    fn write(&mut self, node: NodeIdx, rec: &SrcRecord, seq: u64) {
        let writes = writes_of(rec);
        let mut inputs: Vec<Input> = Vec::with_capacity(writes.len() + 1);
        inputs.push(Input::Life {
            node,
            src: ATSPI,
            seq,
            alive: true,
        });
        inputs.extend(writes.into_iter().map(|write| Input::Obs {
            node,
            src: ATSPI,
            seq,
            write,
        }));
        let rep = self.ivm.apply(inputs);
        self.stats.errors += rep.errors.len() as u64;
    }

    fn upsert(&mut self, n: &SrcNode, seq: u64) {
        let Some(ix) = self.alloc(n.id) else { return };
        if let Some(rec) = &n.rec {
            self.write(ix, rec, seq);
            return;
        }
        // A synthetic node (a text run): the text is its content.
        let mut rec = SrcRecord::default();
        rec.sem.role = SemRole::StaticText;
        rec.known.set(Group::Role, true);
        if let Some(t) = &n.text {
            rec.name = Some(Arc::clone(t));
            rec.known.set(Group::Name, true);
        }
        self.write(ix, &rec, seq);
    }

    fn set_children(&mut self, parent: SrcNodeId, list: &[SrcNodeId], detached: &mut Vec<NodeIdx>) {
        let Some(p) = self.alloc(parent) else { return };
        let mut kids: Vec<NodeIdx> = Vec::with_capacity(list.len().min(MAX_CHILDREN));
        for c in list.iter().take(MAX_CHILDREN) {
            if let Some(k) = self.alloc(*c) {
                kids.push(k);
            }
        }
        let before: Vec<NodeIdx> = self.ivm.children(p).to_vec();
        if self.ivm.set_children(p, &kids).is_err() {
            self.stats.errors += 1;
            return;
        }
        detached.extend(before.into_iter().filter(|c| self.ivm.parent(*c).is_none()));
    }

    /// Apply one settled batch (the same deltas the translator applies) and run the engine.
    /// Returns the source nodes whose entry changed (an entry that equals the published one is
    /// cut off by the engine and not listed).
    pub fn apply(&mut self, batch: &[DocDelta]) -> Vec<SrcNodeId> {
        self.batches += 1;
        self.stats.applies += 1;
        let seq = 2 * self.batches;
        // A new document: everything of the batch belongs to it, like the translator's reset.
        let reset_root = batch.iter().rev().find_map(|d| {
            if let DocDelta::Reset { root, .. } = d {
                Some(*root)
            } else {
                None
            }
        });
        if let Some(root) = reset_root {
            self.reset();
            self.root = Some(root);
        }
        for d in batch {
            if let DocDelta::Upsert(nodes) = d {
                for n in nodes {
                    self.upsert(n, seq);
                }
            }
        }
        // The last list of a parent wins (as in the translator).
        let mut lists: HashMap<SrcNodeId, &[SrcNodeId]> = HashMap::new();
        let mut order: Vec<SrcNodeId> = Vec::new();
        for d in batch {
            if let DocDelta::Children { parent, children } = d
                && lists.insert(*parent, children.as_slice()).is_none()
            {
                order.push(*parent);
            }
        }
        let mut detached: Vec<NodeIdx> = Vec::new();
        for p in order {
            if let Some(list) = lists.get(&p) {
                self.set_children(p, list, &mut detached);
            }
        }
        // Dropped from a list and not taken by another: gone (the translator reaps these).
        for n in detached {
            if self.is_root(n) || self.ivm.parent(n).is_some() || !self.ivm.contains(n) {
                continue;
            }
            self.remove_subtree(n);
        }
        for d in batch {
            if let DocDelta::Removed(ids) = d {
                for id in ids {
                    let Some(n) = self.ids.get(id).copied() else {
                        continue;
                    };
                    // A node a list kept stays (removals only take what nothing kept).
                    if self.is_root(n) || self.ivm.parent(n).is_some() {
                        continue;
                    }
                    self.remove_subtree(n);
                }
            }
        }
        self.run()
    }

    /// Write a record the app actor fetched for a node of this document (a focus fetch of a web
    /// node, SM-8) at `2 * issued_at + 1`. `None` when the node is not in the engine.
    pub fn fact(
        &mut self,
        src: SrcNodeId,
        rec: &SrcRecord,
        issued_at: u64,
    ) -> Option<Vec<SrcNodeId>> {
        let n = self.ids.get(&src).copied()?;
        self.stats.applies += 1;
        // The focus chain (the fetched node and its ancestors, nearest first) fuses first.
        let mut chain = vec![n];
        let mut cur = n;
        while let Some(p) = self.ivm.parent(cur) {
            chain.push(p);
            cur = p;
            if chain.len() > 64 {
                break;
            }
        }
        self.ivm.set_focus_chain(&chain);
        self.write(n, rec, 2 * issued_at + 1);
        Some(self.run())
    }

    fn is_root(&self, n: NodeIdx) -> bool {
        self.root
            .and_then(|r| self.ids.get(&r))
            .is_some_and(|r| *r == n)
    }

    /// Fusions still owed after a budgeted run (dirty nodes and background dependents).
    pub fn pending(&self) -> usize {
        self.ivm.pending()
    }

    /// Continue the fusion the last run left (one more budget); returns the entries that changed.
    pub fn step(&mut self) -> Vec<SrcNodeId> {
        self.run()
    }

    /// One bounded run: the focus chain and the visible window first (the engine's priority
    /// classes), at most [`BATCH_BUDGET`] of work (ADR-0022: the writer's interactive messages
    /// never wait for a whole page to fuse). What is left stays dirty for [`FuseStage::step`].
    fn run(&mut self) -> Vec<SrcNodeId> {
        let rep = self.ivm.run(Budget::until(Instant::now() + BATCH_BUDGET));
        self.stats.fusions += rep.fuse_calls as u64;
        rep.published
            .iter()
            .filter(|p| p.entry.is_some())
            .filter_map(|p| self.src_of.get(p.node.0 as usize).copied().flatten())
            .collect()
    }
}

impl SemLookup for FuseStage {
    fn sem(&self, src: SrcNodeId) -> Option<Arc<SemEntry>> {
        let n = self.ids.get(&src)?;
        self.ivm.entry_view(*n)
    }
}

/// The register writes of a record: a known group holding its default is `Absent` (the engine
/// was asked and said nothing), anything else is a value; an unknown group writes nothing.
pub fn writes_of(rec: &SrcRecord) -> Vec<Write> {
    fn pick<T: Default + PartialEq>(v: T, g: Group, f: impl FnOnce(T) -> Val) -> Write {
        if v == T::default() {
            Write::Absent(g)
        } else {
            Write::Val(f(v))
        }
    }
    let s = &rec.sem;
    let mut out = Vec::with_capacity(gorca_model::sem::GROUP_COUNT);
    for &g in Group::ALL {
        if !rec.known.has(g) {
            continue;
        }
        out.push(match g {
            Group::Role => pick(s.role, g, Val::Role),
            Group::Name => match &rec.name {
                Some(n) if !n.is_empty() => Write::Val(Val::Name(Arc::clone(n))),
                _ => Write::Absent(g),
            },
            Group::Description => match s.description() {
                Some(d) if !d.is_empty() => Write::Val(Val::Description(Arc::from(d))),
                _ => Write::Absent(g),
            },
            Group::Check => pick(s.check, g, Val::Check),
            Group::Press => pick(s.press, g, Val::Press),
            Group::Expand => pick(s.expand, g, Val::Expand),
            Group::Select => pick(s.select, g, Val::Select),
            Group::Current => pick(s.current, g, Val::Current),
            Group::Invalid => pick(s.invalid, g, Val::Invalid),
            Group::Popup => pick(s.popup, g, Val::Popup),
            Group::Avail => pick(s.avail, g, Val::Avail),
            Group::Flags => pick(s.flags, g, Val::Flags),
            Group::Level => pick(s.level, g, Val::Level),
            Group::SetPos => s
                .pos
                .map_or(Write::Absent(g), |p| Write::Val(Val::SetPos(p))),
            Group::Grid => s
                .grid
                .map_or(Write::Absent(g), |p| Write::Val(Val::Grid(p))),
            Group::Live => s
                .live
                .map_or(Write::Absent(g), |l| Write::Val(Val::Live(Some(l)))),
            Group::Orientation => pick(s.orientation, g, Val::Orientation),
            Group::Sort => pick(s.sort, g, Val::Sort),
            Group::RoleDescription
            | Group::Placeholder
            | Group::KeyShortcuts
            | Group::ValueText
            | Group::NameFrom => gorca_fuse::rare_write(s, g).unwrap_or(Write::Absent(g)),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::{Check, GroupMask};
    use gorca_model::{AttrMap, Role, StateSet};

    use super::*;

    fn setup() -> FuseSetup {
        use gorca_fuse::{Profile, Rules};
        let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria());
        profile.default_role = SemRole::Generic;
        FuseSetup {
            name: Arc::from("test"),
            profile,
            quirks: QuirkSet::default(),
        }
    }

    fn rec(role: SemRole, name: Option<&str>) -> Arc<SrcRecord> {
        let mut r = SrcRecord::default();
        r.sem.role = role;
        r.name = name.map(Arc::from);
        r.known = GroupMask::EMPTY;
        r.known.set(Group::Role, true);
        r.known.set(Group::Name, true);
        Arc::new(r)
    }

    fn node(id: u64, role: SemRole, name: Option<&str>) -> SrcNode {
        SrcNode {
            id: SrcNodeId(id),
            role: Role::Unknown,
            states: StateSet::default(),
            name: name.map(Arc::from),
            text: None,
            attrs: AttrMap::default(),
            local: None,
            rec: Some(rec(role, name)),
        }
    }

    fn kids(p: u64, c: &[u64]) -> DocDelta {
        DocDelta::Children {
            parent: SrcNodeId(p),
            children: c.iter().map(|x| SrcNodeId(*x)).collect(),
        }
    }

    fn name_of(s: &FuseStage, id: u64) -> Option<String> {
        s.sem(SrcNodeId(id))
            .and_then(|e| e.name.as_deref().map(str::to_owned))
    }

    fn reset(root: u64) -> DocDelta {
        DocDelta::Reset {
            root: SrcNodeId(root),
            nav_id: 1,
        }
    }

    #[test]
    fn a_document_is_fused_and_content_names_flow_up() {
        let mut s = FuseStage::new(setup());
        let changed = s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Link, None),
                node(3, SemRole::StaticText, Some("Home")),
            ]),
            kids(1, &[2]),
            kids(2, &[3]),
        ]);
        assert!(changed.contains(&SrcNodeId(2)));
        // The link has no name of its own: it is named from its content.
        assert_eq!(name_of(&s, 2).as_deref(), Some("Home"));
        assert_eq!(s.sem(SrcNodeId(2)).map(|e| e.sem.role), Some(SemRole::Link));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn a_later_batch_changes_only_what_it_touches_and_equal_input_changes_nothing() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Button, Some("Save")),
            ]),
            kids(1, &[2]),
        ]);
        let again = s.apply(&[DocDelta::Upsert(vec![node(
            2,
            SemRole::Button,
            Some("Save"),
        )])]);
        assert!(again.is_empty(), "an equal record is cut off: {again:?}");
        let ch = s.apply(&[DocDelta::Upsert(vec![node(
            2,
            SemRole::Button,
            Some("Store"),
        )])]);
        assert_eq!(ch, vec![SrcNodeId(2)]);
        assert_eq!(name_of(&s, 2).as_deref(), Some("Store"));
    }

    #[test]
    fn child_lists_reorder_move_and_reap_like_the_translator() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Group, None),
                node(3, SemRole::Group, None),
                node(4, SemRole::StaticText, Some("a")),
                node(5, SemRole::StaticText, Some("b")),
            ]),
            kids(1, &[2, 3]),
            kids(2, &[4, 5]),
        ]);
        assert_eq!(s.len(), 5);
        // Move 5 under 3 and drop 4 from the tree: 4 is gone, 5 follows its new parent.
        s.apply(&[kids(2, &[]), kids(3, &[5])]);
        assert_eq!(s.len(), 4, "node 4 was dropped from every list and reaped");
        assert!(s.sem(SrcNodeId(4)).is_none());
        assert!(s.sem(SrcNodeId(5)).is_some());
        // An explicit removal takes a subtree nothing kept; one a list kept stays.
        s.apply(&[
            DocDelta::Removed(vec![SrcNodeId(3), SrcNodeId(1)]),
            kids(1, &[2, 3]),
        ]);
        assert_eq!(
            s.len(),
            4,
            "3 stayed (a list kept it), the root is never removed"
        );
        s.apply(&[kids(1, &[2]), DocDelta::Removed(vec![SrcNodeId(3)])]);
        assert_eq!(s.len(), 2, "3 and its child 5 left");
        // Slots are reused.
        let slots = s.src_of.len();
        s.apply(&[
            DocDelta::Upsert(vec![node(9, SemRole::Button, Some("x"))]),
            kids(1, &[2, 9]),
        ]);
        assert_eq!(s.src_of.len(), slots, "a freed engine slot was reused");
    }

    #[test]
    fn a_listed_child_without_an_upsert_is_not_alive_until_it_arrives() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![node(1, SemRole::DocumentWeb, None)]),
            kids(1, &[7]),
        ]);
        assert!(s.sem(SrcNodeId(7)).is_none(), "a placeholder has no entry");
        s.apply(&[DocDelta::Upsert(vec![node(
            7,
            SemRole::Button,
            Some("Late"),
        )])]);
        assert_eq!(name_of(&s, 7).as_deref(), Some("Late"));
    }

    #[test]
    fn a_reset_starts_a_new_document_and_hostile_lists_only_count_errors() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![node(1, SemRole::DocumentWeb, None)]),
            kids(1, &[2]),
        ]);
        s.apply(&[
            reset(10),
            DocDelta::Upsert(vec![node(10, SemRole::DocumentWeb, None)]),
        ]);
        assert_eq!(s.len(), 1, "the old page is gone");
        assert!(s.sem(SrcNodeId(2)).is_none());
        // A cycle (a node listing its ancestor) is refused and counted, nothing panics.
        s.apply(&[
            DocDelta::Upsert(vec![node(11, SemRole::Group, None)]),
            kids(10, &[11]),
            kids(11, &[10]),
        ]);
        assert!(s.stats().errors >= 1);
        assert_eq!(
            s.sem(SrcNodeId(10)).map(|e| e.sem.role),
            Some(SemRole::DocumentWeb)
        );
    }

    #[test]
    fn a_fact_is_older_than_the_next_batch_and_newer_than_the_last() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Button, Some("Old")),
            ]),
            kids(1, &[2]),
        ]);
        let t = s.timeline();
        // Issued after batch t: wins over it.
        let ch = s.fact(SrcNodeId(2), &rec(SemRole::Button, Some("Fresh")), t);
        assert_eq!(ch, Some(vec![SrcNodeId(2)]));
        assert_eq!(name_of(&s, 2).as_deref(), Some("Fresh"));
        // A fact issued before the batch that followed it loses to that batch.
        s.apply(&[DocDelta::Upsert(vec![node(
            2,
            SemRole::Button,
            Some("Batch"),
        )])]);
        let late = s.fact(SrcNodeId(2), &rec(SemRole::Button, Some("Stale")), t);
        assert_eq!(late, Some(Vec::new()), "older than the batch: no change");
        assert_eq!(name_of(&s, 2).as_deref(), Some("Batch"));
        // Unknown node: no entry to write.
        assert!(
            s.fact(SrcNodeId(99), &rec(SemRole::Button, None), t)
                .is_none()
        );
    }

    #[test]
    fn writes_are_absent_for_defaults_and_nothing_for_unknown_groups() {
        let mut r = SrcRecord::default();
        r.sem.role = SemRole::Checkbox;
        r.sem.check = Check::On;
        r.known.set(Group::Role, true);
        r.known.set(Group::Check, true);
        r.known.set(Group::Expand, true);
        let w = writes_of(&r);
        assert_eq!(w.len(), 3, "unknown groups write nothing: {w:?}");
        assert!(w.contains(&Write::Val(Val::Check(Check::On))));
        assert!(w.contains(&Write::Absent(Group::Expand)));
    }

    /// A first block of a big page is fused inside the flush that publishes it: the cost per node
    /// is part of the first-useful latency. The bound is loose (debug build, shared host); the
    /// printed number is the measurement.
    #[test]
    fn a_big_first_block_fuses_in_bounded_time() {
        const N: u64 = 20_000;
        let mut nodes = vec![node(1, SemRole::DocumentWeb, None)];
        let mut lists: Vec<DocDelta> = Vec::new();
        let mut top: Vec<u64> = Vec::new();
        for g in 0..(N / 20) {
            let gid = 2 + g * 21;
            nodes.push(node(gid, SemRole::Group, None));
            let mut ks = Vec::new();
            for i in 1..=20 {
                nodes.push(node(gid + i, SemRole::Link, Some("link")));
                ks.push(gid + i);
            }
            lists.push(kids(gid, &ks));
            top.push(gid);
        }
        lists.push(kids(1, &top));
        let mut batch = vec![reset(1), DocDelta::Upsert(nodes)];
        batch.extend(lists);
        let mut s = FuseStage::new(setup());
        let t = std::time::Instant::now();
        let mut changed = s.apply(&batch);
        let first = t.elapsed();
        while s.pending() > 0 {
            changed.extend(s.step());
        }
        let took = t.elapsed();
        eprintln!("first run {first:?}");
        assert!(
            first < Duration::from_millis(500),
            "the first run is bounded: {first:?}"
        );
        eprintln!(
            "fuse stage: {} nodes in {:?} ({:.2} us/node, debug build)",
            s.len(),
            took,
            took.as_secs_f64() * 1e6 / f64::from(u32::try_from(s.len()).unwrap_or(u32::MAX))
        );
        assert!(changed.len() >= usize::try_from(N).unwrap());
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");
    }
}
