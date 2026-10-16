#!/usr/bin/env python3
"""compare_m8.py - per-scenario and per-step diff of two e2e-m8 step reports (M9 baseline).

  compare_m8.py <chromium-steps.md> <firefox-steps.md> [--diff]

Prints a scenario table (PASS/GAP/XFAIL/XPASS/FAIL per engine; the XFAIL marks are the Firefox
baseline of m8_baseline.rs and apply to both engines, so FAIL on one engine and XFAIL/PASS on the
other is an engine difference by construction) and, with --diff, every step whose status differs.
"""
import re
import sys
from collections import OrderedDict

ROW = re.compile(r"^\| (\S+) \| (P\d) \| (\w+) \| ([^|]*) \| (.*?) \| (.*?) \| (.*) \|$")


def load(path):
    steps = OrderedDict()
    scen = None
    for line in open(path, encoding="utf-8"):
        m = re.match(r"^## (M8-\S+)", line)
        if m:
            scen = m.group(1)
            continue
        m = ROW.match(line.rstrip("\n"))
        if m and scen:
            sid, prio, st, ms, what, wanted, heard = m.groups()
            steps[sid] = dict(scen=scen, prio=prio, st=st, ms=ms.strip(), what=what, wanted=wanted, heard=heard)
    return steps


cr, ff = load(sys.argv[1]), load(sys.argv[2])
cols = ["PASS", "GAP", "XFAIL", "XPASS", "FAIL"]
scen = OrderedDict()
for sid, s in cr.items():
    scen.setdefault(s["scen"], {"cr": dict.fromkeys(cols, 0), "ff": dict.fromkeys(cols, 0), "n": 0})
    scen[s["scen"]]["n"] += 1
    scen[s["scen"]]["cr"][s["st"]] = scen[s["scen"]]["cr"].get(s["st"], 0) + 1
    f = ff.get(sid)
    if f:
        scen[s["scen"]]["ff"][f["st"]] = scen[s["scen"]]["ff"].get(f["st"], 0) + 1
print("| scenario | steps | Chromium P/G/X/XP/F | Firefox P/G/X/XP/F |")
print("|---|---|---|---|")
tot = {"cr": dict.fromkeys(cols, 0), "ff": dict.fromkeys(cols, 0), "n": 0}
for k, v in scen.items():
    print("| %s | %d | %s | %s |" % (k, v["n"], "/".join(str(v["cr"][c]) for c in cols), "/".join(str(v["ff"][c]) for c in cols)))
    tot["n"] += v["n"]
    for e in ("cr", "ff"):
        for c in cols:
            tot[e][c] += v[e][c]
print("| **total** | %d | %s | %s |" % (tot["n"], "/".join(str(tot["cr"][c]) for c in cols), "/".join(str(tot["ff"][c]) for c in cols)))
if "--diff" in sys.argv:
    print()
    print("| step | P | Chromium | Firefox | wanted | Chromium heard / reason |")
    print("|---|---|---|---|---|---|")
    for sid, s in cr.items():
        f = ff.get(sid)
        if f and f["st"] != s["st"]:
            print("| %s | %s | %s | %s | %s | %s |" % (sid, s["prio"], s["st"], f["st"], s["wanted"][:70], s["heard"][:160]))
