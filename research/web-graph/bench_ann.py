"""ANN index benchmark on real structural vectors from the synthetic pages.

Datasets (D = 128, cosine on L2-normalised vectors):
  pool100k   candidate pool of the 100k-node page (canonical, size >= 3)
  all100k    every canonical node of the 100k-node page (heavy exact duplication)
  uniq100k   unique skeleton groups of the pool (what the pipeline actually indexes)

Libraries: numpy brute force, faiss IndexFlatIP, faiss IndexHNSWFlat, hnswlib, usearch.
All single-threaded (a screen reader background worker gets one core).

Run: python bench_ann.py
"""
from __future__ import annotations

import os

os.environ.setdefault("OMP_NUM_THREADS", "1")
os.environ.setdefault("OPENBLAS_NUM_THREADS", "1")
os.environ.setdefault("MKL_NUM_THREADS", "1")

import time  # noqa: E402

import numpy as np  # noqa: E402

import embed as E  # noqa: E402
from diff import subtree_hashes  # noqa: E402
from gen import page  # noqa: E402

K = 10


def datasets():
    t = E.canonical(page(100_000, seed=1))[0]
    order = t.preorder()
    size = E.subtree_sizes(t)
    labels = E.wl_labels(t, 2)
    skel = subtree_hashes(t)[0]
    pool = [v for v in order if size[v] >= E.MIN_SIZE]
    Xp = E.hash_vectors([E.node_features(t, v, labels) for v in pool])
    Xa = E.hash_vectors([E.node_features(t, v, labels) for v in order])
    keys = np.array([skel[v] for v in pool], dtype=np.uint64)
    _, first = np.unique(keys, return_index=True)
    Xu = Xp[np.sort(first)]
    return {"uniq100k": Xu, "pool100k": Xp, "all100k": Xa}


def pct(a, q):
    return float(np.percentile(np.asarray(a), q))


def bench(name, X, Q, truth, build, query, add=None, remove=None):
    t0 = time.perf_counter()
    ix = build(X)
    tb = time.perf_counter() - t0
    lat, rec = [], []
    for i in range(len(Q)):
        a = time.perf_counter()
        ids = query(ix, Q[i:i + 1])
        lat.append((time.perf_counter() - a) * 1e6)
        # recall by similarity value (ties among exact duplicates are equivalent)
        got = np.sort(X[np.asarray(ids, dtype=np.int64)[:K]] @ Q[i])[::-1]
        rec.append(np.mean(got >= truth[i][-1] - 1e-5))
    s = f"  {name:22s} build {tb*1e3:8.1f} ms  query p50 {pct(lat,50):7.1f} us p99 {pct(lat,99):7.1f} us  recall@{K} {np.mean(rec):.3f}"
    if add is not None:
        n_add = 200
        a = time.perf_counter()
        add(ix, X[:n_add], np.arange(len(X), len(X) + n_add))
        s += f"  add {(time.perf_counter()-a)*1e6/n_add:6.1f} us/vec"
    if remove is not None:
        a = time.perf_counter()
        remove(ix, np.arange(200))
        s += f"  remove {(time.perf_counter()-a)*1e6/200:6.2f} us/vec"
    print(s)


def main():
    import faiss
    import hnswlib
    from usearch.index import Index

    faiss.omp_set_num_threads(1)
    rng = np.random.default_rng(0)
    for dname, X in datasets().items():
        n, d = X.shape
        Q = X[rng.choice(n, 300, replace=False)] + 0.01 * rng.standard_normal((300, d)).astype(np.float32)
        Q /= np.linalg.norm(Q, axis=1, keepdims=True)
        sims = Q @ X.T
        truth = np.sort(np.partition(-sims, K, axis=1)[:, :K], axis=1) * -1
        print(f"{dname}: n={n} d={d}")

        bench("numpy brute", X, Q, truth, lambda X: X,
              lambda ix, q: np.argpartition(-(ix @ q[0]), K)[:K])

        def fflat(X):
            ix = faiss.IndexFlatIP(d)
            ix.add(X)
            return ix
        bench("faiss Flat", X, Q, truth, fflat, lambda ix, q: ix.search(q, K)[1][0])

        def fhnsw(X):
            ix = faiss.IndexHNSWFlat(d, 16, faiss.METRIC_INNER_PRODUCT)
            ix.hnsw.efConstruction = 100
            ix.add(X)
            ix.hnsw.efSearch = 64
            return ix
        bench("faiss HNSW M16", X, Q, truth, fhnsw, lambda ix, q: ix.search(q, K)[1][0],
              add=lambda ix, v, ids: ix.add(v))

        def hb(X):
            ix = hnswlib.Index(space="ip", dim=d)
            ix.init_index(max_elements=n + 1000, ef_construction=100, M=16, allow_replace_deleted=True)
            ix.set_num_threads(1)
            ix.add_items(X, np.arange(n))
            ix.set_ef(64)
            return ix
        bench("hnswlib M16", X, Q, truth, hb, lambda ix, q: ix.knn_query(q, K)[0][0],
              add=lambda ix, v, ids: ix.add_items(v, ids),
              remove=lambda ix, ids: [ix.mark_deleted(int(i)) for i in ids])

        for dt in ("f32", "i8"):
            def ub(X, dt=dt):
                ix = Index(ndim=d, metric="ip", dtype=dt, connectivity=16, expansion_add=100, expansion_search=64)
                ix.add(np.arange(n), X, threads=1)
                return ix
            bench(f"usearch M16 {dt}", X, Q, truth, ub,
                  lambda ix, q: ix.search(q[0], K, threads=1).keys,
                  add=lambda ix, v, ids: ix.add(ids, v, threads=1),
                  remove=lambda ix, ids: ix.remove(ids))


if __name__ == "__main__":
    main()
