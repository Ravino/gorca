"""Add-on composition algebra: deterministic ordering, conflict detection, bounded filter chains.

Reference implementation for docs/reports/addon-platform-math.md section 1.
Run: python tools/research/addon_math/compose.py   (self-tests, prints a summary)

Model
-----
An add-on contributes to a *hook point* H with a declaration
    (ext_id, phase, before: set[ext_id], after: set[ext_id])
Phases are a fixed total order owned by the core (no numeric priorities for third parties).
The execution order of H is the lexicographically smallest linear extension of the partial
order  P = (phase order) U (declared before/after edges between present add-ons),
with the tie key (phase, ext_id).  Kahn's algorithm with a min-heap computes it in
O((V + E) log V).  It is a function of the *set* of declarations only, hence independent of
load order (tested below by shuffling).

Cycles: Tarjan SCC.  Edges inside a non-trivial SCC are dropped (all of them, symmetric, so the
result does not depend on which add-on "caused" the cycle), the SCC members fall back to the
tie key, and every SCC is reported.  Edges that contradict the phase order are rejected
(publish-time error) and also dropped at load.
"""

from __future__ import annotations

import heapq
import itertools
import random
from dataclasses import dataclass, field

PHASES = ("normalize", "expand", "annotate", "finalize")  # example phase lattice for text filters
PHASE_RANK = {p: i for i, p in enumerate(PHASES)}


@dataclass(frozen=True)
class Decl:
    ext: str
    phase: str
    before: frozenset = field(default_factory=frozenset)
    after: frozenset = field(default_factory=frozenset)


@dataclass
class OrderResult:
    order: list
    cycles: list  # list of sorted member lists
    rejected: list  # (a, b, reason) edges dropped for contradicting phases


def _edges(decls):
    present = {d.ext: d for d in decls}
    edges, rejected = set(), []
    for d in decls:
        for b in d.before:
            if b in present:
                edges.add((d.ext, b))
        for a in d.after:
            if a in present:
                edges.add((a, d.ext))
    kept = set()
    for a, b in sorted(edges):
        ra, rb = PHASE_RANK[present[a].phase], PHASE_RANK[present[b].phase]
        if ra > rb:
            rejected.append((a, b, "contradicts phase order"))
        elif ra == rb:
            kept.add((a, b))  # cross-phase edges in the right direction are implied anyway
    return present, kept, rejected


def _tarjan(nodes, adj):
    index, low, on, stack, out = {}, {}, set(), [], []
    counter = itertools.count()

    def strong(v):  # recursion depth <= number of add-ons (<= 64 per host today)
        index[v] = low[v] = next(counter)
        stack.append(v)
        on.add(v)
        for w in sorted(adj.get(v, ())):
            if w not in index:
                strong(w)
                low[v] = min(low[v], low[w])
            elif w in on:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop()
                on.discard(w)
                comp.append(w)
                if w == v:
                    break
            out.append(sorted(comp))

    for v in sorted(nodes):
        if v not in index:
            strong(v)
    return out


def order(decls) -> OrderResult:
    present, edges, rejected = _edges(decls)
    adj = {}
    for a, b in edges:
        adj.setdefault(a, set()).add(b)
    sccs = _tarjan(present.keys(), adj)
    cyc = [c for c in sccs if len(c) > 1]
    comp_of = {v: i for i, c in enumerate(cyc) for v in c}
    edges = {(a, b) for a, b in edges if not (a in comp_of and comp_of.get(a) == comp_of.get(b))}
    indeg = {v: 0 for v in present}
    adj = {}
    for a, b in edges:
        adj.setdefault(a, []).append(b)
        indeg[b] += 1
    key = lambda v: (PHASE_RANK[present[v].phase], v)  # noqa: E731
    heap = [key(v) for v, d in indeg.items() if d == 0]
    heapq.heapify(heap)
    out = []
    while heap:
        _, v = heapq.heappop(heap)
        out.append(v)
        for w in adj.get(v, ()):
            indeg[w] -= 1
            if indeg[w] == 0:
                heapq.heappush(heap, key(w))
    assert len(out) == len(present), "acyclic after SCC edge removal"
    return OrderResult(out, sorted(cyc), rejected)


# ---------------------------------------------------------------------------------------------
# Exclusive resources (gestures, quick-nav letters): conflict detection over contexts x matchers.
# ---------------------------------------------------------------------------------------------

