import os, sys, time, subprocess, json
L="/tmp/-www-a11y/3849ba02-d3b2-4b42-8c27-d952cfa36771/scratchpad/lk"
os.environ["DBUS_SESSION_BUS_ADDRESS"]=sys.argv[1]
os.environ["XDG_RUNTIME_DIR"]=L+"/sh/run"
os.environ["WAYLAND_DISPLAY"]="gorca-test-0"; os.environ["GDK_BACKEND"]="wayland"
GORCA=sys.argv[2]; seq=json.loads(sys.argv[3]); tag=sys.argv[4]
sys.path.insert(0,"<repo>/tools/platform-probes/adr6")
import rig
from rig import pump
k=rig.Keys(own=False); g=rig.GtkKeys(); pump(2)
K={"caps":58,"ins":110,"kpins":82,"f2":60,"tab":15,"a":30,"x":45,"down":108,"f1":59,"esc":1,"h":35,"space":57,"shift":42,"ctrl":29}
def probe():
    n=len(g.lines); k.tap(15); pump(.12); k.tap(30); pump(.3); return g.keys(n)
for i in range(5):
    r=probe()
    if 65289 in r and (97 in r or 65 in r): break
    k.tap(125); pump(1.5)
print("pre",r,flush=True)
env=dict(os.environ,XDG_CONFIG_HOME=L+"/sh/config",XDG_DATA_HOME=L+"/sh/data",HOME=L+"/sh/home",GORCA_REPEAT_DELAY_MS="500",GORCA_XKB_OPTIONS="",GORCA_CLOCK_24H="1",RUST_LOG=os.environ.get("RL","info"))
gor=subprocess.Popen([GORCA,"--speech=ssip","--ssip-socket=/nonexistent","--impersonate-orca","--no-probe"],env=env,stderr=open(L+"/rp-%s.err"%tag,"w"),stdout=subprocess.DEVNULL)
pump(2.5); print("gorca",probe(),flush=True)
n0=len(g.lines)
for it in seq:
    if isinstance(it,(int,float)): pump(it); continue
    n,d=it[:-1],it[-1]; k.key(K[n],d=="v"); 
g_after=g.keys(n0)
pump(1.5)
print("seq events",[v for v in g.keys(n0)][:60])
p1=probe(); print("probe1",p1,flush=True)
gor.terminate(); gor.wait(); pump(1.0)
print("no reader after stuck",probe(),flush=True)
gor2=subprocess.Popen([GORCA,"--speech=ssip","--ssip-socket=/nonexistent","--impersonate-orca","--no-probe"],env=env,stderr=open(L+"/rp2-%s.err"%tag,"w"),stdout=subprocess.DEVNULL)
pump(2.5); print("NEW gorca instance",probe(),flush=True)
k.tap(110,0.06); pump(.5); print("after ins tap",probe(),flush=True)
gor2.terminate(); pump(1); k.stop(); g.p.terminate()
