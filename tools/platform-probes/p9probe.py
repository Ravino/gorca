#!/usr/bin/env python3
"""P-9 / P2P probe. Runs against a PRIVATE session (DBUS_SESSION_BUS_ADDRESS must point at it).
Spawns a scripted test toolkit app, subscribes to the a11y bus, and after each scripted mutation
compares (GetItems snapshot + Cache.Add/RemoveAccessible signals) with a ground-truth tree walk.
usage: p9probe.py APP.py [--nolisten] [--steps a,b,c]
Registers as an event listener on the PRIVATE a11y bus only (never use against the real session)."""
import sys, os, time, subprocess, json
import gi
gi.require_version("Gio","2.0")
from gi.repository import Gio, GLib
STATES="INVALID ACTIVE ARMED BUSY CHECKED COLLAPSED DEFUNCT EDITABLE ENABLED EXPANDABLE EXPANDED FOCUSABLE FOCUSED HAS_TOOLTIP HORIZONTAL ICONIFIED MODAL MULTI_LINE MULTISELECTABLE OPAQUE PRESSED RESIZABLE SELECTABLE SELECTED SENSITIVE SHOWING SINGLE_LINE STALE TRANSIENT VERTICAL VISIBLE MANAGES_DESCENDANTS INDETERMINATE REQUIRED TRUNCATED ANIMATED INVALID_ENTRY SUPPORTS_AUTOCOMPLETION SELECTABLE_TEXT IS_DEFAULT VISITED CHECKABLE HAS_POPUP READ_ONLY".split()
def sdesc(bits):
    return [STATES[i] if i<len(STATES) else str(i) for i in range(64) if bits>>i&1]
ITEMS="(a((so)(so)(so)iiassusau))"
sess=Gio.bus_get_sync(Gio.BusType.SESSION)
addr=sess.call_sync("org.a11y.Bus","/org/a11y/bus","org.a11y.Bus","GetAddress",None,None,0,3000,None).unpack()[0]
assert "/tmp/" in addr or "scratchpad" in addr, "refusing: not the private bus "+addr
c=Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(dest,path,iface,m,args=None,sig=None,timeout=3000,conn=None):
    return (conn or c).call_sync(dest,path,iface,m,args,GLib.VariantType(sig) if sig else None,Gio.DBusCallFlags.NONE,timeout,None)
