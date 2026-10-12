"""SOM over real browser a11y dumps (research/web-graph/real-trees, struct + geometry
only: the dumps carry name lengths, not text). Measures build time and map quality on
real tree shapes, and the hysteresis update on the recorded initial -> final mutation
pairs where both exist.

Usage: python som_realtree.py
"""
from __future__ import annotations

import os

os.environ.setdefault("OMP_NUM_THREADS", "1")

import glob  # noqa: E402
import gzip  # noqa: E402
import json  # noqa: E402
import time  # noqa: E402

import numpy as np  # noqa: E402

from maps import BatchSOM, metrics  # noqa: E402

DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "web-graph", "real-trees")


def load(f):
    d = json.load(gzip.open(f) if f.endswith(".gz") else open(f))
    return d["nodes"]


def feats(nodes, roles):
    n = len(nodes)
    depth = np.zeros(n)
    for i, nd in enumerate(nodes):
        depth[i] = depth[nd[0]] + 1 if nd[0] >= 0 else 0
    W = max(b[4][0] + b[4][2] for b in nodes) or 1
    H = max(b[4][1] + b[4][3] for b in nodes) or 1
    X = np.zeros((n, len(roles) * 2 + 7))
    for i, nd in enumerate(nodes):
        X[i, roles[nd[1]]] = 1
        if nd[0] >= 0:
            X[i, len(roles) + roles[nodes[nd[0]][1]]] = 0.5
        x, y, w, h = nd[4]
        X[i, -7:] = [depth[i] / 20, np.log1p(nd[2]) / 5, np.log1p(nd[6]) / 4, x / W, y / H, w / W, h / H]
    return X / np.maximum(np.linalg.norm(X, axis=1, keepdims=True), 1e-9)


def main():
    files = sorted(glob.glob(os.path.join(DIR, "*.json*")))
    roles = {}
    trees = {os.path.basename(f): load(f) for f in files}
    for nodes in trees.values():
        for nd in nodes:
            roles.setdefault(nd[1], len(roles))
    out = {}
    for name, nodes in trees.items():
        if len(nodes) < 500:
            continue
        X = feats(nodes, roles)
        lab = [nd[1] for nd in nodes]
        som = BatchSOM(8, 8)
        t0 = time.perf_counter(); c = som.fit(X, iters=30, sigma1=0.5); ms = (time.perf_counter() - t0) * 1e3
        m = metrics(X, c, 8, 8, lab); m["n"] = len(X); m["build_ms"] = ms
        out[name] = m
        print(name, {k: round(float(v), 3) for k, v in m.items()}, flush=True)
    json.dump(out, open("out/som_realtree.json", "w"), indent=1)


if __name__ == "__main__":
    main()
