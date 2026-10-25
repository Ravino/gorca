"""Warm-instance retention under a RAM budget: LRU vs LFU vs decayed LFU vs decayed LFU + TinyLFU admission.

A "hit" = the plugin's W instance is Ready when its scope's focus change triggers it; a miss pays
instantiate + cold first call (~316 µs, m18a-1 review) and so misses the first utterance.
Workload: 40 installed (plugin, scope) instances, Zipf(1.1) popularity that rotates every 2000
activations (the user changes the app mix), plus 5 % one-off "scan" activations of rare plugins
(a page that matches many plugins once). Budget M instances. Usage: python ram.py
"""
from __future__ import annotations

import numpy as np

N, STEPS, PHASE = 40, 40_000, 2_000


def workload(seed=3):
    rng = np.random.default_rng(seed)
    w = 1.0 / np.arange(1, N + 1) ** 1.1
    w /= w.sum()
    perm = rng.permutation(N)
    seq = []
    for t in range(STEPS):
        if t % PHASE == 0:
            # partial rotation: 30 % of the ranks move
            k = rng.choice(N, size=N * 3 // 10, replace=False)
            perm[k] = perm[rng.permutation(k)]
        if rng.random() < 0.05:
            seq.append(int(rng.integers(N // 2, N)) + N)  # scan: one-off ids outside the working set
        else:
            seq.append(int(perm[rng.choice(N, p=w)]))
    return seq


def simulate(seq, M, policy):
    warm: dict[int, int] = {}  # id -> last use (LRU) ; frequency kept separately
    freq: dict[int, float] = {}
    hits = 0
    half = 512  # decay half-life in activations (decayed LFU)
    decay = 0.5 ** (1 / half)
    t = 0
    for x in seq:
        t += 1
        if policy in ("dlfu", "tinylfu"):
            # lazy exponential decay: (value, time of last update), decayed on read
            f, last = freq.get(x, (0.0, t))
            freq[x] = (f * decay ** (t - last) + 1.0, t)
        else:
            freq[x] = freq.get(x, 0) + 1
        if x in warm:
            hits += 1
            warm[x] = t
            continue
        if len(warm) < M:
            warm[x] = t
            continue

        def score(k):
            if policy == "lru":
                return warm[k]
            if policy == "lfu":
                return freq[k]
            f, last = freq[k]
            return f * decay ** (t - last)

        victim = min(warm, key=lambda k: (score(k), k))
        if policy == "tinylfu":
            fx = freq[x][0]
            if fx <= score(victim):  # admission: candidate must be more frequent than the victim
                continue             # run cold once, do not keep
        del warm[victim]
        warm[x] = t
    return hits / len(seq)


def main():
    seq = workload()
    print("M  " + "  ".join(f"{p:>8s}" for p in ("lru", "lfu", "dlfu", "tinylfu")))
    for M in (4, 8, 12, 20):
        print(f"{M:<3d}" + "  ".join(f"{simulate(seq, M, p):8.3f}" for p in ("lru", "lfu", "dlfu", "tinylfu")))


if __name__ == "__main__":
    main()
