#!/usr/bin/python3
# rd_probe.py BUS : direct org.gnome.Mutter.RemoteDesktop in the PRIVATE shell: which pointer calls work without a ScreenCast stream.
import sys
from gi.repository import Gio, GLib
c = Gio.DBusConnection.new_for_address_sync(sys.argv[1], Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(path, iface, m, a=None, t=None, dest='org.gnome.Mutter.RemoteDesktop'):
    try:
        r = c.call_sync(dest, path, iface, m, a, GLib.VariantType(t) if t else None, 0, 3000, None)
        return r.unpack() if r else None
    except GLib.Error as e:
        return 'ERR ' + e.message
sp = call('/org/gnome/Mutter/RemoteDesktop','org.gnome.Mutter.RemoteDesktop','CreateSession',None,'(o)')
print('CreateSession', sp)
s = sp[0]; I = 'org.gnome.Mutter.RemoteDesktop.Session'
print('Start', call(s, I, 'Start', None, '()'))
print('MoveRel', call(s, I, 'NotifyPointerMotionRelative', GLib.Variant('(dd)', (10.0, 10.0)), '()'))
print('MoveAbs(stream="")', call(s, I, 'NotifyPointerMotionAbsolute', GLib.Variant('(sdd)', ('', 100.0, 100.0)), '()'))
print('Button', call(s, I, 'NotifyPointerButton', GLib.Variant('(ib)', (0x110, True)), '()'))
print('Button up', call(s, I, 'NotifyPointerButton', GLib.Variant('(ib)', (0x110, False)), '()'))
print('Stop', call(s, I, 'Stop', None, '()'))
