// SPDX-License-Identifier: GPL-3.0-or-later
//! SPA identity, ladder variant C (ADR-0003 section 5), as a re-keying layer under the source.
//!
//! The mirror keys nodes by `SrcNodeId`. This module decides which `SrcNodeId` an AT-SPI object
//! gets, so that a node the page re-created (Firefox moves a DOM node: `remove` + `add`) or
//! reused for other content (Chromium unkeyed rewrite) keeps the id of the *logical* item. The
//! mirror, cursor and bookmarks then follow the content without knowing the transport.
//!
//! Ladder (per sibling list, applied to the leftovers of a refresh):
//!
//! * IL-1 object path: a child whose path is still in the list keeps its id (the signature check
//!   for in-place rewrites is [`IdentityMap::reassign_siblings`]).
//! * IL-2 DOM `id`: removed and added objects with the same fresh `id`, role and tag.
//! * IL-3 `(role, name, tag)` keyed multimap in sibling order (moves), then `(role, tag)` LCS
//!   (edits), tie broken by sibling order.
//! * Subtrees of a matched pair are matched recursively with the same ladder.
//!
//! Everything here is pure data, deterministic and bounded: the DP is capped at
//! [`LCS_CELL_CAP`] cells and falls back to an in-order zip.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use gorca_doc_source_api::limits::{MAX_CHILDREN, MAX_DEPTH, MAX_DOC_NODES};
use gorca_doc_source_api::web_attrs;
use gorca_model::{SrcNode, SrcNodeId};
use smallvec::SmallVec;

use crate::fetch::FetchedDoc;
use crate::wire::PathKey;

/// Above this many DP cells the `(role, tag)` LCS degrades to an in-order zip.
pub const LCS_CELL_CAP: usize = 4_000_000;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Sig {
    pub role: u32,
    pub tag: Option<Arc<str>>,
    pub name: Option<Arc<str>>,
}

/// What the matcher knows about one sibling (old or new).
#[derive(Clone, Debug)]
pub struct SibInfo {
    pub sig: Sig,
    pub dom_id: Option<Arc<str>>,
    /// The id was read in the same burst as the rest of the signature (IL-2 precondition).
    pub dom_id_fresh: bool,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct MatchStats {
    pub by_path: u32,
    pub by_dom_id: u32,
    pub by_full_key: u32,
    pub by_role_tag: u32,
    pub new_ids: u32,
    pub dropped: u32,
    /// Fetched nodes cut by the depth, fan-out or node caps, or listed twice (a cycle).
    pub truncated: u32,
}

impl MatchStats {
    pub fn add(&mut self, o: &MatchStats) {
        self.by_path += o.by_path;
        self.by_dom_id += o.by_dom_id;
        self.by_full_key += o.by_full_key;
        self.by_role_tag += o.by_role_tag;
        self.new_ids += o.new_ids;
        self.dropped += o.dropped;
        self.truncated = self.truncated.saturating_add(o.truncated);
    }

    /// Share of matches that came from the heuristic rungs.
    pub fn heuristic_share(&self) -> f64 {
        let h = f64::from(self.by_full_key + self.by_role_tag);
        let t = f64::from(self.by_path + self.by_dom_id + self.by_full_key + self.by_role_tag);
        if t == 0.0 { 0.0 } else { h / t }
    }
}

/// How a burst of sibling changes looked (ADR-0003 10.1, the part the source can see).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Burst {
    /// Nothing changed in the sibling list.
    Quiet,
    /// Paths added and/or removed, signatures of kept children unchanged.
    Structural,
    /// At least two kept siblings changed signature in place: an unkeyed rewrite.
    Rewrite,
}

/// Where each leftover sibling sat relative to the siblings that are known to continue (same object
/// or matched by a strong rung): the number of such anchors before it in its own list. Rung IL-3b
/// only pairs leftovers in the same gap, so an item that left at the top of a list is never paired
/// with one that entered at the bottom (a scrolled virtual list), while an edited item that
/// kept its slot is.
#[derive(Copy, Clone, Debug)]
pub struct Slots<'a> {
    pub old_pre: &'a [u32],
    pub new_pre: &'a [u32],
}

/// Match `new` siblings to `old` siblings. Returns, for each new sibling, the index of the old one
/// it continues. Each old sibling is used at most once. With `slots == None` only the strong rungs
/// (DOM id, full key) run: used when the old and new lists are the same objects with rewritten
/// content, where a positional pairing would hand a leaving row's id to an entering row.
pub fn match_leftovers(
    old: &[SibInfo],
    new: &[SibInfo],
    slots: Option<Slots<'_>>,
    stats: &mut MatchStats,
) -> Vec<Option<usize>> {
    let mut out: Vec<Option<usize>> = vec![None; new.len()];
    let mut used = vec![false; old.len()];

    // IL-2: fresh DOM id + same role and tag.
    let mut by_id: HashMap<&str, VecDeque<usize>> = HashMap::new();
    for (i, o) in old.iter().enumerate() {
        if let (Some(id), true) = (o.dom_id.as_deref(), o.dom_id_fresh) {
            by_id.entry(id).or_default().push_back(i);
        }
    }
    for (j, n) in new.iter().enumerate() {
        let (Some(id), true) = (n.dom_id.as_deref(), n.dom_id_fresh) else {
            continue;
        };
        if let Some(q) = by_id.get_mut(id)
            && let Some(pos) = q
                .iter()
                .position(|&i| old[i].sig.role == n.sig.role && old[i].sig.tag == n.sig.tag)
            && let Some(i) = q.remove(pos)
        {
            out[j] = Some(i);
            used[i] = true;
            stats.by_dom_id += 1;
        }
    }

    // IL-3a: full (role, name, tag) key, in-order multimap (handles moves).
    let mut by_key: HashMap<&Sig, VecDeque<usize>> = HashMap::new();
    for (i, o) in old.iter().enumerate() {
        if !used[i] {
            by_key.entry(&o.sig).or_default().push_back(i);
        }
    }
    for (j, n) in new.iter().enumerate() {
        if out[j].is_some() {
            continue;
        }
        if let Some(q) = by_key.get_mut(&n.sig)
            && let Some(i) = q.pop_front()
        {
            out[j] = Some(i);
            used[i] = true;
            stats.by_full_key += 1;
        }
    }

    // IL-3b: (role, tag) LCS over what is left, per gap between anchors (edits keep their slot).
    let Some(sl) = slots else {
        return out;
    };
    let mut old_gap = Vec::with_capacity(old.len());
    let mut c = 0u32;
    for (i, u) in used.iter().enumerate() {
        old_gap.push(sl.old_pre.get(i).copied().unwrap_or(0) + c);
        c += u32::from(*u);
    }
    let mut new_gap = Vec::with_capacity(new.len());
    let mut c = 0u32;
    for (j, m) in out.iter().enumerate() {
        new_gap.push(sl.new_pre.get(j).copied().unwrap_or(0) + c);
        c += u32::from(m.is_some());
    }
    let mut groups: std::collections::BTreeMap<u32, (Vec<usize>, Vec<usize>)> =
        std::collections::BTreeMap::new();
    for (i, g) in old_gap.iter().enumerate() {
        if !used[i] {
            groups.entry(*g).or_default().0.push(i);
        }
    }
    for (j, g) in new_gap.iter().enumerate() {
        if out[j].is_none() {
            groups.entry(*g).or_default().1.push(j);
        }
    }
    let eq =
        |i: usize, j: usize| old[i].sig.role == new[j].sig.role && old[i].sig.tag == new[j].sig.tag;
    for (lo, ln) in groups.values() {
        for (i, j) in lcs_pairs(lo, ln, eq) {
            out[j] = Some(i);
            stats.by_role_tag += 1;
        }
    }
    out
}

