#!/usr/bin/env python3
"""Phase 2 acceptance summary: one table over every gate of every suite.

    summarize.py <run-dir> --mode MODE --suites "m1 m3 ..." --all-suites "..."

Reads <run-dir>/status.tsv (one line per step attempt), the latest attempt of each step
(<suite>/<step>.aN/{meta.txt,load.tsv,res/**/*.json,x1-run*.tsv}) and writes <run-dir>/summary.md.
Exit status: 0 only when every selected step ran, exited 0, reported no FAIL row and skipped
nothing. Earlier attempts and failing results stay on disk; the summary lists them.
"""
import argparse
import glob
import json
import os
import re
import statistics
import sys

X1_HOOK = re.compile(r"(on_event|present)_")
X1_RESET = re.compile(r"(crash_reset|spin_interrupt_reset)")
X1_HOOK_LIMIT_NS = 100_000       # X1: hook p99 <= 100 us
X1_RESET_LIMIT_NS = 50_000_000   # trap / loop recovery < 50 ms


def read_kv(path):
    kv = {}
    try:
        for line in open(path):
            if "=" in line:
                k, v = line.rstrip("\n").split("=", 1)
                kv[k] = v
    except OSError:
        pass
    return kv


def load_range(step_dir):
    loads, mem = [], []
    try:
        for line in open(os.path.join(step_dir, "load.tsv")):
            p = line.rstrip("\n").split("\t")
            if len(p) >= 3 and p[1]:
                loads.append(float(p[1]))
                if p[2]:
                    mem.append(float(p[2]))
    except (OSError, ValueError):
        pass
    if not loads:
        return None
    return min(loads), max(loads), (max(mem) if mem else None)


def fmt(v):
    if v is None:
        return "-"
    if isinstance(v, (int, float)):
        if v == int(v) and abs(v) < 1e9:
            return str(int(v))
        return f"{v:.3f}"
    return str(v)


def e2e_rows(step_dir):
    """Rows of every e2e/b1 result json under the step dir (the newest json of each directory)."""
    rows, meta = [], {}
    dirs = {}
    for f in glob.glob(os.path.join(step_dir, "**", "*.json"), recursive=True):
        dirs.setdefault(os.path.dirname(f), []).append(f)
    for d, files in dirs.items():
        for f in sorted(files)[-1:]:
            try:
                j = json.load(open(f))
            except (OSError, ValueError):
                continue
            if isinstance(j, dict) and isinstance(j.get("rows"), list) and j["rows"] and "verdict" in j["rows"][0]:
                rows += j["rows"]
                meta.update(j.get("meta", {}))
    return rows, meta


