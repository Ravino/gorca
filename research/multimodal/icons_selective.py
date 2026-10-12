"""Selective naming: accuracy vs coverage when SigLIP abstains below a margin; 1-thread latency."""
import json
import numpy as np
from encoders import VisionTextModel, timeit
from icons import LABELS, PROMPTS, LIGHT, DARK, icon_files, render

files = icon_files(); names = list(files); lab = list(LABELS)
items = [(names.index(i), li) for li, l in enumerate(lab) for i in LABELS[l] if i in files]
out = {}
for kind in ["siglip-q8", "mobileclip-s0"]:
    m = VisionTextModel(kind, threads=1)
    T = np.stack([(lambda e: e / np.linalg.norm(e))(m.encode_texts([p.format(l) for p in PROMPTS]).mean(0)) for l in lab])
    imgs = [render(files[names[i]], 48, 72, *LIGHT) for i, _ in items]
    S = m.encode_images(imgs) @ T.T
    truth = np.array([li for _, li in items])
    srt = np.sort(S, 1); margin = srt[:, -1] - srt[:, -2]; ok = S.argmax(1) == truth
    curve = []
    for cov in [1.0, 0.75, 0.5, 0.25]:
        thr = np.quantile(margin, 1 - cov)
        sel = margin >= thr
        curve.append({"coverage": cov, "accuracy": float(ok[sel].mean())})
    p50, p99 = timeit(lambda: m.encode_images(imgs[:1]), reps=30)
    out[kind] = {"selective": curve, "img_ms_1thread_p50": p50, "img_ms_1thread_p99": p99}
    print(kind, out[kind], flush=True)
json.dump(out, open("out/icons_selective.json", "w"), indent=1)
