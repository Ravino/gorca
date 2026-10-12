"""Page-layout fingerprint for "same layout as last visit": MinHash over the set
of canonical skeleton hashes of subtrees with >= 3 nodes.

Run: python layout_fp.py
"""
import random
import time

import numpy as np

import embed as E
from diff import subtree_hashes
from gen import page
from test_diff import rerender

NPERM = 128
P = (1 << 61) - 1


MASK = (1 << 61) - 1


def skeleton_set(t):
    """Merkle skeleton hashes of subtrees (size >= 3). Fragile: any change
    propagates to every ancestor."""
    ct = E.canonical(t)[0]
    size = E.subtree_sizes(ct)
    skel = subtree_hashes(ct)[0]
    return {skel[v] & MASK for v in range(len(ct)) if size[v] >= 3}


def local_set(t, depth=4):
    """Local shingles: WL-2 labels plus root-ward role paths of length `depth`.
    A mutation only changes shingles within distance 2-4 of it."""
    ct = E.canonical(t)[0]
    lab = E.wl_labels(ct, 2)[2]
    out = {x & MASK for x in lab}
    for v in range(len(ct)):
        path, u = [], v
        for _ in range(depth):
            if u < 0:
                break
            path.append(ct.role[u])
            u = ct.parent[u]
        out.add(E._hk(str(path)) & MASK)
    return out


def minhash(s, seed=0):
    rng = np.random.default_rng(seed)
    a = rng.integers(1, P, NPERM, dtype=np.uint64).astype(object)
    b = rng.integers(0, P, NPERM, dtype=np.uint64).astype(object)
    xs = list(s)
    return np.array([min((ai * x + bi) % P for x in xs) for ai, bi in zip(a, b)], dtype=object)


def jaccard(a, b):
    return len(a & b) / len(a | b)


def evaluate(fn, base, n):
    S0 = fn(base)
    t0 = time.perf_counter()
    M0 = minhash(S0)
    dt = time.perf_counter() - t0
    cases = {
        "same page, light re-render": rerender(base, random.Random(1), p_del=.02, p_ins=.02, p_txt=.1, p_swap=.02, p_wrap=0)[0],
        "same page, default re-render": rerender(base, random.Random(2))[0],
        "same page, heavy re-render": rerender(base, random.Random(3), p_del=.15, p_ins=.15, p_txt=.3, p_swap=.15, p_wrap=.1)[0],
        "other page, same generator": page(n, seed=7),
        "other page, other seed 2": page(n, seed=11),
        "div-soup variant of page": E.divsoup(page(n, seed=1)),
    }
    print(f"{fn.__name__}: |S|={len(S0)}; MinHash-{NPERM} signature in {dt*1e3:.0f} ms (Python, object ints)")
    for name, t in cases.items():
        S = fn(t)
        M = minhash(S)
        est = float(np.mean(M == M0))
        print(f"  {name:30s} exact J={jaccard(S0, S):.3f}  MinHash est={est:.3f}")


if __name__ == "__main__":
    n = 10_000
    base = page(n, seed=1)
    for fn in (skeleton_set, local_set):
        evaluate(fn, base, n)
