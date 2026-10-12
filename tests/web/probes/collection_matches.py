import sys, time, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi
Atspi.init()
which=sys.argv[1]; idx=int(sys.argv[2])
desk=Atspi.get_desktop(0)
app=[desk.get_child_at_index(i) for i in range(desk.get_child_count()) if desk.get_child_at_index(i).get_name()==which][idx]
def find(o,d=0):
    if d>7: return None
    for i in range(o.get_child_count()):
        ch=o.get_child_at_index(i)
        if ch is None: continue
        if ch.get_role_name()=='document web' and ch.get_state_set().contains(Atspi.StateType.SHOWING): return ch
        r=find(ch,d+1)
        if r: return r
doc=find(app)
print("supports collection:", "Collection" in doc.get_interfaces(), doc.get_child_count())
for name in ("SECTION","LINK","IMAGE","PUSH_BUTTON","HEADING","LANDMARK","PARAGRAPH"):
    rule=Atspi.MatchRule.new(Atspi.StateSet(),Atspi.CollectionMatchType.ALL,{},Atspi.CollectionMatchType.ALL,[getattr(Atspi.Role,name)],Atspi.CollectionMatchType.ALL,[],Atspi.CollectionMatchType.ALL,False)
    t=time.perf_counter(); m=Atspi.Collection.get_matches(doc,rule,Atspi.CollectionSortOrder.CANONICAL,0,True); dt=(time.perf_counter()-t)*1000
    print(name,len(m),f"{dt:.2f} ms")
