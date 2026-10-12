"""SOM vs GTM vs Growing Grid vs embedding+snap on the real help corpus.

Part 1 (static): 8x8 maps of ~1.9k fused (text+struct+geom) item vectors; quality,
  occupancy and build time; modality ablation for the SOM.
Part 2 (dynamic): start from 70% of the items, then 10 mutation steps (each deletes
  3%, inserts 3% from a held-out reserve, modifies 3% by swapping in another item's
  text from the same page). Per method: per-step share of untouched survivors that
  change cell, mean displacement, cumulative drift vs step 0, final quality, update time.

Usage: python som_bench.py  (needs out/text_emb_C.npy from text_bench.py)
"""
from __future__ import annotations

import os

os.environ.setdefault("OMP_NUM_THREADS", "1")
os.environ.setdefault("OPENBLAS_NUM_THREADS", "1")
os.environ.setdefault("NUMBA_NUM_THREADS", "1")

import json  # noqa: E402
import time
import warnings

import numpy as np

from features import corpus_blocks, fuse
from maps import (GTM, BatchSOM, GrowingGrid, metrics, pca_basis, snap, sqdist, stability)

warnings.filterwarnings("ignore")
ROWS = COLS = 8
W_DEFAULT = {"text": 1.0, "struct": 0.5, "geom": 0.25}


def timed(f):
    t0 = time.perf_counter(); r = f(); return r, (time.perf_counter() - t0) * 1e3


def tsne2(X, seed=0):
    from sklearn.manifold import TSNE
    return TSNE(2, perplexity=30, init="pca", random_state=seed).fit_transform(X)


def umap_model(X, seed=0):
    import umap
    return umap.UMAP(n_neighbors=15, min_dist=0.1, random_state=seed).fit(X)


def static(it, idx, blocks):
    X = fuse(blocks, W_DEFAULT)
    pages = [it.page[i] for i in idx]; apps = [it.app[i] for i in idx]
    res = {"n": len(X), "dim": X.shape[1]}

    def rec(name, cells, ms):
        m = metrics(X, cells, ROWS, COLS, pages)
        m["app_purity"] = metrics(X, cells, ROWS, COLS, apps)["purity"]
        m["build_ms"] = ms
        res[name] = m
        print(name, {k: round(v, 3) for k, v in m.items()}, flush=True)

    som = BatchSOM(ROWS, COLS)
    cells, ms = timed(lambda: som.fit(X, iters=30)); rec("som_batch", cells, ms)
    for s1 in (0.5, 0.8, 1.2, 2.0):
        sm = BatchSOM(ROWS, COLS); c = sm.fit(X, iters=30, sigma1=s1)
        _, D = sm.bmu(X); o = np.argsort(D, 1)[:, :2]
        ra, ca = np.divmod(o[:, 0], COLS); rb, cb = np.divmod(o[:, 1], COLS)
        te_cb = float(np.mean(np.maximum(abs(ra - rb), abs(ca - cb)) > 1))
        m = metrics(X, c, ROWS, COLS, pages); m["te_codebook"] = te_cb
        res[f"som_sigma1_{s1}"] = m
        print("som sigma1", s1, {k: round(v, 3) for k, v in m.items()}, flush=True)
    g = GTM(ROWS, COLS, m=5)
    cells, ms = timed(lambda: g.fit(X, iters=30)); rec("gtm", cells, ms)
    gg = GrowingGrid(ROWS * COLS)
    cells, ms = timed(lambda: gg.fit(X)); rec(f"growing_grid_{gg.som.rows}x{gg.som.cols}",
                                              cells, ms) if gg.som.rows == ROWS else None
    if gg.som.rows != ROWS:
        res["growing_grid_shape"] = [gg.som.rows, gg.som.cols]
        m = metrics(X, cells, gg.som.rows, gg.som.cols, pages); m["build_ms"] = ms
        res["growing_grid"] = m; print("growing_grid", gg.som.rows, gg.som.cols, m)
    mu, V, _ = pca_basis(X)
    cells, ms = timed(lambda: snap((X - mu) @ V.T, ROWS, COLS)[0]); rec("pca_snap", cells, ms)
    Y, ms = timed(lambda: tsne2(X)); rec("tsne_snap", snap(Y, ROWS, COLS)[0], ms)
    umap_model(X[:200])                              # numba JIT warm-up, not timed
    mdl, ms = timed(lambda: umap_model(X)); rec("umap_snap", snap(mdl.embedding_, ROWS, COLS)[0], ms)
    # modality ablation (SOM)
    for name, w in [("text", {"text": 1}), ("struct", {"struct": 1}), ("struct+geom", {"struct": 1, "geom": .5}),
                    ("fused", W_DEFAULT), ("fused_text_heavy", {"text": 1, "struct": .25, "geom": .1})]:
        Xa = fuse(blocks, w)
        c = BatchSOM(ROWS, COLS).fit(Xa, iters=30)
        res[f"ablation_{name}"] = {"page_purity_in_text_space": metrics(fuse(blocks, {"text": 1}), c, ROWS, COLS, pages)["purity"],
                                   "app_purity": metrics(Xa, c, ROWS, COLS, apps)["purity"]}
        print("ablation", name, res[f"ablation_{name}"], flush=True)
    return res


