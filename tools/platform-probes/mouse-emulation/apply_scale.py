#!/usr/bin/python3
# apply_scale.py BUS INDEX SCALE : set scale of monitor INDEX (0-based order of GetCurrentState), lay monitors left to right.
import sys, os
os.environ['DBUS_SESSION_BUS_ADDRESS']=sys.argv[1]
from gi.repository import Gio, GLib
c = Gio.DBusConnection.new_for_address_sync(sys.argv[1], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(m, a, t):
    return c.call_sync('org.gnome.Mutter.DisplayConfig','/org/gnome/Mutter/DisplayConfig','org.gnome.Mutter.DisplayConfig',m,a,GLib.VariantType(t),0,5000,None)
st = call('GetCurrentState', None, '(ua((ssss)a(siiddada{sv})a{sv})a(iiduba(ssss)a{sv})a{sv})').unpack()
serial, mons = st[0], st[1]
idx, scale = int(sys.argv[2]), float(sys.argv[3])
x = 0; lm = []
for i, m in enumerate(mons):
    modes = m[1]
    cur = [md for md in modes if md[6].get('is-current')][0]
    sc = scale if i == idx else 1.0
    w, h = cur[1], cur[2]
    lm.append((x, 0, sc, 0, i == 0, [(m[0][0], cur[0], {})]))
    print('mon', i, m[0][0], cur[0], 'scale', sc, 'supported', cur[5])
    x += int(round(w / sc))
cfg = GLib.Variant('(uua(iiduba(ssa{sv}))a{sv})', (serial, 1, lm, {}))
call('ApplyMonitorsConfig', cfg, '()')
print('applied')
