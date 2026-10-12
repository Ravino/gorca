"""Structural embeddings of subtrees for "find similar" / "next similar item" and
repeated-structure grouping, end to end.

Representations compared (all text-free: structure + coarse text shape):
  skel      exact Merkle hash of the role skeleton (equality only)
  wl        depth-limited Weisfeiler-Lehman subtree features, feature-hashed to
            D dims, log-scaled, L2-normalised (cosine similarity)
  simhash   64-bit SimHash of the wl vector (Hamming), banded LSH for grouping

Pipeline (background, off the keypress path):
  features -> kNN graph (exact or ANN) -> threshold tau -> union-find clusters
  -> per-cluster ordered sets (document order) -> O(log n) "next similar" query.

Run: python embed.py [n]      (quality + latency report; ANN benchmark in bench_ann.py)
"""
from __future__ import annotations

import hashlib
import sys
import time
from collections import Counter, defaultdict

import numpy as np

from diff import subtree_hashes
from gen import Tree, page

DEPTH = 3          # subtree depth considered by the features
D = 128            # hashed feature dimension
MIN_SIZE = 3       # candidate pool: internal nodes with >= MIN_SIZE nodes


def _hk(s: str) -> int:
    return int.from_bytes(hashlib.blake2b(s.encode(), digest_size=8).digest(), "little")


def canonical(t: Tree):
    """Contract presentational wrappers ("div soup"): a `generic` node is
    transparent if it has exactly one child, or if it is the sole child of its
    nearest non-transparent, non-generic ancestor. Returns (ct, orig) where
    orig[i] is the original id of canonical node i. O(n)."""
    from gen import R, ROLES
    G = R["generic"]
    ct, orig = Tree(), []
    cmap = {}
    st = [(0, None, -1)]                   # (orig node, canonical parent, pending family)
    while st:
        v, cp, pend = st.pop()
        fam = t.family[v] if t.family[v] >= 0 else pend
        transparent = False
        if t.role[v] == G and cp is not None:
            if len(t.children[v]) == 1:
                transparent = True
            else:
                cpo = orig[cp]
                if t.role[cpo] != G and len(t.children[cpo]) == 1:
                    transparent = True
        if transparent:
            nid = cp
            down = fam if len(t.children[v]) == 1 else -1   # family moves to the kept child
        else:
            nid = ct.add(ROLES[t.role[v]], cp, t.name[v], t.level[v], t.visited[v], fam)
            orig.append(v)
            down = -1
        cmap[v] = nid
        st.extend((c, nid, down) for c in reversed(t.children[v]))
    return ct, orig


def subtree_sizes(t: Tree) -> list:
    size = [1] * len(t)
    for v in reversed(t.preorder()):
        for c in t.children[v]:
            size[v] += size[c]
    return size


def wl_labels(t: Tree, iters: int = 2) -> list:
    """WL relabelling bottom-up: l0 = role, l_{i+1} = h(l_i, sorted multiset of child l_i).
    Returns a list of per-iteration label arrays. O(iters * n log deg)."""
    labels = [[t.role[v] for v in range(len(t))]]
    for _ in range(iters):
        prev = labels[-1]
        cur = [0] * len(t)
        for v in range(len(t)):
            cur[v] = _hk(f"{prev[v]}|{sorted(prev[c] for c in t.children[v])}")
        labels.append(cur)
    return labels


def node_features(t: Tree, u: int, labels: list) -> Counter:
    """Bag of structural tokens for the subtree rooted at u (depth <= DEPTH)."""
    f = Counter()
    f[f"r:{t.role[u]}"] += 2
    st = [(u, 0)]
    nodes = 0
    while st:
        x, d = st.pop()
        nodes += 1
        if d > 0:
            f[f"d{d}:{t.role[x]}"] += 1
            f[f"e:{t.role[t.parent[x]]}>{t.role[x]}"] += 1
            for i, lab in enumerate(labels[1:], 1):
                f[f"w{i}:{lab[x]}"] += 1
            nm = t.name[x]
            if nm and not t.children[x]:
                f[f"len:{min(len(nm.split()), 8)}"] += 1
        if d < DEPTH:
            st.extend((c, d + 1) for c in t.children[x])
    f[f"sz:{int(np.log2(nodes))}"] += 1
    return f


