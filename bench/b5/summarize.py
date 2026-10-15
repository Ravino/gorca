#!/usr/bin/env python3
"""Summarise a B-5 judge run: bench/results/b5/<label>/raw/<cond>/<arm>/r<N>/*.json -> summary.{json,md}.

Per (condition, scenario, metric, percentile) and arm: the value of every round, their median, min,
max and standard deviation, and a paired comparison against the first arm (wins = rounds in which
the arm's value was lower than the baseline's in the SAME round; arms are interleaved, so a round
is one comparable slice of host load). Also: whether real-time was granted (from the daemon's
topology reports), watchdog/SIGXCPU demotions, and stray real-time threads.
"""
import json
import statistics
import sys
from pathlib import Path

# (scenario, metric) pairs that carry the verdict, in report order.
SERIES = [
    ("L1", "l1_ms"),
    ("L2", "l2_ms"),
    ("K1", "k1_ms"),
    ("L3-50k", "l1_ms"),
    ("L2-50k", "l2_ms"),
    ("K1-50k", "k1_ms"),
]
PCTS = ["p50", "p95", "p99"]
GATES = {"l1_ms": 15.0, "k1_ms": 15.0, "l2_ms": 5.0}
LANE_THREADS = {"gorca-input", "gorca-speech-io", "gorca-reader"}


def load_rows(path: Path):
    d = json.loads(path.read_text())
    return d["meta"], {r["id"]: r for r in d["rows"]}


def stats(vals):
    return {
        "vals": vals,
        "median": statistics.median(vals),
        "min": min(vals),
        "max": max(vals),
        "stdev": statistics.stdev(vals) if len(vals) > 1 else 0.0,
    }


