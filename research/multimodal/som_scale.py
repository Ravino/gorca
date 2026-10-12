"""SOM cost at scale and the margin certificate for incremental updates.

Data: synthetic mixture on the unit sphere shaped like real text embeddings
(cluster centres drawn from the e5 corpus embeddings, within-cluster noise from
their PCA spectrum). Heuristic stand-in: real multi-app item sets do not exist yet.

For N in {10k, 100k}, D=384 (float32), grids 8x8 and 16x16:
  * batch-SOM iteration time (full fit = ~30 iterations);
  * incremental update of m=100 mutated items using sufficient statistics:
      O(m K D) BMU for touched items + O(K^2 D) anchored refit,
    then the certificate: an untouched item keeps its BMU if
    upper bound u_i >= d(x, w_b) still below lower bound l_i <= min_{j!=b} d(x, w_j), with
    u_i += delta_b and l_i -= max_j delta_j after every refit (Hamerly 2010 bounds, exact
    by the triangle inequality); delta_j = ||W_j' - W_j||. Only items with u_i >= l_i are
    re-checked (O(K D) each). Soundness is asserted on a sample every update.
  Reports the share of items needing a re-check and the measured BMU changes.

Usage: python som_scale.py
"""
from __future__ import annotations

import os

os.environ.setdefault("OMP_NUM_THREADS", "1")
os.environ.setdefault("OPENBLAS_NUM_THREADS", "1")

import json  # noqa: E402
import time  # noqa: E402

import numpy as np  # noqa: E402

from maps import BatchSOM, sqdist  # noqa: E402


def realistic(n: int, d: int = 384, seed: int = 0) -> np.ndarray:
    rng = np.random.default_rng(seed)
    T = np.load("out/text_emb_C.npy").astype(np.float32)
    T = T[np.linalg.norm(T, axis=1) > 0][:, :d]
    mu = T.mean(0)
    U, S, Vt = np.linalg.svd(T - mu, full_matrices=False)
    centres = T[rng.integers(0, len(T), 2000)]
    lab = rng.integers(0, len(centres), n)
    Z = rng.standard_normal((n, len(S))).astype(np.float32) * (S / np.sqrt(len(T)))[None, :] * 0.5
    X = centres[lab] + Z @ Vt
    return X / np.linalg.norm(X, axis=1, keepdims=True)


def main():
    out = {}
    for n in (10_000, 100_000):
        X = realistic(n)
        for rows in (8, 16):
            K = rows * rows
            som = BatchSOM(rows, rows)
            t0 = time.perf_counter(); som.fit(X.astype(np.float64)[:20000] if n > 20000 else X.astype(np.float64), iters=20)
            fit_sub_ms = (time.perf_counter() - t0) * 1e3
            som.W = som.W.astype(np.float32)
            Xf = X
            t0 = time.perf_counter(); D = sqdist(Xf, som.W); iter_ms = (time.perf_counter() - t0) * 1e3
            b = D.argmin(1)
            srt = np.sqrt(np.sort(D, 1)[:, :2])
            # sufficient statistics
            Oh = np.zeros((n, K), np.float32); Oh[np.arange(n), b] = 1
            S, cnt = Oh.T @ Xf, Oh.sum(0)
            rng = np.random.default_rng(1)
            rechecks, changes, upd = [], [], []
            for _ in range(20):
                m = 100
                ids = rng.choice(n, m, replace=False)
                t0 = time.perf_counter()
                newx = realistic(m, seed=int(rng.integers(1 << 30)))
                np.subtract.at(S, b[ids], Xf[ids]); np.subtract.at(cnt, b[ids], 1)
                Xf[ids] = newx
                Dm = sqdist(newx, som.W); bm = Dm.argmin(1)
                srt[ids] = np.sqrt(np.sort(Dm, 1)[:, :2])
                np.add.at(S, bm, newx); np.add.at(cnt, bm, 1); b[ids] = bm
                W0 = som.W.copy()
                H = som.H(0.8).astype(np.float32); lam = 0.1 * n / K
                som.W = (H @ S + lam * W0) / (H @ cnt + lam)[:, None]
                delta = np.linalg.norm(som.W - W0, axis=1)
                # Hamerly-style bounds: u >= d(x, w_b), l <= min_{j != b} d(x, w_j)
                srt[:, 0] += delta[b]
                srt[:, 1] -= delta.max()
                safe = srt[:, 1] > srt[:, 0]
                bad = np.where(~safe)[0]
                if len(bad):
                    Db = sqdist(Xf[bad], som.W); nb = Db.argmin(1)
                    ch = nb != b[bad]
                    changes.append(int(ch.sum()))
                    mv = bad[ch]
                    np.subtract.at(S, b[mv], Xf[mv]); np.subtract.at(cnt, b[mv], 1)
                    np.add.at(S, nb[ch], Xf[mv]); np.add.at(cnt, nb[ch], 1)
                    b[bad] = nb
                    srt[bad] = np.sqrt(np.sort(Db, 1)[:, :2])
                else:
                    changes.append(0)
                upd.append((time.perf_counter() - t0) * 1e3)
                rechecks.append(len(bad) / n)
                # verify certificate soundness on a sample
                sp = np.where(safe)[0]; samp = rng.choice(sp, min(2000, len(sp)), replace=False)
                Ds = sqdist(Xf[samp].astype(np.float64), som.W.astype(np.float64))
                viol = Ds.argmin(1) != b[samp]
                if viol.any():
                    gap = np.sqrt(Ds[viol, b[samp][viol]]) - np.sqrt(Ds[viol].min(1))
                    if gap.max() >= 1e-5:
                        k = samp[viol][gap.argmax()]
                        print("viol item", k, "u,l", srt[k], "true d_b", np.sqrt(Ds[viol][gap.argmax(), b[k]]), "true min", np.sqrt(Ds[viol][gap.argmax()].min()), "in ids", k in ids)
                        raise SystemExit(1)
            r = {"fit20_ms_on_min(n,20k)": fit_sub_ms, "bmu_pass_ms": iter_ms,
                 "update_ms_p50": float(np.median(upd)), "update_ms_max": float(np.max(upd)),
                 "recheck_share": float(np.mean(rechecks)), "bmu_changes_per_update": float(np.mean(changes))}
            out[f"n{n}_k{K}"] = r
            print(n, K, {k: round(v, 4) for k, v in r.items()}, flush=True)
    json.dump(out, open("out/som_scale.json", "w"), indent=1)


if __name__ == "__main__":
    main()
