//! `WebDoc`: generational arena + intrusive tree + augmented implicit treap over document
//! order (ADR-0003 sections 4.1, 4.4, 4.5; GM section 1.2).
//!
//! The treap is keyed by position only (an implicit treap over preorder). Every node of the
//! arena has one treap node in the parallel `tn` array. Each treap node carries a summary of
//! its subtree: node count, OR of categories, flat-text scalar and newline counts, and counts
//! for the ranked categories. A tree subtree is a contiguous treap interval, so detaching or
//! moving a subtree is two splits and a merge, O(log n), without touching the subtree's nodes.
//!
//! Priorities are `splitmix64(seed, slot, gen)`, never an RNG, and the free list is a LIFO
//! stack, so two copies that apply the same operations stay bit-identical (PUB-1).

use std::collections::BTreeSet;
use std::sync::Arc;

use gorca_model::{DocNodeId, Role};

use super::cats::{
    Cat, NodeFlags, NodeMeta, NodeSpec, RANKED, RANKED_N, SubtreeSpec, cats_of, ranked_index,
};
use super::error::DocError;

pub(crate) const NIL: u32 = u32::MAX;

#[derive(Clone, Debug)]
pub(crate) struct Slot {
    gen_: u32,
    live: bool,
    /// Root of a detached (defunct) subtree awaiting re-match or reap.
    detached_root: bool,
    pub(crate) parent: u32,
    pub(crate) first: u32,
    pub(crate) last: u32,
    pub(crate) prev: u32,
    pub(crate) next: u32,
    nchild: u32,
    /// For a detached root: the root of its (intact) treap.
    dtroot: u32,
    role: Role,
    level: u8,
    flags: NodeFlags,
    cats: Cat,
    sig: u64,
    name: Option<Arc<str>>,
    text: Option<Arc<str>>,
    meta: Option<Arc<NodeMeta>>,
    own_scal: u32,
    own_nl: u32,
}

/// Treap node, hot fields only. `own` mirrors `Slot::cats` so `find_*` never leaves this array.
#[derive(Clone, Copy, Debug)]
struct TNode {
    l: u32,
    r: u32,
    up: u32,
    prio: u32,
    size: u32,
    scal: u32,
    nl: u32,
    mask: u64,
    own: u64,
}

/// Borrowed view of one node.
#[derive(Clone, Copy, Debug)]
pub struct NodeView<'a> {
    pub role: Role,
    pub level: u8,
    pub flags: NodeFlags,
    pub cats: Cat,
    pub sig: u64,
    pub name: Option<&'a str>,
    pub text: Option<&'a str>,
    pub meta: Option<&'a NodeMeta>,
}

#[derive(Clone, Debug)]
pub struct WebDoc {
    seed: u64,
    slots: Vec<Slot>,
    tn: Vec<TNode>,
    cnt: Vec<[u32; RANKED_N]>,
    free: Vec<u32>,
    root: u32,
    troot: u32,
    detached: BTreeSet<u32>,
    revision: u64,
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// The scalars a node contributes to the flat text, in order: a line break when `BREAK_BEFORE`,
/// its own text, a line break when `BREAK_AFTER`. A hidden node contributes nothing.
fn own_chars(text: Option<&str>, flags: NodeFlags) -> impl Iterator<Item = char> + '_ {
    let on = !flags.contains(NodeFlags::HIDDEN);
    let before = on && flags.contains(NodeFlags::BREAK_BEFORE);
    let after = on && flags.contains(NodeFlags::BREAK_AFTER);
    let body = if on { text.unwrap_or("") } else { "" };
    before
        .then_some('\n')
        .into_iter()
        .chain(body.chars())
        .chain(after.then_some('\n'))
}

/// Scalars and newlines of a node's contribution to the flat text.
fn contribution(text: Option<&str>, flags: NodeFlags) -> (u32, u32) {
    let mut scal = 0u32;
    let mut nl = 0u32;
    for ch in own_chars(text, flags) {
        scal += 1;
        if ch == '\n' {
            nl += 1;
        }
    }
    (scal, nl)
}

impl Default for WebDoc {
    fn default() -> Self {
        WebDoc::new(0)
    }
}

impl WebDoc {
    /// An empty document. `seed` fixes treap priorities (both publication copies use the same).
    pub fn new(seed: u64) -> Self {
        WebDoc {
            seed,
            slots: Vec::new(),
            tn: Vec::new(),
            cnt: Vec::new(),
            free: Vec::new(),
            root: NIL,
            troot: NIL,
            detached: BTreeSet::new(),
            revision: 0,
        }
    }

    // ---------------------------------------------------------------- ids and arena

    fn id_of(&self, slot: u32) -> DocNodeId {
        DocNodeId::from_parts(slot, self.slots[slot as usize].gen_)
    }

    /// The slot behind `id`, if the id is current.
    pub(crate) fn resolve(&self, id: DocNodeId) -> Result<u32, DocError> {
        match self.slots.get(id.slot() as usize) {
            Some(s) if s.live && s.gen_ == id.generation() => Ok(id.slot()),
            Some(_) | None => Err(DocError::Stale(id)),
        }
    }

    /// True while `id` names a live node (attached or detached).
    pub fn contains(&self, id: DocNodeId) -> bool {
        self.resolve(id).is_ok()
    }

    /// True when `id` is live and part of the document tree (not defunct).
    pub fn is_attached(&self, id: DocNodeId) -> bool {
        match self.resolve(id) {
            Ok(s) => self.slot_attached(s),
            Err(_) => false,
        }
    }

    fn slot_attached(&self, s: u32) -> bool {
        self.troot != NIL && self.treap_root_of(s) == self.troot
    }

