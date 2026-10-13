#!/usr/bin/python3
"""key_forger.py - a rogue session-bus client that forges the manager's signals.

Usage: key_forger.py SESSION_ADDRESS [COUNT] [MODE ...]

Looks up the unique name that owns org.gnome.Orca.KeyboardMonitor (the screen reader) and sends
it unicast signals from OUR connection, exactly like the real manager or the bus daemon would
(the bus stamps the sender, so the reader can tell). A screen reader without a sender gate would
echo "a", run commands, or restart its keyboard session. MODES (default: all):

  keyevent  org.freedesktop.a11y.KeyboardMonitor.KeyEvent for the plain key `a` (press, release)
  chord     KeyEvent Insert down, `x` down/up, Insert up: forges the gorca+X command
  shape     KeyEvent with the wrong path, the wrong interface and the wrong member
  owner     org.freedesktop.DBus.NameOwnerChanged for the manager's name and for the reader's own
            name, claiming both lost their owner (a forged owner change = a forced restart)
  all       every mode above, COUNT times
  flood:MS  keep sending all of the above, as fast as possible, for MS milliseconds (use it while
            the real manager restarts: a forged signal must neither get through during the owner
            transition nor stop the genuine restart)

Prints the number of signals sent. Refuses to run outside a private harness session bus.
"""
import sys
import time

import dbus
import dbus.bus
import dbus.lowlevel

ORCA = "org.gnome.Orca.KeyboardMonitor"
MANAGER = "org.freedesktop.a11y.Manager"
PATH = "/org/freedesktop/a11y/Manager"
IFACE = "org.freedesktop.a11y.KeyboardMonitor"
BUS_PATH = "/org/freedesktop/DBus"
BUS_IFACE = "org.freedesktop.DBus"
INSERT = 0xFF63


def key_signal(owner, path, iface, member, released, keysym, keycode):
    msg = dbus.lowlevel.SignalMessage(path, iface, member)
    msg.set_destination(owner)
    msg.append(dbus.Boolean(released), dbus.UInt32(0), dbus.UInt32(keysym),
               dbus.UInt32(keysym if keysym < 0x80 else 0), dbus.UInt16(keycode), signature="buuuq")
    return msg


def owner_signal(owner, name, old, new):
    msg = dbus.lowlevel.SignalMessage(BUS_PATH, BUS_IFACE, "NameOwnerChanged")
    msg.set_destination(owner)
    msg.append(dbus.String(name), dbus.String(old), dbus.String(new), signature="sss")
    return msg


def round_of(conn, owner, mode):
    sent = []
    if mode in ("keyevent", "all"):
        for released in (False, True):
            sent.append(key_signal(owner, PATH, IFACE, "KeyEvent", released, 0x61, 38))
    if mode in ("chord", "all"):
        sent.append(key_signal(owner, PATH, IFACE, "KeyEvent", False, INSERT, 118))
        sent.append(key_signal(owner, PATH, IFACE, "KeyEvent", False, 0x78, 53))
        sent.append(key_signal(owner, PATH, IFACE, "KeyEvent", True, 0x78, 53))
        sent.append(key_signal(owner, PATH, IFACE, "KeyEvent", True, INSERT, 118))
    if mode in ("shape", "all"):
        sent.append(key_signal(owner, "/x", IFACE, "KeyEvent", False, 0x61, 38))
        sent.append(key_signal(owner, PATH, "org.example.Other", "KeyEvent", False, 0x61, 38))
        sent.append(key_signal(owner, PATH, IFACE, "OtherSignal", False, 0x61, 38))
    if mode in ("owner", "all"):
        real = conn.get_name_owner(MANAGER) if conn.name_has_owner(MANAGER) else ":1.1"
        sent.append(owner_signal(owner, MANAGER, real, ""))
        sent.append(owner_signal(owner, MANAGER, real, ":1.999"))
        sent.append(owner_signal(owner, ORCA, owner, ""))
    for m in sent:
        conn.send_message(m)
    conn.flush()
    return len(sent)


def main():
    addr = sys.argv[1]
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 5
    modes = sys.argv[3:] or ["all"]
    if "gorca-h." not in addr:
        sys.exit("refusing to run: the session bus is not a private harness bus")
    conn = dbus.bus.BusConnection(addr)
    owner = conn.get_name_owner(ORCA)
    total = 0
    for mode in modes:
        if mode.startswith("flood:"):
            end = time.monotonic() + int(mode.split(":", 1)[1]) / 1000.0
            while time.monotonic() < end:
                try:
                    # The reader reconnects during a restart: aim at its current connection.
                    owner = conn.get_name_owner(ORCA)
                    total += round_of(conn, owner, "all")
                except dbus.DBusException:
                    pass
                time.sleep(0.0005)
            continue
        for _ in range(count):
            total += round_of(conn, owner, mode)
    print(total)


main()
