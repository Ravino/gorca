#!/usr/bin/env python3
"""Read-only P-9 survey of apps on the CURRENT a11y bus: GetItems vs a bounded tree walk, staleness of an idle snapshot,
MANAGES_DESCENDANTS nodes, P2P reachability. Calls only Properties.Get, GetState, GetRole, GetChildren, GetItems,
GetApplicationBusAddress. usage: p9live.py BUSNAME[:LIMIT] ..."""
import sys, os, time, collections
import gi
gi.require_version("Gio","2.0")
from gi.repository import Gio, GLib
ITEMS="(a((so)(so)(so)iiassusau))"; ACC="org.a11y.atspi.Accessible"; ROOT="/org/a11y/atspi/accessible/root"
sess=Gio.bus_get_sync(Gio.BusType.SESSION)
addr=sess.call_sync("org.a11y.Bus","/org/a11y/bus","org.a11y.Bus","GetAddress",None,None,0,3000,None).unpack()[0]
c=Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(dest,path,iface,m,args=None,sig=None,timeout=3000,conn=None):
    return (conn or c).call_sync(dest,path,iface,m,args,GLib.VariantType(sig) if sig else None,Gio.DBusCallFlags.NONE,timeout,None)
def prop(dest,path,iface,name): return call(dest,path,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",(iface,name)),"(v)").unpack()[0]
for arg in sys.argv[1:]:
    bus,at,lim=arg.partition("@"); lim=int(lim) if at else 3000
    t=time.perf_counter(); r=call(bus,"/org/a11y/atspi/cache","org.a11y.atspi.Cache","GetItems",None,ITEMS,10000); dt=(time.perf_counter()-t)*1e3
    items={}; order=[]
    for (ref,appr,par,idx,cnt,ifs,name,role,desc,st) in r.unpack()[0]:
        items[ref[1]]=dict(parent=par[1],idx=idx,cnt=cnt,name=name,role=role,st=st[0]|(st[1]<<32)); order.append(ref[1])
    t=time.perf_counter(); truth={}; q=collections.deque([ROOT]); trunc=False
    while q:
        if len(truth)>=lim: trunc=True; break
        p=q.popleft()
        if p in truth: continue
        try:
            n=prop(bus,p,ACC,"Name"); cc=prop(bus,p,ACC,"ChildCount"); st=call(bus,p,ACC,"GetState",None,"(au)").unpack()[0]
            kids=call(bus,p,ACC,"GetChildren",None,"(a(so))").unpack()[0]
        except Exception as e: truth[p]=dict(err=str(e)[-60:]); continue
        truth[p]=dict(name=n,cnt=cc,st=st[0]|(st[1]<<32),kids=len(kids))
        for (b,k) in kids:
            if b==bus: q.append(k)
    wdt=time.perf_counter()-t
    ok={p:v for p,v in truth.items() if "err" not in v}
    inter=[p for p in items if p in ok]
    ndiff=sum(1 for p in inter if items[p]["name"]!=ok[p]["name"]); cdiff=sum(1 for p in inter if items[p]["cnt"]>=0 and items[p]["cnt"]!=ok[p]["cnt"]); sdiff=sum(1 for p in inter if items[p]["st"]!=ok[p]["st"])
    md=sum(1 for v in ok.values() if v["st"]>>31&1); tr=sum(1 for v in ok.values() if v["st"]>>28&1)
    cm1=sum(1 for v in items.values() if v["cnt"]==-1); ip1=sum(1 for v in items.values() if v["idx"]==-1)
    # shape: orphans (parent not in items)
    orph=sum(1 for p,v in items.items() if v["parent"] not in items and v["parent"]!=ROOT and v["parent"]!="/org/a11y/atspi/null")
    try: ab=call(bus,ROOT,"org.a11y.atspi.Application","GetApplicationBusAddress",None,"(s)").unpack()[0]
    except Exception as e: ab="ERR "+str(e)[-50:]
    ext=""
    if ab and not ab.startswith("ERR"):
        try:
            t=time.perf_counter(); pc=Gio.DBusConnection.new_for_address_sync(ab,Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT,None,None); ct=(time.perf_counter()-t)*1e3
            ts=[]
            for _ in range(100):
                t=time.perf_counter(); call(None,ROOT,ACC,"GetChildren",None,"(a(so))",3000,pc); ts.append((time.perf_counter()-t)*1e3)
            ts.sort(); tb=[]
            for _ in range(100):
                t=time.perf_counter(); call(bus,ROOT,ACC,"GetChildren",None,"(a(so))"); tb.append((time.perf_counter()-t)*1e3)
            tb.sort(); pc.close_sync(None)
            sp=ab.split("path=")[-1]; st_=os.stat(sp); dm=os.stat(os.path.dirname(sp))
            ext=f" connect={ct:.2f}ms p2p p50/p99={ts[50]:.3f}/{ts[98]:.3f} bus p50/p99={tb[50]:.3f}/{tb[98]:.3f} sock={oct(st_.st_mode&0o7777)} dir={oct(dm.st_mode&0o7777)}"
        except Exception as e: ext=" P2P-ERR "+str(e)[:80]
    print(f"{bus}: GetItems={len(items)} ({dt:.1f}ms) walk={len(ok)}{'+' if trunc else ''} ({wdt:.2f}s) walk_not_in_items={sum(1 for p in ok if p not in items)} items_not_walked={sum(1 for p in items if p not in ok) if not trunc else 'n/a'} idle_diffs name/childcount/states={ndiff}/{cdiff}/{sdiff} MANAGES_DESC_nodes={md} TRANSIENT_nodes={tr} items cnt=-1:{cm1} idx=-1:{ip1} orphan_parents={orph} p2p={ab}{ext}")