# ---------------------------------------------------------------------------
class World:
    """Mutation process over item ids; X_all holds every candidate vector."""

    def __init__(self, X_all, page_of, seed=0):
        self.rng = np.random.default_rng(seed)
        self.X_all = X_all.copy()
        n = len(X_all)
        perm = self.rng.permutation(n)
        self.live = list(perm[: int(0.7 * n)])
        self.reserve = list(perm[int(0.7 * n):])
        self.page_of = np.asarray(page_of)
        self.X = X_all.copy()

    def step(self, frac=0.03):
        n = len(self.live)
        k = max(1, int(frac * n))
        live = np.array(self.live)
        dele = set(self.rng.choice(live, k, replace=False).tolist())
        remaining = [i for i in self.live if i not in dele]
        ins = [self.reserve.pop() for _ in range(min(k, len(self.reserve)))]
        mod = self.rng.choice(remaining, k, replace=False).tolist()
        for i in mod:   # swap in the text of another item from the same page
            same = np.where(self.page_of == self.page_of[i])[0]
            j = self.rng.choice(same)
            self.X[i] = self.X_all[j]
        self.live = remaining + ins
        return set(ins), set(mod)


def dynamic(it, idx, blocks, steps=10):
    X_all = fuse(blocks, W_DEFAULT)
    pages = np.array([it.page[i] for i in idx])
    res = {}
    methods = ["som_frozen", "som_incr", "som_incr_hyst", "som_warm_retrain", "som_cold_retrain",
               "gtm_warm", "umap_transform", "umap_retrain", "tsne_retrain"]
    for meth in methods:
        w = World(X_all, pages, seed=1)
        Xl = w.X[w.live]
        t0 = time.perf_counter()
        if meth.startswith("som"):
            som = BatchSOM(ROWS, COLS); cells = som.fit(Xl, iters=30)
            b, D = som.bmu(Xl)
            eps = 0.25 * float(np.median(D[np.arange(len(Xl)), b]))
        elif meth.startswith("gtm"):
            g = GTM(ROWS, COLS, m=5); cells = g.fit(Xl, iters=30)
        elif meth.startswith("umap"):
            mdl = umap_model(Xl); cells, edges = snap(mdl.embedding_, ROWS, COLS)
        else:
            cells, edges = snap(tsne2(Xl), ROWS, COLS)
        cur = dict(zip(w.live, cells))
        first = dict(cur)
        per_step, times = [], []
        touched = set()
        n_steps = 3 if meth == "tsne_retrain" else steps
        if meth == "umap_transform":
            pos = dict(zip(w.live, mdl.embedding_))
        for s in range(n_steps):
            ins, mod = w.step()
            touched |= mod
            Xl = w.X[w.live]
            prev = np.array([cur.get(i, -1) for i in w.live])
            t0 = time.perf_counter()
            if meth == "som_frozen":
                cells = som.bmu(Xl)[0]
            elif meth == "som_incr":
                cells = som.update(Xl, prev, sigma=0.8, lam=0.1 * len(Xl) / (ROWS * COLS), iters=2)
            elif meth == "som_incr_hyst":
                cells = som.update(Xl, prev, sigma=0.8, lam=0.1 * len(Xl) / (ROWS * COLS), iters=2, eps=eps)
            elif meth == "som_warm_retrain":
                cells = som.fit(Xl, iters=10, sigma0=2.0, sigma1=0.8, init=False)
            elif meth == "som_cold_retrain":
                som = BatchSOM(ROWS, COLS); cells = som.fit(Xl, iters=30)
            elif meth == "gtm_warm":
                cells = g.fit(Xl, iters=5, init=False)
            elif meth == "umap_transform":
                fresh = [i for i in w.live if i in ins or i in mod or i not in pos]
                if fresh:
                    for i, y in zip(fresh, mdl.transform(w.X[fresh])):
                        pos[i] = y
                cells, _ = snap(np.array([pos[i] for i in w.live]), ROWS, COLS, edges)
            elif meth == "umap_retrain":
                mdl = umap_model(Xl, seed=0); cells, _ = snap(mdl.embedding_, ROWS, COLS)
            else:
                cells, _ = snap(tsne2(Xl), ROWS, COLS)
            times.append((time.perf_counter() - t0) * 1e3)
            new = dict(zip(w.live, cells))
            keep = [i for i in w.live if i in cur and i not in mod and i not in ins]
            align = meth in ("umap_retrain", "tsne_retrain", "som_cold_retrain")
            st = stability(np.array([cur[i] for i in keep]), np.array([new[i] for i in keep]), ROWS, COLS, align)
            per_step.append(st)
            cur = new
        keep0 = [i for i in w.live if i in first and i not in touched]
        drift = stability(np.array([first[i] for i in keep0]), np.array([cur[i] for i in keep0]), ROWS, COLS,
                          meth in ("umap_retrain", "tsne_retrain", "som_cold_retrain"))
        q = metrics(Xl, np.array([cur[i] for i in w.live]), ROWS, COLS, pages[w.live])
        res[meth] = {"moved_per_step": float(np.mean([p["moved"] for p in per_step])),
                     "disp_per_step": float(np.mean([p["disp"] for p in per_step])),
                     "drift_after_10": drift, "final": q,
                     "update_ms_p50": float(np.median(times)), "update_ms_max": float(np.max(times)), "steps": n_steps}
        print(meth, json.dumps(res[meth]), flush=True)
    return res


if __name__ == "__main__":
    it, idx, blocks = corpus_blocks()
    out = {"static": static(it, idx, blocks), "dynamic": dynamic(it, idx, blocks)}
    json.dump(out, open("out/som.json", "w"), indent=1)
