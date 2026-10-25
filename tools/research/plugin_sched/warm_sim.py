"""Warm hook vs replay vs nothing: simulation on the measured host model (plugin-warm-hook-model.md).

Host model (us), from warm_model.py and the M18a-4 matrix; [M] measured, [H] assumption:
  call    = m_core * (g_i + O) + D_f
  O       = per-call host overhead: c_w + F' + E' (1 - exp(-tau/T)) after `tau` without a touch
            (fit: c_w 6.5, F 58, E 68, T 25 ms) [M fit, 4 points]; F' = F (1 - phi * lease) is the
            part of the deep-idle step the cpu_dma_latency lease removes (phi [H], swept);
            E' = lam_E * E (lam_E = 1: the loaded host of the matrix, 0.3: quiet desktop [H]).
            A spinning worker re-colds only by the slow awake term (9.7 us after 3 ms) [M].
            A hook with coverage kappa leaves O = kappa * O_after_hook + (1 - kappa) * O_no_hook
            (replay in the loaded bench: kappa 0.34 [M]).
  D_f     = fresh per-job data (new snapshot rows): a hook cannot warm it, 15 us [H]
  m_core  = 1 (P), 1.21 (E), 2.6 (LP-E) [M, 3 ms gaps]; placement "any" draws P/E/LP-E with
            0.55 / 0.40 / 0.05 [H], placement "P" is always 1
  start   = hot worker: U[1,3] + 5 [M pick-up 6.3]; parked: LN(110, .5) cap 400 [M k1 107-116],
            with the lease the pm_qos mixture of sim.py (median 8 / 18, cap 60) [M m18a-2 probe]
  hook    = runs on the plugin's own worker at key-down; CPU = wake + m (gamma g_i + O_cold);
            a real job that arrives while the hook still runs waits for it (collision)

Policies: LABELS (shipped), SPS (no warm), SPS+REPLAY (current warm_on_arm: armed workers replay
their last job, gamma 1), SPS+HOOK (sps.py keydown_plan warm selection; armed -> hook then spin,
others -> hook then park), HOOK-ONLY (keydown_plan with K = 0: hooks, no spinning),
LABELS+HOOK (arm every non-refinement worker and hook it: the CPU upper bound).

Usage: python warm_sim.py [--n 4000] [--jobs 14] [--out results.json]
"""
from __future__ import annotations

import argparse
import itertools
import json
import math
from multiprocessing import Pool

import numpy as np

import sim
import warm_model
from sps import LABELS, Params, Scheduler

HP = warm_model.params()  # c_w, F, E, T
TAIL = 40
WAKE_CPU = 3
SPIN_WINDOW = 50_000
CORE_M = {"P": 1.0, "E": 1.21, "LPE": 2.6}


def overhead(tau, F, E, slept=True):
    return warm_model.overhead(max(0.0, tau), F, E, HP["T"], slept)


def spin_oh(tau):
    return warm_model.spin_overhead(max(0.0, tau), HP["T"])


def workload(name):
    if name == "mix":
        return [p for p in sim.PLUGINS]
    # ten comparable first-party plugins (the M18a-3/4 bench): tiny guest work, all triggered
    return [(i, f"copy{i}", "interactive", "first", 6, 0.3, 0.9, 1.0, 0.0) for i in range(10)]


