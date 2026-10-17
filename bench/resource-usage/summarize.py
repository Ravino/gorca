#!/usr/bin/env python3
"""Summarize bench/results/resource-usage/<label>/raw into summary.md (median [min..max] over repeats)."""
import glob, json, os, re, statistics, sys

root = sys.argv[1]
KEYS = ["storm_achieved_eps", "steady_cpu_pct", "steady_rss_avg_mib", "peak_rss_hwm_mib",
        "steady_pss_avg_mib", "peak_pss_mib", "threads_max", "k2_ms.p99", "k2_ms.p50",
        "recovery_cpu_s", "after_rss_mib", "after_cpu_pct", "idle_cpu_pct", "pre_rss_mib",
        "lost", "superseded", "storm_max_lag_ms", "cpu_pct_1s_windows.p95"]
rows = {}
for d in sorted(glob.glob(f"{root}/raw/*/*/r*")):
    topo, level, rep = d.split("/")[-3:]
    js = glob.glob(f"{d}/2*.json")
    if not js:
        rows.setdefault((topo, level), {}).setdefault("_failed", []).append(rep)
        continue
    data = json.load(open(js[0]))
    txt = open(js[0].replace(".json", ".txt")).read()
    vals = {}
    for line in txt.splitlines():
        m = re.match(r"RU-\S+?\.(\S+)\s+daemon/\S+\s+([-\d.]+)\s", line)
        if m:
            vals[m.group(1)] = float(m.group(2))
    stats = {}
    for lg in glob.glob(f"{d}/*.reader.log"):
        t = open(lg).read()
        for k in ("stale_dropped", "utterances", "superseded", "replaced"):
            for m in re.finditer(rf"{k}: (\d+)", t):
                stats[k + "_" + str(len(stats))] = int(m.group(1))
        m = re.search(r"core stats=.*?stale_dropped: (\d+)", t)
        if m:
            vals["core_stale_dropped"] = float(m.group(1))
    rows.setdefault((topo, level), {}).setdefault("runs", []).append(vals)


def fmt(vs, f="{:.1f}"):
    if not vs:
        return "-"
    return f.format(statistics.median(vs)) + (f" [{f.format(min(vs))}..{f.format(max(vs))}]" if len(vs) > 1 else "")


def order(l):
    return -1 if l == "0" else (0.5 if l == "typ" else float(l))


out = ["# Resource usage summary", ""]
for topo in sorted({t for t, _ in rows}):
    out += [f"## {topo}", "",
            "| level (ev/s target) | achieved ev/s | CPU % of 1 core | RSS avg MiB | RSS peak (HWM) MiB | PSS avg MiB | threads | K2 p99 ms | recovery s | RSS after MiB | lost | n |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for (t, l) in sorted(rows, key=lambda k: (k[0], order(k[1]))):
        if t != topo:
            continue
        runs = rows[(t, l)].get("runs", [])
        g = lambda k: [r[k] for r in runs if k in r]
        out.append(f"| {l} | {fmt(g('storm_achieved_eps'), '{:.0f}')} | {fmt(g('steady_cpu_pct') or g('idle_cpu_pct'))} | {fmt(g('steady_rss_avg_mib'))} | {fmt(g('peak_rss_hwm_mib'))} | {fmt(g('steady_pss_avg_mib'))} | {fmt(g('threads_max'), '{:.0f}')} | {fmt(g('k2_ms.p99'), '{:.2f}')} | {fmt(g('recovery_cpu_s'))} | {fmt(g('after_rss_mib'))} | {fmt(g('lost'), '{:.0f}')} | {len(runs)} |")
    out.append("")
open(f"{root}/summary.md", "w").write("\n".join(out))
print("\n".join(out))
