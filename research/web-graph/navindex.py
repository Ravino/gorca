"""Structural navigation indexes over a mutable accessibility-tree mirror.

Reference implementations (correctness first; the performance numbers that matter
come from the Rust port in rust-bench/):

  Mirror          arena of nodes: parent, ordered children, category mask.
  OMIndex         order-maintenance labels (Bender et al. 2002 list labelling,
                  u62 universe, T = 1.5) + one sorted label array per category.
  TreapIndex      implicit treap over the document-order (preorder) sequence,
                  augmented with subtree size, OR of category masks and per-category
                  counts. Subtrees of the DOM are contiguous ranges in preorder, so
                  "insert/delete/move a DOM subtree" is split/merge.
  BruteIndex      oracle: recompute preorder on every query.

All indexes answer, for category c and node v:
  next(c, v)  first node strictly after v in document order with category c
  prev(c, v)  last node strictly before v ...
  kth(c, k)   k-th (0-based) node of category c in document order
  count(c)
and are notified by the Mirror of: subtree inserted (preorder list + predecessor),
subtree removed (preorder list), node mask changed.
"""
from __future__ import annotations

import bisect
import random
import sys

from gen import CATS, Tree

sys.setrecursionlimit(10000)
NC = len(CATS)


# ---------------------------------------------------------------------------
class Mirror:
    """Arena mirror. Node ids are slots; removed slots go to a free list.

    In Rust this is a `slotmap::SlotMap<NodeIdx, Node>` (generational keys) with
    intrusive first_child/next_sibling links; here plain lists.
    """

    def __init__(self, t: Tree, indexes: list):
        n = len(t)
        self.parent = list(t.parent)
        self.children = [list(c) for c in t.children]
        self.mask = [t.mask(v) for v in range(n)]
        self.alive = [True] * n
        self.free: list[int] = []
        self.indexes = indexes
        order = t.preorder()
        for ix in indexes:
            ix.build(self, order)

    # -- structure helpers ---------------------------------------------------
    def last_descendant(self, v: int) -> int:
        while self.children[v]:
            v = self.children[v][-1]
        return v

    def subtree_preorder(self, v: int) -> list:
        out, stack = [], [v]
        while stack:
            x = stack.pop()
            out.append(x)
            stack.extend(reversed(self.children[x]))
        return out

    def preorder(self) -> list:
        return self.subtree_preorder(0)

    def _alloc(self) -> int:
        if self.free:
            v = self.free.pop()
            self.alive[v] = True
            self.children[v] = []
            return v
        self.parent.append(-1)
        self.children.append([])
        self.mask.append(0)
        self.alive.append(True)
        return len(self.parent) - 1

    # -- mutations -----------------------------------------------------------
    def insert_subtree(self, p: int, idx: int, sub: Tree) -> int:
        """Insert `sub` (rooted at its node 0) as child number `idx` of `p`."""
        pred = p if idx == 0 else self.last_descendant(self.children[p][idx - 1])
        ids = [self._alloc() for _ in range(len(sub))]
        for local, v in enumerate(ids):
            lp = sub.parent[local]
            self.parent[v] = p if lp < 0 else ids[lp]
            self.children[v] = [ids[c] for c in sub.children[local]]
            self.mask[v] = sub.mask(local)
        self.children[p].insert(idx, ids[0])
        new_order = [ids[x] for x in sub.preorder()]
        for ix in self.indexes:
            ix.on_insert(pred, new_order)
        return ids[0]

    def delete_subtree(self, v: int) -> None:
        assert v != 0
        order = self.subtree_preorder(v)
        for ix in self.indexes:
            ix.on_delete(order)
        p = self.parent[v]
        self.children[p].remove(v)
        for x in order:
            self.alive[x] = False
            self.children[x] = []
            self.free.append(x)

    def set_mask(self, v: int, m: int) -> None:
        old = self.mask[v]
        self.mask[v] = m
        for ix in self.indexes:
            ix.on_mask(v, old, m)