    pub(crate) fn resolve_attached(&self, id: DocNodeId) -> Result<u32, DocError> {
        let s = self.resolve(id)?;
        if self.slot_attached(s) {
            Ok(s)
        } else {
            Err(DocError::NotAttached(id))
        }
    }

    /// Number of nodes in the document tree.
    pub fn len(&self) -> usize {
        self.size(self.troot) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Live nodes including defunct subtrees awaiting reap.
    pub fn live_len(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    /// Number of detached (defunct) subtree roots.
    pub fn detached_len(&self) -> usize {
        self.detached.len()
    }

    /// Bumps on every successful mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn touch(&mut self) {
        self.revision += 1;
    }

    pub fn root(&self) -> Option<DocNodeId> {
        (self.root != NIL).then(|| self.id_of(self.root))
    }

    /// Approximate heap bytes (arena arrays plus string payloads).
    pub fn heap_bytes(&self) -> usize {
        let mut b = self.slots.capacity() * std::mem::size_of::<Slot>()
            + self.tn.capacity() * std::mem::size_of::<TNode>()
            + self.cnt.capacity() * std::mem::size_of::<[u32; RANKED_N]>()
            + self.free.capacity() * 4;
        for s in &self.slots {
            b += s.text.as_ref().map_or(0, |t| t.len());
            b += s.name.as_ref().map_or(0, |t| t.len());
        }
        b
    }

    fn prio(&self, slot: u32, gen_: u32) -> u32 {
        (splitmix(self.seed ^ (u64::from(gen_) << 32 | u64::from(slot))) >> 32) as u32
    }

    fn alloc(&mut self, spec: &NodeSpec) -> Result<u32, DocError> {
        let cats = cats_of(spec.role, spec.level, spec.flags);
        let (own_scal, own_nl) = contribution(spec.text.as_deref(), spec.flags);
        let fresh = |gen_: u32| Slot {
            gen_,
            live: true,
            detached_root: false,
            parent: NIL,
            first: NIL,
            last: NIL,
            prev: NIL,
            next: NIL,
            nchild: 0,
            dtroot: NIL,
            role: spec.role,
            level: spec.level,
            flags: spec.flags,
            cats,
            sig: spec.sig,
            name: spec.name.clone(),
            text: spec.text.clone(),
            meta: spec.meta.clone(),
            own_scal,
            own_nl,
        };
        let slot = if let Some(s) = self.free.pop() {
            let gen_ = self.slots[s as usize].gen_;
            self.slots[s as usize] = fresh(gen_);
            s
        } else {
            if self.slots.len() >= (NIL - 1) as usize {
                return Err(DocError::Full);
            }
            let s = self.slots.len() as u32;
            self.slots.push(fresh(0));
            self.tn.push(TNode {
                l: NIL,
                r: NIL,
                up: NIL,
                prio: 0,
                size: 1,
                scal: 0,
                nl: 0,
                mask: 0,
                own: 0,
            });
            self.cnt.push([0; RANKED_N]);
            s
        };
        let gen_ = self.slots[slot as usize].gen_;
        let prio = self.prio(slot, gen_);
        self.tn[slot as usize] = TNode {
            l: NIL,
            r: NIL,
            up: NIL,
            prio,
            size: 1,
            scal: own_scal,
            nl: own_nl,
            mask: cats.0,
            own: cats.0,
        };
        self.cnt[slot as usize] = Self::own_counts(cats.0);
        Ok(slot)
    }

    fn own_counts(cats: u64) -> [u32; RANKED_N] {
        let mut c = [0u32; RANKED_N];
        for (i, r) in RANKED.iter().enumerate() {
            c[i] = u32::from(cats & r.0 != 0);
        }
        c
    }

    fn free_slot(&mut self, s: u32) {
        let sl = &mut self.slots[s as usize];
        sl.live = false;
        sl.detached_root = false;
        sl.gen_ = sl.gen_.wrapping_add(1);
        sl.text = None;
        sl.name = None;
        sl.meta = None;
        self.free.push(s);
    }

    // ---------------------------------------------------------------- node access

    pub fn node(&self, id: DocNodeId) -> Option<NodeView<'_>> {
        let s = self.resolve(id).ok()?;
        let sl = &self.slots[s as usize];
        Some(NodeView {
            role: sl.role,
            level: sl.level,
            flags: sl.flags,
            cats: sl.cats,
            sig: sl.sig,
            name: sl.name.as_deref(),
            text: sl.text.as_deref(),
            meta: sl.meta.as_deref(),
        })
    }

    pub fn parent(&self, id: DocNodeId) -> Option<DocNodeId> {
        let s = self.resolve(id).ok()?;
        let p = self.slots[s as usize].parent;
        (p != NIL).then(|| self.id_of(p))
    }

    pub fn first_child(&self, id: DocNodeId) -> Option<DocNodeId> {
        let s = self.resolve(id).ok()?;
        let c = self.slots[s as usize].first;
        (c != NIL).then(|| self.id_of(c))
    }

    pub fn next_sibling(&self, id: DocNodeId) -> Option<DocNodeId> {
        let s = self.resolve(id).ok()?;
        let c = self.slots[s as usize].next;
        (c != NIL).then(|| self.id_of(c))
    }

    pub fn child_count(&self, id: DocNodeId) -> Option<u32> {
        let s = self.resolve(id).ok()?;
        Some(self.slots[s as usize].nchild)
    }