/// Longest common subsequence of `a` and `b` under `eq`; returns the matched `(a item, b item)`.
fn lcs_pairs(a: &[usize], b: &[usize], eq: impl Fn(usize, usize) -> bool) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    if n.saturating_mul(m) > LCS_CELL_CAP {
        // In-order zip over matching pairs.
        let mut out = Vec::new();
        let mut bi = 0;
        for &ai in a {
            while bi < m && !eq(ai, b[bi]) {
                bi += 1;
            }
            if bi == m {
                break;
            }
            out.push((ai, b[bi]));
            bi += 1;
        }
        return out;
    }
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if eq(a[i], b[j]) {
                dp[(i + 1) * w + j + 1] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        if eq(a[i], b[j]) {
            out.push((a[i], b[j]));
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// One level of the iterative rebind walk.
struct RebindFrame {
    old_id: SrcNodeId,
    new_root: usize,
    old_children: Vec<SrcNodeId>,
    kids_new: Vec<usize>,
    pairing: Vec<Option<usize>>,
    next: usize,
    used_old: HashSet<usize>,
    list: SmallVec<[SrcNodeId; 8]>,
    depth: u32,
}

#[derive(Clone, Debug)]
struct NodeRec {
    path: PathKey,
    info: SibInfo,
    parent: Option<SrcNodeId>,
    children: Vec<SrcNodeId>,
}

/// Result of applying a refresh: everything the source turns into deltas.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Nodes (re)asserted, with logical ids.
    pub upserts: Vec<SrcNode>,
    /// Complete ordered child lists for `parent` and for every non-leaf upserted node.
    pub children: Vec<(SrcNodeId, SmallVec<[SrcNodeId; 8]>)>,
    /// Top ids whose whole mirror subtree is gone.
    pub removed: Vec<SrcNodeId>,
    pub stats: MatchStats,
    /// `(path, logical id)` pairs whose binding changed, for the join port and revalidation.
    pub rebound: Vec<(PathKey, SrcNodeId)>,
}

/// Path <-> logical id registry plus the signatures needed to match.
#[derive(Debug, Default)]
pub struct IdentityMap {
    /// Ablation switch for the benchmark: ignore DOM ids so that rungs IL-3 are measured alone.
    ignore_dom_id: bool,
    nodes: HashMap<SrcNodeId, NodeRec>,
    by_path: HashMap<PathKey, SrcNodeId>,
    next_synthetic: u64,
}

const SYNTHETIC_BASE: u64 = 1 << 62;

fn sig_of(node: &SrcNode, raw_role: u32, use_dom_id: bool) -> SibInfo {
    let tag = node
        .attrs
        .iter()
        .find(|(k, _)| *k == web_attrs::TAG)
        .map(|(_, v)| Arc::clone(v));
    let dom_id = node
        .attrs
        .iter()
        .find(|(k, _)| *k == web_attrs::DOM_ID && use_dom_id)
        .map(|(_, v)| Arc::clone(v));
    SibInfo {
        sig: Sig {
            role: raw_role,
            tag,
            name: node.name.clone(),
        },
        dom_id_fresh: dom_id.is_some(),
        dom_id,
    }
}

impl IdentityMap {
    pub fn new(use_dom_id: bool) -> Self {
        Self {
            ignore_dom_id: !use_dom_id,
            ..Self::default()
        }
    }

    pub fn use_dom_id(&self) -> bool {
        !self.ignore_dom_id
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn id_of(&self, path: PathKey) -> Option<SrcNodeId> {
        self.by_path.get(&path).copied()
    }

    pub fn path_of(&self, id: SrcNodeId) -> Option<PathKey> {
        self.nodes.get(&id).map(|n| n.path)
    }

    pub fn children_of(&self, id: SrcNodeId) -> Option<&[SrcNodeId]> {
        self.nodes.get(&id).map(|n| n.children.as_slice())
    }

    pub fn parent_of(&self, id: SrcNodeId) -> Option<SrcNodeId> {
        self.nodes.get(&id).and_then(|n| n.parent)
    }

    fn fresh_id(&mut self, path: PathKey) -> SrcNodeId {
        let want = SrcNodeId(path.0);
        if !self.nodes.contains_key(&want) {
            return want;
        }
        loop {
            self.next_synthetic += 1;
            let id = SrcNodeId(SYNTHETIC_BASE | self.next_synthetic);
            if !self.nodes.contains_key(&id) {
                return id;
            }
        }
    }

    /// Register a whole freshly fetched document. Ids are the path keys (IL-1); the fetched
    /// block already carries them. Replaces any previous content.
    pub fn load(&mut self, doc: &FetchedDoc) {
        self.load_parts(&[doc]);
    }

    /// Register a document delivered in several blocks (progressive build): the child lists of
    /// one block may name nodes of another, so parents are linked after every block is in.
    pub fn load_parts(&mut self, docs: &[&FetchedDoc]) {
        self.nodes.clear();
        self.by_path.clear();
        for doc in docs {
            for (i, n) in doc.block.nodes.iter().enumerate() {
                let path = doc.keys[i];
                self.by_path.insert(path, n.id);
                self.nodes.insert(
                    n.id,
                    NodeRec {
                        path,
                        info: sig_of(n, doc.raw_roles[i], self.use_dom_id()),
                        parent: None,
                        children: Vec::new(),
                    },
                );
            }
        }
        for doc in docs {
            for (p, cs) in &doc.block.children {
                if let Some(rec) = self.nodes.get_mut(p) {
                    rec.children = cs.to_vec();
                }
                for c in cs {
                    if let Some(rec) = self.nodes.get_mut(c) {
                        rec.parent = Some(*p);
                    }
                }
            }
        }
    }

    /// Classify a sibling list against the current state without changing it.
    pub fn classify(&self, parent: SrcNodeId, new_paths: &[PathKey]) -> Burst {
        let Some(rec) = self.nodes.get(&parent) else {
            return Burst::Structural;
        };
        let same = rec.children.len() == new_paths.len()
            && rec
                .children
                .iter()
                .zip(new_paths)
                .all(|(c, p)| self.nodes.get(c).is_some_and(|r| r.path == *p));
        if same {
            Burst::Quiet
        } else {
            Burst::Structural
        }
    }

    /// Paths in `new_paths` the map does not know (the source fetches these subtrees).
    pub fn unknown_paths(&self, new_paths: &[PathKey], force: &HashSet<PathKey>) -> Vec<PathKey> {
        new_paths
            .iter()
            .copied()
            .filter(|p| !self.by_path.contains_key(p) || force.contains(p))
            .collect()
    }

    /// Stored signatures of `parent`'s children, in order, with their current paths.
    pub fn child_infos(&self, parent: SrcNodeId) -> Vec<(PathKey, SibInfo)> {
        self.children_of(parent)
            .unwrap_or_default()
            .iter()
            .filter_map(|c| self.nodes.get(c).map(|r| (r.path, r.info.clone())))
            .collect()
    }

    pub fn info_of(&self, id: SrcNodeId) -> Option<&SibInfo> {
        self.nodes.get(&id).map(|r| &r.info)
    }

    /// Store the refreshed signature of a node whose id stays (an in-place edit).
    pub fn set_info(&mut self, id: SrcNodeId, info: SibInfo) {
        if let Some(r) = self.nodes.get_mut(&id) {
            r.info = info;
        }
    }

    /// [`Self::depth_of`] clamped to `u32`, for the depth cap.
    fn depth_u32(&self, id: SrcNodeId) -> u32 {
        u32::try_from(self.depth_of(id))
            .unwrap_or(MAX_DEPTH)
            .min(MAX_DEPTH)
    }

    /// Depth of a node (root = 0); used to process dirty parents top-down.
    pub fn depth_of(&self, id: SrcNodeId) -> usize {
        let mut d = 0;
        let mut cur = id;
        while let Some(p) = self.nodes.get(&cur).and_then(|r| r.parent) {
            d += 1;
            cur = p;
            if d > 100_000 {
                break;
            }
        }
        d
    }

    /// Re-sync the subtree under `id` from a fresh fetch rooted at the same object (or a new
    /// object that continues it). `fetched.roots[0]` is the new root.
    pub fn resync_subtree(&mut self, id: SrcNodeId, fetched: &FetchedDoc) -> Outcome {
        let mut out = Outcome::default();
        let child_idx = child_index(fetched);
        if fetched.block.nodes.is_empty() || !self.nodes.contains_key(&id) {
            return out;
        }
        let depth = self.depth_u32(id);
        self.rebind_subtree(id, 0, fetched, &child_idx, &mut out, depth);
        out
    }

    /// Apply a refresh of `parent`'s child list. `added` holds the fetched subtrees of the paths
    /// in `new_paths` that were unknown (`added.block.nodes` ids are path keys; its `roots` say
    /// where each subtree starts).
    pub fn apply_children(
        &mut self,
        parent: SrcNodeId,
        new_paths: &[PathKey],
        added: &FetchedDoc,
        force: &HashSet<PathKey>,
    ) -> Outcome {
        let mut out = Outcome::default();
        let Some(old_children) = self.nodes.get(&parent).map(|r| r.children.clone()) else {
            return out;
        };
        let new_set: HashSet<PathKey> = new_paths.iter().copied().collect();

        // Which fetched root continues which path.
        let mut root_of_path: HashMap<PathKey, usize> = HashMap::new();
        for &ri in &added.roots {
            root_of_path.insert(added.keys[ri], ri);
        }
        let child_idx = child_index(added);

        // Removed old children (path no longer present).
        let removed_old: Vec<SrcNodeId> = old_children
            .iter()
            .copied()
            .filter(|c| {
                self.nodes
                    .get(c)
                    .is_some_and(|r| !new_set.contains(&r.path) || force.contains(&r.path))
            })
            .collect();
        // Added new roots in list order.
        let added_roots: Vec<usize> = new_paths
            .iter()
            .filter(|p| !self.by_path.contains_key(p) || force.contains(p))
            .filter_map(|p| root_of_path.get(p).copied())
            .collect();

        let old_infos: Vec<SibInfo> = removed_old
            .iter()
            .filter_map(|c| self.nodes.get(c).map(|r| r.info.clone()))
            .collect();
        let new_infos: Vec<SibInfo> = added_roots
            .iter()
            .map(|&ri| {
                let mut info = sig_of(
                    &added.block.nodes[ri],
                    added.raw_roles[ri],
                    self.use_dom_id(),
                );
                // IL-R6: an object that existed before the burst (a forced path) may carry a
                // stale `id`; only ids of new objects are trusted.
                if force.contains(&added.keys[ri]) {
                    info.dom_id_fresh = false;
                }
                info
            })
            .collect();
        // Anchors: children that are still the same object (path kept and not forced).
        let mut old_pre: Vec<u32> = Vec::new();
        let mut cnt = 0u32;
        for c in &old_children {
            if let Some(r) = self.nodes.get(c) {
                if new_set.contains(&r.path) && !force.contains(&r.path) {
                    cnt += 1;
                } else {
                    old_pre.push(cnt);
                }
            }
        }
        let mut new_pre: Vec<u32> = Vec::new();
        cnt = 0;
        for p in new_paths {
            if self.by_path.contains_key(p) && !force.contains(p) {
                cnt += 1;
            } else if root_of_path.contains_key(p) {
                new_pre.push(cnt);
            }
        }
        let pairing = match_leftovers(
            &old_infos,
            &new_infos,
            Some(Slots {
                old_pre: &old_pre,
                new_pre: &new_pre,
            }),
            &mut out.stats,
        );

        let mut matched_old: HashSet<usize> = HashSet::new();
        let mut new_root_id: HashMap<usize, SrcNodeId> = HashMap::new();
        for (j, m) in pairing.iter().enumerate() {
            if let Some(i) = *m {
                matched_old.insert(i);
                new_root_id.insert(added_roots[j], removed_old[i]);
            }
        }

        // Drop the old subtrees that were not continued; continued ones are re-bound below.
        let old_to_new: HashMap<usize, usize> = pairing
            .iter()
            .enumerate()
            .filter_map(|(j, m)| m.map(|i| (i, j)))
            .collect();
        let mut continued_old: Vec<(SrcNodeId, usize)> = Vec::new();
        for (i, c) in removed_old.iter().enumerate() {
            if let Some(&j) = old_to_new.get(&i) {
                continued_old.push((*c, added_roots[j]));
            } else {
                out.removed.push(*c);
                out.stats.dropped += self.drop_subtree(*c);
            }
        }
        for (old_id, new_root) in continued_old {
            let depth = self.depth_u32(parent) + 1;
            self.rebind_subtree(old_id, new_root, added, &child_idx, &mut out, depth);
        }

        // Build the new child list.
        let mut new_list: Vec<SrcNodeId> = Vec::with_capacity(new_paths.len());
        for p in new_paths {
            if let Some(&id) = self.by_path.get(p) {
                new_list.push(id);
                if !force.contains(p) && !root_of_path.contains_key(p) {
                    out.stats.by_path += 1;
                }
            } else if let Some(&ri) = root_of_path.get(p) {
                let depth = self.depth_u32(parent) + 1;
                let mut visited = HashSet::new();
                let id = self.insert_fresh_subtree(
                    ri,
                    parent,
                    added,
                    &child_idx,
                    &mut out,
                    depth,
                    &mut visited,
                );
                new_list.push(id);
            }
        }
        // Re-parent continued roots and store the list.
        for c in &new_list {
            if let Some(r) = self.nodes.get_mut(c) {
                r.parent = Some(parent);
            }
        }
        if let Some(r) = self.nodes.get_mut(&parent) {
            r.children.clone_from(&new_list);
        }
        out.children.push((
            parent,
            new_list.into_iter().collect::<SmallVec<[SrcNodeId; 8]>>(),
        ));
        out
    }

    /// In-place rewrite (Chromium reuses an object for other content): the paths are unchanged
    /// but the signatures at those paths changed. Returns the new `path -> id` assignment and
    /// the usual outcome; ids follow content, per the ladder.
    pub fn reassign_siblings(
        &mut self,
        parent: SrcNodeId,
        new_infos: &[(PathKey, SibInfo)],
    ) -> Outcome {
        let mut out = Outcome::default();
        let Some(old_children) = self.nodes.get(&parent).map(|r| r.children.clone()) else {
            return out;
        };
        let old_infos: Vec<SibInfo> = old_children
            .iter()
            .filter_map(|c| self.nodes.get(c).map(|r| r.info.clone()))
            .collect();
        if old_infos.len() != old_children.len() {
            return out;
        }
        let news: Vec<SibInfo> = new_infos.iter().map(|(_, i)| i.clone()).collect();
        let pairing = match_leftovers(&old_infos, &news, None, &mut out.stats);
        let mut new_list = Vec::with_capacity(news.len());
        let mut used = HashSet::new();
        for (j, (path, info)) in new_infos.iter().enumerate() {
            let id = if let Some(i) = pairing[j] {
                used.insert(i);
                old_children[i]
            } else {
                out.stats.new_ids += 1;
                self.fresh_id(*path)
            };
            new_list.push((id, *path, info.clone()));
        }
        // Old children that continue nowhere are gone.
        for (i, c) in old_children.iter().enumerate() {
            if !used.contains(&i) {
                out.removed.push(*c);
                out.stats.dropped += self.drop_subtree(*c);
            }
        }
        // Rebind: remove stale path bindings of the old siblings, then bind the new ones.
        for c in &old_children {
            if let Some(r) = self.nodes.get(c)
                && self.by_path.get(&r.path) == Some(c)
            {
                self.by_path.remove(&r.path);
            }
        }
        let mut ids = SmallVec::<[SrcNodeId; 8]>::new();
        for (id, path, info) in new_list {
            let rec = self.nodes.entry(id).or_insert_with(|| NodeRec {
                path,
                info: info.clone(),
                parent: Some(parent),
                children: Vec::new(),
            });
            rec.path = path;
            rec.info = info;
            rec.parent = Some(parent);
            self.by_path.insert(path, id);
            out.rebound.push((path, id));
            ids.push(id);
        }
        if let Some(r) = self.nodes.get_mut(&parent) {
            r.children = ids.to_vec();
        }
        out.children.push((parent, ids));
        out
    }

    fn drop_subtree(&mut self, root: SrcNodeId) -> u32 {
        let mut n = 0;
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            if let Some(rec) = self.nodes.remove(&id) {
                if self.by_path.get(&rec.path) == Some(&id) {
                    self.by_path.remove(&rec.path);
                }
                stack.extend(rec.children);
                n += 1;
            }
        }
        n
    }

    /// New subtree `new_root` continues the logical node `old_id` (whose own subtree is dead).
    ///
    /// Iterative (SL-3: the fetched tree is untrusted and can be deeper than any stack): an
    /// explicit frame stack, a cap at [`MAX_DEPTH`] below `depth`, and a visited set so a path
    /// the forest lists twice cannot loop.
    fn rebind_subtree(
        &mut self,
        old_id: SrcNodeId,
        new_root: usize,
        added: &FetchedDoc,
        child_idx: &HashMap<usize, Vec<usize>>,
        out: &mut Outcome,
        depth: u32,
    ) {
        let mut visited: HashSet<usize> = HashSet::new();
        visited.insert(new_root);
        let first = self.rebind_open(old_id, new_root, added, child_idx, out, depth, &mut visited);
        let mut stack = vec![first];
        while let Some(top) = stack.last_mut() {
            if top.next >= top.kids_new.len() {
                if let Some(done) = stack.pop() {
                    self.rebind_close(done, added, out);
                }
                continue;
            }
            let j = top.next;
            top.next += 1;
            let k = top.kids_new[j];
            if let Some(i) = top.pairing[j] {
                top.used_old.insert(i);
                let child = top.old_children[i];
                top.list.push(child);
                let d = top.depth + 1;
                let f = self.rebind_open(child, k, added, child_idx, out, d, &mut visited);
                stack.push(f);
            } else {
                let (parent, d) = (top.old_id, top.depth + 1);
                let id =
                    self.insert_fresh_subtree(k, parent, added, child_idx, out, d, &mut visited);
                if let Some(top) = stack.last_mut() {
                    top.list.push(id);
                }
            }
        }
    }

    /// The matching half of a rebind: pair the children of `old_id` with those of `new_root`.
    #[allow(clippy::too_many_arguments)]
    fn rebind_open(
        &mut self,
        old_id: SrcNodeId,
        new_root: usize,
        added: &FetchedDoc,
        child_idx: &HashMap<usize, Vec<usize>>,
        out: &mut Outcome,
        depth: u32,
        visited: &mut HashSet<usize>,
    ) -> RebindFrame {
        let old_children: Vec<SrcNodeId> = self
            .nodes
            .get(&old_id)
            .map(|r| r.children.clone())
            .unwrap_or_default();
        // The old node's own binding moves to the new object.
        if let Some(rec) = self.nodes.get(&old_id)
            && self.by_path.get(&rec.path) == Some(&old_id)
        {
            let p = rec.path;
            self.by_path.remove(&p);
        }
        let mut kids_new: Vec<usize> = child_idx.get(&new_root).cloned().unwrap_or_default();
        self.clip_children(&mut kids_new, depth, visited, out);

        // Match the children: old ones are all dead (their parent's object is gone).
        let old_infos: Vec<SibInfo> = old_children
            .iter()
            .filter_map(|c| self.nodes.get(c).map(|r| r.info.clone()))
            .collect();
        let new_infos: Vec<SibInfo> = kids_new
            .iter()
            .map(|&k| sig_of(&added.block.nodes[k], added.raw_roles[k], self.use_dom_id()))
            .collect();
        // IL-1 first: a child that is still the same object keeps its id. The ladder then
        // matches what is left (moved, recreated or rewritten children).
        let old_path_idx: HashMap<PathKey, usize> = old_children
            .iter()
            .enumerate()
            .filter_map(|(i, c)| self.nodes.get(c).map(|r| (r.path, i)))
            .collect();
        let mut pairing: Vec<Option<usize>> = vec![None; new_infos.len()];
        let mut taken = vec![false; old_infos.len()];
        for (j, &k) in kids_new.iter().enumerate() {
            if let Some(&i) = old_path_idx.get(&added.keys[k])
                && !taken[i]
            {
                pairing[j] = Some(i);
                taken[i] = true;
                out.stats.by_path += 1;
            }
        }
        // IL-1 signature check: objects that kept their path but changed content. When the new
        // content of at least two of them is the old content of another sibling, the page reused
        // the objects for other items (an unkeyed rewrite): re-match those by content, keep the
        // path pairing only for what content cannot place (plain edits).
        let changed: Vec<(usize, usize)> = pairing
            .iter()
            .enumerate()
            .filter_map(|(j, m)| m.map(|i| (j, i)))
            .filter(|(j, i)| old_infos[*i].sig != new_infos[*j].sig)
            .collect();
        let old_sigs: HashSet<&Sig> = old_infos.iter().map(|i| &i.sig).collect();
        let moved = changed
            .iter()
            .filter(|(j, _)| old_sigs.contains(&new_infos[*j].sig))
            .count();
        if changed.len() >= 2 && moved >= 2 {
            let un_new: Vec<usize> = changed.iter().map(|(j, _)| *j).collect();
            let un_old: Vec<usize> = changed.iter().map(|(_, i)| *i).collect();
            // These objects existed before: their `id` attribute may be stale (IL-R6), so the
            // content decides.
            let not_fresh = |x: &SibInfo| SibInfo {
                dom_id_fresh: false,
                ..x.clone()
            };
            let sub_old: Vec<SibInfo> = un_old.iter().map(|&i| not_fresh(&old_infos[i])).collect();
            let sub_new: Vec<SibInfo> = un_new.iter().map(|&j| not_fresh(&new_infos[j])).collect();
            out.stats.by_path = out
                .stats
                .by_path
                .saturating_sub(u32::try_from(changed.len()).unwrap_or(0));
            for (j, i) in &changed {
                pairing[*j] = None;
                taken[*i] = false;
            }
            let r = match_leftovers(&sub_old, &sub_new, None, &mut out.stats);
            for (x, m) in r.iter().enumerate() {
                if let Some(y) = m {
                    pairing[un_new[x]] = Some(un_old[*y]);
                    taken[un_old[*y]] = true;
                }
            }
            for (j, i) in &changed {
                if pairing[*j].is_none() && !taken[*i] {
                    pairing[*j] = Some(*i);
                    taken[*i] = true;
                    out.stats.by_path += 1;
                }
            }
        }
        let lo: Vec<usize> = (0..old_infos.len()).filter(|i| !taken[*i]).collect();
        let ln: Vec<usize> = (0..new_infos.len())
            .filter(|j| pairing[*j].is_none())
            .collect();
        if !lo.is_empty() && !ln.is_empty() {
            let sub_old: Vec<SibInfo> = lo.iter().map(|&i| old_infos[i].clone()).collect();
            let sub_new: Vec<SibInfo> = ln.iter().map(|&j| new_infos[j].clone()).collect();
            let mut old_pre = Vec::with_capacity(lo.len());
            let mut c = 0u32;
            for t in &taken {
                if *t {
                    c += 1;
                } else {
                    old_pre.push(c);
                }
            }
            let mut new_pre = Vec::with_capacity(ln.len());
            c = 0;
            for p in &pairing {
                if p.is_some() {
                    c += 1;
                } else {
                    new_pre.push(c);
                }
            }
            let r = match_leftovers(
                &sub_old,
                &sub_new,
                Some(Slots {
                    old_pre: &old_pre,
                    new_pre: &new_pre,
                }),
                &mut out.stats,
            );
            for (x, m) in r.iter().enumerate() {
                if let Some(y) = m {
                    pairing[ln[x]] = Some(lo[*y]);
                }
            }
        }

        RebindFrame {
            old_id,
            new_root,
            old_children,
            kids_new,
            pairing,
            next: 0,
            used_old: HashSet::new(),
            list: SmallVec::new(),
            depth,
        }
    }

    /// The finishing half of a rebind, once every child is handled.
    fn rebind_close(&mut self, f: RebindFrame, added: &FetchedDoc, out: &mut Outcome) {
        let RebindFrame {
            old_id,
            new_root,
            old_children,
            used_old,
            list,
            ..
        } = f;
        let n = &added.block.nodes[new_root];
        let info = sig_of(n, added.raw_roles[new_root], self.use_dom_id());
        let path = added.keys[new_root];
        for (i, c) in old_children.iter().enumerate() {
            if !used_old.contains(&i) {
                out.removed.push(*c);
                out.stats.dropped += self.drop_subtree(*c);
            }
        }
        self.by_path.insert(path, old_id);
        out.rebound.push((path, old_id));
        if let Some(rec) = self.nodes.get_mut(&old_id) {
            rec.path = path;
            rec.info = info;
            rec.children = list.to_vec();
        }
        for c in &list {
            if let Some(r) = self.nodes.get_mut(c) {
                r.parent = Some(old_id);
            }
        }
        let mut node = n.clone();
        node.id = old_id;
        out.upserts.push(node);
        if !list.is_empty() {
            out.children.push((old_id, list));
        }
    }

    /// Cut a fetched child list to the caps: nothing below [`MAX_DEPTH`], at most
    /// [`MAX_CHILDREN`], no index seen before in this walk (a cycle or a duplicate), and no
    /// growth past [`MAX_DOC_NODES`]. Every cut is counted in `out.stats.truncated`.
    fn clip_children(
        &self,
        kids: &mut Vec<usize>,
        depth: u32,
        visited: &mut HashSet<usize>,
        out: &mut Outcome,
    ) {
        let before = kids.len();
        if depth >= MAX_DEPTH || self.nodes.len() >= MAX_DOC_NODES {
            kids.clear();
        } else {
            kids.retain(|k| visited.insert(*k));
            kids.truncate(MAX_CHILDREN);
        }
        let cut = before - kids.len();
        if cut > 0 {
            out.stats.truncated = out
                .stats
                .truncated
                .saturating_add(u32::try_from(cut).unwrap_or(u32::MAX));
        }
    }

    /// Insert a brand new subtree with fresh ids; returns the id of its root. Iterative and
    /// capped like [`Self::rebind_subtree`].
    #[allow(clippy::too_many_arguments)]
    fn insert_fresh_subtree(
        &mut self,
        root: usize,
        parent: SrcNodeId,
        added: &FetchedDoc,
        child_idx: &HashMap<usize, Vec<usize>>,
        out: &mut Outcome,
        depth: u32,
        visited: &mut HashSet<usize>,
    ) -> SrcNodeId {
        struct Fresh {
            id: SrcNodeId,
            node: SrcNode,
            kids: Vec<usize>,
            next: usize,
            list: SmallVec<[SrcNodeId; 8]>,
            depth: u32,
        }
        let open = |this: &mut Self,
                    k: usize,
                    parent: SrcNodeId,
                    depth: u32,
                    visited: &mut HashSet<usize>,
                    out: &mut Outcome|
         -> Fresh {
            let path = added.keys[k];
            let id = this.fresh_id(path);
            let mut node = added.block.nodes[k].clone();
            node.id = id;
            let mut kids: Vec<usize> = child_idx.get(&k).cloned().unwrap_or_default();
            this.clip_children(&mut kids, depth, visited, out);
            // Insert the record first so children can find their parent and ids stay unique.
            this.nodes.insert(
                id,
                NodeRec {
                    path,
                    info: sig_of(&added.block.nodes[k], added.raw_roles[k], this.use_dom_id()),
                    parent: Some(parent),
                    children: Vec::new(),
                },
            );
            this.by_path.insert(path, id);
            out.rebound.push((path, id));
            out.stats.new_ids += 1;
            Fresh {
                id,
                node,
                kids,
                next: 0,
                list: SmallVec::new(),
                depth,
            }
        };
        visited.insert(root);
        let first = open(self, root, parent, depth, visited, out);
        let first_id = first.id;
        let mut stack = vec![first];
        while let Some(top) = stack.last_mut() {
            if top.next < top.kids.len() {
                let k = top.kids[top.next];
                top.next += 1;
                let (pid, d) = (top.id, top.depth + 1);
                let f = open(self, k, pid, d, visited, out);
                stack.push(f);
                continue;
            }
            let Some(done) = stack.pop() else { break };
            if let Some(rec) = self.nodes.get_mut(&done.id) {
                rec.children = done.list.to_vec();
            }
            out.upserts.push(done.node);
            if !done.list.is_empty() {
                out.children.push((done.id, done.list));
            }
            if let Some(up) = stack.last_mut() {
                up.list.push(done.id);
            }
        }
        first_id
    }
}

fn child_index(doc: &FetchedDoc) -> HashMap<usize, Vec<usize>> {
    let pos: HashMap<SrcNodeId, usize> = doc
        .block
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let mut m = HashMap::new();
    for (p, cs) in &doc.block.children {
        if let Some(&pi) = pos.get(p) {
            m.insert(pi, cs.iter().filter_map(|c| pos.get(c).copied()).collect());
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fetch::FetchTimings;
    use gorca_doc_source_api::{Completeness, TreeBlock};
    use gorca_model::{AttrKey, StateSet};

    fn zeros(n: usize) -> Vec<u32> {
        vec![0; n]
    }

    fn info(role: u32, tag: &str, name: &str, id: Option<&str>) -> SibInfo {
        SibInfo {
            sig: Sig {
                role,
                tag: Some(tag.into()),
                name: Some(name.into()),
            },
            dom_id: id.map(Into::into),
            dom_id_fresh: id.is_some(),
        }
    }

    #[test]
    fn dom_id_beats_everything() {
        let old = vec![
            info(1, "li", "a", Some("x1")),
            info(1, "li", "b", Some("x2")),
        ];
        let new = vec![
            info(1, "li", "renamed b", Some("x2")),
            info(1, "li", "renamed a", Some("x1")),
        ];
        let mut st = MatchStats::default();
        assert_eq!(
            match_leftovers(
                &old,
                &new,
                Some(Slots {
                    old_pre: &zeros(old.len()),
                    new_pre: &zeros(new.len())
                }),
                &mut st
            ),
            vec![Some(1), Some(0)]
        );
        assert_eq!(st.by_dom_id, 2);
    }

    #[test]
    fn stale_dom_id_is_ignored() {
        let mut o = info(1, "li", "a", Some("x1"));
        o.dom_id_fresh = false;
        let old = vec![o];
        let new = vec![info(1, "li", "zzz", Some("x1"))];
        let mut st = MatchStats::default();
        // Not matched by id (stale); matched by the (role, tag) rung instead.
        let r = match_leftovers(
            &old,
            &new,
            Some(Slots {
                old_pre: &zeros(old.len()),
                new_pre: &zeros(new.len()),
            }),
            &mut st,
        );
        assert_eq!(r, vec![Some(0)]);
        assert_eq!(st.by_dom_id, 0);
        assert_eq!(st.by_role_tag, 1);
    }

    #[test]
    fn firefox_move_matches_by_full_key_in_any_order() {
        let old: Vec<SibInfo> = (0..5)
            .map(|i| info(1, "li", &format!("item {i}"), None))
            .collect();
        let new: Vec<SibInfo> = (0..5)
            .rev()
            .map(|i| info(1, "li", &format!("item {i}"), None))
            .collect();
        let mut st = MatchStats::default();
        let r = match_leftovers(
            &old,
            &new,
            Some(Slots {
                old_pre: &zeros(old.len()),
                new_pre: &zeros(new.len()),
            }),
            &mut st,
        );
        assert_eq!(r, vec![Some(4), Some(3), Some(2), Some(1), Some(0)]);
        assert_eq!(st.by_full_key, 5);
    }

    #[test]
    fn edited_item_keeps_its_place_via_role_tag_lcs() {
        let old = vec![
            info(1, "li", "a", None),
            info(1, "li", "b", None),
            info(1, "li", "c", None),
        ];
        let new = vec![
            info(1, "li", "a", None),
            info(1, "li", "B edited", None),
            info(1, "li", "c", None),
        ];
        let mut st = MatchStats::default();
        assert_eq!(
            match_leftovers(
                &old,
                &new,
                Some(Slots {
                    old_pre: &zeros(old.len()),
                    new_pre: &zeros(new.len())
                }),
                &mut st
            ),
            vec![Some(0), Some(1), Some(2)]
        );
        assert_eq!((st.by_full_key, st.by_role_tag), (2, 1));
    }

    #[test]
    fn duplicates_pair_in_sibling_order() {
        let old = vec![info(1, "li", "same", None), info(1, "li", "same", None)];
        let new = vec![info(1, "li", "same", None), info(1, "li", "same", None)];
        let mut st = MatchStats::default();
        assert_eq!(
            match_leftovers(
                &old,
                &new,
                Some(Slots {
                    old_pre: &zeros(old.len()),
                    new_pre: &zeros(new.len())
                }),
                &mut st
            ),
            vec![Some(0), Some(1)]
        );
    }

    #[test]
    fn different_role_is_not_matched() {
        let old = vec![info(1, "li", "a", None)];
        let new = vec![info(2, "li", "a", None)];
        let mut st = MatchStats::default();
        assert_eq!(
            match_leftovers(
                &old,
                &new,
                Some(Slots {
                    old_pre: &zeros(old.len()),
                    new_pre: &zeros(new.len())
                }),
                &mut st
            ),
            vec![None]
        );
    }

    fn node(id: u64, name: &str, tag: &str) -> SrcNode {
        let mut attrs = SmallVec::new();
        attrs.push((web_attrs::TAG, Arc::<str>::from(tag)));
        SrcNode {
            id: SrcNodeId(id),
            role: gorca_model::Role::Panel,
            states: StateSet::empty(),
            name: Some(name.into()),
            text: None,
            attrs,
            local: None,
        }
    }

    /// root(1) -> [ul(2) -> [li(3 "a"), li(4 "b")]]
    fn base() -> FetchedDoc {
        let nodes = vec![
            node(1, "root", "body"),
            node(2, "list", "ul"),
            node(3, "a", "li"),
            node(4, "b", "li"),
        ];
        FetchedDoc {
            block: TreeBlock {
                root: SrcNodeId(1),
                completeness: Completeness::States,
                nodes,
                children: vec![
                    (SrcNodeId(1), [SrcNodeId(2)].into_iter().collect()),
                    (
                        SrcNodeId(2),
                        [SrcNodeId(3), SrcNodeId(4)].into_iter().collect(),
                    ),
                ],
            },
            keys: vec![PathKey(1), PathKey(2), PathKey(3), PathKey(4)],
            raw_roles: vec![85, 31, 32, 32],
            child_counts: vec![1, 2, 0, 0],
            roots: vec![0],
            timings: FetchTimings::default(),
            incomplete: false,
        }
    }

    fn forest(items: &[(u64, &str)]) -> FetchedDoc {
        let nodes: Vec<SrcNode> = items.iter().map(|(p, n)| node(*p, n, "li")).collect();
        FetchedDoc {
            block: TreeBlock {
                root: nodes[0].id,
                completeness: Completeness::States,
                nodes,
                children: vec![],
            },
            keys: items.iter().map(|(p, _)| PathKey(*p)).collect(),
            raw_roles: vec![32; items.len()],
            child_counts: vec![0; items.len()],
            roots: (0..items.len()).collect(),
            timings: FetchTimings::default(),
            incomplete: false,
        }
    }

    #[test]
    fn recreated_nodes_keep_their_logical_ids() {
        let mut m = IdentityMap::default();
        m.load(&base());
        // Firefox moves both li: new objects at new paths 10, 11, order swapped.
        let added = forest(&[(10, "b"), (11, "a")]);
        let o = m.apply_children(
            SrcNodeId(2),
            &[PathKey(10), PathKey(11)],
            &added,
            &HashSet::new(),
        );
        assert_eq!(o.removed, vec![]);
        let list = &o
            .children
            .iter()
            .find(|(p, _)| *p == SrcNodeId(2))
            .unwrap()
            .1;
        assert_eq!(list.as_slice(), &[SrcNodeId(4), SrcNodeId(3)]);
        assert_eq!(m.id_of(PathKey(10)), Some(SrcNodeId(4)));
        assert_eq!(m.id_of(PathKey(3)), None);
        assert_eq!(o.stats.by_full_key, 2);
        // Upserts carry the logical ids, not the new path keys.
        let ids: Vec<u64> = o.upserts.iter().map(|n| n.id.0).collect();
        assert!(ids.contains(&3) && ids.contains(&4) && !ids.contains(&10));
    }

    #[test]
    fn genuinely_new_and_removed_children() {
        let mut m = IdentityMap::default();
        m.load(&base());
        // li a stays (path 3), li b removed, brand-new "zzz" at path 20 of another role: new id.
        let mut added = forest(&[(20, "zzz")]);
        added.raw_roles = vec![99];
        let o = m.apply_children(
            SrcNodeId(2),
            &[PathKey(3), PathKey(20)],
            &added,
            &HashSet::new(),
        );
        assert_eq!(o.removed, vec![SrcNodeId(4)]);
        assert_eq!(m.id_of(PathKey(20)), Some(SrcNodeId(20)));
        assert_eq!(o.stats.new_ids, 1);
        assert_eq!(
            m.children_of(SrcNodeId(2)).unwrap(),
            &[SrcNodeId(3), SrcNodeId(20)]
        );
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn id_collisions_use_synthetic_ids() {
        let mut m = IdentityMap::default();
        m.load(&base());
        // New object whose path key equals a live logical id (path numbers reused).
        let mut added = forest(&[(4, "other")]);
        added.raw_roles = vec![99];
        // Path 4 is known (kept); to force a collision remove it first via a new path.
        let o = m.apply_children(
            SrcNodeId(2),
            &[PathKey(3)],
            &FetchedDoc {
                roots: vec![],
                ..forest(&[(3, "x")])
            },
            &HashSet::new(),
        );
        assert_eq!(o.removed, vec![SrcNodeId(4)]);
        let o = m.apply_children(
            SrcNodeId(2),
            &[PathKey(3), PathKey(4)],
            &added,
            &HashSet::new(),
        );
        // id 4 was freed by the removal, so the path key is reusable as an id again.
        assert_eq!(m.id_of(PathKey(4)), Some(SrcNodeId(4)));
        assert_eq!(o.stats.new_ids, 1);
        let _ = AttrKey(0);
    }

    /// root(1) -> ul(2) -> li 3..=(2+n) named "a".."" (n items)
    fn base_n(n: u64) -> FetchedDoc {
        let mut nodes = vec![node(1, "root", "body"), node(2, "list", "ul")];
        let mut keys = vec![PathKey(1), PathKey(2)];
        let mut roles = vec![85, 31];
        let mut counts = vec![1, i32::try_from(n).unwrap()];
        let mut kids: SmallVec<[SrcNodeId; 8]> = SmallVec::new();
        for i in 0..n {
            nodes.push(node(3 + i, &format!("item {i}"), "li"));
            keys.push(PathKey(3 + i));
            roles.push(32);
            counts.push(0);
            kids.push(SrcNodeId(3 + i));
        }
        FetchedDoc {
            block: TreeBlock {
                root: SrcNodeId(1),
                completeness: Completeness::States,
                nodes,
                children: vec![
                    (SrcNodeId(1), [SrcNodeId(2)].into_iter().collect()),
                    (SrcNodeId(2), kids),
                ],
            },
            keys,
            raw_roles: roles,
            child_counts: counts,
            roots: vec![0],
            timings: FetchTimings::default(),
            incomplete: false,
        }
    }

    #[test]
    fn scrolled_window_does_not_hand_leaving_ids_to_entering_rows() {
        let mut m = IdentityMap::default();
        m.load(&base_n(4)); // li paths 3, 4, 5, 6
        // Window scrolled by two: li 3, 4 left at the top, two new objects entered at the bottom.
        let added = forest(&[(7, "item 4"), (8, "item 5")]);
        let o = m.apply_children(
            SrcNodeId(2),
            &[PathKey(5), PathKey(6), PathKey(7), PathKey(8)],
            &added,
            &HashSet::new(),
        );
        assert_eq!(o.removed, vec![SrcNodeId(3), SrcNodeId(4)]);
        assert_eq!(
            o.stats.by_role_tag, 0,
            "leaving and entering rows sit in different gaps"
        );
        assert_eq!(m.id_of(PathKey(7)), Some(SrcNodeId(7)));
        assert_eq!(m.id_of(PathKey(5)), Some(SrcNodeId(5)));
        assert_eq!(o.stats.by_path, 2);
    }

    #[test]
    fn edited_recreated_item_keeps_its_slot_between_anchors() {
        let mut m = IdentityMap::default();
        m.load(&base_n(4));
        // li 4 ("item 1") is recreated at a new path with edited text; its neighbours are kept.
        let added = forest(&[(9, "item 1 (edited)")]);
        let o = m.apply_children(
            SrcNodeId(2),
            &[PathKey(3), PathKey(9), PathKey(5), PathKey(6)],
            &added,
            &HashSet::new(),
        );
        assert_eq!(o.removed, vec![]);
        assert_eq!(
            m.id_of(PathKey(9)),
            Some(SrcNodeId(4)),
            "same slot, same logical item"
        );
        assert_eq!(o.stats.by_role_tag, 1);
    }

    /// ul(2) with li 3..=6 named per `names`, same object paths.
    fn list_with_names(names: [&str; 4]) -> FetchedDoc {
        let mut nodes = vec![node(2, "list", "ul")];
        let mut keys = vec![PathKey(2)];
        let mut kids: SmallVec<[SrcNodeId; 8]> = SmallVec::new();
        for (i, n) in names.iter().enumerate() {
            nodes.push(node(3 + i as u64, n, "li"));
            keys.push(PathKey(3 + i as u64));
            kids.push(SrcNodeId(3 + i as u64));
        }
        FetchedDoc {
            block: TreeBlock {
                root: SrcNodeId(2),
                completeness: Completeness::States,
                nodes,
                children: vec![(SrcNodeId(2), kids)],
            },
            keys,
            raw_roles: vec![31, 32, 32, 32, 32],
            child_counts: vec![4, 0, 0, 0, 0],
            roots: vec![0],
            timings: FetchTimings::default(),
            incomplete: false,
        }
    }

    #[test]
    fn resync_follows_content_when_objects_were_reused_for_other_items() {
        let mut m = IdentityMap::new(false);
        m.load(&base_n(4)); // names "item 0".."item 3" at paths 3..6
        let id_of = |m: &IdentityMap, p: u64| m.id_of(PathKey(p)).unwrap();
        let ids: Vec<SrcNodeId> = (3..=6).map(|p| id_of(&m, p)).collect();
        // Same objects, content reversed (React with index keys).
        let o = m.resync_subtree(
            SrcNodeId(2),
            &list_with_names(["item 3", "item 2", "item 1", "item 0"]),
        );
        assert_eq!(o.removed, vec![]);
        assert_eq!(
            m.id_of(PathKey(3)),
            Some(ids[3]),
            "item 3's id moved with its content"
        );
        assert_eq!(m.id_of(PathKey(6)), Some(ids[0]));
        assert!(o.stats.by_full_key >= 4);
    }

    /// Like `list_with_names` but every `li` also carries a DOM id (`ids[i]`).
    fn list_with_names_and_ids(names: [&str; 4], ids: [&str; 4]) -> FetchedDoc {
        let mut d = list_with_names(names);
        for (i, id) in ids.iter().enumerate() {
            d.block.nodes[1 + i]
                .attrs
                .push((web_attrs::DOM_ID, Arc::<str>::from(*id)));
        }
        d
    }

    #[test]
    fn resync_does_not_trust_ids_of_objects_that_existed_before() {
        // Chromium keeps the old `id` attribute on an object whose attribute changed in place:
        // the same ids appear on the same objects while the content moved. Content must win.
        let mut m = IdentityMap::new(true);
        let mut before = list_with_names_and_ids(
            ["item 0", "item 1", "item 2", "item 3"],
            ["a", "b", "c", "d"],
        );
        // base_n has no ids; load the id-carrying version instead.
        before.raw_roles = vec![31, 32, 32, 32, 32];
        before.keys = vec![PathKey(2), PathKey(3), PathKey(4), PathKey(5), PathKey(6)];
        m.load(&before);
        let ids: Vec<SrcNodeId> = (3..=6).map(|p| m.id_of(PathKey(p)).unwrap()).collect();
        let after = list_with_names_and_ids(
            ["item 3", "item 2", "item 1", "item 0"],
            ["a", "b", "c", "d"],
        );
        let o = m.resync_subtree(SrcNodeId(2), &after);
        assert_eq!(o.removed, vec![]);
        assert_eq!(
            m.id_of(PathKey(3)),
            Some(ids[3]),
            "item 3 moved to the first object with its content"
        );
        assert_eq!(m.id_of(PathKey(6)), Some(ids[0]));
        assert_eq!(o.stats.by_dom_id, 0, "the stale ids were not used");
    }

    #[test]
    fn resync_keeps_the_object_id_for_a_plain_edit() {
        let mut m = IdentityMap::new(false);
        m.load(&base_n(4));
        let id3 = m.id_of(PathKey(3)).unwrap();
        let o = m.resync_subtree(
            SrcNodeId(2),
            &list_with_names(["item 0 (edited)", "item 1", "item 2", "item 3"]),
        );
        assert_eq!(m.id_of(PathKey(3)), Some(id3));
        assert_eq!(o.stats.by_path, 4);
    }

    #[test]
    fn rewrite_in_place_moves_ids_with_content() {
        let mut m = IdentityMap::default();
        m.load(&base());
        // Same paths 3, 4; contents swapped (React unkeyed reorder).
        let news = vec![
            (PathKey(3), info(32, "li", "b", None)),
            (PathKey(4), info(32, "li", "a", None)),
        ];
        let o = m.reassign_siblings(SrcNodeId(2), &news);
        let list = &o.children[0].1;
        assert_eq!(list.as_slice(), &[SrcNodeId(4), SrcNodeId(3)]);
        assert_eq!(m.id_of(PathKey(3)), Some(SrcNodeId(4)));
        assert_eq!(m.id_of(PathKey(4)), Some(SrcNodeId(3)));
    }

    // ---- SL-3: untrusted trees (reviewer probes, regression) ---------------------------

    fn chain_forest(root_path: u64, n: u64, cycle: bool) -> FetchedDoc {
        let len = usize::try_from(n).unwrap_or(0);
        let nodes: Vec<SrcNode> = (0..n).map(|i| node(root_path + i, "x", "div")).collect();
        let mut children: Vec<(SrcNodeId, SmallVec<[SrcNodeId; 8]>)> = (0..n - 1)
            .map(|i| {
                (
                    SrcNodeId(root_path + i),
                    [SrcNodeId(root_path + i + 1)].into_iter().collect(),
                )
            })
            .collect();
        if cycle {
            children.push((
                SrcNodeId(root_path + n - 1),
                [SrcNodeId(root_path)].into_iter().collect(),
            ));
        }
        FetchedDoc {
            block: TreeBlock {
                root: SrcNodeId(root_path),
                completeness: Completeness::States,
                nodes,
                children,
            },
            keys: (0..n).map(|i| PathKey(root_path + i)).collect(),
            raw_roles: vec![32; len],
            child_counts: vec![1; len],
            roots: vec![0],
            timings: FetchTimings::default(),
            incomplete: false,
        }
    }

    fn add_under_list(m: &mut IdentityMap, added: &FetchedDoc) -> Outcome {
        m.apply_children(
            SrcNodeId(2),
            &[PathKey(3), PathKey(4), PathKey(1_000)],
            added,
            &HashSet::new(),
        )
    }

    #[test]
    fn a_hundred_thousand_deep_fresh_subtree_is_cut_without_recursion() {
        let mut m = IdentityMap::default();
        m.load(&base());
        let o = add_under_list(&mut m, &chain_forest(1_000, 100_000, false));
        // list is at depth 1: the new chain may reach MAX_DEPTH
        assert_eq!(o.stats.new_ids, MAX_DEPTH - 1);
        assert!(o.stats.truncated > 0);
    }

    #[test]
    fn a_path_listed_twice_does_not_loop() {
        let mut m = IdentityMap::default();
        m.load(&base());
        let o = add_under_list(&mut m, &chain_forest(1_000, 3, true));
        assert_eq!(o.stats.new_ids, 3);
        assert_eq!(o.stats.truncated, 1, "the back edge is cut and counted");
    }

    #[test]
    fn a_deep_rebind_is_iterative_and_capped_too() {
        let mut m = IdentityMap::default();
        m.load(&chain_forest(1, 50_000, false));
        let again = chain_forest(1, 50_000, false);
        let o = m.resync_subtree(SrcNodeId(1), &again);
        assert!(o.stats.truncated > 0);
        let mut cyc = IdentityMap::default();
        cyc.load(&base());
        let o = cyc.resync_subtree(SrcNodeId(1), &chain_forest(1, 4, true));
        assert!(o.stats.truncated >= 1);
    }

    #[test]
    fn the_node_budget_stops_growth() {
        // A fan-out beyond the child cap is cut at the cap.
        let mut m = IdentityMap::default();
        m.load(&base());
        let n = MAX_CHILDREN as u64 + 10;
        let mut f = chain_forest(1_000, 1, false);
        let kids: SmallVec<[SrcNodeId; 8]> = (1..=n).map(|i| SrcNodeId(1_000 + i)).collect();
        for i in 1..=n {
            f.block.nodes.push(node(1_000 + i, "k", "li"));
            f.keys.push(PathKey(1_000 + i));
            f.raw_roles.push(32);
            f.child_counts.push(0);
        }
        f.block.children = vec![(SrcNodeId(1_000), kids)];
        let o = add_under_list(&mut m, &f);
        assert_eq!(o.stats.new_ids as usize, MAX_CHILDREN + 1);
        assert_eq!(o.stats.truncated, 10);
    }
}
