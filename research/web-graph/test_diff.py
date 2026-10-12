"""Tests and quality/latency evaluation for diff.py.

Run: python test_diff.py
"""
import itertools
import random
import time

from diff import (apply_child_diff, keyed_child_diff, match_trees, refetch_cover,
                  restore_cursor)
from gen import Tree, page


# --- 1. keyed child diff ------------------------------------------------------
def min_moves_bruteforce(old, new):
    """Smallest number of single-element moves turning `old` into `new` (same set)."""
    from collections import deque
    start, goal = tuple(old), tuple(new)
    seen, q = {start: 0}, deque([start])
    while q:
        s = q.popleft()
        if s == goal:
            return seen[s]
        for i in range(len(s)):
            rest = s[:i] + s[i + 1:]
            for j in range(len(s)):
                t = rest[:j] + (s[i],) + rest[j:]
                if t not in seen:
                    seen[t] = seen[s] + 1
                    q.append(t)


def test_child_diff():
    rng = random.Random(1)
    for _ in range(2000):
        universe = list(range(12))
        old = rng.sample(universe, rng.randint(0, 9))
        new = rng.sample(universe, rng.randint(0, 9))
        d = keyed_child_diff(old, new)
        assert apply_child_diff(old, d) == new, (old, new, d)
    for m in range(1, 7):
        for perm in itertools.permutations(range(m)):
            d = keyed_child_diff(list(range(m)), list(perm))
            assert len(d["move"]) == min_moves_bruteforce(range(m), perm)
    print("keyed child diff: apply-correct on 2000 random cases; |moves| == optimum for all perms m<=6")


# --- 5. refetch cover -----------------------------------------------------------
def test_cover():
    rng = random.Random(2)
    for trial in range(300):
        n = rng.randint(2, 12)
        parent = [-1] + [rng.randrange(i) for i in range(1, n)]
        children = [[] for _ in range(n)]
        for v in range(1, n):
            children[parent[v]].append(v)
        size = [1] * n
        for v in range(n - 1, 0, -1):
            size[parent[v]] += size[v]
        dirty = {v: rng.uniform(1, 5) for v in rng.sample(range(n), rng.randint(1, n))}
        alpha, beta = rng.uniform(0.5, 4), rng.uniform(0.1, 1.5)
        cost, roots, patched = refetch_cover(parent, children, size, dirty, alpha, beta)
        # brute force over all antichains/sets of refetch roots
        best = float("inf")
        for mask in range(1 << n):
            R = [v for v in range(n) if mask >> v & 1]
            covered = set()
            for r in R:
                st = [r]
                while st:
                    x = st.pop()
                    covered.add(x)
                    st.extend(children[x])
            c = sum(alpha + beta * size[r] for r in R) + sum(a for v, a in dirty.items() if v not in covered)
            best = min(best, c)
        assert abs(best - cost) < 1e-9, (trial, best, cost)
    print("refetch cover DP == brute force on 300 random trees")


# --- 3/4. re-render simulation ------------------------------------------------------
def rerender(a: Tree, rng, p_del=0.05, p_ins=0.05, p_txt=0.10, p_swap=0.05, p_wrap=0.03):
    """Copy `a` with fresh identities and SPA-like perturbations.

    Returns (b, truth) where truth maps surviving old nodes -> new nodes.
    """
    b, truth = Tree(), {}
    from gen import Gen, ROLES
    g = Gen(rng.randrange(10**9))

    def copy(u, pb):
        nm = a.name[u]
        if nm and not a.children[u] and rng.random() < p_txt:
            nm = nm + " " + g.words(1, 2)          # text edit (price, counter...)
        if pb is not None and rng.random() < p_wrap:
            pb = b.add("generic", pb)              # new wrapper div
        w = b.add(ROLES[a.role[u]], pb, nm, a.level[u], a.visited[u], a.family[u])
        truth[u] = w
        kids = list(a.children[u])
        for i in range(len(kids) - 1):
            if rng.random() < p_swap:
                kids[i], kids[i + 1] = kids[i + 1], kids[i]
        for c in kids:
            if len(a.children[c]) and rng.random() < p_del:
                continue                           # component removed
            copy(c, w)
            if a.family[c] >= 0 and rng.random() < p_ins:
                g.card(b, w, rng.randrange(4))     # new component inserted
    copy(0, None)
    return b, truth


def eval_matching(n, seed=0, **kw):
    rng = random.Random(seed)
    a = page(n, seed=seed)
    b, truth = rerender(a, rng, **kw)
    t0 = time.perf_counter()
    M = match_trees(a, b)
    dt = time.perf_counter() - t0
    correct = sum(1 for u, w in M.items() if truth.get(u) == w)
    prec = correct / max(1, len(M))
    rec = correct / max(1, len(truth))
    # cursor restoration on surviving leaves with text
    leaves = [u for u in truth if not a.children[u]]
    sample = rng.sample(leaves, min(2000, len(leaves)))
    ok = sum(1 for u in sample if restore_cursor(a, b, M, u, 0)[0] == truth[u])
    # baseline: same child-index path from the root
    def path_map(u):
        path = []
        while a.parent[u] >= 0:
            p = a.parent[u]
            path.append(a.children[p].index(u))
            u = p
        w = 0
        for i in reversed(path):
            if i >= len(b.children[w]):
                return None
            w = b.children[w][i]
        return w
    ok_path = sum(1 for u in sample if path_map(u) == truth[u])
    print(f"n_old={len(a)} n_new={len(b)} match: {dt*1e3:.0f} ms (Python), precision={prec:.4f} "
          f"recall={rec:.4f} | cursor restore {ok/len(sample):.3f} vs index-path baseline {ok_path/len(sample):.3f}")
    return prec, rec, ok / len(sample)


if __name__ == "__main__":
    test_child_diff()
    test_cover()
    print("-- re-render, default perturbation (5% del, 5% ins, 10% text, 5% swap, 3% wrap)")
    eval_matching(10_000)
    eval_matching(100_000)
    print("-- heavy perturbation (15% del, 15% ins, 30% text, 15% swap, 10% wrap)")
    eval_matching(10_000, p_del=.15, p_ins=.15, p_txt=.3, p_swap=.15, p_wrap=.1)
    print("-- identical re-render (identity loss only)")
    eval_matching(10_000, p_del=0, p_ins=0, p_txt=0, p_swap=0, p_wrap=0)
