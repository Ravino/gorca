# rd2.py: tests/web/m7/rd.py plus a clipboard read ("clip FILE" on stdin writes the clipboard text
# to FILE through the RemoteDesktop session, without a window that would steal focus).
"""Keeps a Mutter RemoteDesktop session open on the private compositor bus.

A headless gnome-shell has no keyboard, so no client window ever gets keyboard focus (and
Firefox emits no AT-SPI focus events). A RemoteDesktop session adds a virtual keyboard and
pointer to the seat. This process holds the session until stdin closes or it is signalled.
It talks only to GORCA_M7_CBUS (the private compositor bus), never to the user's session.
"""
import os
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

bus = Gio.DBusConnection.new_for_address_sync(
    os.environ["GORCA_M7_CBUS"],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None,
    None,
)
NAME = "org.gnome.Mutter.RemoteDesktop"


def call(path, iface, method, params=None, sig=None):
    v = GLib.Variant(sig, params) if sig else None
    return bus.call_sync(NAME, path, iface, method, v, None, Gio.DBusCallFlags.NONE, 5000, None)


r = call("/org/gnome/Mutter/RemoteDesktop", NAME, "CreateSession").unpack()
sess = r[0]
call(sess, NAME + ".Session", "EnableClipboard", ({},), "(a{sv})")
call(sess, NAME + ".Session", "Start")
print("rd session", sess, flush=True)


def click():
    # relative motion then a left click: enough to make mutter focus the window under the pointer
    # each click pushes the pointer further to the bottom right, where it stays (clamped) on the
    # page of the 1200x800 window
    call(sess, NAME + ".Session", "NotifyPointerMotionRelative", (300.0, 300.0), "(dd)")
    call(sess, NAME + ".Session", "NotifyPointerButton", (272, True), "(ib)")
    call(sess, NAME + ".Session", "NotifyPointerButton", (272, False), "(ib)")
    return True




def on_stdin(fd, cond):
    # one command per line: "click" nudges the pointer into the window and clicks; EOF quits
    data = os.read(fd, 4096)
    if not data:
        loop.quit()
        return False
    for line in data.decode().splitlines():
        words = line.split()
        if words[:1] == ["key"] and len(words) >= 2:
            # "key <evdev code>..." presses and releases the codes in order (modifiers first,
            # released last), e.g. "key 29 109" is Ctrl+PageDown
            try:
                codes = [int(w) for w in words[1:]]
                for c in codes:
                    call(sess, NAME + ".Session", "NotifyKeyboardKeycode", (c, True), "(ub)")
                    time.sleep(0.03)
                # A key goes down for a moment, like a finger: a press and a release in the same
                # instant made Firefox lose an occasional letter (M3b FE2).
                time.sleep(0.012)
                for c in reversed(codes):
                    call(sess, NAME + ".Session", "NotifyKeyboardKeycode", (c, False), "(ub)")
            except (ValueError, GLib.Error) as e:
                print("key failed", e, flush=True)
        if words[:1] == ["clip"]:
            # read the clipboard through the session (no window, so no focus change) into a file
            try:
                r, fds = bus.call_with_unix_fd_list_sync(NAME, sess, NAME + ".Session", "SelectionRead",
                    GLib.Variant("(s)", ("text/plain;charset=utf-8",)), None, Gio.DBusCallFlags.NONE, 5000, None, None)
                fd = fds.get(r.unpack()[0])
                data = b""
                import select
                while True:
                    if not select.select([fd], [], [], 2.0)[0]:
                        break
                    chunk = os.read(fd, 65536)
                    if not chunk:
                        break
                    data += chunk
                os.close(fd)
                open(words[1], "wb").write(data)
            except (GLib.Error, OSError) as e:
                open(words[1], "wb").write(("ERR " + str(e)).encode())
        if line.strip() == "click":
            try:
                click()
            except GLib.Error as e:
                print("click failed", e, flush=True)
    return True


loop = GLib.MainLoop()
GLib.unix_fd_add_full(0, sys.stdin.fileno(), GLib.IOCondition.IN | GLib.IOCondition.HUP, on_stdin)
loop.run()
