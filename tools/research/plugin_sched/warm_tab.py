"""Markdown tables from warm_sim.py --out JSON. Usage: python warm_tab.py results.json"""
import json
import sys

POL = ["LABELS", "SPS", "SPS+REPLAY", "SPS+HOOK", "HOOK-ONLY", "LABELS+HOOK"]


def main(path):
    rs = json.load(open(path))
    main_rows = [r for r in rs if "tag" not in r]
    for wl in ("mix", "bench10"):
        for snap in (60, 20):
            for lam in (1.0, 0.3):
                for kap in (0.75, 0.34):
                    print(f"\n#### {wl}, pre-send/reserve {snap}/{52 if snap == 60 else 24}, "
                          f"lam_E {lam}, kappa {kap}\n")
                    print("| lease | place | " + " | ".join(POL) + " |")
                    print("|---|---|" + "---|" * len(POL))
                    for lease in (0, 1):
                        for place in ("any", "P"):
                            cells = []
                            for p in POL:
                                m = [r for r in main_rows if r["workload"] == wl and r["snap"] == snap
                                     and r["lam_e"] == lam and r["kappa"] == kap and r["lease"] == lease
                                     and r["place"] == place and r["policy"] == p]
                                r = m[0]
                                cells.append(f"{r['ffu']:.3f} / {r['stall_p50']:.0f} / {r['stall_p99']:.0f} / "
                                             f"{r['cpu_total'] / 1000:.1f}")
                            print(f"| {'on' if lease else 'off'} | {place} | " + " | ".join(cells) + " |")
    print("\n#### lease effect phi (mix, 60/52, P, kappa .75, lam_E .3)\n")
    print("| phi | " + " | ".join(POL) + " |")
    print("|---|" + "---|" * len(POL))
    for phi in (0.0, 0.5, 0.9):
        cells = []
        for p in POL:
            r = [x for x in rs if x.get("tag") == "phi" and x["phi"] == phi and x["policy"] == p][0]
            cells.append(f"{r['ffu']:.3f} / {r['stall_p50']:.0f} / {r['stall_p99']:.0f} / {r['cpu_total'] / 1000:.1f}")
        print(f"| {phi} | " + " | ".join(cells) + " |")
    print("\n#### sensitivity to fresh data D_f and hook work gamma (mix, 60/52, lease, P, kappa .75, lam_E .3)\n")
    print("| D_f | gamma | SPS+HOOK | HOOK-ONLY |")
    print("|---|---|---|---|")
    for d_f in (5, 15, 40):
        for g in (0.25, 0.5, 1.0):
            cells = []
            for p in ("SPS+HOOK", "HOOK-ONLY"):
                r = [x for x in rs if x.get("tag") == "sens" and x["d_f"] == d_f and x["gamma"] == g
                     and x["policy"] == p][0]
                cells.append(f"{r['ffu']:.3f} / {r['stall_p50']:.0f} / {r['cpu_total'] / 1000:.2f} "
                             f"(hook {r['cpu_hook'] / 1000:.2f}, {r['hooks_per_key']:.1f}/key)")
            print(f"| {d_f} | {g} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main(sys.argv[1])
