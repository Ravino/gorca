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
c=Gio.DBusConnection.new_for_address_sync("unix:path=/run/user/1000/at-spi/bus", Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
dest=sys.argv[3]; ncalls=0
def call(path, iface, m, params=None):
    global ncalls; ncalls+=1
    return c.call_sync(dest,path,iface,m,params,None,Gio.DBusCallFlags.NONE,20000,None)
A="org.a11y.atspi.Accessible"
def prop(path,iface,name):
    return call(path,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",(iface,name)))
def run(per_node):
    global ncalls; ncalls=0
    t=time.perf_counter(); n=0
    stack=[doc.path]
    while stack:
        p=stack.pop(); n+=1
        ch=call(p,A,"GetChildren").get_child_value(0)
        for i in range(ch.n_children()):
            stack.append(ch.get_child_value(i).get_child_value(1).get_string())
        if per_node:
            call(p,A,"GetRole"); call(p,A,"GetState"); call(p,A,"GetInterfaces"); prop(p,A,"Name"); call(p,A,"GetAttributes")
    dt=time.perf_counter()-t
    print(f"per_node={per_node}: nodes={n} calls={ncalls} time={dt*1000:.1f}ms  ({dt/n*1e6:.0f}us/node)")
run(False); run(True)
