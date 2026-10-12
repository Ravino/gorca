import sys, time, gi
gi.require_version("Gio","2.0")
from gi.repository import Gio, GLib
addr=open(sys.argv[1]).read().strip()
c=Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(dest,path,iface,m,args=None,sig=None,timeout=5000):
    t=time.perf_counter()
    r=c.call_sync(dest,path,iface,m,args,GLib.VariantType(sig) if sig else None,Gio.DBusCallFlags.NONE,timeout,None)
    return r,(time.perf_counter()-t)*1e3
for name in sys.argv[2:]:
    try:
        r,dt=call(name,"/org/a11y/atspi/cache","org.a11y.atspi.Cache","GetItems",None,"(a((so)(so)(so)iiassusau))",30000)
    except Exception as e:
        print(name,"GetItems ERR",e); continue
    items=r.get_child_value(0)
    n=items.n_children()
    sz=len(r.print_(False))
    print(f"{name} GetItems: {n} items, {dt:.1f} ms, text-size~{sz} B")
    # ping RTT
    ts=[]
    for i in range(200):
        _,d=call(name,"/org/a11y/atspi/accessible/root","org.freedesktop.DBus.Peer","Ping",None,"()")
        ts.append(d)
    ts.sort(); print(f"  Peer.Ping rtt ms: p50={ts[100]:.3f} p99={ts[197]:.3f} max={ts[-1]:.3f}")
    ts=[]
    for i in range(200):
        _,d=call(name,"/org/a11y/atspi/accessible/root","org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",("org.a11y.atspi.Accessible","Name")),"(v)")
        ts.append(d)
    ts.sort(); print(f"  Properties.Get(Name) rtt ms: p50={ts[100]:.3f} p99={ts[197]:.3f} max={ts[-1]:.3f}")