def main(out: Path):
    raw = out / "raw"
    # data[cond][arm][round] = {row id: value}
    data, metas, reports = {}, {}, {}
    for cond_dir in sorted(p for p in raw.iterdir() if p.is_dir()):
        cond = cond_dir.name
        for arm_dir in sorted(p for p in cond_dir.iterdir() if p.is_dir()):
            arm = arm_dir.name
            for rdir in sorted(p for p in arm_dir.iterdir() if p.is_dir()):
                rnd = int(rdir.name[1:])
                files = sorted(rdir.glob("2*.json"))
                if files:
                    meta, rows = load_rows(files[-1])
                    data.setdefault(cond, {}).setdefault(arm, {})[rnd] = {
                        k: v["value"] for k, v in rows.items()
                    }
                m = {}
                mf = rdir / "meta.txt"
                if mf.exists():
                    for tok in mf.read_text().split("\n"):
                        if "=" in tok:
                            k, v = tok.split("=", 1)
                            m[k.split()[0] if k.startswith("round") else k] = v
                metas.setdefault(cond, {}).setdefault(arm, {})[rnd] = m
                for rep in sorted(rdir.glob("rep.*.json")):
                    try:
                        reports.setdefault(cond, {}).setdefault(arm, []).append(
                            json.loads(rep.read_text())
                        )
                    except json.JSONDecodeError:
                        pass

    summary = {"cells": {}, "rt": {}, "loads": {}}
    arms_order = []
    for cond in data:
        for arm in data[cond]:
            if arm not in arms_order:
                arms_order.append(arm)
    preferred = ["single", "lanes", "lanes-rt", "lanes-rt-nopin", "lanes-pin"]
    arms_order.sort(key=lambda a: preferred.index(a) if a in preferred else len(preferred))
    base = arms_order[0] if arms_order else None

    for cond, arms in data.items():
        cells = summary["cells"].setdefault(cond, {})
        for scn, metric in SERIES:
            for p in PCTS + ["lost"]:
                rid = f"{scn}.{metric}.{p}" if p != "lost" else f"{scn}.lost"
                per_arm = {}
                for arm, rounds in arms.items():
                    vals = [r[rid] for _, r in sorted(rounds.items()) if rid in r]
                    if vals:
                        per_arm[arm] = stats(vals)
                if not per_arm:
                    continue
                # paired against the baseline arm
                if base in arms:
                    for arm, s in per_arm.items():
                        if arm == base:
                            continue
                        wins = ties = n = 0
                        for rnd, r in arms[arm].items():
                            b = arms[base].get(rnd, {}).get(rid)
                            a = r.get(rid)
                            if a is None or b is None:
                                continue
                            n += 1
                            wins += a < b
                            ties += a == b
                        s["paired_vs_" + base] = {"rounds": n, "lower": wins, "equal": ties}
                cells[rid] = per_arm
        # storm rates
        for scn in ("L3-50k", "L2-50k", "K1-50k"):
            rid = f"{scn}.storm_achieved_eps"
            per_arm = {}
            for arm, rounds in arms.items():
                vals = [r[rid] for _, r in sorted(rounds.items()) if rid in r]
                if vals:
                    per_arm[arm] = stats(vals)
            if per_arm:
                cells[rid] = per_arm

    for cond, arms in reports.items():
        for arm, reps in arms.items():
            # Reports are rewritten while the reader runs; use the ones that reached `final`,
            # falling back to the latest snapshot of those that were killed.
            granted_all = sum(1 for r in reps if r.get("rt_granted_all"))
            wd = sum(r.get("watchdog_demotions", 0) for r in reps)
            xc = sum(r.get("xcpu_demotions", 0) for r in reps)
            status = sorted({r.get("rt_status", "?") for r in reps})
            granted_labels = {}
            stray = set()
            for r in reps:
                for lane in r.get("lanes", []):
                    granted_labels[lane["granted"]] = granted_labels.get(lane["granted"], 0) + 1
                for t in r.get("threads", []):
                    if t["policy"] not in ("other",) and t["name"] not in LANE_THREADS and not t[
                        "name"
                    ].startswith("gorca-drain"):
                        stray.add(f'{t["name"]}:{t["policy"]}')
            summary["rt"].setdefault(cond, {})[arm] = {
                "reader_starts": len(reps),
                "rt_granted_all": granted_all,
                "lane_grants": granted_labels,
                "rt_status": status,
                "watchdog_demotions": wd,
                "xcpu_demotions": xc,
                "stray_non_other_threads": sorted(stray),
            }

    for cond, arms in metas.items():
        for arm, rounds in arms.items():
            summary["loads"].setdefault(cond, {})[arm] = {
                str(r): {
                    "load_before": m.get("load_before"),
                    "load_with_hog": m.get("load_with_hog"),
                    "load_after": m.get("load_after"),
                    "governor": m.get("governor"),
                    "exit": m.get("round").split("exit=")[-1] if "round" in m else None,
                }
                for r, m in sorted(rounds.items())
            }

    host = (out / "host.txt").read_text() if (out / "host.txt").exists() else ""
    (out / "summary.json").write_text(json.dumps(summary, indent=1))

    md = [f"# B-5 judge summary: {out.name}", "", "```", host.strip(), "```", ""]
    arms_cols = arms_order
    for cond, cells in summary["cells"].items():
        md += [f"## Condition `{cond}`", ""]
        md.append("Value = median over rounds [min..max]; `W` = rounds in which the arm was lower than "
                  f"`{base}` in the same round / rounds.")
        md.append("")
        md.append("| series | pct | " + " | ".join(arms_cols) + " |")
        md.append("|---|---|" + "---|" * len(arms_cols))
        for scn, metric in SERIES:
            for p in PCTS:
                rid = f"{scn}.{metric}.{p}"
                if rid not in cells:
                    continue
                row = [f"{scn} {metric}", p]
                for arm in arms_cols:
                    s = cells[rid].get(arm)
                    if not s:
                        row.append("-")
                        continue
                    t = f'{s["median"]:.3f} [{s["min"]:.3f}..{s["max"]:.3f}]'
                    pr = s.get("paired_vs_" + str(base))
                    if pr:
                        t += f' W{pr["lower"]}/{pr["rounds"]}'
                    row.append(t)
                md.append("| " + " | ".join(row) + " |")
        lost = {k: v for k, v in cells.items() if k.endswith(".lost")}
        if lost:
            md += ["", "Lost probes (max over rounds): " + "; ".join(
                f'{k.split(".")[0]}: ' + ", ".join(f'{a}={s["max"]:.0f}' for a, s in v.items())
                for k, v in lost.items())]
        storm = {k: v for k, v in cells.items() if k.endswith("storm_achieved_eps")}
        if storm:
            md += ["", "Storm achieved rate (ev/s, median): " + "; ".join(
                f'{k.split(".")[0]}: ' + ", ".join(f'{a}={s["median"]:.0f}' for a, s in v.items())
                for k, v in storm.items())]
        md.append("")
    md += ["## Real-time grants", ""]
    for cond, arms in summary["rt"].items():
        for arm, r in arms.items():
            md.append(
                f'- `{cond}` / `{arm}`: {r["reader_starts"]} reader starts, all-lanes-RT in '
                f'{r["rt_granted_all"]}, grants {r["lane_grants"]}, status {r["rt_status"]}, '
                f'watchdog demotions {r["watchdog_demotions"]}, SIGXCPU demotions {r["xcpu_demotions"]}, '
                f'stray non-OTHER threads {r["stray_non_other_threads"]}'
            )
    md += ["", "## Load per run", ""]
    for cond, arms in summary["loads"].items():
        for arm, rounds in arms.items():
            for r, m in rounds.items():
                md.append(
                    f'- `{cond}` / `{arm}` / r{r}: before {m["load_before"]}, with hog {m["load_with_hog"]}, '
                    f'governor {m["governor"]}, exit {m["exit"]}'
                )
    (out / "summary.md").write_text("\n".join(md) + "\n")
    print("\n".join(md))


if __name__ == "__main__":
    main(Path(sys.argv[1]))
