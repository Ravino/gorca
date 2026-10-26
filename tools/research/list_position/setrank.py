"""Signed-weight set rank over an implicit treap in document order (reference implementation).

Problem: for every list L, answer count(L) = number of items owned by L and rank(x) = 1-based
position of item x among the items owned by its owner, where owner(x) = the nearest *list*
ancestor of x (items of nested lists belong to the nested list). Items and lists are those of the
published (fused, patched) model, hidden nodes excluded. The document root is a sentinel container
that owns every item with no list ancestor, so every visible item has exactly one owner.

Scheme (one extra i32 per node, plus subtree sum and max-prefix in the treap summary):

    w(x) = +1          if x is a visible item
    w(L) = -c(L)       if L is a visible list (c(L) = number of items it owns), root included
    w(x) = 0           otherwise

Invariant W: for every container L, the sum of w over L's subtree *excluding L itself* equals
c(L). Proof: every visible item y in subtree(L) \\ {L} has an owner O with L <= O (ancestor or
equal); if O = L it contributes +1, otherwise O is a container strictly inside L and y's +1 is
cancelled by O's -c(O) (each nested container's own items are exactly c(O)). Induction on nesting
depth gives the claim. Corollary R: for an item x owned by L,
    rank(x) = 1 + sum(w over [pos(L)+1, pos(x)))
because every nested container that starts before x lies wholly before x (x is not inside it,
otherwise its owner would be that container) and contributes net 0.
Corollary K: inside L's interior the relative prefix sum never exceeds its value before entering a
nested container, so the k-th item of L (0-based) is the first position p > pos(L) at which the
relative prefix reaches k+1. A (sum, max-prefix) treap summary finds it in O(log n).

Every mutation changes the w of O(1) nodes (a subtree move changes only its old and new owners), so
each costs O(log n) treap work plus an ancestor walk (O(depth)) to find an owner.

Counters: `doc.tsteps` counts treap node visits, `doc.asteps` counts ancestor-walk steps.
"""

from __future__ import annotations

import random
from dataclasses import dataclass, field

NIL = -1
NEG = -(1 << 60)

ITEM, LIST, OTHER = "item", "list", "other"


@dataclass
class Spec:
    """A subtree to insert: role in {item, list, other}."""

    role: str = OTHER
    hidden: bool = False
    children: list["Spec"] = field(default_factory=list)

    def count(self) -> int:
        return 1 + sum(c.count() for c in self.children)


