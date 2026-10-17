import os, sys, time, subprocess, json
L="/tmp/scratch/lk"
os.environ["DBUS_SESSION_BUS_ADDRESS"]=sys.argv[1]
os.environ["XDG_RUNTIME_DIR"]=L+"/sh/run"
os.environ["WAYLAND_DISPLAY"]="gorca-test-0"; os.environ["GDK_BACKEND"]="wayland"
hold=float(sys.argv[2]); mods=[int(x,0) for x in sys.argv[3].split(",")]; kc=int(sys.argv[4])
sys.path.insert(0,__import__("os").path.dirname(__import__("os").path.abspath(__file__))+"/../adr6")
import rig
from rig import pump
k=rig.Keys(own=False); g=rig.GtkKeys(); pump(2)
def probe():
    n=len(g.lines); k.tap(15); pump(.12); k.tap(30); pump(.3); return g.keys(n)
for i in range(5):
    r=probe()
    if 65289 in r and (97 in r or 65 in r): break
    k.tap(125); pump(1.5)
print("pre",r)
o=rig.Keys(own=True)
o.grabs(mods,[])
pump(.3)
print("grabbed; baseline",probe())
t0=time.time(); o.ev.clear()
k.key(kc,True); pump(hold); k.key(kc,False); pump(1.0)
print("events:",[(round(t-t0,2),"rel" if r else "prs",hex(ks)) for (t,r,st,ks,kc_) in o.ev][:12], "n=",len(o.ev))
o.ev.clear()
print("after hold %.2f of keycode %d"%(hold,kc),probe()); print("events during probe",[(("rel" if r else "prs"),hex(ks)) for (t,r,st,ks,kc_) in o.ev])
k.tap(kc,0.05); pump(.5)
print("after tap",probe())
k.stop(); g.p.terminate()
