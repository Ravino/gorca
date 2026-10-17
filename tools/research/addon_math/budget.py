"""Latency budget allocation for many add-ons on the hot path (report section 2).

Run: python tools/research/addon_math/budget.py

Inputs (measured in this repo, see docs/decisions.md 2026-10-16/17):
  X1 hook p99 0.2-0.5 us (present/policy WASM, trivial hooks); A1 Shell p99 5.5 ms gorca-side;
  limits: present/policy 2 ms CPU (fuel), backstop 6 ms CPU, on-event 2 ms, command 20 ms.
Assumptions (NOT measured, labelled in the report):
  - cross-thread handoff for a parallel fan-out: 5-20 us per wake (tokio/std park-unpark on a
    loaded desktop); we use 10 us.
  - app-actor core service time per coalesced event: 15 us mean (exponential + 5 us fixed).
"""

from __future__ import annotations

import numpy as np

rng = np.random.default_rng(7)


def lognorm_from_p50_p99(p50, p99, size):
    mu = np.log(p50)
    sigma = (np.log(p99) - mu) / 2.3263478740408408
    return rng.lognormal(mu, sigma, size)


def tail_composition(samples=400_000):
    print("== 2.1 tail composition: N hooks, each p50 0.25 us / p99 0.5 us (X1-like) ==")
    print(f"{'N':>4} {'seq p99':>10} {'sum p99s':>10} {'par p99 (+10us)':>16}")
    for n in (1, 4, 16, 64):
        x = lognorm_from_p50_p99(0.25, 0.5, (samples, n))
        seq = np.quantile(x.sum(1), 0.99)
        par = np.quantile(x.max(1), 0.99) + 10.0
        print(f"{n:>4} {seq:>9.2f}u {0.5 * n:>9.2f}u {par:>15.2f}u")
    print("-- one heavy add-on (p50 80 us / p99 400 us) among 15 light ones --")
    light = lognorm_from_p50_p99(0.25, 0.5, (samples, 15)).sum(1)
    heavy = lognorm_from_p50_p99(80, 400, samples)
    print(f"   seq p99 = {np.quantile(light + heavy, 0.99):.1f} us (heavy alone {np.quantile(heavy, .99):.1f})")


def union_bound_allocation(alpha=0.01, n=8, samples=400_000):
    """P(sum X_i > sum q_i) <= sum P(X_i > q_i): giving each hook its (1 - alpha/N) quantile as
    its budget makes the sum a valid (1 - alpha) bound for ANY dependence structure."""
    print("== 2.2 union-bound allocation (dependence-free guarantee) ==")
    # comonotone worst case: all hooks slow together (same uniform drives all)
    u = rng.random(samples)
    from scipy.stats import norm
    mu, sig = np.log(0.25), (np.log(0.5) - np.log(0.25)) / norm.ppf(0.99)
    x = np.exp(mu + sig * norm.ppf(u))[:, None] * np.ones(n)
    q_i = np.exp(mu + sig * norm.ppf(1 - alpha / n))
    budget = n * q_i
    viol = np.mean(x.sum(1) > budget)
    print(f"   N={n}: per-hook budget q(1-a/N)={q_i:.3f} us, total {budget:.2f} us,"
          f" comonotone violation rate {viol:.4f} <= {alpha}")


