import sys, time, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi, Gio, GLib
Atspi.init()
which=sys.argv[1]; idx=int(sys.argv[2]); dest=sys.argv[3]
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
doc=find(app); path=doc.path
bus=Gio.DBusConnection.new_for_address_sync("unix:path=/run/user/1000/at-spi/bus", Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
r=bus.call_sync(dest,"/org/a11y/atspi/accessible/root","org.a11y.atspi.Application","GetApplicationBusAddress",None,None,Gio.DBusCallFlags.NONE,5000,None)
addr=r.get_child_value(0).get_string(); print("p2p addr:", addr[:60])
p2p=Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT, None, None)
def lat(label, conn, d, k=500):
    ts=[]
    for _ in range(k):
        t=time.perf_counter(); conn.call_sync(d,path,"org.a11y.atspi.Accessible","GetRole",None,None,Gio.DBusCallFlags.NONE,5000,None); ts.append((time.perf_counter()-t)*1e6)
    ts.sort(); print(f"{label}: p50 {ts[len(ts)//2]:.0f}us p99 {ts[int(len(ts)*.99)]:.0f}us max {ts[-1]:.0f}us")
lat("GetRole via bus daemon", bus, dest)
lat("GetRole via p2p       ", p2p, None)
lat("GetRole via bus daemon", bus, dest)
lat("GetRole via p2p       ", p2p, None)