def glob_intersects(p: str, q: str) -> bool:
    """True iff some string matches both globs (`*` any run, `?` one char). O(|p||q|) BFS over
    the product automaton; exact (regular-language intersection emptiness)."""
    seen, todo = set(), [(0, 0)]
    while todo:
        i, j = todo.pop()
        if (i, j) in seen:
            continue
        seen.add((i, j))
        if i == len(p) and j == len(q):
            return True
        # epsilon moves: a star may match nothing
        if i < len(p) and p[i] == "*":
            todo.append((i + 1, j))
        if j < len(q) and q[j] == "*":
            todo.append((i, j + 1))
        # consume one common character
        if i < len(p) and j < len(q):
            a, b = p[i], q[j]
            if a not in "*?" and b not in "*?" and a != b:
                continue
            ni = i if a == "*" else i + 1
            nj = j if b == "*" else j + 1
            todo.append((ni, nj))
    return False


@dataclass(frozen=True)
class Claim:
    ext: str
    resource: str  # canonical chord "gorca+shift+f9#1" or quick-nav letter "nav:h"
    contexts: frozenset  # keymap contexts where it is bound
    apps: tuple  # app-name globs; () = any app
    installed_seq: int  # position in the user's install ledger (persisted), not load order


def may_coexist(a: Claim, b: Claim) -> bool:
    """Can both claims be live at the same time (same app, same context)?"""
    if not (a.contexts & b.contexts):
        return False
    pa, pb = a.apps or ("*",), b.apps or ("*",)
    return any(glob_intersects(x, y) for x in pa for y in pb)


def assign(claims, reserved: set):
    """Incumbent-wins assignment. reserved = resources the user or the core preset mention
    (precedence user > core > extension, as in commands/compile.rs). Among extensions the one
    earliest in the install ledger keeps the resource; losers are reported, never silent.
    Returns (granted claims, conflicts[(winner, loser, resource)])."""
    granted, conflicts = [], []
    for c in sorted(claims, key=lambda c: (c.installed_seq, c.ext, c.resource)):
        if c.resource in reserved:
            conflicts.append(("<user/core>", c.ext, c.resource))
            continue
        clash = next((g for g in granted if g.resource == c.resource and may_coexist(g, c)), None)
        if clash:
            conflicts.append((clash.ext, c.ext, c.resource))
        else:
            granted.append(c)
    return granted, conflicts


# ---------------------------------------------------------------------------------------------
# Ordered transformation chain (speech text filters): single pass, bounded output.
# ---------------------------------------------------------------------------------------------

def out_cap(n0: int) -> int:
    """Output cap of every filter in the chain, relative to the chain's ORIGINAL input n0 (not
    to the previous stage, which would compound geometrically): 2 x n0 + 1024, at most 16 Ki."""
    return min(2 * n0 + 1024, 16 * 1024)


def run_chain(text: str, filters):
    """Each filter runs exactly once (no fixpoint iteration): termination is structural.
    A filter whose output exceeds its cap is treated as identity (and counted)."""
    work, over = 0, 0
    cap = out_cap(len(text))
    for f in filters:
        out = f(text)
        work += len(text) + min(len(out), cap)
        if len(out) > cap:
            over += 1
            continue
        text = out
    return text, work, over


def chain_work_bound(n0: int, k: int) -> int:
    """Upper bound on characters handled by a k-filter chain from input length n0."""
    m = out_cap(n0)
    return n0 + m + (k - 1) * 2 * m if k else 0  # first stage reads n0, later ones read <= m


def first_wins(answers):
    """present/policy/on-event: left-zero monoid; result = first non-None."""
    return next((a for a in answers if a is not None), None)


def and_chain_optimal(tests):
    """Sequential AND (veto) with short-circuit: expected cost minimised by sorting on
    c_i / (1 - p_i), p_i = P(test passes).  Exchange argument; brute-force checked below."""
    return sorted(tests, key=lambda t: t[0] / max(1e-12, 1 - t[1]))


def and_chain_cost(tests):
    cost, reach = 0.0, 1.0
    for c, p in tests:
        cost += reach * c
        reach *= p
    return cost


# ---------------------------------------------------------------------------------------------
# Self-tests
# ---------------------------------------------------------------------------------------------

def _random_decls(rng, n):
    ids = [f"ext{i:02d}" for i in range(n)]
    decls = []
    for e in ids:
        others = [o for o in ids if o != e]
        decls.append(Decl(e, rng.choice(PHASES),
                          frozenset(rng.sample(others, rng.randint(0, min(2, len(others))))),
                          frozenset(rng.sample(others, rng.randint(0, min(2, len(others)))) + ["missing-ext"])))
    return decls


def test_load_order_independence(trials=2000):
    rng = random.Random(1)
    cycles_seen = 0
    for _ in range(trials):
        decls = _random_decls(rng, rng.randint(1, 24))
        ref = order(decls)
        cycles_seen += bool(ref.cycles)
        for _ in range(5):
            sh = decls[:]
            rng.shuffle(sh)
            r = order(sh)
            assert r.order == ref.order and r.cycles == ref.cycles and r.rejected == ref.rejected
        # every kept constraint is respected
        pos = {v: i for i, v in enumerate(ref.order)}
        _, kept, _ = _edges(decls)
        in_cycle = {v for c in ref.cycles for v in c}
        for a, b in kept:
            if not (a in in_cycle and b in in_cycle):
                assert pos[a] < pos[b], (a, b)
        # phases are monotone
        ph = [PHASE_RANK[{d.ext: d for d in decls}[v].phase] for v in ref.order]
        assert ph == sorted(ph)
    return cycles_seen


