#!/usr/bin/env python3
"""Calls per focus presentation of the B-1 walk, per arm (median of repeats).

    tools/b1-calls-per-focus.py <session dir> [<arm> ...]

Total AT-SPI calls the daemon made (all apps, `counters.json` -> `calls`) divided by the first
utterances of the run (`present.jsonl`, `refines` null). The harness's own `calls_per_focus`
info counts only the accounting window and focus presentations, so this can differ slightly.
"""

import json
import pathlib
import statistics
import sys


def one(rep: pathlib.Path) -> float:
    c = json.loads((rep / "counters.json").read_text())
    calls = sum(v for app in c["calls"].values() for v in app.values())
    with (rep / "present.jsonl").open() as fh:
        next(fh, None)
        firsts = sum(1 for line in fh if json.loads(line).get("refines") is None)
    return calls / max(firsts, 1)


def main() -> None:
    root = pathlib.Path(sys.argv[1])
    arms = sys.argv[2:] or sorted(p.name for p in (root / "runs").iterdir())
    for arm in arms:
        vals = [one(r) for r in sorted((root / "runs" / arm / "walk").iterdir())]
        print(f"{arm:4} calls/focus median {statistics.median(vals):.2f}  "
              f"({', '.join(f'{v:.2f}' for v in vals)})")


if __name__ == "__main__":
    main()
