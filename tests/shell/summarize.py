#!/usr/bin/python3
"""summarize.py DIR - one line per scenario: which of window/focus/announcement/... fired."""
import collections
import glob
import gzip
import json
import os
import sys

d = sys.argv[1]
tot = collections.Counter()
for f in sorted(glob.glob(os.path.join(d, "*.json*"))):
    b = os.path.basename(f)
    if b.startswith("_") or b.startswith(("identity.", "registration.")):
        continue
    r = json.load((gzip.open if f.endswith('.gz') else open)(f, 'rt'))
    c = collections.Counter((e["iface"], e["member"], e.get("detail", "")) for e in r["events"])
    tot.update(c)
    key = {}
    for (i, m, dt), n in c.items():
        if i in ("Window", "Focus", "Document") or m in ("Announcement", "TextChanged", "ActiveDescendantChanged",
                                                           "SelectionChanged", "TextCaretMoved") \
                or (m == "StateChanged" and dt in ("focused", "active")):
            key[f"{i}.{m}[{dt}]"] = n
    print(f"{r['scenario']:26s} events={r['n_events']:5d} load={r['load']:>6s} {key}")
print("TOTAL BY KIND:")
for k, n in sorted(tot.items(), key=lambda kv: -kv[1]):
    print(f"  {n:6d} {k}")
