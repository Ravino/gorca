"""Version resolution for add-ons (report section 3).

Run: python tools/research/addon_math/resolve.py

1. MVS (minimal version selection, Go modules): lower bounds only, one version per (package,
   major). Polynomial: O(reachable nodes + edges). Checked against brute force.
2. NP-hardness once exact/upper bounds are allowed: 3-SAT encoding, checked against brute force.
3. Host compatibility window: Component Model semver matching as in wasmtime
   (refs/wasmtime/crates/environ/src/component/names.rs `alternate_lookup_key`).
"""

from __future__ import annotations

import itertools
import random

# ------------------------------------------------------------------------------- 1. MVS

def mvs(root_reqs, reqs):
    """root_reqs: {pkg: min_version}; reqs[(pkg, v)] = {dep: min_version}.
    Returns {pkg: selected version} = max over all reachable minimums."""
    sel, todo, seen = {}, list(root_reqs.items()), set()
    while todo:
        p, v = todo.pop()
        if (p, v) in seen:
            continue
        seen.add((p, v))
        sel[p] = max(sel.get(p, 0), v)
        todo.extend(reqs.get((p, v), {}).items())
    return sel


def satisfies(sel, root_reqs, reqs):
    need = dict(root_reqs)
    for p, v in sel.items():
        for d, m in reqs.get((p, v), {}).items():
            need[d] = max(need.get(d, 0), m)
    return all(sel.get(p, 0) >= m for p, m in need.items())


def random_monotone_instance(rng, n_pkg=4, n_ver=3):
    """Requirements monotone in the version (a newer version never needs less), the assumption
    under which MVS is the componentwise least solution."""
    pkgs = [f"p{i}" for i in range(n_pkg)]
    reqs = {}
    for p in pkgs:
        prev = {}
        for v in range(1, n_ver + 1):
            cur = dict(prev)
            for d in pkgs:
                if d != p and rng.random() < 0.3:
                    cur[d] = max(cur.get(d, 0), rng.randint(1, n_ver))
            reqs[(p, v)] = cur
            prev = cur
    root = {p: rng.randint(1, n_ver) for p in rng.sample(pkgs, 2)}
    return pkgs, root, reqs


def test_mvs(trials=3000):
    rng = random.Random(11)
    for _ in range(trials):
        pkgs, root, reqs = random_monotone_instance(rng)
        sel = mvs(root, reqs)
        assert satisfies(sel, root, reqs)
        # least: every satisfying total assignment (absent = 0 allowed) dominates MVS
        for vs in itertools.product(range(0, 4), repeat=len(pkgs)):
            s = {p: v for p, v in zip(pkgs, vs) if v}
            if satisfies(s, root, reqs):
                assert all(s.get(p, 0) >= v for p, v in sel.items())
        # order independence: shuffling requirement dicts changes nothing
        sh = {k: dict(rng.sample(list(v.items()), len(v))) for k, v in reqs.items()}
        assert mvs(dict(rng.sample(list(root.items()), len(root))), sh) == sel


# ------------------------------------------------------------------------------- 2. SAT

def encode_3sat(clauses, n_vars):
    """Package x_i has versions {1: false, 2: true}; package c_j has versions 1..3, version k
    requires literal k of clause j via an EXACT pin (needs an upper bound). Root needs every c_j."""
    exact = {}
    for j, cl in enumerate(clauses):
        for k, lit in enumerate(cl, start=1):
            exact[(f"c{j}", k)] = {f"x{abs(lit)}": 2 if lit > 0 else 1}
    return exact


def resolvable(clauses, n_vars):
    exact = encode_3sat(clauses, n_vars)
    for xs in itertools.product((1, 2), repeat=n_vars):
        assign = {f"x{i + 1}": v for i, v in enumerate(xs)}
        if all(any(assign[d] == v for k in (1, 2, 3) for d, v in exact[(f"c{j}", k)].items())
               for j in range(len(clauses))):
            return True
    return False


def satisfiable(clauses, n_vars):
    return any(all(any((lit > 0) == xs[abs(lit) - 1] for lit in cl) for cl in clauses)
               for xs in itertools.product((False, True), repeat=n_vars))


def test_sat(trials=400):
    rng = random.Random(12)
    sat = 0
    for _ in range(trials):
        n = rng.randint(3, 7)
        cls = [tuple(rng.choice((1, -1)) * v for v in rng.sample(range(1, n + 1), 3))
               for _ in range(rng.randint(2, 5 * n))]
        r = resolvable(cls, n)
        assert r == satisfiable(cls, n)
        sat += r
    return sat, trials


# ------------------------------------------------------------------------------- 3. window

def parse(v):
    return tuple(int(x) for x in v.split("."))


def compat_track(v):
    """wasmtime alternate_lookup_key: 1.x.y -> (1,), 0.x.y (x>0) -> (0, x), 0.0.y -> exact."""
    M, m, p = parse(v)
    if M:
        return (M,)
    if m:
        return (0, m)
    return (0, 0, p)


def links(guest_import, host_provides):
    """A guest importing interface@g links against a host providing @h iff same track and h >= g."""
    return compat_track(guest_import) == compat_track(host_provides) and \
        parse(host_provides) >= parse(guest_import)


def test_window():
    assert links("1.2.0", "1.5.3") and not links("1.5.0", "1.2.0") and not links("1.2.0", "2.0.0")
    assert links("0.9.1", "0.9.4") and not links("0.9.1", "0.10.0") and not links("0.0.1", "0.0.2")


if __name__ == "__main__":
    test_mvs()
    s, t = test_sat()
    test_window()
    print("MVS: 3000 random monotone instances: satisfying, least (brute force), order-independent")
    print(f"3-SAT encoding with exact pins: resolvable == satisfiable on {t} instances ({s} sat)")
    print("compat window: wasmtime semver tracks reproduced (1.x by major, 0.x by minor)")