def hash_vectors(feats: list, dim: int = D) -> np.ndarray:
    X = np.zeros((len(feats), dim), dtype=np.float32)
    for i, f in enumerate(feats):
        for tok, c in f.items():
            h = _hk(tok)
            X[i, h % dim] += (1.0 if (h >> 63) & 1 else -1.0) * np.log1p(c)
    X /= np.maximum(np.linalg.norm(X, axis=1, keepdims=True), 1e-9)
    return X


def simhash64(X: np.ndarray, seed: int = 0) -> np.ndarray:
    rng = np.random.default_rng(seed)
    P = rng.standard_normal((X.shape[1], 64)).astype(np.float32)
    bits = (X @ P) > 0
    return np.packbits(bits, axis=1, bitorder="little").view(np.uint64).ravel()


# ---------------------------------------------------------------------------
class UF:
    def __init__(self, n):
        self.p = list(range(n))

    def find(self, x):
        while self.p[x] != x:
            self.p[x] = self.p[self.p[x]]
            x = self.p[x]
        return x

    def union(self, a, b):
        a, b = self.find(a), self.find(b)
        if a != b:
            self.p[max(a, b)] = min(a, b)


def clusters_from_knn(I: np.ndarray, S: np.ndarray, tau: float) -> np.ndarray:
    uf = UF(I.shape[0])
    for i in range(I.shape[0]):
        for j, s in zip(I[i], S[i]):
            if j >= 0 and j != i and s >= tau:
                uf.union(i, int(j))
    return np.array([uf.find(i) for i in range(I.shape[0])])


def exact_knn(X: np.ndarray, k: int, chunk: int = 4096):
    n = X.shape[0]
    I = np.empty((n, k), dtype=np.int64)
    S = np.empty((n, k), dtype=np.float32)
    for a in range(0, n, chunk):
        sims = X[a:a + chunk] @ X.T
        idx = np.argpartition(-sims, k, axis=1)[:, :k]
        s = np.take_along_axis(sims, idx, axis=1)
        o = np.argsort(-s, axis=1)
        I[a:a + chunk] = np.take_along_axis(idx, o, axis=1)
        S[a:a + chunk] = np.take_along_axis(s, o, axis=1)
    return I, S


