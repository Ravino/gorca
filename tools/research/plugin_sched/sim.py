"""Synthetic-workload simulation of plugin scheduling policies for ADR-0026 B'.

Policies
  WA-park   wait-for-all, workers parked (r5 code as reviewed: every triggered plugin in the
            countdown, deadline D, no pre-wake; batch wake and spinning writer assumed fixed)
  WA-wake   wait-for-all plus pre-wake of every plugin worker of the scope on every non-typing key
  LABELS    fixed labels only: pre-wake workers of interactive+default plugins on non-typing keys,
            wait for interactive+default plugins to D, refinement never waited
  SPS       the statistical scheduler (sps.py)

Host model (µs), calibrated to docs/reports/m18a-2-review.md section 2 probe (default C-states):
  hot (spinning) thread start U[1,3]; parked thread start = mixture 25 % LN(8,.5), 60 % LN(70,.4),
  15 % LN(200,.3) capped 340; writer spins (no late wake) but is preempted with p = 1 % by
  U[30,120] (loaded host); snapshot+send 6, apply+fuse 8, both inside D = 300.
Exec times are per-call wall times on the plugin thread (MQ-4 parallel grants: each call capped at
200 µs). Usage: `python sim.py [--n 20000] [--seed 1] [--json out.json]`.
"""
from __future__ import annotations

import argparse
import json
import math

import numpy as np

from sps import Q, Params, Scheduler

D = 300
SNAP, FUSE = 6, 8
DW = D - SNAP - FUSE
SPIN_WINDOW = 50_000  # pre-wake spin window when no focus change arrives (µs)
TAIL_SPIN = 40        # adaptive spin before parking (µs)
WAKE_CPU = 3          # CPU cost of a futex wake + context switch (µs)
STALL_EXEC = 190      # adversary stall: just under the 200 µs per-call cap (no strike)

# id, name, label, tier, exec median, sigma, useful prob, base activation, busy prob
PLUGINS = [
    (0, "telegram", "interactive", "first", 45, 0.5, 0.80, 0.95, 0.0),
    (1, "cheap-a", "default", "first", 8, 0.4, 0.30, 0.60, 0.0),
    (2, "cheap-b", "default", "trusted", 12, 0.5, 0.50, 0.40, 0.0),
    (3, "cheap-c", "interactive", "first", 6, 0.3, 0.60, 0.30, 0.0),
    (4, "medium-a", "default", "trusted", 60, 0.6, 0.40, 0.50, 0.2),
    (5, "medium-b", "interactive", "first", 90, 0.5, 0.70, 0.20, 0.0),
    (6, "heavy", "refinement", "first", 180, 0.4, 0.30, 0.50, 0.2),
    (7, "slow-tail", "default", "trusted", 20, 1.2, 0.40, 0.40, 0.0),
    (8, "rare", "default", "first", 30, 0.5, 0.90, 0.03, 0.0),
    (9, "adversary", "interactive", "third", 10, 0.3, 0.50, 0.70, 0.0),  # self-labelled interactive
]
ADV = 9
BENIGN_WAITABLE = [p[0] for p in PLUGINS if p[2] != "refinement" and p[0] != ADV]

# context: (weight, P(focus change), activation multiplier, telegram active)
CTX = {
    "tg-nav": (0.30, 0.95, 1.0, True),
    "tg-tab": (0.08, 0.98, 0.8, True),
    "tg-enter": (0.05, 0.70, 0.6, True),
    "tg-edit-arrow": (0.10, 0.02, 1.0, True),
    "tg-type": (0.17, 0.0, 0.0, True),
    "oth-nav": (0.15, 0.80, 1.0, False),
    "oth-tab": (0.05, 0.98, 0.8, False),
    "oth-type": (0.10, 0.0, 0.0, False),
}
CTX_NAMES = list(CTX)
CTX_W = np.array([CTX[c][0] for c in CTX_NAMES])
CTX_W = CTX_W / CTX_W.sum()


def in_scope(pid: int, ctx: str) -> bool:
    return pid != 0 or CTX[ctx][3]


HOST = {"name": "default"}


def parked_start(rng) -> int:
    u = rng.random()
    if HOST["name"] == "pmqos":  # pool CPUs with pm_qos_resume_latency_us = 20 (no C2/C3)
        x = rng.lognormal(math.log(8), 0.5) if u < 0.7 else rng.lognormal(math.log(18), 0.4)
        return int(min(60, x))
    if u < 0.25:
        x = rng.lognormal(math.log(8), 0.5)
    elif u < 0.85:
        x = rng.lognormal(math.log(70), 0.4)
    else:
        x = rng.lognormal(math.log(200), 0.3)
    return int(min(340, x))


