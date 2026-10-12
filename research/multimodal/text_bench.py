"""Text-encoder latency and quality on the real help corpus.

  * latency per string (batch 1, batch 64) for multilingual-e5-small (int8) and MiniLM-L6 (int8);
  * cross-lingual alignment: English item -> its Russian translation among all Russian
    items (pages whose role sequence matches), top-1 retrieval;
  * topical coherence: kNN (k=10) page-label agreement, vs a hashed char-3-gram baseline.

Writes out/text_emb_C.npy (e5 embeddings of the English corpus) for som.py.
"""
from __future__ import annotations

import json
import os
from collections import defaultdict

import numpy as np

from corpus import load
from encoders import TextEncoder, timeit
import hashlib

TEXT_ROLES = {"heading", "paragraph", "listitem", "link", "cell", "note"}


def char_ngrams(texts, dim=384, n=3):
    X = np.zeros((len(texts), dim), np.float32)
    for i, t in enumerate(texts):
        t = f"  {t.lower()}  "
        for j in range(len(t) - n + 1):
            h = int.from_bytes(hashlib.blake2b(t[j:j + n].encode(), digest_size=8).digest(), "little")
            X[i, h % dim] += 1.0 if (h >> 63) & 1 else -1.0
    return X / np.maximum(np.linalg.norm(X, axis=1, keepdims=True), 1e-9)


def knn_agree(X, lab, k=10):
    S = X @ X.T
    np.fill_diagonal(S, -np.inf)
    I = np.argsort(-S, 1)[:, :k]
    lab = np.asarray(lab)
    return float(np.mean(lab[I] == lab[:, None]))


def main():
    en, ru = load("C"), load("ru")
    res = {}
    # aligned pairs
    by = defaultdict(list)
    for i in range(len(ru)):
        by[ru.page[i]].append(i)
    byen = defaultdict(list)
    for i in range(len(en)):
        byen[en.page[i]].append(i)
    pairs = []
    for p, ri in by.items():
        ei = byen.get(p, [])
        if len(ei) == len(ri) and [en.role[i] for i in ei] == [ru.role[i] for i in ri]:
            pairs += [(a, b) for a, b in zip(ei, ri) if en.role[a] in TEXT_ROLES and ru.text[b] != en.text[a]]
    res["aligned_pairs"] = len(pairs)
    idx_en = [i for i in range(len(en)) if en.role[i] in TEXT_ROLES]
    texts = [en.text[i] for i in idx_en]
    for kind in ["e5-small-q8", "minilm-q8"]:
        enc = TextEncoder(kind, threads=4)
        r = {}
        r["ms_b1_p50"], r["ms_b1_p99"] = timeit(lambda: enc.encode(["Close the current tab"]), reps=50)
        p50, _ = timeit(lambda: enc.encode(texts[:64]), reps=5, warm=1)
        r["ms_per_text_b64"] = p50 / 64
        E = enc.encode(texts)
        r["knn10_page_agree"] = knn_agree(E, [en.page[i] for i in idx_en])
        r["knn10_app_agree"] = knn_agree(E, [en.app[i] for i in idx_en])
        A = enc.encode([en.text[a] for a, _ in pairs])
        ru_idx = [i for i in range(len(ru)) if ru.role[i] in TEXT_ROLES]
        B = enc.encode([ru.text[i] for i in ru_idx])
        pos = {b: j for j, b in enumerate(ru_idx)}
        hit = (A @ B.T).argmax(1) == np.array([pos[b] for _, b in pairs])
        r["xling_top1"] = float(hit.mean())
        res[kind] = r
        print(kind, r, flush=True)
        if kind == "e5-small-q8":
            os.makedirs("out", exist_ok=True)
            full = np.zeros((len(en), E.shape[1]), np.float32)
            full[idx_en] = E
            np.save("out/text_emb_C.npy", full)
    X = char_ngrams(texts)
    res["char3"] = {"knn10_page_agree": knn_agree(X, [en.page[i] for i in idx_en]),
                    "knn10_app_agree": knn_agree(X, [en.app[i] for i in idx_en])}
    print("char3", res["char3"])
    json.dump(res, open("out/text.json", "w"), indent=1)


if __name__ == "__main__":
    main()
