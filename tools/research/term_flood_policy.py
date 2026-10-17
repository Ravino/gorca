#!/usr/bin/env python3
"""Flood speech policy simulator (research prototype, see
docs/reports/terminal-live-review-math.md section 3).

Arrivals: Markov-modulated Poisson process of terminal lines (quiet / chatty /
flood phases). Speech: one channel, an utterance of c chars lasts c / mu_c s.

Policies
- fifo:   speak every line in order (Orca-like queueing).
- pull:   one-slot "latest wins" scheduler. When the channel is idle and there
          are unspoken lines, speak them if they are few and the EWMA rate is
          below capacity, else speak one summary "N lines, last: ...".
          Hysteresis: enter flood mode at rate > enter*mu, leave below exit*mu
          held for `hold` s. Utterances are capped at cmax chars.

Metric: staleness = speech start time - arrival time of the newest line the
utterance conveys (for fifo: of the line itself); and coverage = lines spoken
verbatim / lines arrived.
"""
from __future__ import annotations

import math
import random

import numpy as np

PHASES = {  # name: (rate lines/s, mean dwell s)
    "quiet": (0.3, 20.0),
    "chatty": (3.0, 10.0),
    "flood": (3000.0, 4.0),
}
NEXT = {"quiet": ["chatty", "flood"], "chatty": ["quiet", "flood"], "flood": ["quiet", "chatty"]}


ARR_PHASE: list[bool] = []  # True when the line arrived outside a flood phase


def arrivals(T: float, seed: int) -> np.ndarray:
    rng = random.Random(seed)
    t, ph, out = 0.0, "quiet", []
    while t < T:
        lam, dwell = PHASES[ph]
        end = t + rng.expovariate(1 / dwell)
        while True:
            t += rng.expovariate(lam)
            if t >= min(end, T):
                break
            out.append(t)
            ARR_PHASE.append(ph != "flood")
        t = end
        ph = rng.choice(NEXT[ph])
    return np.array(out)


def fifo(arr: np.ndarray, line_chars: float, mu_c: float) -> dict:
    d = line_chars / mu_c
    free = 0.0
    stale = np.empty(len(arr))
    for i, a in enumerate(arr):
        start = max(a, free)
        stale[i] = start - a
        free = start + d
    return dict(policy="fifo", p50=float(np.percentile(stale, 50)), p99=float(np.percentile(stale, 99)),
                max=float(stale.max()), coverage=1.0, utterances=len(arr))


def pull(arr: np.ndarray, line_chars: float, mu_c: float, burst: float = 3.0,
         enter: float = 0.7, cmax: int = 120, min_gap_summary: float = 2.0) -> dict:
    """Token bucket (rate enter*mu_lines, depth `burst`) gates verbatim speech.

    Idle channel + pending lines: if not flooding and pending <= tokens, speak
    them verbatim; otherwise speak one summary covering all pending lines, at
    most once per min_gap_summary. Flood mode ends when the bucket is full again.
    """
    mu_lines = mu_c / min(line_chars, cmax)
    r = enter * mu_lines
    i, n = 0, len(arr)
    t = 0.0
    tokens, tok_t = burst, 0.0
    flood = False
    spoken_upto = 0
    stale, verbatim, utt, summaries = [], 0, 0, 0
    last_summary = -1e9
    while spoken_upto < n:
        while i < n and arr[i] <= t:
            i += 1
        tokens = min(burst, tokens + (t - tok_t) * r)
        tok_t = t
        pending = i - spoken_upto
        if flood and tokens >= burst - 1e-9 and pending <= 1:
            flood = False
        if pending == 0:
            t = arr[i]
            continue
        if not flood and pending <= tokens:
            stale.append(t - arr[i - 1])
            verbatim += pending
            utt += 1
            tokens -= pending
            spoken_upto = i
            t += pending * min(line_chars, cmax) / mu_c
            continue
        flood = True
        if t - last_summary < min_gap_summary:
            t = last_summary + min_gap_summary  # let lines accumulate
            continue
        stale.append(t - arr[i - 1])
        summaries += 1
        utt += 1
        last_summary = t
        spoken_upto = i
        t += min(cmax, 20 + line_chars) / mu_c  # "N new lines, last: <line>"
    st = np.array(stale)
    return dict(policy="pull", p50=float(np.percentile(st, 50)), p99=float(np.percentile(st, 99)),
                max=float(st.max()), verbatim=verbatim, utterances=utt, summaries=summaries)


def main() -> None:
    arr = arrivals(3600.0, 7)
    print(f"arrivals: {len(arr)} lines in 3600 s, {sum(ARR_PHASE)} outside floods")

    for mu_c in (15.0, 30.0):  # chars/s: ~180 wpm and ~360 wpm
        for line_chars in (60.0,):
            print(f"mu_c={mu_c} chars/s, line={line_chars} chars")
            r = fifo(arr, line_chars, mu_c)
            print("  ", {k: (round(v, 2) if isinstance(v, float) else v) for k, v in r.items()})
            for enter in (0.5, 0.7, 0.9):
                r = pull(arr, line_chars, mu_c, enter=enter)
                print("  ", f"enter={enter}", {k: (round(v, 3) if isinstance(v, float) else v) for k, v in r.items()})
    # quiet-only sanity: pull must speak everything verbatim
    r = pull(np.array([i * 5.0 for i in range(50)]), 60.0, 30.0)
    assert r["verbatim"] == 50 and r["summaries"] == 0, r
    print("sanity (1 line / 5 s): all verbatim, no summaries: ok")


if __name__ == "__main__":
    main()