def run(cfg):
    n, seed = cfg["n"], cfg["seed"]
    pol = cfg["policy"]
    lease, phi, place, kappa, lam_e = cfg["lease"], cfg["phi"], cfg["place"], cfg["kappa"], cfg["lam_e"]
    snap, fuse, d_f = cfg["snap"], cfg["fuse"], cfg["d_f"]
    gamma_hook = cfg["gamma"]
    kappa_replay = cfg.get("kappa_replay", min(1.0, kappa + 0.1))
    plugins = workload(cfg["workload"])
    bench = cfg["workload"] == "bench10"
    dw = 300 - snap - fuse
    F_eff = HP["F"] * (1 - phi * lease)
    E_eff = HP["E"] * lam_e
    prm = Params(snap_us=snap, fuse_us=fuse)
    sch = Scheduler([(p[0], p[2], p[3]) for p in plugins], prm) if pol.startswith(("SPS", "HOOK")) else None
    rng = np.random.default_rng(seed)
    last_touch = {p[0]: -10**9 for p in plugins}
    last_job = {p[0]: -10**9 for p in plugins}
    last_had_job = {p[0]: False for p in plugins}
    now = 0
    stalls, arrived, trig_n, all_ok, turns = [], 0, 0, 0, 0
    cpu_spin = cpu_hook = cpu_wake = 0
    hooks_n = 0
    collisions = 0
    adv = sim.ADV if not bench else -1
    waitable = {p[0] for p in plugins if p[2] != "refinement" and p[0] != adv}

    def core_m():
        if place == "P":
            return 1.0
        u = rng.random()
        return CORE_M["P"] if u < 0.55 else (CORE_M["E"] if u < 0.95 else CORE_M["LPE"])

    def parked_start():
        if lease:
            u = rng.random()
            x = rng.lognormal(math.log(8), 0.5) if u < 0.7 else rng.lognormal(math.log(18), 0.4)
            return int(min(60, x)) + 5
        return int(min(400, rng.lognormal(math.log(110), 0.5)))

    for _ in range(n):
        now += int(rng.exponential(200_000))
        ctx = sim.CTX_NAMES[rng.choice(len(sim.CTX_NAMES), p=sim.CTX_W)]
        _, pfc, mult, tg = sim.CTX[ctx]
        typing = ctx.endswith("type")
        if bench:
            pfc, mult = (0.0, 0.0) if typing else (1.0, 1.0)
        focus = rng.random() < pfc
        L = int(min(SPIN_WINDOW, rng.lognormal(math.log(3000), 0.7)))
        triggered = []
        if focus:
            for p in plugins:
                if (bench or sim.in_scope(p[0], ctx)) and rng.random() < min(1.0, p[7] * mult):
                    triggered.append(p[0])
        draws = {}
        for p in plugins:
            pid, _, _, _, med, sig, up, _, busyp = p
            draws[pid] = dict(g=float(max(1, rng.lognormal(math.log(med), sig))), useful=rng.random() < up,
                              m=core_m(), cold=parked_start(), hot=int(rng.integers(1, 4)) + 5,
                              hm=core_m(), hcold=parked_start())
        noise = int(rng.integers(30, 121)) if rng.random() < 0.01 else 0

        # ---- key-down: arm and warm
        if pol in ("LABELS", "LABELS+HOOK"):
            armed = [] if typing else [p[0] for p in plugins if p[2] != "refinement"
                                       and (bench or sim.in_scope(p[0], ctx))]
            hooks = list(armed) if pol == "LABELS+HOOK" else []
            k = len(armed)
        else:
            want_warm = pol in ("SPS+HOOK", "HOOK-ONLY")
            k, armed, hooks = sch.keydown_plan(ctx, now // 1000, warm=want_warm,
                                               k_force=0 if pol == "HOOK-ONLY" else None)
            if pol == "SPS+REPLAY":
                hooks = [pid for pid in armed if last_had_job[pid]]
        hook_end = {}
        for h in hooks:
            d = draws[h]
            wake = 0 if h in armed else d["hcold"]
            replay = pol == "SPS+REPLAY"
            gam = 1.0 if replay else gamma_hook
            o_cold = overhead(now + wake - last_touch[h], F_eff, E_eff)
            cost = d["hm"] * (gam * d["g"] + o_cold) + (d_f if replay else 0)
            hook_end[h] = wake + cost
            last_touch[h] = now + wake + cost
            cpu_hook += cost + WAKE_CPU + TAIL
            hooks_n += 1
            if sch and pol != "SPS+REPLAY":
                sch.observe_warm(h, int(cost + WAKE_CPU), sch.p.warm_est_us if not sch.warm_cost_h.total()
                                 else sch.warm_cost_h.quantile(1 << 15))
        spin_wait = L if focus else SPIN_WINDOW
        if not triggered:
            cpu_spin += k * (spin_wait + TAIL)
            if sch:
                sch.spin_refund(k * sch.p.spin_est_us, k * (spin_wait + TAIL))
                sch.observe_context(ctx, set())
            continue

        # ---- dispatch at t_d = now + L
        t_d = now + L
        kap = kappa_replay if pol == "SPS+REPLAY" else kappa
        jobs = {}
        for pid in triggered:
            d = draws[pid]
            hot = pid in armed
            start = d["hot"] if hot else d["cold"]
            hooked = pid in hook_end
            if hooked and hook_end[pid] > L:
                start = max(start, int(hook_end[pid] - L) + (d["hot"] if hot else 5))
                collisions += 1
            t_run = t_d + snap + start
            o_base = overhead(t_run - last_job[pid], F_eff, E_eff)
            if hooked:
                lead = t_run - last_touch[pid]
                if hot:
                    o_after = spin_oh(lead)
                else:
                    o_after = overhead(lead, F_eff, E_eff, slept=lease or lead >= warm_model.C2_RESIDENCY_US)
                o = kap * o_after + (1 - kap) * o_base
            else:
                o = o_base
            ex = int(d["m"] * (d["g"] + o) + d_f)
            ex = min(ex, 400)
            jobs[pid] = dict(start=start, exec=ex, resp=snap + start + ex, hot=hot, warm=hooked)
            last_touch[pid] = last_job[pid] = t_run + ex
            last_had_job[pid] = True

        if sch:
            plan = sch.dispatch(triggered, 0, armed=list(armed), warm=frozenset(p for p in triggered if p in hook_end))
            wait = set(plan.wait)
            amap = {a.pid: a for a in plan.assign}
            end = max([min(jobs[p]["resp"], snap + amap[p].hold_us) for p in wait] + [snap])
        else:
            wait = {p for p in triggered if p in waitable or (p == adv)}
            wait = {p for p in wait if dict((q[0], q[2]) for q in plugins)[p] != "refinement"}
            end = min(snap + dw, max([jobs[p]["resp"] for p in wait] + [snap]))
        stalls.append(end + fuse + noise)
        turns += 1
        ok_all = True
        for pid in triggered:
            if pid in waitable:
                trig_n += 1
                ok = jobs[pid]["resp"] <= end
                arrived += ok
                ok_all &= ok
        all_ok += ok_all
        hot_end = max([jobs[p]["start"] + jobs[p]["exec"] for p in triggered if jobs[p]["hot"]] + [0])
        n_cold = sum(1 for p in triggered if not jobs[p]["hot"])
        cpu_spin += k * (spin_wait + TAIL) + hot_end
        cpu_wake += n_cold * (WAKE_CPU + TAIL)
        if sch:
            sch.spin_refund(k * sch.p.spin_est_us, k * (spin_wait + TAIL))
            sch.observe_context(ctx, set(triggered))
            for pid in triggered:
                j = jobs[pid]
                a = amap[pid]
                sch.observe_reply(pid, "hot" if j["hot"] else "cold", j["start"], j["exec"], j["resp"] - snap,
                                  a, pid in wait, draws[pid]["useful"], warm=j["warm"])
    st = np.array(stalls) if stalls else np.array([0])
    return dict(cfg, ffu=arrived / max(1, trig_n), all_in_time=all_ok / max(1, turns),
                stall_p50=float(np.percentile(st, 50)), stall_p99=float(np.percentile(st, 99)),
                cpu_spin=cpu_spin / n, cpu_hook=cpu_hook / n, cpu_wake=cpu_wake / n,
                cpu_total=(cpu_spin + cpu_hook + cpu_wake) / n, hooks_per_key=hooks_n / n,
                collisions=collisions / n)


