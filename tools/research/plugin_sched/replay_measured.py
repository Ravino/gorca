"""Feed the measured timings of the Rust bench into the reference model (sps.py / sim.py).
Run with: python3 -I -c 'import sys; sys.path.insert(0, "tools/research/plugin_sched"); exec(open("/var/tmp/m18a3/replay_sim.py").read())'
"""
import math
import sim
from sps import Params, Q

def run(label, snap, fuse, exec_scale, cold_median, all_active, n=3000):
    sim.SNAP, sim.FUSE = snap, fuse
    sim.DW = sim.D - snap - fuse
    pl = []
    for p in sim.PLUGINS:
        pid, name, lab, tier, med, sig, up, act, busy = p
        if all_active:
            # the bench: ten comparable first-party plugins, all triggered on every key, one median
            pl.append((pid, name, "interactive", "first", 45 * exec_scale, 0.35, 0.9, 1.0, 0.0))
        else:
            pl.append((pid, name, lab, tier, med * exec_scale, sig, up, act, busy))
    sim.PLUGINS[:] = pl
    # parked start: the measured one (median cold_median, sigma .5, cap 340)
    def parked(rng):
        return int(min(340, rng.lognormal(math.log(cold_median), 0.5)))
    sim.parked_start = parked
    if all_active:
        for c in sim.CTX:
            w, pfc, mult, tg = sim.CTX[c]
            sim.CTX[c] = (w, 1.0 if not c.endswith("type") else 0.0, 1.0, True)
    prm = Params(snap_us=snap, fuse_us=fuse)
    out = {}
    for pol in ("LABELS", "SPS"):
        r = sim.run(pol, "honest", n, 1, prm if pol == "SPS" else None)
        out[pol] = r
    print("%-52s LABELS ffu=%.3f min=%.3f p99=%4.0f | SPS ffu=%.3f min=%.3f p99=%4.0f cpu=%.0f us/key" % (
        label, out["LABELS"]["ffu_benign"], out["LABELS"]["ffu_min"], out["LABELS"]["stall_p99"],
        out["SPS"]["ffu_benign"], out["SPS"]["ffu_min"], out["SPS"]["stall_p99"], out["SPS"]["cpu_us_per_key"]))

import copy
ORIG = copy.deepcopy(sim.PLUGINS)
CTX0 = copy.deepcopy(sim.CTX)
def reset():
    sim.PLUGINS[:] = copy.deepcopy(ORIG)
    sim.CTX.clear(); sim.CTX.update(copy.deepcopy(CTX0))

for args in [
    ("A model defaults (snap 6, fuse 8, exec x1, cold 70)", 6, 8, 1.0, 70, False),
    ("B measured pre-send 75 + reserve 63", 75, 63, 1.0, 70, False),
    ("C B + measured exec (hot reply 125 us => x2.8)", 75, 63, 2.8, 70, False),
    ("D C + measured cold start median 120", 75, 63, 2.8, 120, False),
    ("E all ten plugins trigger, model timings", 6, 8, 1.0, 70, True),
    ("F all ten trigger, measured (75/63/x2.8/cold120)", 75, 63, 2.8, 120, True),
    ("G all ten trigger, measured but reserve 8", 75, 8, 2.8, 120, True),
    ("H all ten trigger, measured but pre-send 6", 6, 63, 2.8, 120, True),
]:
    reset()
    run(*args)
