import sys, time, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi, Gio, GLib
from collections import Counter
c=Gio.DBusConnection.new_for_address_sync("unix:path=/run/user/1000/at-spi/bus", Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
for name in sys.argv[1:]:
    ts=[]
    for _ in range(5):
        t=time.perf_counter(); r=c.call_sync(name,"/org/a11y/atspi/cache","org.a11y.atspi.Cache","GetItems",None,None,Gio.DBusCallFlags.NONE,20000,None); ts.append(time.perf_counter()-t)
    items=r.get_child_value(0); n=items.n_children()
    roles=Counter(); apps=Counter()
    for i in range(n):
        it=items.get_child_value(i)
        roles[Atspi.Role(it.get_child_value(7).get_uint32()).value_nick]+=1
    print(name,"items",n,"reply bytes(approx text)",len(r.print_(False)),"min call ms",round(min(ts)*1000,2)); print(roles.most_common(10))