def carry_forward(trials=200_000, n=12, e_hot=3000.0):
    """Pool E split as g_i = E/N guaranteed; call i may use g_i + leftover of earlier calls.
    Claim: total used <= E and call i is offered >= g_i."""
    print("== 2.3 carry-forward pool (E_hot = 3 ms, N = 12) ==")
    g = e_hot / n
    worst_total, min_offer, skipped = 0.0, np.inf, 0
    for _ in range(trials // 1000):
        demand = np.where(rng.random((1000, n)) < 0.02, rng.uniform(0, 2000, (1000, n)),
                          lognorm_from_p50_p99(0.25, 0.5, (1000, n)))
        left = np.zeros(1000)
        total = np.zeros(1000)
        for i in range(n):
            offer = g + left
            min_offer = min(min_offer, offer.min())
            used = np.minimum(demand[:, i], offer)
            skipped += int((demand[:, i] > offer).sum())
            left = offer - used
            total += used
        worst_total = max(worst_total, total.max())
    print(f"   max total used {worst_total:.1f} us <= {e_hot}; min offer {min_offer:.1f} >= g={g:.1f};"
          f" cut calls {skipped / trials / n:.4%}")


def storm_queue(lam=10_000.0, n_events=1_000_000, ext_cost_us=(0.3,) * 16, heavy_us=0.0,
                bucket=None):
    """Lindley recursion for the app actor during an event storm. Returns p50/p99/p99.9 waiting
    time (us) seen by an arbitrary arrival (PASTA: equals what a focus event would wait if it is
    queued FIFO behind the storm). `bucket` = (rate_us_per_s, burst_us) CPU token bucket for the
    heavy add-on: when empty its hook is skipped (Busy semantics) and costs ~0."""
    inter = rng.exponential(1e6 / lam, n_events)
    core = 5.0 + rng.exponential(10.0, n_events)
    light = sum(ext_cost_us)
    heavy = np.full(n_events, heavy_us)
    if bucket and heavy_us > 0:
        rate, burst = bucket
        tokens, t = burst, 0.0
        for k in range(n_events):
            t_new = t + inter[k]
            tokens = min(burst, tokens + rate * (inter[k] / 1e6))
            t = t_new
            if tokens >= heavy_us:
                tokens -= heavy_us
            else:
                heavy[k] = 0.05  # skipped call: bookkeeping only
    s = core + light + heavy
    w = np.empty(n_events)
    acc = 0.0
    for k in range(n_events):
        w[k] = acc
        acc = max(0.0, acc + s[k] - (inter[k + 1] if k + 1 < n_events else 0))
    rho = lam * s.mean() / 1e6
    storm_queue.last_mean = w.mean()
    return rho, np.quantile(w, [0.5, 0.99, 0.999])


def pk_mean_wait(lam, s):
    """Pollaczek-Khinchine M/G/1 mean wait, us."""
    lam_us = lam / 1e6
    rho = lam_us * s.mean()
    return lam_us * (s ** 2).mean() / (2 * (1 - rho))


def storms():
    print("== 2.4 app actor under a 10k ev/s storm (FIFO, worst case: no priority lane) ==")
    cases = [
        ("16 light add-ons (0.3 us)", dict()),
        ("+ heavy 60 us on-event, unlimited", dict(heavy_us=60.0)),
        ("+ heavy 60 us, bucket 50 ms/s burst 10 ms", dict(heavy_us=60.0, bucket=(50_000.0, 10_000.0))),
        ("+ heavy 90 us, unlimited", dict(heavy_us=90.0)),
        ("+ heavy 90 us, bucket 50 ms/s burst 10 ms", dict(heavy_us=90.0, bucket=(50_000.0, 10_000.0))),
    ]
    for name, kw in cases:
        rho, q = storm_queue(n_events=300_000, **kw)
        print(f"   {name:45s} rho={rho:5.2f}  wait p50/p99/p99.9 = "
              f"{q[0]:8.1f} {q[1]:8.1f} {q[2]:8.1f} us")
    s = 5.0 + rng.exponential(10.0, 1_000_000) + 4.8
    rho, q = storm_queue(n_events=300_000)
    print(f"   P-K check (light case): analytic mean wait {pk_mean_wait(1e4, s):.2f} us,"
          f" simulated {storm_queue.last_mean:.2f} us")


def max_ext_cost(lam, core_mean, target_rho=0.5):
    """Largest per-event extension CPU that keeps actor utilisation <= target."""
    return target_rho * 1e6 / lam - core_mean


if __name__ == "__main__":
    tail_composition()
    union_bound_allocation()
    carry_forward()
    storms()
    print("== 2.5 per-event extension CPU allowed for rho <= 0.5 ==")
    for lam in (1_000, 10_000, 20_000):
        print(f"   lambda={lam:>6}/s: sum c_ext <= {max_ext_cost(lam, 15.0):7.1f} us/event")