class Doc:
    def __init__(self, seed: int = 1):
        self.rng = random.Random(seed)
        # tree
        self.parent: list[int] = []
        self.kids: list[list[int]] = []
        self.role: list[str] = []
        self.hidden: list[bool] = []
        self.alive: list[bool] = []
        # treap (implicit, by preorder position)
        self.l: list[int] = []
        self.r: list[int] = []
        self.up: list[int] = []
        self.prio: list[float] = []
        self.size: list[int] = []
        self.w: list[int] = []
        self.sm: list[int] = []
        self.mp: list[int] = []  # max over non-empty prefixes of the subtree's sequence
        # per container
        self.c: dict[int, int] = {}
        self.troot = NIL
        self.tsteps = 0
        self.asteps = 0
        root = self._new(OTHER, False)
        self.root = root
        self.c[root] = 0  # sentinel container: owns items without a list ancestor
        self.troot = root

    # ------------------------------------------------------------------ node kinds
    def kind(self, x: int) -> str:
        if x == self.root:
            return LIST
        if self.hidden[x]:
            return OTHER
        return self.role[x]

    def _new(self, role: str, hidden: bool) -> int:
        i = len(self.parent)
        self.parent.append(NIL)
        self.kids.append([])
        self.role.append(role)
        self.hidden.append(hidden)
        self.alive.append(True)
        self.l.append(NIL)
        self.r.append(NIL)
        self.up.append(NIL)
        self.prio.append(self.rng.random())
        self.size.append(1)
        self.w.append(0)
        self.sm.append(0)
        self.mp.append(0)
        return i

    # ------------------------------------------------------------------ treap core
    def _pull(self, t: int) -> None:
        self.tsteps += 1
        l, r = self.l[t], self.r[t]
        size = 1
        sm = 0
        mp = NEG
        if l != NIL:
            size += self.size[l]
            sm = self.sm[l]
            mp = self.mp[l]
        sm += self.w[t]
        mp = max(mp, sm)
        if r != NIL:
            size += self.size[r]
            mp = max(mp, sm + self.mp[r])
            sm += self.sm[r]
        self.size[t] = size
        self.sm[t] = sm
        self.mp[t] = mp

    def _sz(self, t: int) -> int:
        return 0 if t == NIL else self.size[t]

    def _split(self, t: int, k: int) -> tuple[int, int]:
        """First k nodes -> left. Iterative to keep Python's stack shallow."""
        if t == NIL:
            return NIL, NIL
        # descend, recording the path and the side each node goes to
        path = []
        while t != NIL:
            self.tsteps += 1
            ls = self._sz(self.l[t])
            if k <= ls:
                path.append((t, False))
                t = self.l[t]
            else:
                path.append((t, True))
                k -= ls + 1
                t = self.r[t]
        a = b = NIL
        for node, left in reversed(path):
            if left:  # node and its left subtree go left; its right child becomes `a`
                self.r[node] = a
                if a != NIL:
                    self.up[a] = node
                a = node
            else:
                self.l[node] = b
                if b != NIL:
                    self.up[b] = node
                b = node
            self._pull(node)
        if a != NIL:
            self.up[a] = NIL
        if b != NIL:
            self.up[b] = NIL
        return a, b

    def _merge(self, a: int, b: int) -> int:
        if a == NIL:
            return b
        if b == NIL:
            return a
        path = []
        while a != NIL and b != NIL:
            self.tsteps += 1
            if self.prio[a] > self.prio[b]:
                path.append((a, True))
                a = self.r[a]
            else:
                path.append((b, False))
                b = self.l[b]
        rest = a if a != NIL else b
        for node, is_a in reversed(path):
            if is_a:
                self.r[node] = rest
            else:
                self.l[node] = rest
            if rest != NIL:
                self.up[rest] = node
            self._pull(node)
            rest = node
        self.up[rest] = NIL
        return rest

    def _build(self, nodes: list[int]) -> int:
        """Linear-time treap build from a sequence (Cartesian tree on priorities)."""
        stack: list[int] = []
        for x in nodes:
            self.l[x] = self.r[x] = self.up[x] = NIL
            last = NIL
            while stack and self.prio[stack[-1]] < self.prio[x]:
                last = stack.pop()
            if last != NIL:
                self.l[x] = last
                self.up[last] = x
            if stack:
                self.r[stack[-1]] = x
                self.up[x] = stack[-1]
            stack.append(x)
        # pull bottom-up: postorder
        if not stack:
            return NIL
        root = stack[0]
        order = []
        st = [root]
        while st:
            t = st.pop()
            order.append(t)
            if self.l[t] != NIL:
                st.append(self.l[t])
            if self.r[t] != NIL:
                st.append(self.r[t])
        for t in reversed(order):
            self._pull(t)
        return root

    def pos(self, x: int) -> int:
        r = self._sz(self.l[x])
        cur = x
        while True:
            self.tsteps += 1
            p = self.up[cur]
            if p == NIL:
                return r
            if self.r[p] == cur:
                r += self._sz(self.l[p]) + 1
            cur = p

    def prefix(self, k: int) -> int:
        """Sum of w over positions [0, k)."""
        t, acc = self.troot, 0
        while t != NIL and k > 0:
            self.tsteps += 1
            ls = self._sz(self.l[t])
            if k <= ls:
                t = self.l[t]
            else:
                if self.l[t] != NIL:
                    acc += self.sm[self.l[t]]
                acc += self.w[t]
                k -= ls + 1
                t = self.r[t]
        return acc

    def range_sum(self, lo: int, hi: int) -> int:
        return self.prefix(hi) - self.prefix(lo)

    def set_w(self, x: int, v: int) -> None:
        if self.w[x] == v:
            return
        self.w[x] = v
        t = x
        while t != NIL:
            self._pull(t)
            t = self.up[t]

    def first_reach(self, lo: int, target: int) -> int | None:
        """First position p >= lo with prefix(p+1) >= target, or None."""
        return self._first_reach(self.troot, 0, 0, lo, target)

    def _first_reach(self, t: int, off: int, acc: int, lo: int, target: int):
        # acc = prefix sum of everything left of this subtree
        if t == NIL:
            return None
        self.tsteps += 1
        size = self.size[t]
        if off + size <= lo:
            return None
        if off >= lo and acc + self.mp[t] < target:
            return None
        l = self.l[t]
        ls = self._sz(l)
        res = self._first_reach(l, off, acc, lo, target)
        if res is not None:
            return res
        accl = acc + (self.sm[l] if l != NIL else 0)
        p = off + ls
        here = accl + self.w[t]
        if p >= lo and here >= target:
            return p
        return self._first_reach(self.r[t], p + 1, here, lo, target)

    def at(self, p: int) -> int:
        t = self.troot
        while t != NIL:
            self.tsteps += 1
            ls = self._sz(self.l[t])
            if p < ls:
                t = self.l[t]
            elif p == ls:
                return t
            else:
                p -= ls + 1
                t = self.r[t]
        raise IndexError(p)

    # ------------------------------------------------------------------ tree helpers
    def last_desc(self, x: int) -> int:
        while self.kids[x]:
            self.asteps += 1
            x = self.kids[x][-1]
        return x

    def owner_above(self, x: int) -> int:
        """Nearest container strictly above x (the root sentinel at worst)."""
        p = self.parent[x]
        while True:
            self.asteps += 1
            if self.kind(p) == LIST:
                return p
            p = self.parent[p]

    def _bump(self, container: int, delta: int) -> None:
        if delta:
            self.c[container] += delta
            self.set_w(container, -self.c[container])

    def _pos_before_child(self, parent: int, idx: int) -> int:
        """Preorder position where a new child at index idx of parent goes."""
        if idx == 0:
            return self.pos(parent) + 1
        return self.pos(self.last_desc(self.kids[parent][idx - 1])) + 1

    # ------------------------------------------------------------------ mutations
    def insert_subtree(self, parent: int, idx: int, spec: Spec) -> int:
        seq: list[int] = []
        exported = 0

        def build(s: Spec, par: int, owner_in: int | None) -> int:
            nonlocal exported
            x = self._new(s.role, s.hidden)
            self.parent[x] = par
            seq.append(x)
            k = self.kind(x)
            if k == ITEM:
                self.w[x] = 1
                if owner_in is None:
                    exported += 1
                else:
                    self.c[owner_in] += 1
            own = owner_in
            if k == LIST:
                self.c[x] = 0
                own = x
            for cs in s.children:
                y = build(cs, x, own)
                self.kids[x].append(y)
            if k == LIST:
                self.w[x] = -self.c[x]
            return x

        x = build(spec, parent, None)
        p = self._pos_before_child(parent, idx)
        self.kids[parent].insert(idx, x)
        piece = self._build(seq)
        a, b = self._split(self.troot, p)
        self.troot = self._merge(self._merge(a, piece), b)
        self._bump(self.owner_above(x), exported)
        return x

    def _cut(self, x: int) -> int:
        lo = self.pos(x)
        hi = self.pos(self.last_desc(x)) + 1
        a, rest = self._split(self.troot, lo)
        mid, b = self._split(rest, hi - lo)
        self.troot = self._merge(a, b)
        return mid

    def remove_subtree(self, x: int) -> None:
        o = self.owner_above(x)
        piece = self._cut(x)
        self._bump(o, -self.sm[piece])
        p = self.parent[x]
        self.kids[p].remove(x)
        st = [x]
        while st:
            y = st.pop()
            self.alive[y] = False
            self.c.pop(y, None)
            st.extend(self.kids[y])

    def move_subtree(self, x: int, new_parent: int, idx: int) -> None:
        o = self.owner_above(x)
        piece = self._cut(x)
        s = self.sm[piece]
        self._bump(o, -s)
        self.kids[self.parent[x]].remove(x)
        self.parent[x] = new_parent
        p = self._pos_before_child(new_parent, idx)
        self.kids[new_parent].insert(idx, x)
        a, b = self._split(self.troot, p)
        self.troot = self._merge(self._merge(a, piece), b)
        self._bump(self.owner_above(x), s)

    def set_props(self, x: int, role: str | None = None, hidden: bool | None = None) -> None:
        old = self.kind(x)
        if role is not None:
            self.role[x] = role
        if hidden is not None:
            self.hidden[x] = hidden
        new = self.kind(x)
        if old == new:
            return
        o = self.owner_above(x)
        # leave the old kind
        if old == ITEM:
            self.set_w(x, 0)
            self._bump(o, -1)
        elif old == LIST:
            cx = self.c.pop(x)
            self.set_w(x, 0)
            self._bump(o, cx)
        # enter the new kind
        if new == ITEM:
            self.set_w(x, 1)
            self._bump(o, 1)
        elif new == LIST:
            px = self.pos(x)
            end = self.pos(self.last_desc(x)) + 1
            s = self.range_sum(px + 1, end)  # items inside that O owned
            self.c[x] = s
            self.set_w(x, -s)
            self._bump(o, -s)

    def wrap(self, parent: int, i: int, j: int, role: str = LIST) -> int:
        """Synthetic container (ADR-0029) over kids[i:j] of parent. Preorder of the members is
        unchanged: one treap insert at the first member's position, then the kind change."""
        members = self.kids[parent][i:j]
        n = self._new(OTHER, False)
        self.parent[n] = parent
        p = self.pos(members[0])
        a, b = self._split(self.troot, p)
        self._pull(n)
        self.troot = self._merge(self._merge(a, n), b)
        self.kids[parent][i:j] = [n]
        self.kids[n] = members
        for m in members:
            self.parent[m] = n
        self.set_props(n, role=role)
        return n

    def dissolve(self, n: int) -> None:
        self.set_props(n, role=OTHER)
        parent = self.parent[n]
        p = self.pos(n)
        a, rest = self._split(self.troot, p)
        _, b = self._split(rest, 1)
        self.troot = self._merge(a, b)
        i = self.kids[parent].index(n)
        self.kids[parent][i : i + 1] = self.kids[n]
        for m in self.kids[n]:
            self.parent[m] = parent
        self.kids[n] = []
        self.alive[n] = False

    # ------------------------------------------------------------------ queries
    def count(self, lst: int) -> int | None:
        return self.c.get(lst) if lst != self.root else None

    def owner(self, x: int) -> int | None:
        if self.kind(x) != ITEM:
            return None
        o = self.owner_above(x)
        return None if o == self.root else o

    def rank(self, x: int) -> tuple[int, int] | None:
        """(1-based position, owner) of item x."""
        o = self.owner(x)
        if o is None:
            return None
        return 1 + self.range_sum(self.pos(o) + 1, self.pos(x)), o

    def kth(self, lst: int, k: int) -> int | None:
        """k-th (0-based) item of list `lst`."""
        if lst not in self.c or not (0 <= k < self.c[lst]):
            return None
        lo = self.pos(lst) + 1
        target = self.prefix(lo) + k + 1
        p = self.first_reach(lo, target)
        return None if p is None else self.at(p)


