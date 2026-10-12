"""Plain-assert tests for maps.py and the test vectors for the Rust port.

  python test_maps.py          run tests
  python test_maps.py --write  regenerate testvec-som.json (commit it)

Test vector: 300 points in R^8 (3 Gaussian blobs, seed 7), 4x4 batch SOM, 20 iters,
sigma 2.0 -> 0.5, PCA init with the sign convention in maps.pca_basis. Rust must
reproduce the codebook to 1e-6 and the assignments exactly; then the incremental
update with hysteresis on a fixed mutation.
"""
from __future__ import annotations

import os

os.environ.setdefault("OMP_NUM_THREADS", "1")

import json  # noqa: E402
import sys  # noqa: E402

import numpy as np  # noqa: E402

from maps import GTM, BatchSOM, metrics, snap, sqdist, stability  # noqa: E402

TV = os.path.join(os.path.dirname(os.path.abspath(__file__)), "testvec-som.json")


def blobs(seed=7, n=300, d=8):
    rng = np.random.default_rng(seed)
    C = rng.standard_normal((3, d)) * 3
    lab = np.arange(n) % 3
    return C[lab] + rng.standard_normal((n, d)), lab


def build_vector():
    X, _ = blobs()
    som = BatchSOM(4, 4)
    cells = som.fit(X, iters=20, sigma0=2.0, sigma1=0.5)
    W1 = som.W.copy()
    X2 = X.copy()
    X2[:10] += 0.5            # mutate 10 items
    prev = cells.copy()
    _, D = som.bmu(X)
    eps = 0.25 * float(np.median(D[np.arange(len(X)), cells]))
    cells2 = som.update(X2, prev, sigma=0.5, lam=2.0, iters=2, eps=eps)
    return {"X": X.tolist(), "rows": 4, "cols": 4, "iters": 20, "sigma0": 2.0, "sigma1": 0.5,
            "W": W1.tolist(), "cells": cells.tolist(),
            "update": {"delta_first10": 0.5, "sigma": 0.5, "lam": 2.0, "iters": 2, "eps": eps,
                       "W": som.W.tolist(), "cells": cells2.tolist()}}


def test_vector():
    v = build_vector()
    ref = json.load(open(TV))
    assert np.allclose(v["W"], ref["W"], atol=1e-6)
    assert v["cells"] == ref["cells"]
    assert np.allclose(v["update"]["W"], ref["update"]["W"], atol=1e-6)
    assert v["update"]["cells"] == ref["update"]["cells"]


def test_som_deterministic_and_ordered():
    X, lab = blobs()
    a = BatchSOM(6, 6).fit(X, iters=25)
    b = BatchSOM(6, 6).fit(X, iters=25)
    assert np.array_equal(a, b), "PCA init + batch rule must be deterministic"
    m = metrics(X, a, 6, 6, lab)
    assert m["purity"] > 0.99 and m["trust"] > 0.8, m


def test_hysteresis_keeps_untouched_items():
    X, _ = blobs()
    som = BatchSOM(6, 6)
    cells = som.fit(X, iters=25)
    _, D = som.bmu(X)
    eps = 0.25 * float(np.median(D[np.arange(len(X)), cells]))
    rng = np.random.default_rng(0)
    for _ in range(10):
        ids = rng.choice(len(X), 5, replace=False)
        X[ids] += rng.standard_normal((5, X.shape[1])) * 0.3
        new = som.update(X, cells, sigma=0.8, lam=5.0, iters=2, eps=eps)
        others = np.setdiff1d(np.arange(len(X)), ids)
        moved = np.mean(new[others] != cells[others])
        assert moved < 0.02, moved
        cells = new


def test_bmu_and_snap():
    rng = np.random.default_rng(1)
    X, W = rng.standard_normal((50, 4)), rng.standard_normal((9, 4))
    ref = np.array([np.argmin(((W - x) ** 2).sum(1)) for x in X])
    assert np.array_equal(sqdist(X, W).argmin(1), ref)
    Y = rng.uniform(size=(1000, 2))
    c, edges = snap(Y, 4, 4)
    assert np.bincount(c, minlength=16).min() > 30
    c2, _ = snap(Y, 4, 4, edges)
    assert np.array_equal(c, c2)
    s = stability(c, c, 4, 4)
    assert s["moved"] == 0 and s["disp"] == 0


def test_gtm_runs():
    X, lab = blobs()
    c = GTM(5, 5, m=4).fit(X, iters=15)
    assert c.min() >= 0 and c.max() < 25
    assert metrics(X, c, 5, 5, lab)["purity"] > 0.95


if __name__ == "__main__":
    if "--write" in sys.argv:
        json.dump(build_vector(), open(TV, "w"))
        print("wrote", TV)
    for name, f in list(globals().items()):
        if name.startswith("test_"):
            f(); print("ok", name)