POLICIES = ["LABELS", "SPS", "SPS+REPLAY", "SPS+HOOK", "HOOK-ONLY", "LABELS+HOOK"]


def grid(n):
    base = dict(n=n, seed=11, d_f=15, gamma=0.5, phi=0.5)
    out = []
    for wl, (snap, fuse), lease, place, kappa, lam_e, pol in itertools.product(
            ["mix", "bench10"], [(60, 52), (20, 24)], [0, 1], ["any", "P"], [0.34, 0.75], [1.0, 0.3], POLICIES):
        c = dict(base, workload=wl, snap=snap, fuse=fuse, lease=lease, place=place, kappa=kappa, lam_e=lam_e,
                 policy=pol)
        if kappa == 0.34:
            c["kappa_replay"] = 0.34   # the loaded-bench calibration applies to replay itself
        out.append(c)
    # phi sweep (lease effect on the deep-idle step) for the headline scenario
    for phi, pol in itertools.product([0.0, 0.5, 0.9], POLICIES):
        out.append(dict(base, workload="mix", snap=60, fuse=52, lease=1, place="P", kappa=0.75, lam_e=0.3,
                        policy=pol, phi=phi, tag="phi"))
    # D_f and gamma sensitivity
    for d_f, gam, pol in itertools.product([5, 15, 40], [0.25, 0.5, 1.0], ["SPS+HOOK", "HOOK-ONLY"]):
        out.append(dict(base, workload="mix", snap=60, fuse=52, lease=1, place="P", kappa=0.75, lam_e=0.3,
                        policy=pol, d_f=d_f, gamma=gam, tag="sens"))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=4000)
    ap.add_argument("--jobs", type=int, default=12)
    ap.add_argument("--out")
    ap.add_argument("--one", help="one scenario, all policies: key=value,... e.g. workload=bench10,lease=1")
    a = ap.parse_args()
    if a.one:
        base = dict(n=a.n, seed=11, d_f=15, gamma=0.5, phi=0.5, workload="mix", snap=60, fuse=52, lease=0,
                    place="any", kappa=0.75, lam_e=1.0)
        for kv in a.one.split(","):
            k, v = kv.split("=")
            base[k] = v if k in ("workload", "place") else (float(v) if "." in v else int(v))
        cfgs = [dict(base, policy=p) for p in POLICIES]
    else:
        cfgs = grid(a.n)
    with Pool(a.jobs) as pool:
        res = pool.map(run, cfgs, chunksize=1)
    if a.out:
        with open(a.out, "w") as f:
            json.dump(res, f, indent=0)
    for r in res:
        print(f"{r['workload']:7s} snap{r['snap']:3d} lease{r['lease']} phi{r['phi']:.1f} {r['place']:3s} "
              f"k{r['kappa']:.2f} lE{r['lam_e']:.1f} df{r['d_f']:2d} g{r['gamma']:.2f} {r['policy']:11s} "
              f"ffu={r['ffu']:.3f} all={r['all_in_time']:.3f} p50={r['stall_p50']:4.0f} p99={r['stall_p99']:4.0f} "
              f"cpu={r['cpu_total']:6.0f} (spin {r['cpu_spin']:6.0f} hook {r['cpu_hook']:5.0f}) "
              f"hooks/key={r['hooks_per_key']:.2f} coll={r['collisions']:.3f} {r.get('tag', '')}")


if __name__ == "__main__":
    main()
