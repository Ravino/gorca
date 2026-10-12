import time, gi
gi.require_version("Gio","2.0")
from gi.repository import Gio, GLib
c=Gio.bus_get_sync(Gio.BusType.SESSION,None)
def rtt(dest,path,n=2000):
    ts=[]
    for i in range(n):
        t=time.perf_counter()
        c.call_sync(dest,path,"org.freedesktop.DBus.Peer","Ping",None,None,Gio.DBusCallFlags.NONE,2000,None)
        ts.append((time.perf_counter()-t)*1e3)
    ts.sort(); return ts[n//2],ts[int(n*.99)],ts[int(n*.999)],ts[-1]
for name,path in [("org.freedesktop.a11y.Manager","/org/freedesktop/a11y/Manager"),("org.freedesktop.portal.Desktop","/org/freedesktop/portal/desktop"),("org.gnome.Orca.KeyboardMonitor","/")]:
    try: print(name,"Ping p50=%.3f p99=%.3f p99.9=%.3f max=%.3f ms"%rtt(name,path))
    except Exception as e: print(name,"ERR",e)