# ---------------------------------------------------------------------------
class BruteIndex:
    def build(self, m: Mirror, order: list) -> None:
        self.m = m

    def on_insert(self, pred, order):
        pass

    def on_delete(self, order):
        pass

    def on_mask(self, v, old, new):
        pass

    def _list(self, c: int) -> list:
        return [v for v in self.m.preorder() if self.m.mask[v] >> c & 1]

    def next(self, c, v):
        order = self.m.preorder()
        i = order.index(v)
        for x in order[i + 1:]:
            if self.m.mask[x] >> c & 1:
                return x
        return None

    def prev(self, c, v):
        order = self.m.preorder()
        i = order.index(v)
        for x in reversed(order[:i]):
            if self.m.mask[x] >> c & 1:
                return x
        return None

    def kth(self, c, k):
        lst = self._list(c)
        return lst[k] if 0 <= k < len(lst) else None

    def count(self, c):
        return len(self._list(c))


# ---------------------------------------------------------------------------
class OMIndex:
    """Order-maintenance labels + per-category sorted label arrays.

    Labelling: Bender, Cole, Demaine, Farach-Colton, Zito, "Two simplified
    algorithms for maintaining order in a list" (ESA 2002). Universe 2^U, U = 62.
    On overflow, the smallest aligned label block of size 2^i around the insertion
    point whose density after insertion is below T^-i is relabelled evenly.
    Amortised O(log n) relabels per insert; compare is O(1) (integer compare).
    """

    U = 62
    T = 1.5

    def build(self, m: Mirror, order: list) -> None:
        self.m = m
        n = len(order)
        gap = (1 << self.U) // (n + 1)
        self.label = {}
        self.order = []              # sorted global labels
        self.node_at = {}            # label -> node
        self.cat = [[] for _ in range(NC)]
        for i, v in enumerate(order):
            lab = (i + 1) * gap
            self.label[v] = lab
            self.order.append(lab)
            self.node_at[lab] = v
            mk = m.mask[v]
            while mk:
                c = (mk & -mk).bit_length() - 1
                self.cat[c].append(lab)
                mk &= mk - 1
        self.relabels = 0

    # -- labelling -------------------------------------------------------------
    def _make_room(self, lp: int, k: int) -> None:
        """Relabel the smallest aligned block around lp whose density after inserting
        k items is below T^-i; category arrays are rewritten in place (order is kept)."""
        i = 1
        while True:
            size = 1 << i
            lo = lp & ~(size - 1)
            hi = lo + size
            a = bisect.bisect_left(self.order, lo)
            b = bisect.bisect_left(self.order, hi)
            cnt = b - a + k
            if cnt * (self.T ** i) < size and size // (cnt + 1) >= 2:
                break
            i += 1
            assert i <= self.U, "label universe exhausted"
        labs = self.order[a:b]
        pos = bisect.bisect_right(labs, lp)
        step = size // (cnt + 1)
        newlabs = [lo + ((r if r < pos else r + k) + 1) * step for r in range(len(labs))]
        # category arrays: each category's entries inside [lo, hi) are a contiguous run;
        # rewrite the run with mapped labels (order preserved).
        remap = dict(zip(labs, newlabs))
        for lst in self.cat:
            ca = bisect.bisect_left(lst, lo)
            cb = bisect.bisect_left(lst, hi)
            for t in range(ca, cb):
                lst[t] = remap[lst[t]]
        nodes = [self.node_at.pop(x) for x in labs]
        for v, nl in zip(nodes, newlabs):
            self.label[v] = nl
            self.node_at[nl] = v
        self.order[a:b] = newlabs
        self.relabels += len(labs)

    def on_insert(self, pred: int, order: list) -> None:
        k = len(order)
        lp = self.label[pred]
        j = bisect.bisect_right(self.order, lp)
        ls = self.order[j] if j < len(self.order) else (1 << self.U)
        if ls - lp <= k:
            self._make_room(lp, k)
            lp = self.label[pred]
            j = bisect.bisect_right(self.order, lp)
            ls = self.order[j] if j < len(self.order) else (1 << self.U)
        step = (ls - lp) // (k + 1)
        labs = [lp + (i + 1) * step for i in range(k)]
        self.order[j:j] = labs
        for v, lab in zip(order, labs):
            self.label[v] = lab
            self.node_at[lab] = v
            mk = self.m.mask[v]
            while mk:
                c = (mk & -mk).bit_length() - 1
                bisect.insort(self.cat[c], lab)
                mk &= mk - 1

    def on_delete(self, order: list) -> None:
        labs = [self.label[v] for v in order]   # contiguous in global order
        a = bisect.bisect_left(self.order, labs[0])
        del self.order[a:a + len(labs)]
        for v, lab in zip(order, labs):
            del self.node_at[lab]
            del self.label[v]
            mk = self.m.mask[v]
            while mk:
                c = (mk & -mk).bit_length() - 1
                lst = self.cat[c]
                del lst[bisect.bisect_left(lst, lab)]
                mk &= mk - 1

    def on_mask(self, v, old, new):
        lab = self.label[v]
        for c in range(NC):
            o, n = old >> c & 1, new >> c & 1
            if o and not n:
                lst = self.cat[c]
                del lst[bisect.bisect_left(lst, lab)]
            elif n and not o:
                bisect.insort(self.cat[c], lab)

    # -- queries -----------------------------------------------------------------
    def next(self, c, v):
        lst = self.cat[c]
        i = bisect.bisect_right(lst, self.label[v])
        return self.node_at[lst[i]] if i < len(lst) else None

    def prev(self, c, v):
        lst = self.cat[c]
        i = bisect.bisect_left(lst, self.label[v])
        return self.node_at[lst[i - 1]] if i > 0 else None

    def kth(self, c, k):
        lst = self.cat[c]
        return self.node_at[lst[k]] if 0 <= k < len(lst) else None

    def count(self, c):
        return len(self.cat[c])

    def before(self, a, b) -> bool:
        return self.label[a] < self.label[b]