# ---------------------------------------------------------------------- brute-force oracle
def oracle(doc: Doc) -> dict[int, list[int]]:
    """members[list] in document order, recomputed from the tree."""
    members: dict[int, list[int]] = {}

    def walk(x: int, owner: int) -> None:
        stack = [(x, owner)]
        while stack:
            y, o = stack.pop()
            k = doc.kind(y)
            if k == ITEM and y != doc.root:
                members.setdefault(o, []).append(y)
            no = y if k == LIST else o
            if k == LIST:
                members.setdefault(y, [])
            for z in reversed(doc.kids[y]):
                stack.append((z, no))

    walk(doc.root, doc.root)
    return members


def preorder(doc: Doc) -> list[int]:
    out, st = [], [doc.root]
    while st:
        y = st.pop()
        out.append(y)
        st.extend(reversed(doc.kids[y]))
    return out


def check(doc: Doc) -> None:
    """Full consistency check against the oracle (slow; tests only)."""
    order = preorder(doc)
    assert doc.size[doc.troot] == len(order), (doc.size[doc.troot], len(order))
    for i, y in enumerate(order):
        assert doc.at(i) == y, ("order", i, y)
    m = oracle(doc)
    assert doc.sm[doc.troot] == 0
    for lst, items in m.items():
        if lst == doc.root:
            assert doc.c[lst] == len(items)
            continue
        assert doc.count(lst) == len(items), ("count", lst, doc.count(lst), len(items))
        for k, it in enumerate(items):
            rk = doc.rank(it)
            assert rk == (k + 1, lst), ("rank", it, rk, k + 1, lst)
            assert doc.kth(lst, k) == it, ("kth", lst, k)
        assert doc.kth(lst, len(items)) is None