    pub fn children(&self, id: DocNodeId) -> Children<'_> {
        let next = self
            .resolve(id)
            .map_or(NIL, |s| self.slots[s as usize].first);
        Children { doc: self, next }
    }

    /// Document-order iterator over the whole tree.
    pub fn preorder(&self) -> Preorder<'_> {
        Preorder {
            doc: self,
            next: self.root,
            stop: NIL,
        }
    }

    /// Document-order iterator that starts at `id` and runs to the end of the document.
    pub fn preorder_from(&self, id: DocNodeId) -> Preorder<'_> {
        match self.resolve_attached(id) {
            Ok(s) => Preorder {
                doc: self,
                next: s,
                stop: NIL,
            },
            Err(_) => Preorder {
                doc: self,
                next: NIL,
                stop: NIL,
            },
        }
    }

    /// Preorder iterator over the subtree rooted at `id` (inclusive).
    pub fn subtree(&self, id: DocNodeId) -> Preorder<'_> {
        match self.resolve(id) {
            Ok(s) => Preorder {
                doc: self,
                next: s,
                stop: s,
            },
            Err(_) => Preorder {
                doc: self,
                next: NIL,
                stop: NIL,
            },
        }
    }

    /// Defunct subtree roots, in slot order.
    pub fn detached_roots(&self) -> impl Iterator<Item = DocNodeId> + '_ {
        self.detached.iter().map(|s| self.id_of(*s))
    }

    /// First defunct subtree root whose signature equals `sig` (re-match candidates).
    pub fn find_detached(&self, sig: u64) -> Option<DocNodeId> {
        self.detached
            .iter()
            .find(|s| self.slots[**s as usize].sig == sig)
            .map(|s| self.id_of(*s))
    }

    pub(crate) fn child_slots(&self, p: u32) -> Vec<u32> {
        let mut v = Vec::with_capacity(self.slots[p as usize].nchild as usize);
        let mut c = self.slots[p as usize].first;
        while c != NIL {
            v.push(c);
            c = self.slots[c as usize].next;
        }
        v
    }

    pub(crate) fn pre_next(&self, mut x: u32) -> u32 {
        let f = self.slots[x as usize].first;
        if f != NIL {
            return f;
        }
        loop {
            let n = self.slots[x as usize].next;
            if n != NIL {
                return n;
            }
            x = self.slots[x as usize].parent;
            if x == NIL {
                return NIL;
            }
        }
    }

    fn sub_next(&self, mut x: u32, root: u32) -> u32 {
        let f = self.slots[x as usize].first;
        if f != NIL {
            return f;
        }
        while x != root {
            let n = self.slots[x as usize].next;
            if n != NIL {
                return n;
            }
            x = self.slots[x as usize].parent;
            if x == NIL {
                return NIL;
            }
        }
        NIL
    }

    fn last_desc(&self, mut x: u32) -> u32 {
        loop {
            let l = self.slots[x as usize].last;
            if l == NIL {
                return x;
            }
            x = l;
        }
    }

    pub(crate) fn depth_of(&self, mut x: u32) -> u32 {
        let mut d = 0;
        while self.slots[x as usize].parent != NIL {
            x = self.slots[x as usize].parent;
            d += 1;
        }
        d
    }

    pub(crate) fn is_ancestor_or_self(&self, a: u32, mut d: u32) -> bool {
        while d != NIL {
            if d == a {
                return true;
            }
            d = self.slots[d as usize].parent;
        }
        false
    }

    // ---------------------------------------------------------------- treap core

    fn size(&self, t: u32) -> u32 {
        if t == NIL {
            0
        } else {
            self.tn[t as usize].size
        }
    }

    fn scal(&self, t: u32) -> u32 {
        if t == NIL {
            0
        } else {
            self.tn[t as usize].scal
        }
    }

    fn nlines(&self, t: u32) -> u32 {
        if t == NIL { 0 } else { self.tn[t as usize].nl }
    }

    fn mask(&self, t: u32) -> u64 {
        if t == NIL {
            0
        } else {
            self.tn[t as usize].mask
        }
    }

    fn cnt_at(&self, t: u32, i: usize) -> u32 {
        if t == NIL { 0 } else { self.cnt[t as usize][i] }
    }

    fn pull(&mut self, x: u32) {
        let xi = x as usize;
        let (l, r) = (self.tn[xi].l, self.tn[xi].r);
        let (own_scal, own_nl) = (self.slots[xi].own_scal, self.slots[xi].own_nl);
        let size = 1 + self.size(l) + self.size(r);
        let scal = own_scal + self.scal(l) + self.scal(r);
        let nl = own_nl + self.nlines(l) + self.nlines(r);
        let mask = self.tn[xi].own | self.mask(l) | self.mask(r);
        let mut c = Self::own_counts(self.tn[xi].own);
        if l != NIL {
            let lc = &self.cnt[l as usize];
            for i in 0..RANKED_N {
                c[i] += lc[i];
            }
        }
        if r != NIL {
            let rc = &self.cnt[r as usize];
            for i in 0..RANKED_N {
                c[i] += rc[i];
            }
        }
        self.cnt[xi] = c;
        let n = &mut self.tn[xi];
        n.size = size;
        n.scal = scal;
        n.nl = nl;
        n.mask = mask;
    }

    fn pull_path(&mut self, mut x: u32) {
        while x != NIL {
            self.pull(x);
            x = self.tn[x as usize].up;
        }
    }

    fn merge(&mut self, a: u32, b: u32) -> u32 {
        if a == NIL {
            return b;
        }
        if b == NIL {
            return a;
        }
        if self.tn[a as usize].prio > self.tn[b as usize].prio {
            let ar = self.tn[a as usize].r;
            let m = self.merge(ar, b);
            self.tn[a as usize].r = m;
            self.tn[m as usize].up = a;
            self.pull(a);
            a
        } else {
            let bl = self.tn[b as usize].l;
            let m = self.merge(a, bl);
            self.tn[b as usize].l = m;
            self.tn[m as usize].up = b;
            self.pull(b);
            b
        }
    }

    fn merge_root(&mut self, a: u32, b: u32) -> u32 {
        let r = self.merge(a, b);
        if r != NIL {
            self.tn[r as usize].up = NIL;
        }
        r
    }

    /// Split into (first `k` nodes, rest).
    fn split(&mut self, t: u32, k: u32) -> (u32, u32) {
        if t == NIL {
            return (NIL, NIL);
        }
        let l = self.tn[t as usize].l;
        let ls = self.size(l);
        if k <= ls {
            let (a, b) = self.split(l, k);
            self.tn[t as usize].l = b;
            if b != NIL {
                self.tn[b as usize].up = t;
            }
            self.pull(t);
            (a, t)
        } else {
            let r = self.tn[t as usize].r;
            let (a, b) = self.split(r, k - ls - 1);
            self.tn[t as usize].r = a;
            if a != NIL {
                self.tn[a as usize].up = t;
            }
            self.pull(t);
            (t, b)
        }
    }

    fn split_root(&mut self, t: u32, k: u32) -> (u32, u32) {
        let (a, b) = self.split(t, k);
        if a != NIL {
            self.tn[a as usize].up = NIL;
        }
        if b != NIL {
            self.tn[b as usize].up = NIL;
        }
        (a, b)
    }

    fn treap_root_of(&self, mut x: u32) -> u32 {
        loop {
            let u = self.tn[x as usize].up;
            if u == NIL {
                return x;
            }
            x = u;
        }
    }

    /// 0-based position of `x` in its treap.
    fn rank_of(&self, x: u32) -> u32 {
        let mut r = self.size(self.tn[x as usize].l);
        let mut cur = x;
        loop {
            let p = self.tn[cur as usize].up;
            if p == NIL {
                return r;
            }
            if self.tn[p as usize].r == cur {
                r += self.size(self.tn[p as usize].l) + 1;
            }
            cur = p;
        }
    }

    /// (scalars, newlines) strictly before `x`'s own contribution.
    fn prefix_text(&self, x: u32) -> (u32, u32) {
        let l = self.tn[x as usize].l;
        let mut s = self.scal(l);
        let mut n = self.nlines(l);
        let mut cur = x;
        loop {
            let p = self.tn[cur as usize].up;
            if p == NIL {
                return (s, n);
            }
            if self.tn[p as usize].r == cur {
                let pl = self.tn[p as usize].l;
                s += self.scal(pl) + self.slots[p as usize].own_scal;
                n += self.nlines(pl) + self.slots[p as usize].own_nl;
            }
            cur = p;
        }
    }

    /// Build a treap from `nodes` (document order) in O(k); returns its root.
    fn build_treap(&mut self, nodes: &[u32]) -> u32 {
        let mut stack: Vec<u32> = Vec::new();
        for &x in nodes {
            let xi = x as usize;
            self.tn[xi].l = NIL;
            self.tn[xi].r = NIL;
            self.tn[xi].up = NIL;
            let mut last = NIL;
            while let Some(&top) = stack.last() {
                if self.tn[top as usize].prio < self.tn[xi].prio {
                    last = top;
                    stack.pop();
                } else {
                    break;
                }
            }
            self.tn[xi].l = last;
            if last != NIL {
                self.tn[last as usize].up = x;
            }
            if let Some(&top) = stack.last() {
                self.tn[top as usize].r = x;
                self.tn[xi].up = top;
            }
            stack.push(x);
        }
        let Some(&root) = stack.first() else {
            return NIL;
        };
        // Pull children before parents: DFS order reversed.
        let mut order: Vec<u32> = Vec::with_capacity(nodes.len());
        let mut dfs = vec![root];
        while let Some(t) = dfs.pop() {
            order.push(t);
            let (l, r) = (self.tn[t as usize].l, self.tn[t as usize].r);
            if l != NIL {
                dfs.push(l);
            }
            if r != NIL {
                dfs.push(r);
            }
        }
        for &t in order.iter().rev() {
            self.pull(t);
        }
        root
    }

    // ---------------------------------------------------------------- tree link primitives

    fn link_child(&mut self, p: u32, c: u32, before: u32) {
        let (prev, next) = if before == NIL {
            (self.slots[p as usize].last, NIL)
        } else {
            (self.slots[before as usize].prev, before)
        };
        {
            let cs = &mut self.slots[c as usize];
            cs.parent = p;
            cs.prev = prev;
            cs.next = next;
        }
        if prev == NIL {
            self.slots[p as usize].first = c;
        } else {
            self.slots[prev as usize].next = c;
        }
        if next == NIL {
            self.slots[p as usize].last = c;
        } else {
            self.slots[next as usize].prev = c;
        }
        self.slots[p as usize].nchild += 1;
    }

    fn unlink_child(&mut self, c: u32) {
        let (p, prev, next) = {
            let cs = &self.slots[c as usize];
            (cs.parent, cs.prev, cs.next)
        };
        if prev == NIL {
            self.slots[p as usize].first = next;
        } else {
            self.slots[prev as usize].next = next;
        }
        if next == NIL {
            self.slots[p as usize].last = prev;
        } else {
            self.slots[next as usize].prev = prev;
        }
        self.slots[p as usize].nchild -= 1;
        let cs = &mut self.slots[c as usize];
        cs.parent = NIL;
        cs.prev = NIL;
        cs.next = NIL;
    }

    /// Treap position where a new child of `p` placed before `before` (or last) starts.
    fn insert_pos(&self, p: u32, before: u32) -> u32 {
        if before == NIL {
            self.rank_of(self.last_desc(p)) + 1
        } else {
            self.rank_of(before)
        }
    }

    // ---------------------------------------------------------------- structural primitives

    /// Create nodes for `spec` (preorder), linked among themselves. Returns slots in preorder.
    fn create_nodes(&mut self, spec: &SubtreeSpec) -> Result<Vec<u32>, DocError> {
        let mut out: Vec<u32> = Vec::new();
        let mut stack: Vec<(&SubtreeSpec, u32)> = vec![(spec, NIL)];
        while let Some((s, parent)) = stack.pop() {
            let slot = match self.alloc(&s.node) {
                Ok(slot) => slot,
                Err(e) => {
                    // Roll back: free what was created (reverse order keeps the LIFO stack
                    // deterministic on both copies).
                    for &c in out.iter().rev() {
                        self.free_slot(c);
                    }
                    return Err(e);
                }
            };
            if parent != NIL {
                self.link_child(parent, slot, NIL);
            }
            out.push(slot);
            for c in s.children.iter().rev() {
                stack.push((c, slot));
            }
        }
        Ok(out)
    }

    /// Insert `spec` as a child of `p` (NIL: as the document root) before `before` (NIL: last).
    pub(crate) fn insert_spec(
        &mut self,
        p: u32,
        before: u32,
        spec: &SubtreeSpec,
    ) -> Result<Vec<u32>, DocError> {
        let pos = if p == NIL {
            0
        } else {
            self.insert_pos(p, before)
        };
        let nodes = self.create_nodes(spec)?;
        let top = nodes[0];
        if p != NIL {
            self.link_child(p, top, before);
        }
        let t = self.build_treap(&nodes);
        let (a, b) = self.split_root(self.troot, pos);
        let m = self.merge_root(a, t);
        self.troot = self.merge_root(m, b);
        if p == NIL {
            self.root = top;
        }
        Ok(nodes)
    }

    pub(crate) fn root_slot(&self) -> u32 {
        self.root
    }

    pub(crate) fn tree_root_exists(&self) -> bool {
        self.root != NIL
    }

    /// Detach `r` and its subtree from the document: tree unlink plus treap interval cut.
    /// Leaves the subtree's own treap intact in `dtroot`.
    pub(crate) fn detach_core(&mut self, r: u32) {
        let start = self.rank_of(r);
        let end = self.rank_of(self.last_desc(r)) + 1;
        let (a, rest) = self.split_root(self.troot, start);
        let (mid, c) = self.split_root(rest, end - start);
        self.troot = self.merge_root(a, c);
        self.unlink_child(r);
        self.slots[r as usize].dtroot = mid;
    }

    /// Re-attach a subtree cut by `detach_core` under `p` before `before`.
    pub(crate) fn attach_core(&mut self, r: u32, p: u32, before: u32) {
        let mid = self.slots[r as usize].dtroot;
        self.slots[r as usize].dtroot = NIL;
        let pos = self.insert_pos(p, before);
        self.link_child(p, r, before);
        let (a, b) = self.split_root(self.troot, pos);
        let m = self.merge_root(a, mid);
        self.troot = self.merge_root(m, b);
    }

    pub(crate) fn mark_detached(&mut self, r: u32) {
        self.slots[r as usize].detached_root = true;
        self.detached.insert(r);
    }

    pub(crate) fn unmark_detached(&mut self, r: u32) {
        self.slots[r as usize].detached_root = false;
        self.detached.remove(&r);
    }

    pub(crate) fn is_detached_root(&self, s: u32) -> bool {
        self.slots[s as usize].detached_root
    }

    pub(crate) fn parent_slot(&self, s: u32) -> u32 {
        self.slots[s as usize].parent
    }

    pub(crate) fn slot_id(&self, s: u32) -> DocNodeId {
        self.id_of(s)
    }

    /// Free a detached subtree (preorder, so the free list is deterministic).
    pub(crate) fn reap_slot(&mut self, r: u32) {
        let mut nodes = Vec::new();
        let mut x = r;
        while x != NIL {
            nodes.push(x);
            x = self.sub_next(x, r);
        }
        self.unmark_detached(r);
        for s in nodes {
            self.free_slot(s);
        }
    }

    pub(crate) fn detached_slots(&self) -> Vec<u32> {
        self.detached.iter().copied().collect()
    }

    pub(crate) fn set_text_slot(&mut self, s: u32, text: Option<Arc<str>>) {
        let flags = self.slots[s as usize].flags;
        let (scal, nl) = contribution(text.as_deref(), flags);
        let sl = &mut self.slots[s as usize];
        sl.text = text;
        sl.own_scal = scal;
        sl.own_nl = nl;
        self.pull_path(s);
    }

    pub(crate) fn patch_slot(&mut self, s: u32, patch: &super::cats::PropsPatch) {
        let sl = &mut self.slots[s as usize];
        if let Some(r) = patch.role {
            sl.role = r;
        }
        if let Some(l) = patch.level {
            sl.level = l;
        }
        if let Some(f) = patch.flags {
            sl.flags = f;
        }
        if let Some(g) = patch.sig {
            sl.sig = g;
        }
        if let Some(n) = &patch.name {
            sl.name.clone_from(n);
        }
        if let Some(m) = &patch.meta {
            sl.meta.clone_from(m);
        }
        let cats = cats_of(sl.role, sl.level, sl.flags);
        let (scal, nl) = contribution(sl.text.as_deref(), sl.flags);
        sl.cats = cats;
        sl.own_scal = scal;
        sl.own_nl = nl;
        self.tn[s as usize].own = cats.0;
        self.pull_path(s);
    }

    // ---------------------------------------------------------------- navigation queries

    /// Position of `id` in document order (0-based), if attached.
    pub fn position(&self, id: DocNodeId) -> Option<u32> {
        let s = self.resolve_attached(id).ok()?;
        Some(self.rank_of(s))
    }

    /// The node at document-order position `pos`.
    pub fn at_position(&self, mut pos: u32) -> Option<DocNodeId> {
        let mut t = self.troot;
        while t != NIL {
            let ls = self.size(self.tn[t as usize].l);
            match pos.cmp(&ls) {
                std::cmp::Ordering::Less => t = self.tn[t as usize].l,
                std::cmp::Ordering::Equal => return Some(self.id_of(t)),
                std::cmp::Ordering::Greater => {
                    pos -= ls + 1;
                    t = self.tn[t as usize].r;
                }
            }
        }
        None
    }

    fn find_next(&self, t: u32, off: u32, r: u32, c: u64) -> Option<(u32, u32)> {
        if t == NIL {
            return None;
        }
        let n = &self.tn[t as usize];
        if n.mask & c == 0 || off + n.size <= r {
            return None;
        }
        let ls = self.size(n.l);
        if off + ls > r
            && let Some(x) = self.find_next(n.l, off, r, c)
        {
            return Some(x);
        }
        let pos = off + ls;
        if pos >= r && n.own & c != 0 {
            return Some((t, pos));
        }
        self.find_next(n.r, pos + 1, r, c)
    }

    fn find_prev(&self, t: u32, off: u32, r: u32, c: u64) -> Option<(u32, u32)> {
        if t == NIL {
            return None;
        }
        let n = &self.tn[t as usize];
        if n.mask & c == 0 || off >= r {
            return None;
        }
        let pos = off + self.size(n.l);
        if let Some(x) = self.find_prev(n.r, pos + 1, r, c) {
            return Some(x);
        }
        if pos < r && n.own & c != 0 {
            return Some((t, pos));
        }
        self.find_prev(n.l, off, r, c)
    }

    /// First node strictly after `from` in document order whose categories intersect `c`.
    /// O(log n). `None` when there is none or `from` is not attached.
    pub fn next(&self, from: DocNodeId, c: Cat) -> Option<DocNodeId> {
        let s = self.resolve_attached(from).ok()?;
        let r = self.rank_of(s) + 1;
        self.find_next(self.troot, 0, r, c.0)
            .map(|(t, _)| self.id_of(t))
    }

    /// Last node strictly before `from` whose categories intersect `c`.
    pub fn prev(&self, from: DocNodeId, c: Cat) -> Option<DocNodeId> {
        let s = self.resolve_attached(from).ok()?;
        let r = self.rank_of(s);
        self.find_prev(self.troot, 0, r, c.0)
            .map(|(t, _)| self.id_of(t))
    }

    /// First node in the document matching `c` (the start of a wrap-around).
    pub fn first_of(&self, c: Cat) -> Option<DocNodeId> {
        self.find_next(self.troot, 0, 0, c.0)
            .map(|(t, _)| self.id_of(t))
    }

    /// Last node in the document matching `c`.
    pub fn last_of(&self, c: Cat) -> Option<DocNodeId> {
        self.find_prev(self.troot, 0, u32::MAX, c.0)
            .map(|(t, _)| self.id_of(t))
    }

    /// Document-order interval `[start, end)` of the subtree of `scope`.
    pub fn subtree_range(&self, scope: DocNodeId) -> Option<(u32, u32)> {
        let s = self.resolve_attached(scope).ok()?;
        Some((self.rank_of(s), self.rank_of(self.last_desc(s)) + 1))
    }

    /// Like `next`, restricted to the subtree of `scope` (SN-2: an interval check).
    pub fn next_in(&self, from: DocNodeId, c: Cat, scope: DocNodeId) -> Option<DocNodeId> {
        let s = self.resolve_attached(from).ok()?;
        let (lo, hi) = self.subtree_range(scope)?;
        let r = self.rank_of(s) + 1;
        let (t, pos) = self.find_next(self.troot, 0, r.max(lo), c.0)?;
        (pos < hi).then(|| self.id_of(t))
    }

    /// Like `prev`, restricted to the subtree of `scope`.
    pub fn prev_in(&self, from: DocNodeId, c: Cat, scope: DocNodeId) -> Option<DocNodeId> {
        let s = self.resolve_attached(from).ok()?;
        let (lo, hi) = self.subtree_range(scope)?;
        let r = self.rank_of(s).min(hi);
        let (t, pos) = self.find_prev(self.troot, 0, r, c.0)?;
        (pos >= lo).then(|| self.id_of(t))
    }

    /// Nearest ancestor (not `id` itself) that is a landmark or region.
    pub fn landmark_of(&self, id: DocNodeId) -> Option<DocNodeId> {
        let mut x = self.slots[self.resolve_attached(id).ok()? as usize].parent;
        while x != NIL {
            if self.slots[x as usize].cats.intersects(Cat::ANY_LANDMARK) {
                return Some(self.id_of(x));
            }
            x = self.slots[x as usize].parent;
        }
        None
    }

    /// Per-landmark subsequence: next `c` after `from` inside `from`'s nearest landmark
    /// (the whole document when `from` is outside every landmark).
    pub fn next_in_landmark(&self, from: DocNodeId, c: Cat) -> Option<DocNodeId> {
        match self.landmark_of(from) {
            Some(l) => self.next_in(from, c, l),
            None => self.next(from, c),
        }
    }

    /// Per-landmark subsequence, backwards.
    pub fn prev_in_landmark(&self, from: DocNodeId, c: Cat) -> Option<DocNodeId> {
        match self.landmark_of(from) {
            Some(l) => self.prev_in(from, c, l),
            None => self.prev(from, c),
        }
    }

    /// Number of nodes matching a ranked single-bit category.
    pub fn count(&self, c: Cat) -> Option<u32> {
        let i = ranked_index(c)?;
        Some(self.cnt_at(self.troot, i))
    }

    /// k-th (0-based) node of a ranked category.
    pub fn kth(&self, c: Cat, mut k: u32) -> Option<DocNodeId> {
        let i = ranked_index(c)?;
        let mut t = self.troot;
        while t != NIL {
            let l = self.tn[t as usize].l;
            let lc = self.cnt_at(l, i);
            if k < lc {
                t = l;
                continue;
            }
            k -= lc;
            if self.tn[t as usize].own & c.0 != 0 {
                if k == 0 {
                    return Some(self.id_of(t));
                }
                k -= 1;
            }
            t = self.tn[t as usize].r;
        }
        None
    }

    /// 0-based index of `id` among nodes of ranked category `c` ("k of N"); `None` if `id`
    /// does not have category `c`.
    pub fn rank(&self, id: DocNodeId, c: Cat) -> Option<u32> {
        let i = ranked_index(c)?;
        let s = self.resolve_attached(id).ok()?;
        if self.tn[s as usize].own & c.0 == 0 {
            return None;
        }
        let mut r = self.cnt_at(self.tn[s as usize].l, i);
        let mut cur = s;
        loop {
            let p = self.tn[cur as usize].up;
            if p == NIL {
                return Some(r);
            }
            if self.tn[p as usize].r == cur {
                r += self.cnt_at(self.tn[p as usize].l, i)
                    + u32::from(self.tn[p as usize].own & c.0 != 0);
            }
            cur = p;
        }
    }

    /// Document order of two attached nodes.
    pub fn cmp_order(&self, a: DocNodeId, b: DocNodeId) -> Option<std::cmp::Ordering> {
        Some(self.position(a)?.cmp(&self.position(b)?))
    }

    // ---------------------------------------------------------------- flat text

    /// Total length of the flat text in scalars.
    pub fn text_len(&self) -> u32 {
        self.scal(self.troot)
    }

    /// Number of lines (hard breaks + 1).
    pub fn line_count(&self) -> u32 {
        self.nlines(self.troot) + 1
    }

    /// Flat-text offset where `id`'s own contribution starts.
    pub fn offset_of(&self, id: DocNodeId) -> Option<u32> {
        let s = self.resolve_attached(id).ok()?;
        Some(self.prefix_text(s).0)
    }

    /// The node whose contribution contains flat-text `offset`, and the offset inside it.
    /// `None` at or past the end of the text.
    pub fn at_offset(&self, mut offset: u32) -> Option<(DocNodeId, u32)> {
        let mut t = self.troot;
        while t != NIL {
            let l = self.tn[t as usize].l;
            let ls = self.scal(l);
            if offset < ls {
                t = l;
                continue;
            }
            offset -= ls;
            let own = self.slots[t as usize].own_scal;
            if offset < own {
                return Some((self.id_of(t), offset));
            }
            offset -= own;
            t = self.tn[t as usize].r;
        }
        None
    }

    /// Flat-text offset where line `k` (0-based) starts.
    pub fn line_start(&self, k: u32) -> Option<u32> {
        if k == 0 {
            return Some(0);
        }
        let mut k = k;
        let mut t = self.troot;
        let mut base = 0u32;
        while t != NIL {
            let l = self.tn[t as usize].l;
            let lnl = self.nlines(l);
            if k <= lnl {
                t = l;
                continue;
            }
            k -= lnl;
            base += self.scal(l);
            let own_nl = self.slots[t as usize].own_nl;
            if k <= own_nl {
                let idx = self.nth_newline_in(t, k)?;
                return Some(base + idx + 1);
            }
            k -= own_nl;
            base += self.slots[t as usize].own_scal;
            t = self.tn[t as usize].r;
        }
        None
    }

    /// Index (in scalars) of the k-th (1-based) newline of slot `t`'s contribution.
    fn nth_newline_in(&self, t: u32, mut k: u32) -> Option<u32> {
        let sl = &self.slots[t as usize];
        for (idx, ch) in own_chars(sl.text.as_deref(), sl.flags).enumerate() {
            if ch == '\n' {
                k -= 1;
                if k == 0 {
                    return u32::try_from(idx).ok();
                }
            }
        }
        None
    }

    /// Line index containing flat-text `offset` (`offset` may equal the text length).
    pub fn line_of(&self, offset: u32) -> u32 {
        match self.at_offset(offset) {
            None => self.nlines(self.troot),
            Some((id, local)) => {
                let Ok(s) = self.resolve(id) else {
                    return 0;
                };
                let (_, nl_before) = self.prefix_text(s);
                let sl = &self.slots[s as usize];
                let n = own_chars(sl.text.as_deref(), sl.flags)
                    .take(local as usize)
                    .filter(|ch| *ch == '\n')
                    .count() as u32;
                nl_before + n
            }
        }
    }

    /// `[start, end)` of the line containing `offset`; `end` excludes the line break.
    pub fn line_range(&self, offset: u32) -> (u32, u32) {
        let total = self.text_len();
        let offset = offset.min(total);
        let k = self.line_of(offset);
        let start = self.line_start(k).unwrap_or(0);
        let end = match self.line_start(k + 1) {
            Some(next) => next.saturating_sub(1),
            None => total,
        };
        (start, end.max(start))
    }

    /// Append the flat text in `[start, end)` to `out`.
    pub fn read_text(&self, start: u32, end: u32, out: &mut String) {
        let end = end.min(self.text_len());
        if start >= end {
            return;
        }
        let Some((id, mut local)) = self.at_offset(start) else {
            return;
        };
        let Ok(mut s) = self.resolve(id) else {
            return;
        };
        let mut remaining = end - start;
        while remaining > 0 && s != NIL {
            let sl = &self.slots[s as usize];
            let own = sl.own_scal;
            if own > local {
                let take = (own - local).min(remaining);
                out.extend(
                    own_chars(sl.text.as_deref(), sl.flags)
                        .skip(local as usize)
                        .take(take as usize),
                );
                remaining -= take;
            }
            local = 0;
            s = self.pre_next(s);
        }
    }

    /// Flat-text interval `[start, end)` that `id`'s subtree contributes (its own text and every
    /// descendant's). O(log n). `None` when `id` is not in the document tree.
    pub fn subtree_text_range(&self, id: DocNodeId) -> Option<(u32, u32)> {
        let s = self.resolve_attached(id).ok()?;
        let last = self.last_desc(s);
        let start = self.prefix_text(s).0;
        let end = self.prefix_text(last).0 + self.slots[last as usize].own_scal;
        Some((start, end))
    }

    /// Length in scalars of the node's own contribution (text plus breaks).
    pub fn own_len(&self, id: DocNodeId) -> Option<u32> {
        let s = self.resolve(id).ok()?;
        Some(self.slots[s as usize].own_scal)
    }

    // ---------------------------------------------------------------- invariants

    /// Check every structural invariant (DM-I1..I3); used by tests and debug assertions.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen_live = 0usize;
        if self.root == NIL {
            if self.troot != NIL {
                return Err("treap root without tree root".into());
            }
        } else {
            seen_live += self.validate_tree(self.root, self.troot, true)?;
        }
        for &d in &self.detached {
            let sl = &self.slots[d as usize];
            if !sl.live || !sl.detached_root || sl.parent != NIL {
                return Err(format!("bad detached root {d}"));
            }
            seen_live += self.validate_tree(d, sl.dtroot, false)?;
        }
        if seen_live != self.live_len() {
            return Err(format!(
                "live count mismatch: reachable {seen_live}, arena {}",
                self.live_len()
            ));
        }
        for &f in &self.free {
            if self.slots[f as usize].live {
                return Err(format!("free list holds live slot {f}"));
            }
        }
        Ok(())
    }

    fn validate_tree(&self, root: u32, troot: u32, main: bool) -> Result<usize, String> {
        if troot == NIL || self.tn[troot as usize].up != NIL {
            return Err("treap root missing or has a parent".into());
        }
        // Preorder via tree links.
        let mut pre = Vec::new();
        let mut x = root;
        while x != NIL {
            pre.push(x);
            x = self.sub_next(x, root);
        }
        // In-order via treap.
        let mut ino = Vec::with_capacity(pre.len());
        let mut stack: Vec<u32> = Vec::new();
        let mut cur = troot;
        while cur != NIL || !stack.is_empty() {
            while cur != NIL {
                stack.push(cur);
                cur = self.tn[cur as usize].l;
            }
            let Some(t) = stack.pop() else { break };
            ino.push(t);
            cur = self.tn[t as usize].r;
        }
        if pre != ino {
            return Err(format!(
                "treap order differs from preorder (main={main}, tree {} vs treap {})",
                pre.len(),
                ino.len()
            ));
        }
        for &t in &ino {
            let n = &self.tn[t as usize];
            let sl = &self.slots[t as usize];
            if !sl.live {
                return Err(format!("dead slot {t} in tree"));
            }
            for ch in [n.l, n.r] {
                if ch != NIL {
                    if self.tn[ch as usize].up != t {
                        return Err(format!("bad up pointer at {ch}"));
                    }
                    if self.tn[ch as usize].prio > n.prio {
                        return Err(format!("heap order violated at {t}"));
                    }
                }
            }
            let mut size = 1;
            let mut scal = sl.own_scal;
            let mut nl = sl.own_nl;
            let mut mask = n.own;
            let mut c = Self::own_counts(n.own);
            for ch in [n.l, n.r] {
                if ch != NIL {
                    let cn = &self.tn[ch as usize];
                    size += cn.size;
                    scal += cn.scal;
                    nl += cn.nl;
                    mask |= cn.mask;
                    for (acc, v) in c.iter_mut().zip(&self.cnt[ch as usize]) {
                        *acc += *v;
                    }
                }
            }
            if size != n.size
                || scal != n.scal
                || nl != n.nl
                || mask != n.mask
                || c != self.cnt[t as usize]
            {
                return Err(format!("stale summary at {t}"));
            }
            if n.own != sl.cats.0 {
                return Err(format!("own mask differs from cats at {t}"));
            }
            let (es, en) = contribution(sl.text.as_deref(), sl.flags);
            if es != sl.own_scal || en != sl.own_nl {
                return Err(format!("stale own text counts at {t}"));
            }
            // Tree links.
            let mut kids = 0;
            let mut ch = sl.first;
            let mut prev = NIL;
            while ch != NIL {
                let cs = &self.slots[ch as usize];
                if cs.parent != t || cs.prev != prev {
                    return Err(format!("bad sibling links at {ch}"));
                }
                prev = ch;
                ch = cs.next;
                kids += 1;
            }
            if prev != sl.last || kids != sl.nchild {
                return Err(format!("bad child list at {t}"));
            }
        }
        if main && self.tn[troot as usize].size as usize != pre.len() {
            return Err("main treap size mismatch".into());
        }
        Ok(pre.len())
    }
}

/// Iterator over a node's children.
#[derive(Debug)]
pub struct Children<'a> {
    doc: &'a WebDoc,
    next: u32,
}

impl Iterator for Children<'_> {
    type Item = DocNodeId;
    fn next(&mut self) -> Option<DocNodeId> {
        if self.next == NIL {
            return None;
        }
        let s = self.next;
        self.next = self.doc.slots[s as usize].next;
        Some(self.doc.id_of(s))
    }
}

/// Document-order iterator, optionally bounded to a subtree.
#[derive(Debug)]
pub struct Preorder<'a> {
    doc: &'a WebDoc,
    next: u32,
    stop: u32,
}

impl Iterator for Preorder<'_> {
    type Item = DocNodeId;
    fn next(&mut self) -> Option<DocNodeId> {
        if self.next == NIL {
            return None;
        }
        let s = self.next;
        self.next = if self.stop == NIL {
            self.doc.pre_next(s)
        } else {
            self.doc.sub_next(s, self.stop)
        };
        Some(self.doc.id_of(s))
    }
}
