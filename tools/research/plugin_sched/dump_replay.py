"""Dump the recorded scheduler call log of one SPS simulation run, with the reference outputs, in a
plain text format that crates/gorca-plugin-sched/tests/replay.rs replays (SPS-6, cross-language).

Usage: python dump_replay.py [--n 3000] [--seed 7] OUT
(the committed corpus is `--n 3000 --seed 7`: 17 594 calls, output hash 9f3a899cdbc785ac)

Line formats (fields separated by single spaces; lists by commas, empty list = `-`):
  H <pid> <label> <tier>
  K <ctx> <now_ms> | <k>
  R <est> <actual>
  D <k_hot> <triggered> | <t_star> | <wait pids> | <pid:hot:offset:deadline:grant:hold>...   (hot = -1 parked)
  C <ctx> <triggered>
  O <pid> <state> <start> <exec> <resp> <waited> <useful> <deadline> <hold> <exec_allow> <clipped>
"""
from __future__ import annotations

import argparse

import run_all
import sim
import sps as S


def lst(xs):
    xs = list(xs)
    return ",".join(str(x) for x in xs) if xs else "-"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--n", type=int, default=3000)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    run_all.Recorder.log = []
    orig = sim.Scheduler
    sim.Scheduler = run_all.Recorder
    try:
        sim.run("SPS", "oracle", a.n, a.seed)
    finally:
        sim.Scheduler = orig
    ctx_ix = {c: i for i, c in enumerate(sim.CTX_NAMES)}
    with open(a.out, "w") as f:
        for p in sim.PLUGINS:
            f.write(f"H {p[0]} {p[2]} {p[3]}\n")
        for name, args, out in run_all.Recorder.log:
            if name == "keydown":
                ctx, now_ms = args
                f.write(f"K {ctx_ix[ctx]} {now_ms} | {out}\n")
            elif name == "spin_refund":
                f.write(f"R {args[0]} {args[1]}\n")
            elif name == "dispatch":
                trig, k = args
                _k, assign, wait, t_star = out
                asg = " ".join(
                    f"{pid}:{-1 if hot is None else hot}:{off}:{d}:{g}:{h}" for pid, hot, off, d, g, h in assign
                )
                f.write(f"D {k} {lst(trig)} | {t_star} | {lst(wait)} | {asg}\n")
            elif name == "observe_context":
                ctx, trig = args
                f.write(f"C {ctx_ix[ctx]} {lst(sorted(trig))}\n")
            elif name == "observe_reply":
                pid, state, start, ex, resp, asg, waited, useful = args
                f.write(
                    f"O {pid} {state} {start} {ex} {resp} {int(waited)} {int(useful)} "
                    f"{asg.deadline_us} {asg.hold_us} {asg.exec_allow_us} {int(asg.clipped)}\n"
                )
    print("calls", len(run_all.Recorder.log))


if __name__ == "__main__":
    main()
