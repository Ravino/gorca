#!/usr/bin/env python3
"""Split the B-1 walk correction rate (B1-M6) by direction of travel.

    tools/b1-m6-direction.py <session dir> [<arm> ...]

Reads `runs/<arm>/walk/<n>/present.jsonl` of a `bench/b1/run-all.sh` session. A presentation is
a first utterance (`refines` is null); it counts as corrected when a later utterance refines it.
A step from focus A to focus B is *backward* when the walk went B -> A earlier (Shift+Tab or Up
over controls already walked), otherwise *forward*. This approximates the harness's M6 (it
counts `Refined` but not `Late` verdicts and every presentation, not only focus ones); the
overall column is printed beside it so the gap is visible.
"""

import json
import pathlib
import statistics
import sys


def split(present: pathlib.Path) -> dict[str, tuple[int, int]]:
    firsts = []
    refined = set()
    with present.open() as fh:
        next(fh, None)  # header line (redaction key)
        for line in fh:
            d = json.loads(line)
            if d.get("refines") is not None:
                refined.add(d["refines"])
            else:
                firsts.append((d["utt_id"], d["path"]))
    seen = set()
    out = {"fwd": [0, 0], "back": [0, 0], "all": [0, 0]}
    prev = None
    for utt, path in firsts:
        side = "back" if prev is not None and (path, prev) in seen else "fwd"
        if prev is not None and prev != path:
            seen.add((prev, path))
        hit = int(utt in refined)
        for k in (side, "all"):
            out[k][0] += hit
            out[k][1] += 1
        prev = path
    return {k: (v[0], v[1]) for k, v in out.items()}


def main() -> None:
    root = pathlib.Path(sys.argv[1])
    arms = sys.argv[2:] or sorted(p.name for p in (root / "runs").iterdir())
    print("arm  rep  M6.fwd  (n)      M6.back (n)      M6.all~")
    for arm in arms:
        cols = {"fwd": [], "back": [], "all": []}
        for rep in sorted((root / "runs" / arm / "walk").iterdir()):
            s = split(rep / "present.jsonl")
            r = {k: (c / n if n else float("nan")) for k, (c, n) in s.items()}
            for k in cols:
                cols[k].append(r[k])
            print(
                f"{arm:4} {rep.name:>3}  {r['fwd']:.3f} ({s['fwd'][1]:5})  "
                f"{r['back']:.3f} ({s['back'][1]:5})  {r['all']:.3f}"
            )
        med = {k: statistics.median(v) for k, v in cols.items() if v}
        print(
            f"{arm:4} med  {med['fwd']:.3f}          {med['back']:.3f}          {med['all']:.3f}"
        )


if __name__ == "__main__":
    main()