# ---------------------------------------------------------------------------
def member_prf(cl: np.ndarray, truth: np.ndarray):
    """Pairwise precision/recall/F1 restricted to ground-truth family members."""
    m = truth >= 0
    cl, tr = cl[m], truth[m]

    def pairs(c):
        return sum(x * (x - 1) // 2 for x in Counter(c).values())
    tp = pairs(list(zip(cl.tolist(), tr.tolist())))
    pred, true = pairs(cl.tolist()), pairs(tr.tolist())
    p = tp / pred if pred else 1.0
    r = tp / true if true else 1.0
    return p, r, (2 * p * r / (p + r) if p + r else 0.0)


def next_similar_accuracy(t: Tree, pool: list, cl: np.ndarray, rank: dict):
    """User-facing metric. From each family member, "jump to next similar" goes to
    the next pool node (document order) in the same cluster. Correct iff that is
    the true next member of the same family. Non-members in the cluster count as
    errors (the jump lands on something that is not the next card)."""
    by_cluster, by_family = defaultdict(list), defaultdict(list)
    for i, v in enumerate(pool):
        by_cluster[cl[i]].append(v)
        if t.family[v] >= 0:
            by_family[t.family[v]].append(v)
    nxt_c = {}
    for lst in by_cluster.values():
        lst.sort(key=rank.get)
        for a, b in zip(lst, lst[1:]):
            nxt_c[a] = b
    ok = tot = 0
    for lst in by_family.values():
        lst.sort(key=rank.get)
        for a, b in zip(lst, lst[1:]):
            tot += 1
            ok += nxt_c.get(a) == b
    return ok / max(tot, 1)


def dedup_knn_clusters(keys: np.ndarray, X: np.ndarray, k: int, tau: float):
    """Group exact duplicates by `keys` first, then link groups whose mean vectors
    have cosine >= tau among their k nearest groups. Returns (cluster ids, #groups,
    kNN seconds). The kNN runs on unique groups only (typically 5-15% of the pool)."""
    uniq, inv = np.unique(keys, return_inverse=True)
    G = np.zeros((len(uniq), X.shape[1]), dtype=np.float32)
    np.add.at(G, inv, X)
    G /= np.maximum(np.linalg.norm(G, axis=1, keepdims=True), 1e-9)
    t0 = time.perf_counter()
    kk = min(k, len(uniq) - 1)
    I, S = exact_knn(G, kk + 1) if kk > 0 else (np.zeros((len(uniq), 0), int), np.zeros((len(uniq), 0)))
    dt = time.perf_counter() - t0
    gcl = clusters_from_knn(I, S, tau)
    return gcl[inv], len(uniq), dt


def divsoup(t: Tree) -> Tree:
    """Strip container semantics, as on SPA pages built from bare <div>s:
    article/listitem/list/group/region become generic. Families are kept."""
    from gen import R
    strip = {R["article"], R["listitem"], R["list"], R["group"], R["region"]}
    t.role = [R["generic"] if r in strip else r for r in t.role]
    return t


def run(n: int, canon: bool = True, soup: bool = False):
    tp = page(n, seed=1)
    if soup:
        tp = divsoup(tp)
    t = canonical(tp)[0] if canon else tp
    order = t.preorder()
    rank = {v: i for i, v in enumerate(order)}
    size = subtree_sizes(t)
    pool = [v for v in order if size[v] >= MIN_SIZE]
    truth = np.array([t.family[v] for v in pool])

    t0 = time.perf_counter()
    skel, _, _, _ = subtree_hashes(t)
    t_skel = time.perf_counter() - t0
    t0 = time.perf_counter()
    labels = wl_labels(t, 2)
    feats = [node_features(t, v, labels) for v in pool]
    X = hash_vectors(feats)
    t_feat = time.perf_counter() - t0
    skel_keys = np.array([skel[v] for v in pool], dtype=np.uint64)

    res = {"baseline: role": np.array([t.role[v] for v in pool], dtype=np.uint64),
           "baseline: role+parent": np.array([t.role[v] * 1000 + t.role[t.parent[v]] for v in pool], dtype=np.uint64),
           "skel-hash (exact)": skel_keys,
           "wl2-label (exact)": np.array([labels[2][v] for v in pool], dtype=np.uint64)}
    t0 = time.perf_counter()
    I, S = exact_knn(X, 16)
    t_knn_all = time.perf_counter() - t0
    res["knn16 all, tau=.90"] = clusters_from_knn(I, S, 0.90)
    info = {}
    for tau in (0.80, 0.85, 0.90, 0.95):
        cl, ng, dt = dedup_knn_clusters(skel_keys, X, 16, tau)
        res[f"dedup+knn16 tau={tau:.2f}"] = cl
        info["groups"], info["knn_groups_s"] = ng, dt
    sh = simhash64(X)
    uf = UF(len(pool))
    for band in range(4):
        b = ((sh >> np.uint64(16 * band)) & np.uint64(0xFFFF)).tolist()
        first = {}
        for i, key in enumerate(b):
            if key in first:
                uf.union(i, first[key])
            else:
                first[key] = i
    res["simhash 4x16 LSH"] = np.array([uf.find(i) for i in range(len(pool))])

    print(f"n={len(tp)} soup={soup} canonical={canon} -> {len(t)} nodes; pool={len(pool)}, members={int((truth >= 0).sum())}, "
          f"unique skeletons={info['groups']}")
    print(f"  Python times: skeleton hashes {t_skel*1e3:.0f} ms (all nodes); WL features D={D} "
          f"{t_feat*1e3:.0f} ms (pool); exact kNN pool {t_knn_all*1e3:.0f} ms; "
          f"exact kNN on unique groups {info['knn_groups_s']*1e3:.1f} ms")
    print(f"  {'method':24s} {'memP':>6s} {'memR':>6s} {'F1':>6s} {'next-sim':>8s} {'clusters':>8s}")
    out = {}
    for name, cl in res.items():
        p, r, f1 = member_prf(np.asarray(cl), truth)
        acc = next_similar_accuracy(t, pool, np.asarray(cl), rank)
        out[name] = (p, r, f1, acc)
        print(f"  {name:24s} {p:6.3f} {r:6.3f} {f1:6.3f} {acc:8.3f} {len(set(np.asarray(cl).tolist())):8d}")
    return out


if __name__ == "__main__":
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 10_000
    for soup in (False, True):
        run(n, canon=False, soup=soup)
        run(n, canon=True, soup=soup)
