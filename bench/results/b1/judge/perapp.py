#!/usr/bin/env python3
"""Per-app focus latency (truth focus -> first presentation of that path) and calls per app.

Judge helper for B-1. Usage: perapp.py <run-dir> [<run-dir> ...]
Latency here is fake-app focus emit -> presentation log line (not tap -> SPEAK), so it is a
relative per-app breakdown, not a gate value.
"""
import json
import sys
from collections import defaultdict


def pct(xs, p):
    if not xs:
        return float("nan")
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(round(p / 100 * (len(xs) - 1))))]


def load(path):
    out = []
    with open(path) as f:
        for line in f:
            try:
                out.append(json.loads(line))
            except ValueError:
                pass
    return out


def main():
    for d in sys.argv[1:]:
        truth = [t for t in load(f"{d}/truth.jsonl") if t.get("focus")]
        pres = [p for p in load(f"{d}/present.jsonl") if "path" in p and p.get("refines") is None]
        by_path = defaultdict(list)
        for p in pres:
            by_path[p["path"]].append(p)
        first = {}
        for p in pres:
            first.setdefault(p.get("app_bus"), p.get("name", ""))
        sig = {"Type to search": "shell", "Search": "settings", "Home": "files",
               "hung-app window": "hung", "Accessibility": "gtk3"}
        model = {b: sig.get(n, n[:12]) for b, n in first.items()}
        lat = defaultdict(lambda: defaultdict(list))
        for i, t in enumerate(truth):
            nxt = truth[i + 1]["t_ns"] if i + 1 < len(truth) else float("inf")
            cand = [p for p in by_path.get(t["path"], []) if t["t_ns"] <= p["t_ns"] < nxt]
            if not cand:
                continue
            p = cand[0]
            lat[model.get(p.get("app_bus"), t["app"])][p["source"]].append((p["t_ns"] - t["t_ns"]) / 1e6)
        c = json.load(open(f"{d}/counters.json"))
        calls = {a: sum(v.values()) for a, v in c.get("calls", {}).items()}
        print(f"== {d}")
        for app in sorted(lat):
            tot = sum(len(v) for v in lat[app].values())
            row = [f"hit-rate={len(lat[app].get('hit', []))/tot:.3f}"]
            for src in sorted(lat[app]):
                xs = lat[app][src]
                row.append(f"{src}: n={len(xs)} p50={pct(xs,50):.2f} p99={pct(xs,99):.2f} max={max(xs):.2f}")
            print(f"  {app:10s} " + " | ".join(row))
        print("  calls per app:", json.dumps(calls))


if __name__ == "__main__":
    main()
