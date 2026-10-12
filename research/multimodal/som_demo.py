"""Render the 8x8 semantic map of the English help corpus as text: each cell gets a
spoken label (top distinctive words by TF-IDF over member texts + dominant app/role),
which is what a user would hear when arrowing over the map.

Usage: python som_demo.py  -> out/som_map_C.txt
"""
from __future__ import annotations

import os

os.environ.setdefault("OMP_NUM_THREADS", "1")

import math  # noqa: E402
import re  # noqa: E402
from collections import Counter  # noqa: E402

import numpy as np  # noqa: E402

from features import corpus_blocks, fuse  # noqa: E402
from maps import BatchSOM  # noqa: E402

STOP = set("the a an and or of to in on for is are be by with this that you your it as from can at use "
           "if will not which when has have all also into then there these their its was were may".split())


def words(t):
    return [w for w in re.findall(r"[a-z][a-z\-]{2,}", t.lower()) if w not in STOP]


def main():
    it, idx, blocks = corpus_blocks()
    X = fuse(blocks, {"text": 1.0, "struct": 0.5, "geom": 0.25})
    som = BatchSOM(8, 8)
    cells = som.fit(X, iters=30, sigma1=0.5)
    docs = {j: Counter() for j in range(64)}
    for k, i in enumerate(idx):
        docs[cells[k]].update(words(it.text[i]))
    df = Counter()
    for c in docs.values():
        df.update(set(c))
    lines = []
    for r in range(8):
        for c in range(8):
            j = r * 8 + c
            members = [idx[k] for k in np.where(cells == j)[0]]
            if not members:
                lines.append(f"[{r},{c}] (empty)"); continue
            tf = docs[j]
            top = sorted(tf, key=lambda w: -tf[w] * math.log(64 / df[w]))[:4]
            app = Counter(it.app[i] for i in members).most_common(1)[0]
            role = Counter(it.role[i] for i in members).most_common(1)[0]
            lines.append(f"[{r},{c}] n={len(members):3d} {app[0]}({app[1]}) {role[0]}s: {', '.join(top)}")
    os.makedirs("out", exist_ok=True)
    open("out/som_map_C.txt", "w").write("\n".join(lines) + "\n")
    print("\n".join(lines[:24]))


if __name__ == "__main__":
    main()
