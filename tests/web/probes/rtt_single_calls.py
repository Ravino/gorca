import sys, time, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi, Gio, GLib
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
print("doc path", doc.path, "app", app.get_process_id())
bus=doc.get_application().get_name()
c=Gio.DBusConnection.new_for_address_sync("unix:path=/run/user/1000/at-spi/bus", Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
dest=sys.argv[3]
def call(path, iface, m, params=None):
    return c.call_sync(dest,path,iface,m,params,None,Gio.DBusCallFlags.NONE,20000,None)
path=doc.path
def lat(label, f, n=300):
    ts=[]
    for _ in range(n):
        t=time.perf_counter(); f(); ts.append((time.perf_counter()-t)*1e6)
    ts.sort(); print(f"{label}: p50 {ts[len(ts)//2]:.0f}us p99 {ts[int(len(ts)*.99)]:.0f}us")
lat("GetChildren(doc)", lambda: call(path,"org.a11y.atspi.Accessible","GetChildren"))
lat("GetRole(doc)", lambda: call(path,"org.a11y.atspi.Accessible","GetRole"))
lat("Prop Get ChildCount", lambda: call(path,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",("org.a11y.atspi.Accessible","ChildCount"))))
lat("GetState", lambda: call(path,"org.a11y.atspi.Accessible","GetState"))
lat("GetAttributes", lambda: call(path,"org.a11y.atspi.Accessible","GetAttributes"))
lat("GetInterfaces", lambda: call(path,"org.a11y.atspi.Accessible","GetInterfaces"))
r=call(path,"org.a11y.atspi.Accessible","GetChildren"); print("children of doc:", r.get_child_value(0).n_children())
