#!/usr/bin/python3 -I
"""Read-only AT-SPI tree dump for an application whose name matches argv[1].
No writes: no actions, selections, focus changes. Prints roles/ifaces/states only (no names/text content)."""
import sys, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi
pat = sys.argv[1].lower(); maxd = int(sys.argv[2]) if len(sys.argv)>2 else 8; maxn=int(sys.argv[3]) if len(sys.argv)>3 else 400
show_names = len(sys.argv)>4 and sys.argv[4]=='names'
desk = Atspi.get_desktop(0)
apps=[]
for i in range(desk.get_child_count()):
    a=desk.get_child_at_index(i)
    if a and pat in (a.get_name() or '').lower(): apps.append(a)
print("apps:",[ (a.get_name(), a.get_toolkit_name(), a.get_toolkit_version() if hasattr(a,'get_toolkit_version') else '') for a in apps])
cnt=[0]
def ifaces(n):
    try: return ",".join(x[:4] for x in n.get_interfaces())
    except Exception: return "?"
def st(n):
    s=n.get_state_set(); out=[]
    for name in ('EDITABLE','FOCUSABLE','FOCUSED','SHOWING','MULTI_LINE','SELECTABLE','EXPANDABLE','SENSITIVE','ACTIVE','SINGLE_LINE'):
        if s.contains(getattr(Atspi.StateType,name)): out.append(name[:4])
    return ",".join(out)
def walk(n,d):
    if cnt[0]>=maxn or d>maxd: return
    cnt[0]+=1
    try:
        attrs=n.get_attributes() or {}
    except Exception: attrs={}
    keep={k:v for k,v in attrs.items() if k in ('xml-roles','tag','id','class','live','explicit-name') and k!='class'}
    nm = (n.get_name() or '')[:30] if show_names else ('N' if n.get_name() else '')
    print("  "*d+f"{n.get_role_name()}|{ifaces(n)}|{st(n)}|{keep}|ch={n.get_child_count()}|{nm}")
    for i in range(min(n.get_child_count(),40)):
        c=n.get_child_at_index(i)
        if c: walk(c,d+1)
for a in apps: walk(a,0)
