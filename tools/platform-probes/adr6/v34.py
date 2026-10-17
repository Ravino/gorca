"""V-3 GetOffsetAtPoint + V-4 bounds-changed. usage: v34.py gtk3|gtk4. PRIVATE session only."""
import sys; sys.path.insert(0,__import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from rig import *
import json
which=sys.argv[1]
K=Keys(own=False); pump(1); K.tap(125); pump(1)
p=subprocess.Popen(["/usr/bin/python3",__import__("os").path.dirname(__import__("os").path.abspath(__file__))+"/"+("app3.py" if which=="gtk3" else "app4.py")],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,bufsize=1,stderr=subprocess.DEVNULL)
def say(c): p.stdin.write(c+"\n"); p.stdin.flush()
def truth():
    say("truth")
    while True:
        l=p.stdout.readline()
        if l.startswith("TRUTH"): return json.loads(l[6:])
p.stdout.readline(); b=app_by_pid(p.pid); root="/org/a11y/atspi/accessible/root"
ev=[]; listen(["object:bounds-changed","object:visible-data-changed","object:property-change:accessible-parent","window:resize"]); collect(ev,b)
nodes=dump(b,root,0,12)
def byrole(role,pred=lambda i:True): return [(pth,i,ifs) for d,pth,i,ifs in nodes if i["role"]==role and pred(i)]
labels=[n for n in byrole("label") if "Wrapped" in n[1]["name"]]
tvs=[n for n in nodes if "Text" in "".join(n[3]) and n[2]["role"] in ("text","text box") and n[2]["name"]=="" ]
tvs=[(pth,i,ifs) for d,pth,i,ifs in nodes if any(x.endswith("Text") for x in ifs) and i["role"] in ("text","text box") ]
print("text-ish nodes:",[(i["role"],pth.rsplit('/',1)[-1][:8],int(gp(b,pth,"org.a11y.atspi.Text","CharacterCount"))) for pth,i,ifs in tvs])
def offat(pth,x,y,c=1):
    t=time.perf_counter()
    try: r=ac(b,pth,TXT,"GetOffsetAtPoint",GLib.Variant("(iiu)",(x,y,c)),"(i)").unpack()[0]
    except Exception as e: r="ERR "+str(e).split(":")[-1][:50]
    return r,(time.perf_counter()-t)*1e3
print("==== V-3")
tv=None
for pth,i,ifs in tvs:
    n=int(gp(b,pth,"org.a11y.atspi.Text","CharacterCount"))
    if n>1000: tv=pth
ent=[pth for pth,i,ifs in tvs if int(gp(b,pth,"org.a11y.atspi.Text","CharacterCount"))==10]
lab=labels[0][0] if labels else None
if tv:
    for sc in (0,23,100,311,600,1500):
        say(f"scroll {sc}"); pump(0.4); tr=truth()
        x,y,w,h=extents(b,tv,1)
        r=[offat(tv,x+1,y+1,1)]
        r.append(offat(tv,x+1,y+h-2,1))
        print(f"textview scroll={sc} truth_first_offset={tr['tv_first_visible_offset']} line={tr.get('tv_first_visible_line')} extents={(x,y,w,h)} GetOffsetAtPoint(top-left, bottom-left; WINDOW)={r}")
        # Is char extent API sane
        try:
            ce=ac(b,tv,TXT,"GetCharacterExtents",GLib.Variant("(iu)",(tr['tv_first_visible_offset'],1)),"(iiii)").unpack()[0]
        except Exception as e: ce="ERR "+str(e).split(":")[-1][:40]
        print("   charextents(first visible)=",ce)
if lab:
    x,y,w,h=extents(b,lab,1); cc=int(gp(b,lab,TXT,"CharacterCount"))
    print("label extents",(x,y,w,h),"chars",cc,"lines:",[offat(lab,x+1,y+yy,1) for yy in (2,h//2,h-2)], "SCREEN->",offat(lab,x+1,y+2,0))
for e in ent[:1]:
    x,y,w,h=extents(b,e,1); print("entry OffsetAtPoint:",offat(e,x+3,y+h//2,1)); 
    try: print("entry RangeExtents:",ac(b,e,TXT,"GetRangeExtents",GLib.Variant("(iiu)",(0,5,1)),"(iiii)").unpack()[0])
    except Exception as ex: print("entry RangeExtents ERR",str(ex).split(":")[-1][:50])
print("==== V-4")
def evs(label,fn,wait=0.8):
    n0=len(ev); fn(); pump(wait)
    new=ev[n0:]; bc=[e for e in new if e[4] in ("BoundsChanged",)]
    print(f"{label:34s} total_events={len(new)} BoundsChanged={len(bc)} others={sorted(set(e[4] for e in new if e[4]!='BoundsChanged'))}")
    for e in bc[:6]: print("      ",e[2].rsplit('/',1)[-1][:10],e[5])
    return bc
evs("idle",lambda:None,1.0)
evs("scroll textview by 300px",lambda:say("scroll 300"))
evs("scroll listbox 200px",lambda:say("lbscroll 200"))
evs("scroll listbox 600px",lambda:say("lbscroll 600"))
evs("grow button to 120px",lambda:say("grow 120"))
if which=="gtk4": evs("resize window 640x500",lambda:say("resize 640 500"),1.5)
else: evs("resize window 640x500",lambda:say("resize 640 500"),1.5)
say("quit"); pump(0.5)
