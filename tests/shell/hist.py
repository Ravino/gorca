#!/usr/bin/python3
"""hist.py FILE.json - events per 250 ms bucket and the first interesting roles in the burst."""
import collections
import gzip
import json
import sys

r = json.load((gzip.open if sys.argv[1].endswith('.gz') else open)(sys.argv[1], 'rt'))
h = collections.Counter(int(e["t_rel_ms"] // 250) for e in r["events"])
print(r["scenario"], "events", r["n_events"], "buckets(250ms):", sorted(h.items())[:40])
names = collections.Counter((e.get("src") or {}).get("name") for e in r["events"] if e["member"] == "PropertyChange" and e.get("detail") == "accessible-name")
print("names changed:", names.most_common(12))
print("marks:", [(m["t_rel_ms"], m["mark"]) for m in r["marks"]])
