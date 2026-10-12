#!/usr/bin/env python3
"""Compare e2e result files (the JSON written by `e2e run`).

    compare.py A.json B.json [C.json ...]     side-by-side medians, PASS/FAIL per candidate
    compare.py --fails A.json                 list the failing metrics of one file
    compare.py --dir bench/results/e2e  newest full run of every label in the directory

Rows are keyed by (metric id, bus). "better" is lower for latency/CPU/memory and higher for
throughput; the arrow shows the direction of the change against the first file.
"""
import glob
import json
import os
import sys

HIGHER_IS_BETTER = ("storm_achieved_eps",)


def load(path):
    with open(path) as f:
        d = json.load(f)
    rows = {(r["id"], r["bus"]): r for r in d["rows"]}
    return d["meta"], rows


def fmt(v):
    if v is None:
        return "-"
    if abs(v) >= 1000 or v == int(v):
        return f"{v:,.0f}"
    return f"{v:.2f}"


def newest_full(label_dir):
    files = sorted(f for f in glob.glob(os.path.join(label_dir, "*.json"))
                   if "-quick" not in f and "-quick" not in f)
    return files[-1] if files else None


def main(argv):
    if not argv or argv[0] in ("-h", "--help"):
        print(__doc__)
        return 0
    if argv[0] == "--fails":
        meta, rows = load(argv[1])
        bad = [r for r in rows.values() if r["verdict"] == "FAIL"]
        for r in bad:
            print(f'{r["id"]:<34} {r["bus"]:<12} {fmt(r["value"]):>12} {r["op"]} {fmt(r["threshold"])}  {r["note"]}')
        print(f"{len(bad)} failing of {len(rows)}")
        return 1 if bad else 0
    if argv[0] == "--dir":
        argv = [p for p in (newest_full(d) for d in sorted(glob.glob(os.path.join(argv[1], "*")))) if p]
    files = argv
    loaded = [load(f) for f in files]
    labels = [m.get("label", os.path.basename(f)) for (m, _), f in zip(loaded, files)]
    ids = []
    for _, rows in loaded:
        for k in rows:
            if k not in ids:
                ids.append(k)
    w = max(len(l) for l in labels + ["metric"]) + 2
    print(f'{"metric":<34} {"bus":<11} ' + " ".join(f"{l:>{max(w, 14)}}" for l in labels) + "   threshold")
    for k in ids:
        cells = []
        base = loaded[0][1].get(k)
        thr = ""
        for i, (_, rows) in enumerate(loaded):
            r = rows.get(k)
            if r is None:
                cells.append("-")
                continue
            thr = f'{r["op"]} {fmt(r["threshold"])}' if r["threshold"] is not None else thr
            mark = {"PASS": "+", "FAIL": "!", "info": " "}[r["verdict"]]
            cell = f'{fmt(r["value"])}{mark}'
            if i > 0 and base and base["value"] and r["value"] is not None:
                ratio = r["value"] / base["value"]
                if abs(ratio - 1) > 0.05:
                    better = (ratio > 1) == any(s in k[0] for s in HIGHER_IS_BETTER)
                    cell += f' ({ratio:.2f}x {"better" if better else "worse"})'
            cells.append(cell)
        print(f"{k[0]:<34} {k[1]:<11} " + " ".join(f"{c:>{max(w, 14)}}" for c in cells) + f"   {thr}")
    print()
    for l, (m, rows) in zip(labels, loaded):
        fails = sum(1 for r in rows.values() if r["verdict"] == "FAIL")
        passes = sum(1 for r in rows.values() if r["verdict"] == "PASS")
        print(f'{l}: {passes} pass, {fails} fail  (git {m.get("git", "?")}, {m.get("date", "?")}, {m.get("profile", "?")}, load {m.get("loadavg_at_start", "?")})')
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
