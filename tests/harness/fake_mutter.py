#!/usr/bin/python3
"""fake_mutter.py - emulates Mutter's org.freedesktop.a11y.KeyboardMonitor on the PRIVATE session bus.

Contract (docs/platform/org.freedesktop.a11y.KeyboardMonitor.md, GNOME 48.7):
  * name `org.freedesktop.a11y.Manager`, path `/org/freedesktop/a11y/Manager`
  * methods WatchKeyboard, UnwatchKeyboard, GrabKeyboard, UngrabKeyboard, SetKeyGrabs(au a(uu))
  * every method is access-checked: the caller's unique name must currently own
    `org.gnome.Orca.KeyboardMonitor`, otherwise `org.freedesktop.DBus.Error.AccessDenied`
  * signal KeyEvent(b u u u q) = released, state, keysym, unichar, keycode, UNICAST to each
    client that called WatchKeyboard or GrabKeyboard (destination = its unique name)

Run ONLY inside tests/harness/run-isolated.sh (refuses to start when the session bus is not
under a gorca-h.* temp dir). Control channel like fakeapp.py: JSON lines on stdin, replies and
notifications on stdout, EOF on stdin quits.

  {"cmd":"key","keycode":38,"keysym":97,"unichar":97,"state":0,"released":false,"id":1}
      -> {"event":"reply","id":1,"ok":true,"t_emit_ns":<CLOCK_MONOTONIC ns just before send>,"sent":<clients>}
  {"cmd":"stats"} -> watchers, grabbers, calls, denied
"""
import json
import os
import sys
import time

import dbus
import dbus.bus
import dbus.lowlevel
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

NAME = "org.freedesktop.a11y.Manager"
PATH = "/org/freedesktop/a11y/Manager"
IFACE = "org.freedesktop.a11y.KeyboardMonitor"
ORCA = "org.gnome.Orca.KeyboardMonitor"


def out(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


class Manager(dbus.service.Object):
    def __init__(self, conn):
        super().__init__(conn, PATH)
        self.conn = conn
        self.watchers = set()
        self.grabbers = set()
        self.stats = {"calls": 0, "denied": 0, "keys": 0}

    def check(self, sender):
        self.stats["calls"] += 1
        try:
            owner = self.conn.get_name_owner(ORCA)
        except dbus.DBusException:
            owner = None
        if owner != sender:
            self.stats["denied"] += 1
            raise dbus.exceptions.DBusException("Access denied",
                                                name="org.freedesktop.DBus.Error.AccessDenied")

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s")
    def WatchKeyboard(self, s=None):
        self.check(s)
        self.watchers.add(s)

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s")
    def UnwatchKeyboard(self, s=None):
        self.check(s)
        self.watchers.discard(s)

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s")
    def GrabKeyboard(self, s=None):
        self.check(s)
        self.grabbers.add(s)

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s")
    def UngrabKeyboard(self, s=None):
        self.check(s)
        self.grabbers.discard(s)

    @dbus.service.method(IFACE, in_signature="aua(uu)", out_signature="", sender_keyword="s")
    def SetKeyGrabs(self, modifiers, keystrokes, s=None):
        self.check(s)

    def key(self, keycode, keysym, unichar, state, released):
        targets = sorted(self.watchers | self.grabbers)
        t = time.monotonic_ns()
        for dest in targets:
            msg = dbus.lowlevel.SignalMessage(PATH, IFACE, "KeyEvent")
            msg.set_destination(dest)
            msg.append(dbus.Boolean(released), dbus.UInt32(state), dbus.UInt32(keysym),
                       dbus.UInt32(unichar), dbus.UInt16(keycode & 0xFFFF), signature="buuuq")
            self.conn.send_message(msg)
        self.conn.flush()
        self.stats["keys"] += 1
        return t, len(targets)


class Stdin:
    def __init__(self, mgr, loop):
        self.mgr, self.loop, self.buf = mgr, loop, b""
        os.set_blocking(0, False)
        GLib.io_add_watch(0, GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, self.ready)

    def ready(self, fd, cond):
        try:
            data = os.read(0, 65536)
        except BlockingIOError:
            return True
        if not data:
            self.loop.quit()
            return False
        self.buf += data
        while b"\n" in self.buf:
            line, self.buf = self.buf.split(b"\n", 1)
            if not line.strip():
                continue
            rid = None
            try:
                cmd = json.loads(line)
                rid = cmd.get("id")
                c = cmd.get("cmd")
                res = {}
                if c == "key":
                    t, n = self.mgr.key(int(cmd.get("keycode", 38)), int(cmd.get("keysym", 97)),
                                        int(cmd.get("unichar", 97)), int(cmd.get("state", 0)),
                                        bool(cmd.get("released", False)))
                    res = {"t_emit_ns": t, "sent": n}
                elif c == "stats":
                    res = dict(self.mgr.stats, watchers=len(self.mgr.watchers),
                               grabbers=len(self.mgr.grabbers))
                elif c == "quit":
                    self.loop.quit()
                else:
                    raise ValueError("unknown cmd %r" % c)
                out(dict(res, event="reply", id=rid, ok=True))
            except Exception as ex:  # noqa: BLE001
                out({"event": "reply", "id": rid, "ok": False, "error": repr(ex)})
        return True


def main():
    addr = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    if "/gorca-h." not in addr:
        sys.stderr.write("fake_mutter: refusing to run outside tests/harness/run-isolated.sh "
                         "(session bus %r is not a harness bus)\n" % addr)
        sys.exit(2)
    DBusGMainLoop(set_as_default=True)
    conn = dbus.bus.BusConnection(addr)
    conn.request_name(NAME, dbus.bus.NAME_FLAG_DO_NOT_QUEUE)
    mgr = Manager(conn)
    loop = GLib.MainLoop()
    Stdin(mgr, loop)
    out({"event": "ready", "bus_name": conn.get_unique_name()})
    try:
        loop.run()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