def test_glob():
    cases = [("firefox*", "*fox", True), ("gnome-terminal*", "firefox", False),
             ("a*b", "*c", False), ("a?c", "ab*", True), ("*", "", True), ("?", "", False),
             ("ab*cd", "*bc*", True), ("x*", "y*", False)]
    for p, q, want in cases:
        assert glob_intersects(p, q) == want, (p, q)
        assert glob_intersects(q, p) == want, (q, p)
    # brute-force cross-check on a small alphabet
    rng = random.Random(2)
    alpha = "ab"
    words = [""] + ["".join(w) for n in range(1, 7) for w in itertools.product(alpha, repeat=n)]
    import fnmatch
    for _ in range(3000):
        p = "".join(rng.choice("ab*?") for _ in range(rng.randint(0, 4)))
        q = "".join(rng.choice("ab*?") for _ in range(rng.randint(0, 4)))
        brute = any(fnmatch.fnmatchcase(w, p) and fnmatch.fnmatchcase(w, q) for w in words)
        assert glob_intersects(p, q) == brute, (p, q)


def test_assign():
    c = [
        Claim("ext-b", "gorca+f9#1", frozenset({"global"}), ("firefox*",), 2),
        Claim("ext-a", "gorca+f9#1", frozenset({"global"}), ("firefox",), 1),
        Claim("ext-c", "gorca+f9#1", frozenset({"global"}), ("gnome-terminal*",), 3),
        Claim("ext-d", "gorca+f9#1", frozenset({"browse"}), (), 4),
        Claim("ext-e", "gorca+f1#1", frozenset({"global"}), (), 5),
    ]
    g, conf = assign(c, reserved={"gorca+f1#1"})
    assert {x.ext for x in g} == {"ext-a", "ext-c", "ext-d"}, g
    assert ("ext-a", "ext-b", "gorca+f9#1") in conf and ("<user/core>", "ext-e", "gorca+f1#1") in conf
    for perm in itertools.permutations(c):  # load-order independence
        assert assign(list(perm), {"gorca+f1#1"}) == (g, conf)
    # stability: adding a later-installed add-on never changes existing grants
    g2, _ = assign(c + [Claim("ext-z", "gorca+f9#1", frozenset({"global"}), (), 9)], {"gorca+f1#1"})
    assert [x for x in g2 if x.ext != "ext-z"] == g


def test_chain():
    rng = random.Random(3)
    bomb = lambda s: s * 1000  # noqa: E731
    dbl = lambda s: s + s  # noqa: E731
    up = str.upper
    for _ in range(500):
        fs = [rng.choice([bomb, dbl, up, str.strip]) for _ in range(rng.randint(0, 16))]
        n0 = rng.randint(0, 300)
        out, work, _ = run_chain("x" * n0, fs)
        assert len(out) <= 16 * 1024 and work <= chain_work_bound(n0, len(fs))
    # bound is linear in k once the absolute cap is reached
    return chain_work_bound(256, 16), chain_work_bound(256, 64)


def test_and_chain():
    rng = random.Random(4)
    for _ in range(300):
        t = [(rng.uniform(0.1, 5), rng.uniform(0, 0.99)) for _ in range(rng.randint(1, 6))]
        best = min(and_chain_cost(list(p)) for p in itertools.permutations(t))
        assert abs(and_chain_cost(and_chain_optimal(t)) - best) < 1e-9


def test_first_wins_monoid():
    vals = [None, "a", "b", None]
    for x, y, z in itertools.product(vals, repeat=3):
        fw = lambda a, b: a if a is not None else b  # noqa: E731
        assert fw(fw(x, y), z) == fw(x, fw(y, z))  # associative
    assert any(first_wins([x, y]) != first_wins([y, x]) for x in vals for y in vals)  # not commutative


if __name__ == "__main__":
    cyc = test_load_order_independence()
    test_glob()
    test_assign()
    w16, w64 = test_chain()
    test_and_chain()
    test_first_wins_monoid()
    print(f"ordering: 2000 random hook points x 5 shuffles identical; {cyc} had cycles, all reported")
    print("glob intersection: exact vs brute force on 3000 random pairs OK")
    print("assign: permutation-invariant, incumbent-stable, user/core reserved")
    print(f"filter chain work bound from 256 chars: k=16 -> {w16} chars, k=64 -> {w64} chars")
    print("AND-chain c/(1-p) ordering optimal vs brute force (300 cases)")
    print("first-wins: associative, not commutative")