# ---------------------------------------------------------------------------
class TreapIndex:
    """Implicit treap over preorder. Treap node id == mirror node id.

    Aggregates per treap node: size, OR-mask, per-category counts.
    rank(v) walks parent pointers: O(depth) = O(log n) expected.
    """

    def build(self, m: Mirror, order: list) -> None:
        self.m = m
        cap = len(m.parent)
        self.L, self.Rt, self.P = [-1] * cap, [-1] * cap, [-1] * cap
        self.pr, self.sz, self.agg = [0.0] * cap, [0] * cap, [0] * cap
        self.cnt = [None] * cap
        self.rng = random.Random(12345)
        self.root = self._build(order)

    def _ensure(self, v):
        while len(self.L) <= v:
            for a in (self.L, self.Rt, self.P):
                a.append(-1)
            self.pr.append(0.0)
            self.sz.append(0)
            self.agg.append(0)
            self.cnt.append(None)

    def _pull(self, v):
        l, r = self.L[v], self.Rt[v]
        mk = self.m.mask[v]
        s, a = 1, mk
        cnt = [(mk >> c) & 1 for c in range(NC)]
        if l >= 0:
            s += self.sz[l]
            a |= self.agg[l]
            cl = self.cnt[l]
            cnt = [x + y for x, y in zip(cnt, cl)]
        if r >= 0:
            s += self.sz[r]
            a |= self.agg[r]
            cr = self.cnt[r]
            cnt = [x + y for x, y in zip(cnt, cr)]
        self.sz[v], self.agg[v], self.cnt[v] = s, a, cnt

    def _build(self, order):
        """O(k) Cartesian-tree construction from a sequence (max-heap on priority)."""
        stack = []
        for v in order:
            self._ensure(v)
            self.pr[v] = self.rng.random()
            self.L[v] = self.Rt[v] = self.P[v] = -1
            last = -1
            while stack and self.pr[stack[-1]] < self.pr[v]:
                last = stack.pop()
            if last >= 0:
                self.L[v] = last
                self.P[last] = v
            if stack:
                self.Rt[stack[-1]] = v
                self.P[v] = stack[-1]
            stack.append(v)
        # pull bottom-up: postorder
        if not stack:
            return -1
        root = stack[0]
        post, st = [], [root]
        while st:
            x = st.pop()
            post.append(x)
            if self.L[x] >= 0:
                st.append(self.L[x])
            if self.Rt[x] >= 0:
                st.append(self.Rt[x])
        for x in reversed(post):
            self._pull(x)
        self.P[root] = -1
        return root

    def _split(self, t, k):
        """Split into (first k, rest). Iterative-recursive."""
        if t < 0:
            return -1, -1
        l = self.L[t]
        ls = self.sz[l] if l >= 0 else 0
        if k <= ls:
            a, b = self._split(l, k)
            self.L[t] = b
            if b >= 0:
                self.P[b] = t
            self._pull(t)
            if a >= 0:
                self.P[a] = -1
            self.P[t] = -1
            return a, t
        a, b = self._split(self.Rt[t], k - ls - 1)
        self.Rt[t] = a
        if a >= 0:
            self.P[a] = t
        self._pull(t)
        if b >= 0:
            self.P[b] = -1
        self.P[t] = -1
        return t, b

    def _merge(self, a, b):
        if a < 0:
            return b
        if b < 0:
            return a
        if self.pr[a] > self.pr[b]:
            r = self._merge(self.Rt[a], b)
            self.Rt[a] = r
            self.P[r] = a
            self._pull(a)
            self.P[a] = -1
            return a
        l = self._merge(a, self.L[b])
        self.L[b] = l
        self.P[l] = b
        self._pull(b)
        self.P[b] = -1
        return b

    def rank(self, v) -> int:
        l = self.L[v]
        r = self.sz[l] if l >= 0 else 0
        while self.P[v] >= 0:
            p = self.P[v]
            if self.Rt[p] == v:
                lp = self.L[p]
                r += (self.sz[lp] if lp >= 0 else 0) + 1
            v = p
        return r

    def on_insert(self, pred, order):
        pos = self.rank(pred) + 1
        sub = self._build(order)
        a, b = self._split(self.root, pos)
        self.root = self._merge(self._merge(a, sub), b)

    def on_delete(self, order):
        pos = self.rank(order[0])
        a, rest = self._split(self.root, pos)
        _, b = self._split(rest, len(order))
        self.root = self._merge(a, b)

    def on_mask(self, v, old, new):
        while v >= 0:
            self._pull(v)
            v = self.P[v]

    # -- queries ---------------------------------------------------------------
    def _first_in(self, t, bit):
        while t >= 0:
            l = self.L[t]
            if l >= 0 and self.agg[l] & bit:
                t = l
            elif self.m.mask[t] & bit:
                return t
            else:
                t = self.Rt[t]
        return None

    def _last_in(self, t, bit):
        while t >= 0:
            r = self.Rt[t]
            if r >= 0 and self.agg[r] & bit:
                t = r
            elif self.m.mask[t] & bit:
                return t
            else:
                t = self.L[t]
        return None

    def next(self, c, v):
        bit = 1 << c
        r = self.Rt[v]
        if r >= 0 and self.agg[r] & bit:
            return self._first_in(r, bit)
        while self.P[v] >= 0:
            p = self.P[v]
            if self.L[p] == v:
                if self.m.mask[p] & bit:
                    return p
                rp = self.Rt[p]
                if rp >= 0 and self.agg[rp] & bit:
                    return self._first_in(rp, bit)
            v = p
        return None

    def prev(self, c, v):
        bit = 1 << c
        l = self.L[v]
        if l >= 0 and self.agg[l] & bit:
            return self._last_in(l, bit)
        while self.P[v] >= 0:
            p = self.P[v]
            if self.Rt[p] == v:
                if self.m.mask[p] & bit:
                    return p
                lp = self.L[p]
                if lp >= 0 and self.agg[lp] & bit:
                    return self._last_in(lp, bit)
            v = p
        return None

    def kth(self, c, k):
        t = self.root
        if t < 0 or not (0 <= k < self.cnt[t][c]):
            return None
        while t >= 0:
            l = self.L[t]
            cl = self.cnt[l][c] if l >= 0 else 0
            if k < cl:
                t = l
                continue
            k -= cl
            if self.m.mask[t] >> c & 1:
                if k == 0:
                    return t
                k -= 1
            t = self.Rt[t]
        return None

    def count(self, c):
        return self.cnt[self.root][c] if self.root >= 0 else 0

    def depth_stats(self):
        best, tot, n = 0, 0, 0
        st = [(self.root, 1)]
        while st:
            x, d = st.pop()
            if x < 0:
                continue
            best = max(best, d)
            tot += d
            n += 1
            st.append((self.L[x], d + 1))
            st.append((self.Rt[x], d + 1))
        return best, tot / max(n, 1)
