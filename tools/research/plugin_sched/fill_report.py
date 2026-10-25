"""Render results.json tables into docs/reports/plugin-statistical-scheduler.md placeholders.

Usage: python fill_report.py RESULTS_JSON REPORT_MD
"""
import json
import sys


def row(r, lead):
    return (f"| {lead} | {r['ffu_benign']:.4f} | {r['ffu_min']:.4f} | {r['stall_p50']:.0f} | "
            f"{r['stall_p99']:.0f} | {r['cpu_core_pct_5kps']:.1f} | {r['adv_stalls_waited']} / {r['adv_activations']} |")


def table(rs, key):
    head = (f"| {key} | FFU | worst plugin | stall p50 | p99 | CPU % core | adversary stalls waited / activations |\n"
            "|---|---|---|---|---|---|---|")
    return head + "\n" + "\n".join(row(r, r[key] if key in r else r["policy"]) for r in rs)


def main():
    res = json.load(open(sys.argv[1]))
    md = open(sys.argv[2]).read()
    main_rows = "\n".join(
        f"| {r['strategy']} | {r['policy']} | {r['ffu_benign']:.4f} | {r['ffu_min']:.4f} | {r['stall_p50']:.0f} | "
        f"{r['stall_p99']:.0f} | {r['cpu_core_pct_5kps']:.1f} | {r['adv_stalls_waited']} |" for r in res["main"])
    md = md.replace("RESULTS_MAIN", main_rows)
    md = md.replace("RESULTS_SEED2", table(res["seed2"], "policy"))
    md = md.replace("RESULTS_KMULT", table(res["kmult"], "k_mult"))
    md = md.replace("RESULTS_LAM", table(res["lam"], "lam_q16"))
    md = md.replace("RESULTS_BUDGET", table(res["budget"], "spin_rate"))
    md = md.replace("RESULTS_PMQOS", table(res["pmqos"], "policy"))
    sps = [r for r in res["main"] if r["policy"] == "SPS"]
    worst = max(max(v for k, v in r["max_excl_run"].items() if k != "adversary") for r in sps)
    md = md.replace("MAXRUN", f"{worst} activations (max over all SPS runs; adversary excluded)")
    rp = res["replay"]
    md = md.replace("REPLAY", f"{rp['calls']} recorded scheduler calls (oracle adversary, seed 7, 3 000 keypresses) "
                    f"replayed into a fresh scheduler: **{rp['mismatches']} mismatches**, output hash "
                    f"`{rp['sha256']}`.")
    open(sys.argv[2], "w").write(md)


if __name__ == "__main__":
    main()
