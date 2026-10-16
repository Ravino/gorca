#!/usr/bin/env python3
"""frames_summary.py - read a probe_at.py JSON on stdin; print node count and the children of every internal frame."""
import json
import sys

t = sys.stdin.read()
d = json.loads(t[t.index("{"):])
print(d.get("mode"), "nodes", d.get("nodes"),
      [(x["name"], x["kids"]) for x in d.get("dump", []) if x["role"] == "internal frame"])
