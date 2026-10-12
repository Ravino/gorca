"""Navigation-index workload: trace generation (shared with the Rust bench) and
Python replay timings.

  python bench_navindex.py gen   N  OUT.trace [n_mut] [n_q]   write trace + expected answers
  python bench_navindex.py run   N                            Python timings (OM vs treap)

Trace format (text, whitespace separated; consumed by rust-bench/):
  T n                        initial tree, followed by n lines "parent mask"
                             (ids 0..n-1; children order = increasing id)
  I p idx k  (id lp mask)*k  insert subtree as child #idx of p; triples in preorder,
                             lp = local index of parent within this list, -1 = root
  D v                        delete subtree rooted at v
  M v mask                   set category mask
  Q kind cat arg expected    kind: n (next after node arg), p (prev), k (k-th, arg=k);
                             expected = node id or -1
"""
from __future__ import annotations

import random
import statistics
import sys
import time

from gen import page, random_subtree
from navindex import NC, BruteIndex, Mirror, OMIndex, TreapIndex

MIX = (("leaf", 0.40), ("mask", 0.20), ("replace", 0.25), ("append", 0.15))


class Workload:
    """Realistic mutation mix on a page mirror.

    leaf     replace one text leaf (ticking clock, counter)         k = 1
    mask     toggle a category bit (visited, role/level change)     k = 0
    replace  SPA re-render of a component: delete + insert          k ~ U[20, 500]
    append   infinite scroll: append a batch at the end of a list   k ~ U[50, 300]
    """

    def __init__(self, m: Mirror, seed: int):
        self.m = m
        self.rng = random.Random(seed)
        self.step = 0
        self.lists = [v for v in range(len(m.parent)) if m.children[v] and len(m.children[v]) >= 8]

    def _random_node(self):
        while True:
            v = self.rng.randrange(len(self.m.parent))
            if self.m.alive[v]:
                return v

    def _component(self):
        """Pick a subtree of modest size to re-render (climb from a random node)."""
        m = self.m
        v = self._random_node()
        target = self.rng.randint(20, 500)
        size = 1
        while m.parent[v] > 0:
            p = m.parent[v]
            ps = len(m.subtree_preorder(p)) if size < target else 10**9
            if ps > target:
                break
            v, size = p, ps
        return v

    def mutate(self, emit):
        m, rng = self.m, self.rng
        self.step += 1
        r = rng.random()
        acc = 0.0
        kind = MIX[-1][0]
        for name, p in MIX:
            acc += p
            if r < acc:
                kind = name
                break
        if kind == "leaf":
            v = self._random_node()
            while m.children[v]:
                v = m.children[v][-1]
            if v == 0:
                return kind
            p = m.parent[v]
            idx = m.children[p].index(v)
            emit(("D", v))
            m.delete_subtree(v)
            sub = random_subtree(self.step, 1)
            root = m.insert_subtree(p, idx, sub)
            emit(("I", p, idx, root))
        elif kind == "mask":
            v = self._random_node()
            nm = m.mask[v] ^ (1 << rng.randrange(NC))
            m.set_mask(v, nm)
            emit(("M", v, nm))
        elif kind == "replace":
            v = self._component()
            if v == 0:
                return kind
            p = m.parent[v]
            idx = m.children[p].index(v)
            k = len(m.subtree_preorder(v))
            emit(("D", v))
            m.delete_subtree(v)
            root = m.insert_subtree(p, idx, random_subtree(self.step, max(1, k)))
            emit(("I", p, idx, root))
        else:
            lst = [x for x in self.lists if m.alive[x]]
            p = rng.choice(lst) if lst else 0
            idx = len(m.children[p])
            root = m.insert_subtree(p, idx, random_subtree(self.step, rng.randint(50, 300)))
            emit(("I", p, idx, root))
        return kind


def write_trace(n: int, out: str, n_mut: int = 2000, n_q: int = 20000, seed: int = 1):
    t = page(n, seed=seed)
    brute = BruteIndex()
    om = OMIndex()   # fast oracle; validated against brute in test_navindex.py
    m = Mirror(t, [om])
    rng = random.Random(seed + 1)
    with open(out, "w") as f:
        f.write(f"T {len(t)}\n")
        for v in range(len(t)):
            f.write(f"{t.parent[v]} {m.mask[v]}\n")

        def emit(op):
            if op[0] == "D":
                f.write(f"D {op[1]}\n")
            elif op[0] == "M":
                f.write(f"M {op[1]} {op[2]}\n")
            else:
                _, p, idx, root = op
                order = m.subtree_preorder(root)
                local = {v: i for i, v in enumerate(order)}
                parts = []
                for v in order:
                    lp = -1 if v == root else local[m.parent[v]]
                    parts.append(f"{v} {lp} {m.mask[v]}")
                f.write(f"I {p} {idx} {len(order)} " + " ".join(parts) + "\n")

        def queries(q):
            order = None
            for _ in range(q):
                c = rng.randrange(NC)
                kind = rng.choice("npk")
                if kind == "k":
                    cnt = om.count(c)
                    if not cnt:
                        continue
                    k = rng.randrange(cnt)
                    e = om.kth(c, k)
                    f.write(f"Q k {c} {k} {e}\n")
                else:
                    while True:
                        v = rng.randrange(len(m.parent))
                        if m.alive[v]:
                            break
                    e = om.next(c, v) if kind == "n" else om.prev(c, v)
                    f.write(f"Q {kind} {c} {v} {-1 if e is None else e}\n")

        wl = Workload(m, seed)
        qper = max(1, n_q // n_mut)
        for _ in range(n_mut):
            wl.mutate(emit)
            queries(qper)
    del brute


def run(n: int, n_mut: int = 300, n_q: int = 3000):
    for cls in (OMIndex, TreapIndex):
        t = page(n, seed=1)
        t0 = time.perf_counter()
        ix = cls()
        m = Mirror(t, [ix])
        build = time.perf_counter() - t0
        rng = random.Random(5)
        order = m.preorder()
        qs = []
        for _ in range(n_q):
            c, v = rng.randrange(NC), rng.choice(order)
            a = time.perf_counter()
            ix.next(c, v)
            qs.append(time.perf_counter() - a)
        wl = Workload(m, 1)
        per_kind = {}
        for _ in range(n_mut):
            a = time.perf_counter()
            kind = wl.mutate(lambda op: None)
            per_kind.setdefault(kind, []).append(time.perf_counter() - a)
        qs.sort()
        print(f"{cls.__name__:10s} n={len(t):6d} build={build*1e3:7.1f} ms  next p50={qs[len(qs)//2]*1e6:6.1f} us "
              f"p99={qs[int(len(qs)*.99)]*1e6:6.1f} us  " +
              " ".join(f"{k}={statistics.mean(v)*1e3:.2f}ms" for k, v in sorted(per_kind.items())))


if __name__ == "__main__":
    if sys.argv[1] == "gen":
        write_trace(int(sys.argv[2]), sys.argv[3], *(int(x) for x in sys.argv[4:]))
    else:
        run(int(sys.argv[2]))