def x1_rows(step_dir):
    runs = {}
    for f in sorted(glob.glob(os.path.join(step_dir, "x1-run*.tsv"))):
        for line in open(f):
            p = line.rstrip("\n").split("\t")
            if len(p) >= 4 and p[0] != "case":
                try:
                    runs.setdefault(p[0], []).append((float(p[2]), float(p[3])))
                except ValueError:
                    pass
    rows = []
    for case, vals in sorted(runs.items()):
        p99 = [v[1] for v in vals]
        if X1_RESET.search(case):
            op, thr, lim = "<=", X1_RESET_LIMIT_NS, X1_RESET_LIMIT_NS
        elif X1_HOOK.search(case):
            op, thr, lim = "<=", X1_HOOK_LIMIT_NS, X1_HOOK_LIMIT_NS
        else:
            op, thr, lim = "", None, None
        verdict = "info" if lim is None else ("PASS" if max(p99) <= lim else "FAIL")
        # Gated on the worst repeat: a hook budget is a ceiling, not a typical value.
        rows.append({"id": f"X1.{case}.p99", "scenario": "X1", "bus": "-",
                     "value": statistics.median(p99) / 1000.0, "min": min(p99) / 1000.0,
                     "max": max(p99) / 1000.0, "runs": len(vals), "unit": "us", "op": op,
                     "threshold": None if thr is None else thr / 1000.0,
                     "verdict": verdict, "note": "gated on the max over repeats"})
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("run_dir")
    ap.add_argument("--mode", default="ACCEPTANCE")
    ap.add_argument("--suites", default="")
    ap.add_argument("--all-suites", default="")
    a = ap.parse_args()
    out = a.run_dir
    selected = a.suites.split()

    attempts = {}  # (suite, step) -> list of (attempt, exit, res)
    try:
        for line in open(os.path.join(out, "status.tsv")):
            p = line.rstrip("\n").split("\t")
            if len(p) == 5:
                attempts.setdefault((p[0], p[1]), []).append((int(p[2]), int(p[3]), p[4]))
    except OSError:
        pass

    problems = []      # reasons the run is not a pass
    step_lines, gate_sections, failing = [], [], []
    topo = set()
    suites_seen = []
    for (suite, step), lst in sorted(attempts.items(), key=lambda kv: (selected.index(kv[0][0]) if kv[0][0] in selected else 99, kv[0][1])):
        if suite not in suites_seen:
            suites_seen.append(suite)
        n, rc, res = lst[-1]
        sd = os.path.join(out, suite, f"{step}.a{n}")
        meta = read_kv(os.path.join(sd, "meta.txt"))
        lr = load_range(sd)
        load_s = f"{lr[0]:.1f}-{lr[1]:.1f}" if lr else "n/a"
        mem_s = fmt(lr[2]) if lr else "-"
        kind = meta.get("kind", "e2e")
        rows, rmeta = ([], {})
        if kind == "x1":
            rows = x1_rows(sd)
        else:
            rows, rmeta = e2e_rows(sd)
        if rmeta.get("topology_arms"):
            topo.add(rmeta["topology_arms"])
        gated = [r for r in rows if r.get("op")]
        fails = [r for r in gated if r["verdict"] == "FAIL"]
        skipped = [r for r in rows if r["id"].endswith(".skipped") and r.get("value")]
        status = res
        why = []
        if res != "ok":
            why.append(f"exit {rc}")
        if fails:
            status = "FAIL"
            why.append(f"{len(fails)} gate rows FAIL")
        if skipped:
            status = "FAIL"
            why.append(f"{len(skipped)} scenarios SKIPPED")
        if res == "ok" and not gated:
            status = "FAIL"
            why.append("no gated rows found (result json missing?)")
        if len(lst) > 1:
            why.append(f"{len(lst)} attempts, earlier ones kept")
        if status != "ok":
            problems.append(f"{suite}/{step}: " + ", ".join(why))
        steps_md = os.path.join(sd, "res")
        step_lines.append(
            f"| {suite}/{step} | {'PASS' if status == 'ok' else status} | a{n} | {meta.get('seconds', '?')} s "
            f"| {load_s} | {mem_s} | {len(gated)} | {len(fails)} | {'; '.join(why) or '-'} |")
        lines = [f"### {suite}/{step} (attempt {n}, load {load_s}, mem PSI some avg10 max {mem_s})", ""]
        if gated:
            lines += ["| gate | threshold | median | max | verdict | load |", "|---|---|---|---|---|---|"]
            for r in sorted(gated, key=lambda r: (r["verdict"] != "FAIL", r["id"])):
                bus = "" if r.get("bus") in (None, "", "-") else f" ({r['bus']})"
                lines.append(
                    f"| {r['id']}{bus} | {r['op']} {fmt(r['threshold'])} {r.get('unit', '')} | "
                    f"{fmt(r['value'])} | {fmt(r['max'])} | {r['verdict']} | {load_s} |")
                if r["verdict"] == "FAIL":
                    failing.append(f"{suite}/{step}: {r['id']} median {fmt(r['value'])} max {fmt(r['max'])} {r.get('unit', '')} (gate {r['op']} {fmt(r['threshold'])})")
        else:
            lines.append("_no gated rows_")
        for r in skipped:
            lines.append(f"\nSKIPPED: {r['id']}")
        lines.append("")
        gate_sections.append("\n".join(lines))

    for s in selected:
        if s not in suites_seen:
            problems.append(f"suite {s}: no step recorded (not run)")

    partial = len(selected) < len(a.all_suites.split()) if a.all_suites else False
    if a.mode.startswith("DRY RUN"):
        verdict = "DRY RUN, NOT ACCEPTANCE DATA" + (" (gates " + ("pass" if not problems else "FAIL") + ")")
    elif problems:
        verdict = "FAIL"
    elif "DEGRADED" in a.mode:
        verdict = "PASS ON A DEGRADED HOST, NOT ACCEPTANCE"
    elif partial:
        verdict = "PASS (PARTIAL: not every suite ran)"
    else:
        verdict = "PASS"

    host = open(os.path.join(out, "host.txt")).read() if os.path.exists(os.path.join(out, "host.txt")) else ""
    md = [f"# Phase 2 acceptance: {verdict}", "",
          f"Mode: **{a.mode}**. Run dir: `{out}`.",
          f"Resolved topology (from the reader reports): {', '.join(sorted(topo)) or 'not reported (wrappers without --expect-topology only)'}.",
          "", "## Steps", "",
          "| step | result | attempt | wall | load1 min-max | mem PSI max | gated rows | FAIL rows | notes |",
          "|---|---|---|---|---|---|---|---|---|"] + step_lines + [""]
    if problems:
        md += ["## Failures", ""] + [f"- {p}" for p in problems] + [""]
    if failing:
        md += ["## Failing gates", ""] + [f"- {f}" for f in failing] + [""]
    md += ["## Gates", ""] + gate_sections
    md += ["## Host", "", "```", host.rstrip(), "```", ""]
    open(os.path.join(out, "summary.md"), "w").write("\n".join(md))
    print("\n".join(md[:3 + len(step_lines) + 6]))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
