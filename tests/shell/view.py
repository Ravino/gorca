#!/usr/bin/python3
"""view.py - condensed view of a probe scenario JSON: view.py FILE.json [all]

Prints only the 'interesting' events (showing/focus/active/selected/checked/expanded/modal state
changes, window:*, focus, announcement, name/value property changes, children-changed whose
child is a notification/dialog/menu/label with a name) with the source's role/name/ancestors.
"""
import gzip
import json
import sys

rec = json.load((gzip.open if sys.argv[1].endswith('.gz') else open)(sys.argv[1], 'rt'))
allv = len(sys.argv) > 2
KEYSTATES = {"showing", "focused", "active", "selected", "checked", "expanded", "modal", "armed", "pressed", "busy"}
print("#", rec["scenario"], "load", rec["load"], "events", rec["n_events"], "|", rec["note"])
for m in rec["marks"]:
    print(f"   MARK {m['t_rel_ms']:>9.1f}  {m['mark']}")
for e in rec["events"]:
    s = e.get("src") or {}
    keep = allv
    d = e.get("detail", "")
    if e["iface"] in ("Window", "Focus", "Document"):
        keep = True
    elif e["member"] == "Announcement":
        keep = True
    elif e["member"] == "StateChanged" and d in KEYSTATES and (s.get("name") or s.get("role") not in ("panel", "filler") or s.get("last_known")):
        keep = True
    elif e["member"] == "PropertyChange" and d in ("accessible-name", "accessible-value", "accessible-description") and (s.get("role") != "<gone>" or s.get("last_known")):
        keep = True
    elif e["member"] in ("TextChanged", "TextCaretMoved", "ActiveDescendantChanged", "SelectionChanged", "ValueChanged"):
        keep = True
    elif e["member"] == "ChildrenChanged" and (e.get("any_src") or {}).get("role") in ("notification", "dialog", "window", "alert", "menu", "popup-menu"):
        keep = True
    if not keep:
        continue
    chain = []
    if s.get("role") == "<gone>" and s.get("last_known"):
        chain.append("GONE(last " + s["last_known"] + ")")
    p = s
    while isinstance(p, dict):
        chain.append(f"{p.get('role')}:{p.get('name')!r}")
        p = p.get("parent")
    extra = ""
    if s.get("text"):
        extra += f" text={s['text']!r}"
    if "value.CurrentValue" in s:
        extra += f" value={s['value.CurrentValue']}[{s.get('value.MinimumValue')}..{s.get('value.MaximumValue')}]"
    if e.get("any_src"):
        a = e["any_src"]
        extra += f" child={a.get('role')}:{a.get('name')!r}"
    print(f"{e['t_rel_ms']:>9.1f} #{e['seq']} {e['iface']}.{e['member']}[{d}] d1={e.get('d1')} d2={e.get('d2')} "
          f"any={json.dumps(e.get('any'))[:70]} {e['sender']}/{e['src_path'].rsplit('/', 1)[-1]} "
          f"{' < '.join(chain[:3])} states={','.join(s.get('states', [])) if isinstance(s.get('states'), list) else ''}{extra}")
d = rec["diff"]
print("diff nodes", d["stats"])
for k in ("added_showing", "renamed", "showing_changed", "removed"):
    for v in d[k][:40]:
        t = v.get("text")
        print(f"   {k}: {v.get('role')} name={v.get('name', v.get('new'))!r}{(' text=' + repr(t)) if t else ''} chain={v.get('chain', '')}")
