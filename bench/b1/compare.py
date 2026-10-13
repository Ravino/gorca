#!/usr/bin/env python3
"""Compare B-1 result files (the JSON written by `b1 run`), arms side by side.

    compare.py RESULT.json [RESULT2.json ...]    medians per metric, one column per arm, min..max
    compare.py --fails RESULT.json               failing gates per arm, exit 1 if any

Rows are keyed by metric id; the arm is the row's `bus` field. A cell is `median (min..max)`.
Cells that fail their gate are marked with `!`. The arms are NOT ranked here (SPEC 9 ranks the
passing arms by the reviewer); this only shows the numbers and the gate.
"""
import json
import sys


def load(path):
    with open(path) as f:
        d = json.load(f)
    return d["meta"], d["rows"]


def fmt(v):
    if v is None:
        return "-"
    if abs(v) >= 1000 or v == int(v):
        return f"{v:,.0f}"
    return f"{v:.3g}"


def main(argv):
    if not argv or argv[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    if argv[0] == "--fails":
        _, rows = load(argv[1])
        bad = [r for r in rows if r["verdict"] == "FAIL"]
        for r in bad:
            print(f'{r["bus"]:<12} {r["id"]:<26} {fmt(r["value"]):>10} {r["op"]} {fmt(r["threshold"])}  {r["note"]}')
        print(f"{len(bad)} failing of {len(rows)}")
        return 1 if bad else 0
    cells = {}
    arms = []
    for p in argv:
        _, rows = load(p)
        for r in rows:
            if r["bus"] not in arms:
                arms.append(r["bus"])
            cells[(r["id"], r["bus"])] = r
    ids = sorted({k[0] for k in cells})
    w = 22
    print(f'{"metric":<26}' + "".join(f"{a:>{w}}" for a in arms))
    for i in ids:
        line = f"{i:<26}"
        for a in arms:
            r = cells.get((i, a))
            if r is None:
                line += f'{"-":>{w}}'
                continue
            mark = "!" if r["verdict"] == "FAIL" else " "
            c = f'{fmt(r["value"])} ({fmt(r["min"])}..{fmt(r["max"])}){mark}'
            line += f"{c:>{w}}"
        print(line)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
