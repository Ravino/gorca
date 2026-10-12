"""Per-node multimodal feature blocks and fusion.

Blocks (each L2-normalised separately):
  struct : role one-hot, parent-role one-hot, depth, log text length, position in page
  text   : multilingual-e5-small sentence embedding (384-d), see text_bench.py
  geom   : (x, y, w, h) normalised by the window / document extent
  visual : optional image embedding (CLIP-class), absent for the help corpus

Fusion = weighted concatenation of unit blocks:
    v = [w_s s, w_t t, w_g g] / sqrt(w_s^2 + w_t^2 + w_g^2)
so ||u - v||^2 = sum_m w_m^2 ||u_m - v_m||^2 / sum w^2, i.e. a weighted product
metric of per-modality chordal (cosine) distances. Missing blocks are zero, which
adds a constant w_m^2 per mismatched pair; we therefore renormalise by the
weights of the blocks that are present (see `fuse`).
"""
from __future__ import annotations

import numpy as np

from corpus import ROLES, Items, load

TEXT_ROLES = {"heading", "paragraph", "listitem", "link", "cell", "note"}


def struct_block(it: Items) -> np.ndarray:
    R = {r: i for i, r in enumerate(ROLES)}
    n = len(it)
    X = np.zeros((n, 2 * len(ROLES) + 3), np.float32)
    page_len = {}
    for i in range(n):
        page_len[it.page[i]] = max(page_len.get(it.page[i], 0), it.ordinal[i] + 1)
    for i in range(n):
        X[i, R[it.role[i]]] = 1
        if it.parent[i] >= 0:
            X[i, len(ROLES) + R[it.role[it.parent[i]]]] = 0.5
        X[i, -3] = it.depth[i] / 8
        X[i, -2] = np.log1p(len(it.text[i])) / 6
        X[i, -1] = it.ordinal[i] / page_len[it.page[i]]
    return X


def geom_block(it: Items) -> np.ndarray:
    B = np.array(it.bounds, np.float32)
    ymax = {}
    for i, p in enumerate(it.page):
        ymax[p] = max(ymax.get(p, 1.0), B[i, 1] + B[i, 3])
    Y = np.array([ymax[p] for p in it.page], np.float32)
    return np.stack([B[:, 0] / 900, B[:, 1] / Y, B[:, 2] / 900, B[:, 3] / 100], 1)


def unit(X):
    return X / np.maximum(np.linalg.norm(X, axis=1, keepdims=True), 1e-9)


def fuse(blocks: dict, weights: dict) -> np.ndarray:
    parts, wsum = [], 0.0
    for k, w in weights.items():
        if w > 0 and k in blocks:
            parts.append(w * unit(blocks[k])); wsum += w * w
    return (np.hstack(parts) / np.sqrt(wsum)).astype(np.float64)


def corpus_blocks(text_emb_path: str = "out/text_emb_C.npy"):
    it = load("C")
    T = np.load(text_emb_path)
    idx = np.array([i for i in range(len(it)) if it.role[i] in TEXT_ROLES])
    blocks = {"struct": struct_block(it)[idx], "geom": geom_block(it)[idx], "text": T[idx]}
    return it, idx, blocks
