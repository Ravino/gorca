import sys, gi
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
tot=0;ok=0;mism=0;withtext=0;samples=[]
def walk(o):
    global tot,ok,mism,withtext
    tot+=1
    if "Text" in o.get_interfaces():
        n=Atspi.Text.get_character_count(o)
        if n:
            withtext+=1
            s=Atspi.Text.get_text(o,0,n)
            k=s.count('￼')
            cc=o.get_child_count()
            # embedded objects should equal child count for Firefox (text leaves omitted); for Chrome includes static children
            if k==cc: ok+=1
            else:
                mism+=1
                if len(samples)<3: samples.append((o.get_role_name(),n,k,cc))
    for i in range(o.get_child_count()):
        c=o.get_child_at_index(i)
        if c: walk(c)
walk(doc)
print(which,"nodes",tot,"with text",withtext,"FFFC==childcount",ok,"mismatch",mism,samples)