def adversary_exec(strategy: str, k: int, rng, waited_if_known: bool) -> int:
    fast = int(rng.lognormal(math.log(10), 0.3))
    if strategy == "honest":
        return fast
    if strategy == "periodic":   # every 20th activation stalls
        return STALL_EXEC if k % 20 == 19 else fast
    if strategy == "random5":
        return STALL_EXEC if rng.random() < 0.05 else fast
    if strategy == "burst":      # 300 fast, then 30 stalls
        return STALL_EXEC if k % 330 >= 300 else fast
    if strategy == "slow":       # always slow but "useful"
        return STALL_EXEC
    if strategy == "oracle":     # knows whether it is waited on (side channel) and stalls then
        return STALL_EXEC if waited_if_known else fast
    raise ValueError(strategy)


def run(policy: str, strategy: str, n: int, seed: int, params: Params | None = None, debug=False,
        host: str = "default", trace=None) -> dict:
    HOST["name"] = host
    rng = np.random.default_rng(seed)  # same workload stream for all policies with the same seed
    sch = Scheduler([(p[0], p[2], p[3]) for p in PLUGINS], params or Params()) if policy == "SPS" else None
    dbg = []
    stalls, stalls_all = [], []
    arrived = {p[0]: 0 for p in PLUGINS}
    trig = {p[0]: 0 for p in PLUGINS}
    useful_arr = useful_tot = 0
    sched_cpu = 0
    adv_k = 0
    now_us = 0
    max_excl_run = {p[0]: 0 for p in PLUGINS}
    cur_excl_run = {p[0]: 0 for p in PLUGINS}
    adv_stalls_waited = 0
    for _ in range(n):
        now_us += int(rng.exponential(200_000))
        ctx = CTX_NAMES[rng.choice(len(CTX_NAMES), p=CTX_W)]
        _, pfc, mult, _tg = CTX[ctx]
        focus = rng.random() < pfc
        f_delay = int(min(SPIN_WINDOW, rng.lognormal(math.log(3000), 0.7)))
        triggered = []
        if focus:
            for p in PLUGINS:
                if in_scope(p[0], ctx) and rng.random() < min(1.0, p[7] * mult):
                    triggered.append(p[0])
        # Per-plugin random draws, drawn identically for every policy.
        draws = {}
        for p in PLUGINS:
            pid, _, _, _, med, sig, up, _, busyp = p
            draws[pid] = dict(exec=int(max(1, rng.lognormal(math.log(med), sig))),
                              useful=rng.random() < up, cold=parked_start(rng), hot=int(rng.integers(1, 4)),
                              busy=int(rng.integers(5, 41)) if rng.random() < busyp else 0)
        adv_draw_rng = np.random.default_rng(int(rng.integers(1 << 31)))
        noise = int(rng.integers(30, 121)) if rng.random() < 0.01 else 0

        # (a) key-down
        typing = ctx.endswith("type")
        if policy == "WA-park":
            hot_threads = 0
        elif policy == "WA-wake":
            hot_threads = 0 if typing else sum(in_scope(p[0], ctx) for p in PLUGINS)
        elif policy == "LABELS":
            hot_threads = 0 if typing else sum(in_scope(p[0], ctx) and p[2] != "refinement" for p in PLUGINS)
        else:
            hot_threads = sch.keydown(ctx, now_us // 1000)
        spin_wait = f_delay if focus else SPIN_WINDOW

        if not triggered:
            sched_cpu += hot_threads * (spin_wait + TAIL_SPIN)
            if sch:
                sch.spin_refund(hot_threads * sch.p.spin_est_us, hot_threads * (spin_wait + TAIL_SPIN))
                sch.observe_context(ctx, set())
            stalls_all.append(0)
            continue

        # (b) dispatch
        jobs = {}
        if policy == "SPS":
            plan = sch.dispatch(triggered, hot_threads)
            wait = set(plan.wait)
            t_end_cap = plan.t_star
            if debug:
                dbg.append((ctx, hot_threads, sorted(triggered), sorted(wait), plan.t_star,
                            [(a.pid, a.hot, a.offset_us, a.deadline_us) for a in plan.assign]))
            free = [0] * hot_threads
            for a in plan.assign:  # assignment list is in queue order
                d = draws[a.pid]
                if a.hot is not None:
                    st = free[a.hot] + d["hot"] + d["busy"]
                    state = "hot"
                else:
                    st, state = d["cold"] + d["busy"], "cold"
                lat = st - (free[a.hot] if a.hot is not None else 0)
                jobs[a.pid] = dict(start=st, lat=lat, state=state, deadline=a.deadline_us, a=a)
                if a.hot is not None:
                    ex = exec_of(a.pid, d, strategy, adv_k, adv_draw_rng, a.pid in wait)
                    jobs[a.pid]["exec"] = ex
                    free[a.hot] = st + ex
        else:
            if policy == "WA-park":
                wait = set(triggered)
            elif policy == "WA-wake":
                wait = set(triggered)
            else:
                wait = {pid for pid in triggered if PLUGINS[pid][2] != "refinement"}
            t_end_cap = DW
            for pid in triggered:
                d = draws[pid]
                hot = policy != "WA-park" and not typing and (policy == "WA-wake" or PLUGINS[pid][2] != "refinement")
                st = (d["hot"] if hot else d["cold"]) + d["busy"]
                jobs[pid] = dict(start=st, lat=st, state="hot" if hot else "cold", deadline=DW)
        for pid in triggered:
            j = jobs[pid]
            if "exec" not in j:
                j["exec"] = exec_of(pid, draws[pid], strategy, adv_k, adv_draw_rng, pid in wait)
            j["resp"] = SNAP + j["start"] + j["exec"]
        if ADV in triggered:
            adv_k += 1
            if ADV in wait and jobs[ADV]["exec"] >= STALL_EXEC:
                adv_stalls_waited += 1
        # writer end (from t0, before fuse)
        if wait and policy == "SPS":
            end = max(min(jobs[p]["resp"], SNAP + jobs[p]["a"].hold_us) for p in wait)
        elif wait:
            end = min(SNAP + t_end_cap, max(jobs[p]["resp"] for p in wait))
        else:
            end = SNAP
        stall = end + FUSE + noise
        stalls.append(stall)
        stalls_all.append(stall)
        for pid in triggered:
            trig[pid] += 1
            ok = jobs[pid]["resp"] <= end
            arrived[pid] += ok
            if debug and not ok:
                dbg.append(("miss", pid, jobs[pid]["state"], pid in wait, jobs[pid]["resp"],
                            jobs[pid]["deadline"] + SNAP, end, jobs[pid]["exec"], ctx, hot_threads))
            if draws[pid]["useful"] and pid != ADV:
                useful_tot += 1
                useful_arr += ok
        # CPU overhead: hot threads spin from key-down to focus, then until their last job + tail.
        hot_busy = [jobs[p]["start"] + jobs[p]["exec"] for p in triggered if jobs[p]["state"] == "hot"]
        hot_end = max(hot_busy) if hot_busy else 0
        n_cold = sum(1 for p in triggered if jobs[p]["state"] == "cold")
        spin_cost = hot_threads * (spin_wait + TAIL_SPIN) + (hot_end if hot_threads else 0)
        sched_cpu += spin_cost + n_cold * (WAKE_CPU + TAIL_SPIN)
        if sch:
            sch.spin_refund(hot_threads * sch.p.spin_est_us, hot_threads * (spin_wait + TAIL_SPIN))
            sch.observe_context(ctx, set(triggered))
            for pid in triggered:
                j = jobs[pid]
                sch.observe_reply(pid, j["state"], j["lat"], j["exec"], j["resp"] - SNAP,
                                  j["a"], pid in wait, draws[pid]["useful"])
        for pid in triggered:
            if PLUGINS[pid][2] == "refinement":
                continue
            if pid in wait:
                cur_excl_run[pid] = 0
            else:
                cur_excl_run[pid] += 1
                max_excl_run[pid] = max(max_excl_run[pid], cur_excl_run[pid])

    st = np.array(stalls)
    ffu = {PLUGINS[p][1]: arrived[p] / trig[p] if trig[p] else float("nan") for p in arrived}
    benign_tot = sum(trig[p] for p in BENIGN_WAITABLE)
    benign_arr = sum(arrived[p] for p in BENIGN_WAITABLE)
    return dict(policy=policy, strategy=strategy, n=n,
                ffu_benign=benign_arr / benign_tot,
                ffu_min=min(ffu[PLUGINS[p][1]] for p in BENIGN_WAITABLE if trig[p] >= 50),
                ffu_useful=useful_arr / max(1, useful_tot),
                ffu=ffu,
                stall_p50=float(np.percentile(st, 50)), stall_p99=float(np.percentile(st, 99)),
                stall_max=float(st.max()), stall_mean=float(st.mean()),
                cpu_us_per_key=sched_cpu / n,
                cpu_core_pct_5kps=sched_cpu / n * 5 / 1e6 * 100,
                adv_stalls_waited=adv_stalls_waited, adv_activations=adv_k,
                max_excl_run={PLUGINS[p][1]: max_excl_run[p] for p in max_excl_run if PLUGINS[p][2] != "refinement"},
                dbg=dbg)


def exec_of(pid, d, strategy, adv_k, rng, waited):
    if pid == ADV:
        return adversary_exec(strategy, adv_k, rng, waited)
    return min(200, d["exec"])  # per-call cap (B-2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=20000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--json")
    a = ap.parse_args()
    out = []
    for strategy in ["honest", "periodic", "random5", "burst", "slow", "oracle"]:
        for policy in ["WA-park", "WA-wake", "LABELS", "SPS"]:
            r = run(policy, strategy, a.n, a.seed)
            out.append(r)
            print(f"{strategy:9s} {policy:8s} ffu={r['ffu_benign']:.3f} min={r['ffu_min']:.3f} "
                  f"useful={r['ffu_useful']:.3f} stall p50/p99/max={r['stall_p50']:.0f}/"
                  f"{r['stall_p99']:.0f}/{r['stall_max']:.0f} cpu={r['cpu_us_per_key']:.0f}us/key "
                  f"({r['cpu_core_pct_5kps']:.1f}% core@5k/s) adv_waited_stalls={r['adv_stalls_waited']}")
    if a.json:
        with open(a.json, "w") as f:
            json.dump(out, f, indent=1)


if __name__ == "__main__":
    main()
