"""Startup preload policy for plugin instances (plugin-warm-hook-model.md section 3).

A preloaded instance is deserialized, instantiated and its warm hook run once (lazy guest init),
so its first activation in the session meets the first utterance; an instance that is not resident
pays the cold start (~316 us, m18a-1 review: instantiate + cold first call, plus its lazy init) and
misses the first utterance once. Retention after startup is the dLFU + TinyLFU policy of ram.py.

Session model [H]: N = 40 installed (plugin, scope) instances, Zipf(1.1) popularity that carries over
from the previous session with 30 % of the ranks reshuffled; S activations per session; labels
correlate imperfectly with popularity; 30 % of instances are third-party (never preloaded on their
own label). Costs per instance [M wasm-plugin-cost-spike.md + H]: RSS 18 KiB + touched guest memory
LN(256 KiB, sigma 1); startup CPU 0.2 ms (deserialize) + lazy init LN(0.3 ms, sigma 1).

Policies:
  NONE        load on first activation
  LABELS      preload first-party / trusted `interactive`
  LABELS+UNL  also unlabelled first-party / trusted
  HISTORY     LABELS + the top-H instances by decayed activation count persisted from the last
              session (third-party included: the user's own use is the evidence, not the label)
  REC         LABELS+UNL + HISTORY (the recommendation)
  ALL         every instance (upper bound)
Usage: python preload.py
"""
from __future__ import annotations

import numpy as np

N, S, SESS, M_KEEP = 40, 600, 200, 20


def session_stream(rng, perm, S):
    w = 1.0 / np.arange(1, N + 1) ** 1.1
    w /= w.sum()
    return [int(perm[rng.choice(N, p=w)]) for _ in range(S)]


def main(S=S, H=16):
    rng = np.random.default_rng(5)
    rank_of = np.arange(N)  # instance id == initial popularity rank
    tier = np.where(rng.random(N) < 0.3, "third", "first")
    # labels correlate with popularity: top-10 interactive with p .6, rest .15; refinement 15 %
    lab = []
    for i in range(N):
        u = rng.random()
        p_int = 0.6 if i < 10 else 0.15
        lab.append("interactive" if u < p_int else ("refinement" if u < p_int + 0.15 else "unlabelled"))
    lab = np.array(lab)
    rss_kib = 18 + rng.lognormal(np.log(256), 1.0, N)
    cpu_ms = 0.2 + rng.lognormal(np.log(0.3), 1.0, N)

    pols = ["NONE", "LABELS", "LABELS+UNL", "HISTORY", "REC", "ALL"]
    res = {p: dict(miss_first=0, miss=0, act=0, rss=0.0, cpu=0.0, wasted=0) for p in pols}
    perm = rng.permutation(N)
    hist_counts = np.zeros(N)
    for s in range(SESS):
        k = rng.choice(N, size=N * 3 // 10, replace=False)
        perm[k] = perm[rng.permutation(k)]
        seq = session_stream(rng, perm, S)
        used = set(seq)
        top_hist = set(np.argsort(-hist_counts)[:H].tolist()) if s else set()
        for p in pols:
            if p == "NONE":
                pre = set()
            elif p == "ALL":
                pre = set(range(N))
            else:
                ok_lab = {"interactive"} | ({"unlabelled"} if p in ("LABELS+UNL", "REC") else set())
                pre = {i for i in range(N) if tier[i] != "third" and lab[i] in ok_lab}
                if p in ("HISTORY", "REC"):
                    pre |= {i for i in top_hist if lab[i] != "refinement"}
            r = res[p]
            r["rss"] += sum(rss_kib[i] for i in pre)
            r["cpu"] += sum(cpu_ms[i] for i in pre)
            r["wasted"] += len(pre - used)
            resident = set(pre)
            for x in seq:
                r["act"] += 1
                if x not in resident:
                    r["miss"] += 1
                    resident.add(x)  # retention budget M_KEEP is far above the session's set
        hist_counts = hist_counts * 0.5 + np.bincount(seq, minlength=N)
    print(f"\n{SESS} sessions x {S} activations, N = {N} instances, history top-{H}; per session averages")
    print("policy       first-activation misses  preloaded RSS (MiB)  startup CPU (ms)  preloaded-unused")
    for p in pols:
        r = res[p]
        print(f"{p:12s} {r['miss'] / SESS:23.2f}  {r['rss'] / SESS / 1024:19.2f}  {r['cpu'] / SESS:16.2f}"
              f"  {r['wasted'] / SESS:16.2f}")


if __name__ == "__main__":
    for s_len in (50, 600):
        main(S=s_len)
