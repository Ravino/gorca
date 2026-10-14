"""Does the process group of APID hold a connection on the private a11y bus? argv[1] = APID (a pgid)."""
import os, sys
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib
addr = os.environ["DBUS_SESSION_BUS_ADDRESS"]
assert addr.startswith("unix:path=/tmp/dbus-"), addr
sess = Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
a = sess.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress", None, GLib.VariantType("(s)"), 0, 3000, None).unpack()[0]
assert "/tmp/" in a or "/run/user/1000/" not in a, a
bus = Gio.DBusConnection.new_for_address_sync(a, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
pg = int(sys.argv[1])
names = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ListNames", None, GLib.VariantType("(as)"), 0, 3000, None).unpack()[0]
found = []
for n in names:
    if not n.startswith(":"):
        continue
    try:
        pid = bus.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetConnectionUnixProcessID", GLib.Variant("(s)", (n,)), GLib.VariantType("(u)"), 0, 3000, None).unpack()[0]
        if os.getpgid(pid) == pg:
            found.append((n, pid, open("/proc/%d/comm" % pid).read().strip()))
    except Exception:
        pass
print("  a11y-bus connections from the app's process group:", found if found else "NONE")
