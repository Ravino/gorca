"""Run every experiment of docs/reports/plugin-statistical-scheduler.md and write results.json.

Usage: python run_all.py [--n 20000] [--out DIR]   (about 5 minutes on one core)
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os

import sps as S
import sim
from sps import Q, Params


def plan_tuple(k, plan):
    return (k, [(a.pid, a.hot, a.offset_us, a.deadline_us, a.grant_fuel, a.hold_us) for a in plan.assign],
            plan.wait, plan.t_star)


class Recorder(S.Scheduler):
    log: list = []

    def keydown(self, ctx, now_ms):
        k = super().keydown(ctx, now_ms)
        Recorder.log.append(("keydown", (ctx, now_ms), k))
        return k

    def spin_refund(self, est, act):
        super().spin_refund(est, act)
        Recorder.log.append(("spin_refund", (est, act), None))

    def dispatch(self, trig, k):
        plan = super().dispatch(trig, k)
        Recorder.log.append(("dispatch", (list(trig), k), plan_tuple(k, plan)))
        return plan

    def observe_context(self, ctx, trig):
        super().observe_context(ctx, trig)
        Recorder.log.append(("observe_context", (ctx, set(trig)), None))

    def observe_reply(self, pid, state, start, ex, resp, a, waited, useful):
        super().observe_reply(pid, state, start, ex, resp, a, waited, useful)
        Recorder.log.append(("observe_reply", (pid, state, start, ex, resp, copy.copy(a), waited, useful), None))


def replay_check(n):
    """Record every scheduler call of one SPS run, replay into a fresh scheduler, compare outputs."""
    Recorder.log = []
    orig = sim.Scheduler
    sim.Scheduler = Recorder
    try:
        sim.run("SPS", "oracle", n, 7)
    finally:
        sim.Scheduler = orig
    log = Recorder.log
    fresh = S.Scheduler([(p[0], p[2], p[3]) for p in sim.PLUGINS], Params())
    h = hashlib.sha256()
    mism = 0
    for name, args, out in log:
        if name == "dispatch":
            plan = fresh.dispatch(*args)
            got = plan_tuple(args[1], plan)
        elif name == "keydown":
            got = fresh.keydown(*args)
        else:
            getattr(fresh, name)(*args)
            got = None
        mism += got != out
        h.update(repr(got).encode())
    return dict(calls=len(log), mismatches=mism, sha256=h.hexdigest()[:16])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=20000)
    ap.add_argument("--out", default=os.path.dirname(os.path.abspath(__file__)))
    a = ap.parse_args()
    res = {"main": [], "lam": [], "kmult": [], "pmqos": [], "seed2": [], "budget": []}
    strategies = ["honest", "periodic", "random5", "burst", "slow", "oracle"]
    for st in strategies:
        for pol in ["WA-park", "WA-wake", "LABELS", "SPS"]:
            r = sim.run(pol, st, a.n, 1)
            r.pop("dbg", None)
            res["main"].append(r)
            print(st, pol, round(r["ffu_benign"], 4), round(r["ffu_min"], 4), r["stall_p50"], r["stall_p99"],
                  round(r["cpu_core_pct_5kps"], 1), r["adv_stalls_waited"], flush=True)
    for pol in ["WA-park", "LABELS", "SPS"]:
        r = sim.run(pol, "oracle", a.n, 2)
        r.pop("dbg", None)
        res["seed2"].append(r)
    half = a.n // 2
    for lam in [3, 13, 33, 65]:
        r = sim.run("SPS", "oracle", half, 1, Params(lam_q16=lam))
        r.pop("dbg", None)
        r["lam_q16"] = lam
        res["lam"].append(r)
    for km in [Q // 2, Q, 3 * Q // 2, 2 * Q]:
        r = sim.run("SPS", "oracle", half, 1, Params(k_mult_q16=km))
        r.pop("dbg", None)
        r["k_mult"] = km / Q
        res["kmult"].append(r)
    for rate in [30_000, 75_000, 150_000]:
        r = sim.run("SPS", "oracle", half, 1, Params(spin_rate_us_per_s=rate))
        r.pop("dbg", None)
        r["spin_rate"] = rate
        res["budget"].append(r)
    for pol in ["WA-park", "LABELS", "SPS"]:
        r = sim.run(pol, "oracle", half, 1, host="pmqos")
        r.pop("dbg", None)
        res["pmqos"].append(r)
    res["replay"] = replay_check(3000)
    print("replay", res["replay"])
    with open(os.path.join(a.out, "results.json"), "w") as f:
        json.dump(res, f, indent=1, default=str)


if __name__ == "__main__":
    main()
