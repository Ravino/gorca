#!/usr/bin/python3
"""In-order fake org.freedesktop.a11y.Manager (like Mutter: one main loop, calls handled in
arrival order). The first GrabKeyboard blocks the main loop STALL seconds (a shell stall).
Prints one JSON line per handled call, and the final per-sender state on SIGTERM-less 'dump'
timer every 0.5 s."""
import json, sys, time
from gi.repository import Gio, GLib

STALL = float(sys.argv[1]); STALL_ON = sys.argv[2]
XML = """<node><interface name='org.freedesktop.a11y.KeyboardMonitor'>
<method name='WatchKeyboard'/><method name='UnwatchKeyboard'/>
<method name='GrabKeyboard'/><method name='UngrabKeyboard'/>
<method name='SetKeyGrabs'><arg type='au' direction='in'/><arg type='a(uu)' direction='in'/></method>
<signal name='KeyEvent'><arg type='b'/><arg type='u'/><arg type='u'/><arg type='u'/><arg type='q'/></signal>
</interface></node>"""
node = Gio.DBusNodeInfo.new_for_xml(XML)
state = {}
stalled = [False]
t0 = time.monotonic()


def log(**kw):
    kw["t"] = round(time.monotonic() - t0, 3)
    print(json.dumps(kw), flush=True)


def call(conn, sender, path, iface, method, params, inv):
    g = state.setdefault(sender, {"grab_all": False, "modifiers": None, "watch": False})
    if method == STALL_ON and not stalled[0]:
        stalled[0] = True
        log(ev="stall-begin", method=method)
        time.sleep(STALL)
    if method == "GrabKeyboard":
        g["grab_all"] = True
    elif method == "UngrabKeyboard":
        g["grab_all"] = False
    elif method == "WatchKeyboard":
        g["watch"] = True
    elif method == "SetKeyGrabs":
        g["modifiers"] = list(params.unpack()[0])
    log(ev="call", sender=sender, method=method, state=g)
    inv.return_value(None)


def on_bus(conn, name):
    conn.register_object("/org/freedesktop/a11y/Manager", node.interfaces[0], call, None, None)


def dump():
    log(ev="state", state=state)
    return True


Gio.bus_own_name(Gio.BusType.SESSION, "org.freedesktop.a11y.Manager", 0, on_bus, None, None)
GLib.timeout_add(1000, dump)
GLib.MainLoop().run()