def prop(dest,path,iface,name,conn=None):
    return call(dest,path,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",(iface,name)),"(v)",3000,conn).unpack()[0]
ACC="org.a11y.atspi.Accessible"
events=[]; t0=time.time()
def on_sig(conn,sender,path,iface,member,params,*_):
    events.append((time.time()-t0,sender,path,iface,member,params))
c.signal_subscribe(None,None,None,None,None,Gio.DBusSignalFlags.NONE,on_sig)
app=sys.argv[1]; nolisten="--nolisten" in sys.argv
steps=None
if "--steps" in sys.argv: steps=sys.argv[sys.argv.index("--steps")+1].split(",")
if not nolisten:
    for ev in ("object:children-changed","object:state-changed","object:property-change","object:bounds-changed","object:text-changed","object:selection-changed","window:","object:text-caret-moved"):
        call("org.a11y.atspi.Registry","/org/a11y/atspi/registry","org.a11y.atspi.Registry","RegisterEvent",GLib.Variant("(sass)",(ev,[],"")),None)
def pump(sec):
    end=time.time()+sec; ctx=GLib.MainContext.default()
    while time.time()<end:
        while ctx.iteration(False): pass
        time.sleep(0.005)
env=dict(os.environ)
p=subprocess.Popen([sys.executable,app],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,env=env,text=True,bufsize=1)
bus=None
for _ in range(100):
    pump(0.1)
    try:
        kids=call("org.a11y.atspi.Registry","/org/a11y/atspi/accessible/root",ACC,"GetChildren",None,"(a(so))").unpack()[0]
    except Exception: kids=[]
    for (n,pth) in kids:
        try:
            pid=call("org.freedesktop.DBus","/org/freedesktop/DBus","org.freedesktop.DBus","GetConnectionUnixProcessID",GLib.Variant("(s)",(n,)),"(u)").unpack()[0]
        except Exception: continue
        if pid==p.pid: bus=n
    if bus: break
print("app pid",p.pid,"bus",bus)
if not bus: p.kill(); sys.exit("app not on a11y bus")
ROOT="/org/a11y/atspi/accessible/root"
def toolkit(): return prop(bus,ROOT,"org.a11y.atspi.Application","ToolkitName")+" "+prop(bus,ROOT,"org.a11y.atspi.Application","Version")
print("toolkit",toolkit())
def get_items():
    t=time.perf_counter()
    r=call(bus,"/org/a11y/atspi/cache","org.a11y.atspi.Cache","GetItems",None,ITEMS,10000)
    dt=(time.perf_counter()-t)*1e3
    d={}
    for it in r.unpack()[0]:
        (ref,appr,par,idx,cnt,ifs,name,role,desc,st)=it
        d[ref[1]]=dict(parent=par[1],idx=idx,cnt=cnt,name=name,role=role,st=st[0]|(st[1]<<32),ifs=ifs)
    return d,dt
def walk(limit=3000):
    truth={}; q=[ROOT]
    while q and len(truth)<limit:
        pth=q.pop(0)
        if pth in truth: continue
        try:
            n=prop(bus,pth,ACC,"Name"); cc=prop(bus,pth,ACC,"ChildCount")
            st=call(bus,pth,ACC,"GetState",None,"(au)").unpack()[0]; role=call(bus,pth,ACC,"GetRole",None,"(u)").unpack()[0]
            kids=call(bus,pth,ACC,"GetChildren",None,"(a(so))").unpack()[0]
        except Exception as e:
            truth[pth]=dict(err=str(e)[:80]); continue
        truth[pth]=dict(name=n,cnt=cc,st=st[0]|(st[1]<<32),role=role,nkids=len(kids))
        for (b,k) in kids:
            if b==bus: q.append(k)
    return truth
def short(pth): return "root" if pth==ROOT else pth.rsplit("/",1)[-1][:6]
def mirror_apply(m,evs):
    for (t,snd,pth,iface,member,params) in evs:
        if snd!=bus or iface!="org.a11y.atspi.Cache": continue
        if member=="AddAccessible":
            (ref,appr,par,idx,cnt,ifs,name,role,desc,st)=params.unpack()[0]
            m[ref[1]]=dict(parent=par[1],idx=idx,cnt=cnt,name=name,role=role,st=st[0]|(st[1]<<32),ifs=ifs)
        elif member=="RemoveAccessible":
            m.pop(params.unpack()[0][1],None)
def report(label,m,evs):
    truth=walk(); fresh,dt=get_items()
    out={"step":label}
    ev=[]
    for (t,snd,pth,iface,member,params) in evs:
        if snd!=bus: continue
        if iface=="org.a11y.atspi.Cache":
            if member=="AddAccessible":
                u=params.unpack()[0]; ev.append(f"Cache.Add {short(u[0][1])}:{u[6]!r}{'(hidden)' if u[9][0]>>25&1==0 and False else ''}")
            elif member=="RemoveAccessible": ev.append(f"Cache.Remove {short(params.unpack()[0][1])}")
            else: ev.append(f"Cache.{member}")
        elif iface.startswith("org.a11y.atspi.Event."):
            u=params.unpack()
            ev.append(f"{iface.split('.')[-1]}.{member}[{u[0]}|{u[1]},{u[2]}] {short(pth)}")
    out["events"]=ev
    mm=dict(m)
    un=[short(p_)+":"+str(truth[p_].get("name")) for p_ in truth if p_ not in mm and "err" not in truth[p_]]
    ghost=[]
    for p_ in mm:
        if p_ not in truth:
            alive=True
            try: prop(bus,p_,ACC,"Name")
            except Exception as e: alive="dead:"+str(e).split(":")[-1][:30]
            ghost.append(f"{short(p_)}:{mm[p_]['name']!r}:{alive}")
    diffs=[]
    for p_ in mm:
        if p_ in truth and "err" not in truth[p_]:
            a,b=mm[p_],truth[p_]
            if a["name"]!=b["name"]: diffs.append(f"{short(p_)} name {a['name']!r}->{b['name']!r}")
            if a["cnt"]>=0 and a["cnt"]!=b["cnt"]: diffs.append(f"{short(p_)} childcount {a['cnt']}->{b['cnt']} ({b['name']!r})")
            if a["st"]!=b["st"]: diffs.append(f"{short(p_)} states {b['name']!r} +{sdesc(b['st']&~a['st'])} -{sdesc(a['st']&~b['st'])}")
    def fl(t):
        return "".join(("T" if t["st"]>>28&1 else "")+("M" if t["st"]>>31&1 else "")+("D" if t["st"]>>6&1 else ""))
    nr=[short(p_)+":"+str(truth[p_].get("name"))+"{role"+str(truth[p_].get("role"))+","+fl(truth[p_])+"}" for p_ in truth if p_ not in fresh and "err" not in truth[p_]]
    mdesc=[short(p_)+":"+str(truth[p_].get("name"))+"{role"+str(truth[p_].get("role"))+"}" for p_ in truth if "err" not in truth[p_] and truth[p_]["st"]>>31&1]
    orph=[short(p_)+":"+fresh[p_]['name']+"(st:"+("SHOWING" if fresh[p_]['st']>>25&1 else "!showing")+")" for p_ in fresh if p_ not in truth]
    out.update(mirror_n=len(mm),truth_n=len(truth),getitems_n=len(fresh),getitems_ms=round(dt,1),
      truth_not_in_mirror=un,mirror_not_in_truth=ghost,field_diffs=diffs,truth_not_in_getitems=nr,manages_descendants=mdesc,getitems_not_walkable=orph)
    return out,fresh
res=[]
pump(1.0)
m,dt=get_items(); print("initial GetItems",len(m),"items",round(dt,1),"ms")
events.clear()
out,fresh=report("initial",m,[]); res.append(out)
pump(1.5); events.clear()
print("initial walk realised: GetItems",len(get_items()[0]),"items; initial truth",out["truth_n"])
# P2P probe
try:
    ab=call(bus,ROOT,"org.a11y.atspi.Application","GetApplicationBusAddress",None,"(s)").unpack()[0]
except Exception as e: ab="ERR "+str(e)[:100]
print("GetApplicationBusAddress ->",repr(ab))
p2p={"addr":ab}
if ab and not ab.startswith("ERR"):
    try:
        t=time.perf_counter()
        pc=Gio.DBusConnection.new_for_address_sync(ab, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT, None, None)
        p2p["connect_ms"]=round((time.perf_counter()-t)*1e3,2)
        ts=[]
        for _ in range(100):
            t=time.perf_counter(); call(None,ROOT,ACC,"GetChildren",None,"(a(so))",3000,pc); ts.append((time.perf_counter()-t)*1e3)
        ts.sort(); p2p["p2p_getchildren_p50_ms"]=round(ts[50],3); p2p["p2p_p99_ms"]=round(ts[98],3)
        ts=[]
        for _ in range(100):
            t=time.perf_counter(); call(bus,ROOT,ACC,"GetChildren",None,"(a(so))"); ts.append((time.perf_counter()-t)*1e3)
        ts.sort(); p2p["bus_getchildren_p50_ms"]=round(ts[50],3); p2p["bus_p99_ms"]=round(ts[98],3)
        try:
            r=call(None,"/org/a11y/atspi/cache","org.a11y.atspi.Cache","GetItems",None,ITEMS,5000,pc); p2p["p2p_getitems_n"]=len(r.unpack()[0])
        except Exception as e: p2p["p2p_getitems"]="ERR "+str(e)[:80]
        # does the p2p connection receive signals? (no: signals go to bus)
        pc.close_sync(None)
    except Exception as e: p2p["connect_err"]=str(e)[:150]
sp=ab.split("path=")[-1].split(",")[0] if "path=" in ab else None
if sp and os.path.exists(sp):
    st=os.stat(sp); p2p["socket_mode"]=oct(st.st_mode&0o7777); d=os.stat(os.path.dirname(sp)); p2p["dir_mode"]=oct(d.st_mode&0o7777); p2p["owner_uid"]=st.st_uid
print("P2P",json.dumps(p2p))
STEPLIST=steps or ["m_button_label","m_label_text","m_check_on","m_labelfor_text","m_scale","m_entry_text","m_lb_append","m_lb_remove","m_hide_extra","m_show_extra","m_stack_b","m_expand","m_insensitive","m_switch","m_tooltip","m_a11y_label","m_add_new_subtree","m_rm_subtree","m_hide_then_destroy","m_destroy_extra","m_tv_append","m_tv_remove"]
for s in STEPLIST:
    m,_=get_items(); events.clear()
    p.stdin.write(s+"\n"); p.stdin.flush()
    ok=False
    for _ in range(100):
        pump(0.05)
        import select
        if select.select([p.stdout],[],[],0)[0]:
            l=p.stdout.readline()
            if l.startswith("done") or l.startswith("ERR"): ok=True; break
    if not ok: print("step",s,"no ack"); continue
    pump(0.6)
    evs=list(events); mirror_apply(m,evs)
    out,_=report(s,m,evs); res.append(out)
    print(json.dumps(out))
try:
    p.stdin.write("quit\n"); p.stdin.flush(); p.wait(timeout=3)
except Exception: p.kill()
json.dump(dict(app=app,toolkit=toolkit() if False else None,p2p=p2p,steps=res),open(app+".result.json","w"),indent=1)
