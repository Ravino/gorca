#!/usr/bin/python3
"""facts.py DIR - cross-scenario facts: descriptions, relations, attributes, value ranges, enrich lag."""
import collections
import glob
import gzip
import json
import os
import sys

d = sys.argv[1]
rel = collections.Counter()
desc = collections.Counter()
attrs = collections.Counter()
lag = []
cost = []
roles_with_value = collections.Counter()
ifaces = collections.defaultdict(collections.Counter)
for f in sorted(glob.glob(os.path.join(d, "*.json*"))):
    b = os.path.basename(f)
    if b.startswith("_") or b.startswith(("identity.", "registration.")):
        continue
    r = json.load((gzip.open if f.endswith('.gz') else open)(f, 'rt'))
    for e in r["events"]:
        s = e.get("src") or {}
        role = s.get("role")
        if "relations" in s:
            rel[(role, str(s["relations"]))] += 1
        if s.get("desc"):
            desc[(role, str(s["desc"])[:60])] += 1
        if s.get("attrs"):
            attrs[json.dumps(s["attrs"], sort_keys=True)] += 1
        if "value.CurrentValue" in s:
            roles_with_value[(role, s.get("value.MinimumValue"), s.get("value.MaximumValue"))] += 1
        if role:
            for i in s.get("ifaces", []):
                ifaces[role][i] += 1
        if "enrich_lag_ms" in e:
            lag.append(e["enrich_lag_ms"])
            cost.append(e.get("enrich_cost_ms", 0))
print("relations:", rel.most_common(12))
print("descriptions:", desc.most_common(12))
print("attributes:", attrs.most_common(8))
print("value iface:", roles_with_value.most_common(8))
lag.sort()
cost.sort()
if lag:
    print("enrich lag ms p50/p90/p99/max:", lag[len(lag) // 2], lag[int(len(lag) * .9)], lag[int(len(lag) * .99)], lag[-1])
    print("enrich cost per event ms p50/p90/max:", cost[len(cost) // 2], cost[int(len(cost) * .9)], cost[-1])
for role in ("notification", "dialog", "level bar", "label", "push button", "window", "menu", "text", "password text"):
    print("ifaces", role, dict(ifaces[role]))
