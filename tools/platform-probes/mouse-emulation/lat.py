#!/usr/bin/python3
# lat.py BUS : D-Bus round trip of MoveAbs and Click against the probe (private shell).
import sys, time
from gi.repository import Gio, GLib
c = Gio.DBusConnection.new_for_address_sync(sys.argv[1], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(m, a, t):
    return c.call_sync('org.gorca.MouseProbe','/org/gorca/MouseProbe','org.gorca.MouseProbe',m,a,GLib.VariantType(t),0,3000,None)
for name, mk, t in (('MoveAbs', lambda i: GLib.Variant('(dd)', (100.0+i%500, 200.0)), '(x)'), ('Click', lambda i: GLib.Variant('(u)', (1,)), '(x)'), ('Pointer', lambda i: None, '(ii)')):
    ds = []
    for i in range(500):
        t0 = time.perf_counter(); call(name, mk(i), t); ds.append((time.perf_counter()-t0)*1e3)
    ds.sort(); print('%-8s p50 %.2f ms  p99 %.2f ms  max %.2f ms' % (name, ds[250], ds[494], ds[-1]))
