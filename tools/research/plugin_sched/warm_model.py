"""Cold-call decomposition and decay model for the plugin warm hook (report: plugin-warm-hook-model.md).

Model of the per-call host overhead O (us) of one guest call after the call path was last touched
`tau` us ago on the same thread (guest work and fresh per-job data excluded):

    O(tau) = c_w + F * [core slept in a power-gated idle state] + E * (1 - exp(-tau / T))

  c_w  warm floor (back-to-back call; measured 6.5 us on this host)
  F    step paid when the core enters a deep idle state or another task runs on it (L1/L2 flush
       on power gating, frequency drop, pollution): it does not grow with tau once paid
  E,T  eviction of the call path from L2/L3/TLB by other work, growing with tau

A spinning thread (pre-wake) never sleeps, so only a slow eviction term applies there (3 ms spin
gap: 9.7 us). Usage:
  python warm_model.py                 # fit to the M18a-4 matrix, print the decomposition
  python warm_model.py --fit FILE...   # fit to CALL lines of the proposed microbench (see report)
"""
from __future__ import annotations

import argparse
import math
import re
import sys

import numpy as np
from scipy.optimize import least_squares

C_W = 6.5  # us, 1 thread back to back (bench/results/m18a4/call-matrix-v1.txt)
SPIN_3MS = 9.7

# M18a-4 isolated-call matrix (p50 whole_us), load1 9-16, governor powersave, unpinned unless noted.
SLEEP_1T = {3_000: 72.1, 100_000: 131.0}
SLEEP_10T = {3_000: 61.5, 20_000: 86.6}
PRESPIN_100MS = {0: 131.0, 300: 131.0, 3_000: 133.0, 20_000: 108.0}
CORE_SPIN = {"P": 16.7, "E": 20.3, "LPE": 39.8}    # 3 ms spin gap, pinned
CORE_SLEEP = {"P": 73.8, "E": 89.3, "LPE": 204.9}  # 3 ms sleep gap, pinned
BENCH = {"cold_worker_call": 101.0, "after_replay": 70.0, "hot_worker_call": 115.0}
C2_RESIDENCY_US = 381


def overhead(tau_us, F, E, T, slept=True, c_w=C_W):
    return c_w + (F if slept else 0.0) + E * (1.0 - math.exp(-tau_us / T))


def spin_overhead(tau_us, T):
    """Awake (spinning) thread: only slow eviction, same T, amplitude from the 3 ms spin point."""
    e_s = (SPIN_3MS - C_W) / (1.0 - math.exp(-3_000 / T))
    return C_W + e_s * (1.0 - math.exp(-tau_us / T))


def fit_points():
    """(F, E, T) from the 3, 20 and 100 ms sleep points; 20 ms is 10 threads, scaled by 72.1/61.5."""
    return [(3_000, SLEEP_1T[3_000]), (20_000, SLEEP_10T[20_000] * SLEEP_1T[3_000] / SLEEP_10T[3_000]),
            (100_000, SLEEP_1T[100_000])]


def fit(pts):
    def res(x):
        F, E, T = x
        return [overhead(t, F, E, T) - y for t, y in pts]

    r = least_squares(res, x0=[50.0, 80.0, 40_000.0], bounds=([0, 0, 200], [400, 800, 1e7]))
    return r.x, float(np.sqrt(np.mean(np.square(r.fun))))


def params():
    (F, E, T), _ = fit(fit_points())
    return dict(c_w=C_W, F=float(F), E=float(E), T=float(T), spin3=SPIN_3MS)


def decompose():
    pts = fit_points()
    (F, E, T), rms = fit(pts)
    print("Fit O(tau) = c_w + F + E (1 - exp(-tau/T)) to sleep gaps (1-thread equivalent):")
    print(f"  c_w = {C_W:.1f} us, F = {F:.1f} us, E = {E:.1f} us, T = {T / 1000:.1f} ms, rms {rms:.2f}")
    for t, y in pts:
        print(f"  tau = {t / 1000:6.1f} ms  measured {y:6.1f}  model {overhead(t, F, E, T):6.1f}")
    print(f"  spinning 3 ms: measured {SPIN_3MS} -> awake eviction {SPIN_3MS - C_W:.1f} us per 3 ms")
    pre = PRESPIN_100MS
    print(f"Frequency/uncore ramp bound (100 ms gap, 0.3 vs 20 ms pre-spin): <= {pre[300] - pre[20_000]:.0f} us"
          f" (3 ms pre-spin: {pre[3000]:.0f}, no gain)")
    print("Core-type multipliers vs P (spin, sleep):",
          {k: (round(CORE_SPIN[k] / CORE_SPIN['P'], 2), round(CORE_SLEEP[k] / CORE_SLEEP['P'], 2)) for k in CORE_SPIN})
    print("\nCold call after 100 ms idle:")
    for name, v in [("warm floor c_w", C_W), ("deep-idle / preemption step F", F),
                    ("eviction E(1-exp(-100ms/T))", E * (1 - math.exp(-100_000 / T)))]:
        print(f"  {v:6.1f} us  {name}")
    print(f"  {overhead(100_000, F, E, T):6.1f} us  total")
    k = (BENCH["cold_worker_call"] - BENCH["after_replay"]) / (BENCH["cold_worker_call"] - SPIN_3MS)
    print(f"Replay in the bench (load1 9-40): {BENCH['cold_worker_call']:.0f} -> {BENCH['after_replay']:.0f} us,"
          f" effective coverage kappa = {k:.2f}")
    return F, E, T


def lead_curve(F, E, T, phis=(0.0, 0.5, 0.9)):
    print("\nReal-call overhead after a full-coverage hook, by lead time and what the thread does after it:")
    print("lead_ms   spin  park/no-lease  " + "  ".join(f"park/lease phi={p:.1f}" for p in phis))
    for lead in (100, 300, 1_000, 3_000, 10_000, 30_000, 100_000):
        spin = spin_overhead(lead, T)
        nolease = overhead(lead, F, E, T, slept=lead >= C2_RESIDENCY_US)
        vals = [overhead(lead, F * (1 - p), E, T) for p in phis]
        print(f"{lead / 1000:7.1f}  {spin:5.1f}  {nolease:13.1f}  " + "  ".join(f"{v:18.1f}" for v in vals))


CALL_RE = re.compile(r"CALL .*?gap_us=(\d+) (\w+).*?whole_us p50=([\d.]+)")


def fit_file(paths):
    """Fit the decay model to CALL lines of plugin_call_cost (sleep gaps only); one fit per file."""
    for p in paths:
        pts = []
        for line in open(p):
            m = CALL_RE.search(line)
            if m and m.group(2) == "sleep" and int(m.group(1)) > 0:
                pts.append((int(m.group(1)), float(m.group(3))))
        if len(pts) < 3:
            print(f"{p}: need >= 3 sleep-gap CALL lines", file=sys.stderr)
            continue
        (F, E, T), rms = fit(pts)
        print(f"{p}: F = {F:.1f} us  E = {E:.1f} us  T = {T / 1000:.1f} ms  rms = {rms:.1f} us  (n = {len(pts)})")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--fit", nargs="*")
    a = ap.parse_args()
    if a.fit:
        sys.exit(fit_file(a.fit))
    F, E, T = decompose()
    lead_curve(F, E, T)


if __name__ == "__main__":
    main()
