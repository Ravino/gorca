#!/usr/bin/python3
"""keyshapes.py DIR - per scenario: the nodes that became showing (with 9-level ancestor chain),
and the first-event latency of the interesting event kinds relative to the trigger call."""
import gzip
import json
import os
import sys

d = sys.argv[1]
only = sys.argv[2:]
for f in sorted(os.listdir(d)):
    if not f.endswith((".json", ".json.gz")) or f.startswith("_") or f.startswith(("identity.", "registration.")):
        continue
    r = json.load((gzip.open if f.endswith('.gz') else open)(os.path.join(d, f), 'rt'))
    if only and r["scenario"] not in only:
        continue
    print("##", r["scenario"], "load", r["load"], "events", r["n_events"])
    first = {}
    for e in r["events"]:
        s = e.get("src") or {}
        role = (s.get("role") or "").replace(" ", "-") or None
        if role == "<gone>":
            role = (s.get("last_known") or "gone:").split(":")[0].replace(" ", "-")
        k = None
        det = e.get("detail", "")
        if e["iface"] == "Window":
            k = f"Window.{e['member']} any={e.get('any')}"
        elif e["member"] == "Announcement":
            k = f"Announcement d1={e.get('d1')} any={e.get('any')}"
        elif e["member"] == "StateChanged" and det in ("showing", "focused", "active", "selected", "checked", "expanded") and role in (
                "notification", "dialog", "window", "level-bar", "alert", "menu", "popup-menu", "password-text", "text",
                "push-button", "toggle-button", "menu-item", "slider", "list-item"):
            k = f"StateChanged[{det}]={e['d1']} {role}"
        elif e["member"] == "ChildrenChanged" and (e.get("any_src") or {}).get("role") in ("notification", "dialog", "window", "alert"):
            k = f"ChildrenChanged[{det}] child={(e.get('any_src') or {}).get('role')} parent={s.get('path')}"
        elif e["member"] == "PropertyChange" and det in ("accessible-value",) :
            k = f"PropertyChange[{det}] {role}"
        elif e["member"] == "TextChanged":
            k = f"TextChanged[{det}] {role}"
        if k and k not in first:
            first[k] = (e["t_rel_ms"], e["seq"], e["sender"] + "/" + s.get("path", "?"))
    for k, (t, seq, who) in sorted(first.items(), key=lambda kv: kv[1][0]):
        print(f"   {t:>9.1f}ms #{seq:<6d} {who:14s} {k}")
    for pk, dff in [("END", r["diff"])] + list((r.get("peaks") or {}).items()):
      print(f"   -- peak '{pk}' (state {dff.get('state')}): nodes newly showing vs scenario start (name/text a reader can read):")
      seen = set()
      for v in dff["added_showing"] + [x for x in dff["showing_changed"] if x.get("now_showing")]:
        key = (v.get("role"), v.get("name"), v.get("text"))
        if key in seen:
            continue
        seen.add(key)
        if v.get("role") in ("panel", "filler") and not (v.get("name") or v.get("text")) and v.get("role") != "notification":
            continue
        if v.get("role") == "text" and not v.get("text"):
            continue
        print(f"      {v.get('role'):14s} name={v.get('name')!r} text={v.get('text')!r} chain={v.get('chain')}")
        if len(seen) > 30:
            break
