#!/usr/bin/python3 -I
"""Read-only: find FOCUSED nodes, document attrs, node count (no names/text printed)."""
import sys, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi
pat=sys.argv[1].lower()
desk=Atspi.get_desktop(0)
app=None
for i in range(desk.get_child_count()):
    a=desk.get_child_at_index(i)
    if a and pat in (a.get_name() or '').lower(): app=a
S=Atspi.StateType
def path(n):
    out=[]
    while n:
        out.append(n.get_role_name()); n=n.get_parent()
    return " < ".join(out)
total=[0]; focused=[]; docs=[]; roles={}; apps_role=[]
def walk(n,d):
    total[0]+=1
    r=n.get_role_name(); roles[r]=roles.get(r,0)+1
    s=n.get_state_set()
    if s.contains(S.FOCUSED): focused.append(n)
    if r=='document web': docs.append(n)
    try:
        attrs=n.get_attributes() or {}
        if attrs.get('xml-roles') in ('application','document','main','dialog','alertdialog','textbox','log','feed','grid','tree','treegrid'): apps_role.append((attrs.get('xml-roles'),r,d))
    except Exception: pass
    for i in range(n.get_child_count()):
        c=n.get_child_at_index(i)
        if c: walk(c,d+1)
walk(app,0)
print("total nodes",total[0]); print("roles",dict(sorted(roles.items(),key=lambda x:-x[1])[:25]))
print("aria roles of interest",apps_role[:40])
for f in focused: print("FOCUSED:",path(f), "ifaces",f.get_interfaces())
for d in docs:
    print("DOC attrs",dict(d.get_attributes() or {}))
    try: print("doc attr Document:",Atspi.Document.get_document_attributes(d) and dict(Atspi.Document.get_document_attributes(d)))
    except Exception as e: print("docattr err",e)
    print("doc state", [x for x in ('FOCUSABLE','FOCUSED','SHOWING','ACTIVE','EDITABLE','MODAL','BUSY') if d.get_state_set().contains(getattr(S,x))])
    print("doc rel", [r.get_relation_type() for r in d.get_relation_set()])
