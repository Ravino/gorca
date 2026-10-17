"""Store math (report section 4): rating scores, near-duplicate detection, staged rollout SPRT.

Run: python tools/research/addon_math/store.py [component.wasm ...]
Defaults to the two first-party components under target/release/extensions if present.
"""

from __future__ import annotations

import hashlib
import math
import sys
from pathlib import Path

import numpy as np
from scipy.stats import norm

rng = np.random.default_rng(5)

# ------------------------------------------------------------------------------- ratings

def bayes_avg(ratings, weights=None, prior_mean=4.0, prior_weight=20.0):
    """(C*m + sum w*r) / (C + sum w). Weights = rater trust in [0, 1]."""
    r = np.asarray(ratings, float)
    w = np.ones_like(r) if weights is None else np.asarray(weights, float)
    return (prior_weight * prior_mean + (w * r).sum()) / (prior_weight + w.sum())


def wilson_lower(pos, n, z=1.959964):
    if n == 0:
        return 0.0
    p = pos / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    a = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return (c - a) / d


def ratings_demo():
    print("== 4.1 ratings ==")
    honest = rng.choice([3, 4, 5], 300, p=[0.1, 0.4, 0.5])
    print(f"   300 honest (mean {honest.mean():.2f}): bayes {bayes_avg(honest):.3f};"
          f" 3 x 5-star new add-on: bayes {bayes_avg([5, 5, 5]):.3f} (raw 5.00)")
    for k in (10, 50, 200):
        fake = np.full(k, 5.0)
        low = np.full(40, 2.0)  # a genuinely poor add-on: 40 honest 2-star ratings
        unweighted = bayes_avg(np.r_[low, fake])
        weighted = bayes_avg(np.r_[low, fake], np.r_[np.ones(40), np.full(k, 0.05)])
        print(f"   poor add-on (40 x 2-star) + {k:3d} sock-puppet 5-stars: unweighted {unweighted:.2f},"
              f" trust-weighted (w=0.05) {weighted:.2f}")
    for pos, n in ((3, 3), (90, 100), (900, 1000)):
        print(f"   wilson lower bound {pos}/{n} up-votes: {wilson_lower(pos, n):.3f}")


# ------------------------------------------------------------------------------- MinHash

P61 = (1 << 61) - 1


def shingles(data: bytes, k=8):
    """Set of 64-bit hashes of k-byte shingles (prototype: raw bytes; production: normalised
    per-function opcode sequences via wasmparser, see the report)."""
    mv = memoryview(data)
    return {int.from_bytes(hashlib.blake2b(mv[i:i + k], digest_size=8).digest(), "little")
            for i in range(len(data) - k + 1)}


def minhash(sh, n_perm=256, seed=1):
    r = np.random.default_rng(seed)
    a = r.integers(1, P61, n_perm, dtype=np.uint64).astype(object)
    b = r.integers(0, P61, n_perm, dtype=np.uint64).astype(object)
    x = np.fromiter(sh, dtype=object, count=len(sh))
    return np.array([int(((ai * x + bi) % P61).min()) for ai, bi in zip(a, b)], dtype=object)


def lsh_threshold(bands, rows):
    return (1 / bands) ** (1 / rows)


def mutate(data: bytes, frac: float, seed=3) -> bytes:
    r = np.random.default_rng(seed)
    b = bytearray(data)
    for i in r.choice(len(b), int(len(b) * frac), replace=False):
        b[i] = (b[i] + 1 + int(r.integers(0, 254))) % 256
    return bytes(b)


def minhash_demo(paths):
    print("== 4.2 near-duplicate detection (MinHash, 256 permutations, 8-byte shingles) ==")
    blobs = {p.name: p.read_bytes() for p in paths if p.is_file()}
    if len(blobs) < 1:
        print("   no components found; skipped")
        return
    names = list(blobs)
    base = blobs[names[0]]
    variants = {
        f"{names[0]}": base,
        "clone: 0.5% bytes mutated": mutate(base, 0.005),
        "clone: 5% bytes mutated": mutate(base, 0.05),
        "clone: + 50% appended payload": base + rng.integers(0, 256, len(base) // 2, dtype=np.uint8).tobytes(),
    }
    for n in names[1:]:
        variants[f"other add-on: {n}"] = blobs[n]
    sh = {k: shingles(v) for k, v in variants.items()}
    mh = {k: minhash(v) for k, v in sh.items()}
    ref = names[0]
    for k in variants:
        if k == ref:
            continue
        inter = len(sh[ref] & sh[k])
        jac = inter / len(sh[ref] | sh[k])
        cont = inter / len(sh[ref])
        est = float(np.mean(mh[ref] == mh[k]))
        print(f"   {k:40s} J={jac:.3f}  est={est:.3f}  containment={cont:.3f}")
    for b, r in ((32, 8), (64, 4), (16, 16)):
        print(f"   LSH {b} bands x {r} rows: 50% detection at J ~ {lsh_threshold(b, r):.2f}")


# ------------------------------------------------------------------------------- rollout SPRT

def sprt_bounds(alpha, beta):
    return math.log(beta / (1 - alpha)), math.log((1 - beta) / alpha)


def conditional_sprt(n_new_fail, n_old_fail, share, r1, alpha=0.01, beta=0.05):
    """Failures pooled over both arms; given a failure, P(new arm) = pi(r) = r s / (r s + 1 - s).
    Wald SPRT H0 r=1 vs H1 r=r1. Exposure-free: only failure counts and the arm share s."""
    pi0 = share
    pi1 = r1 * share / (r1 * share + 1 - share)
    llr = n_new_fail * math.log(pi1 / pi0) + n_old_fail * math.log((1 - pi1) / (1 - pi0))
    lo, hi = sprt_bounds(alpha, beta)
    return "rollback" if llr >= hi else "accept" if llr <= lo else "continue"


def simulate_rollout(true_ratio, p0=0.002, share=0.05, sessions_per_hour=20_000, hours=168,
                     r1=2.0, runs=400):
    """Returns (P(rollback), median hours to decision)."""
    res, times = 0, []
    for _ in range(runs):
        nn = no = 0
        decided = None
        for h in range(hours):
            s_new = rng.binomial(sessions_per_hour, share)
            nn += rng.binomial(s_new, min(1.0, p0 * true_ratio))
            no += rng.binomial(sessions_per_hour - s_new, p0)
            d = conditional_sprt(nn, no, share, r1)
            if d != "continue":
                decided = d
                times.append(h + 1)
                break
        res += decided == "rollback"
    return res / runs, (float(np.median(times)) if times else float("nan")), len(times) / runs


def rollout_demo():
    print("== 4.3 staged rollout: conditional SPRT, H0 r=1 vs H1 r=2, alpha=0.01 beta=0.05 ==")
    print("   (p0 = 0.2% sessions with a fault-disable, 5% stage, 20k sessions/h overall)")
    for r in (1.0, 1.5, 2.0, 3.0, 5.0):
        p_rb, t, decided = simulate_rollout(r)
        print(f"   true rate ratio {r:3.1f}: P(rollback)={p_rb:.3f}  median hours to decision {t:5.1f}"
              f"  decided within a week {decided:.2f}")


if __name__ == "__main__":
    ratings_demo()
    paths = [Path(p) for p in sys.argv[1:]] or [
        Path("target/release/extensions/ext-gnome-shell/ext_gnome_shell.wasm"),
        Path("target/release/extensions/ext-vte/ext_vte.wasm")]
    minhash_demo(paths)
    rollout_demo()
