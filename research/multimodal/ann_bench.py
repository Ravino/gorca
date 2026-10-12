"""Vector search for the multimodal store: when do HNSW and PQ pay off?

Data: `som_scale.realistic` (sphere mixture shaped like e5 text embeddings), D=384.
Queries: 500 held-out points from the same distribution. Ground truth: exact top-10 by
inner product. Query timing is single-threaded (one actor), build may use 8 threads.

Usage: python ann_bench.py [N ...]   (default 100000 1000000)
"""
from __future__ import annotations

import json
import sys
import time

import faiss
import numpy as np
from usearch.index import Index

from som_scale import realistic


def recall(I, G):
    return float(np.mean([len(set(a) & set(b)) / len(b) for a, b in zip(I, G)]))


def qtimes(fn, Q):
    ts = []
    out = []
    for q in Q:
        t0 = time.perf_counter(); out.append(fn(q)); ts.append((time.perf_counter() - t0) * 1e3)
    ts = np.array(ts)
    return out, float(np.median(ts)), float(np.percentile(ts, 99))


def run(n, d=384, k=10):
    X = realistic(n, d, seed=3).astype(np.float32)
    Q = realistic(500, d, seed=4).astype(np.float32)
    res = {"n": n, "d": d}
    faiss.omp_set_num_threads(8)
    flat = faiss.IndexFlatIP(d); flat.add(X)
    _, G = flat.search(Q, k)
    faiss.omp_set_num_threads(1)
    _, p50, p99 = qtimes(lambda q: flat.search(q[None], k), Q)
    res["flat_f32"] = {"p50_ms": p50, "p99_ms": p99, "recall": 1.0, "bytes_per_vec": 4 * d}
    print(res["flat_f32"], flush=True)

    # brute force over float16 copy (numpy) - what a Rust f16 scan would do
    X16 = X.astype(np.float16)
    out, p50, p99 = qtimes(lambda q: np.argpartition(-(X16 @ q.astype(np.float16)), k)[:k], Q[:100])
    res["flat_f16_numpy"] = {"p50_ms": p50, "p99_ms": p99, "recall": recall(out, G[:100]), "bytes_per_vec": 2 * d}
    print("f16", res["flat_f16_numpy"], flush=True)

    for m in (48, 96):
        nlist = int(4 * np.sqrt(n))
        faiss.omp_set_num_threads(8)
        q = faiss.IndexFlatIP(d)
        ivf = faiss.IndexIVFPQ(q, d, nlist, m, 8, faiss.METRIC_INNER_PRODUCT)
        t0 = time.perf_counter(); ivf.train(X[:min(n, 100_000)]); ivf.add(X); bt = time.perf_counter() - t0
        faiss.omp_set_num_threads(1)
        for nprobe in (8, 32):
            ivf.nprobe = nprobe
            # re-rank the PQ shortlist (k*10) exactly from float vectors kept elsewhere
            def srch(qv):
                _, I = ivf.search(qv[None], k * 10)
                I = I[0][I[0] >= 0]
                s = X[I] @ qv
                return I[np.argsort(-s)[:k]]
            out, p50, p99 = qtimes(srch, Q)
            key = f"ivfpq_m{m}_nprobe{nprobe}_rerank"
            res[key] = {"p50_ms": p50, "p99_ms": p99, "recall": recall(out, G), "build_s": bt,
                        "bytes_per_vec_index": m}
            print(key, res[key], flush=True)

    for dtype in ("f16", "i8"):
        idx = Index(ndim=d, metric="ip", dtype=dtype, connectivity=16, expansion_add=128, expansion_search=64)
        t0 = time.perf_counter(); idx.add(np.arange(n), X, threads=8); bt = time.perf_counter() - t0
        out, p50, p99 = qtimes(lambda qv: idx.search(qv, k, threads=1).keys, Q)
        key = f"usearch_hnsw16_{dtype}"
        res[key] = {"p50_ms": p50, "p99_ms": p99, "recall": recall(out, G), "build_s": bt,
                    "bytes_per_vec_index": idx.memory_usage / n}
        print(key, res[key], flush=True)
        # update cost: remove + re-add 1000 vectors
        ids = np.arange(1000)
        t0 = time.perf_counter(); idx.remove(ids); idx.add(ids, X[:1000], threads=1)
        res[key]["update_us_per_vec"] = (time.perf_counter() - t0) * 1e6 / 1000
    return res


if __name__ == "__main__":
    ns = [int(a) for a in sys.argv[1:]] or [100_000, 1_000_000]
    allr = {}
    for n in ns:
        allr[n] = run(n)
    json.dump(allr, open(f"out/ann_{'_'.join(map(str, ns))}.json", "w"), indent=1)
