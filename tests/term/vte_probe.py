#!/usr/bin/python3
"""Characterise a REAL VTE (gnome-terminal-server) through AT-SPI, for gorca's terminal arm (M6).

Run inside tests/term/run_real_vte.sh (private compositor + private AT-SPI session). It starts
gnome-terminal-server and a window with a bash prompt, gives the headless seat a keyboard
(tests/web/m7/rd.py), types scripted sessions and writes everything a screen reader would see as
JSON lines to $GORCA_VTE_OUT (default vte-events.jsonl):

  {"t": <ms since start>, "ev": "text-changed", "kind": "insert"|"delete", "off": N, "len": N, "text": "..."}
  {"t": ..., "ev": "caret", "off": N}
  {"t": ..., "ev": "key", "name": "...", "evdev": [codes]}        what the probe pressed
  {"t": ..., "ev": "marker", "name": "..."}
  {"t": ..., "ev": "snapshot", "name": "...", "text": "...", "caret": N, "count": N, "chars": N}

It never touches anything outside the two private sessions and kills only the PIDs it started.
"""
import json
import os
import signal
import subprocess
import sys
import time

import dbus
import dbus.mainloop.glib
from gi.repository import GLib

OUT = os.environ.get("GORCA_VTE_OUT") or "vte-events.jsonl"
WL = os.environ.get("GORCA_VTE_WAYLAND")
CBUS = os.environ.get("GORCA_VTE_CBUS")
TOOLS = os.environ.get("GORCA_VTE_TOOLS", os.path.dirname(os.path.abspath(__file__)))
SESSION = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
if "/gorca-h." not in SESSION or not WL or not CBUS:
    sys.exit("refusing to run outside tests/term/run_real_vte.sh (private sessions)")

KEYS = {c: 16 + i for i, c in enumerate("qwertyuiop")}
KEYS.update({c: 30 + i for i, c in enumerate("asdfghjkl")})
KEYS.update({c: 44 + i for i, c in enumerate("zxcvbnm")})
KEYS.update({c: 2 + i for i, c in enumerate("1234567890")})
KEYS.update({" ": 57, "-": 12, "=": 13, "|": None, "\n": 28, ".": 52, "/": 53})
SPECIAL = {"enter": [28], "backspace": [14], "tab": [15], "left": [105], "right": [106], "up": [103],
           "down": [108], "home": [102], "end": [107], "ctrl-c": [29, 46], "ctrl-a": [29, 30],
           "ctrl-e": [29, 18], "ctrl-u": [29, 22], "ctrl-left": [29, 105], "ctrl-l": [29, 38],
           "pipe": [42, 43]}

T0 = time.monotonic()
out_f = open(OUT, "w")
pids = []


def now_ms():
    return round((time.monotonic() - T0) * 1000, 2)


def emit(obj):
    obj["t"] = now_ms()
    out_f.write(json.dumps(obj, ensure_ascii=False) + "\n")
    out_f.flush()


dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
sess = dbus.bus.BusConnection(SESSION)
addr = str(sess.call_blocking("org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress", "", ()))
a11y = dbus.bus.BusConnection(addr)

# ---- start the terminal ------------------------------------------------------------------------------
env = dict(os.environ, GDK_BACKEND="wayland", WAYLAND_DISPLAY=WL, GTK_A11Y="atspi", LANG="C.UTF-8",
           PS1="$ ")
env.pop("DISPLAY", None)
server = subprocess.Popen(["/usr/libexec/gnome-terminal-server", "--app-id", "org.gnome.Terminal"], env=env,
                          stdout=open("/dev/null", "w"), stderr=open(OUT + ".server.log", "w"),
                          start_new_session=True)
pids.append(server.pid)


def teardown(*_):
    for p in reversed(pids):
        try:
            os.kill(p, signal.SIGTERM)
        except ProcessLookupError:
            pass
    time.sleep(0.5)
    for p in pids:
        try:
            os.kill(p, signal.SIGKILL)
        except ProcessLookupError:
            pass


def wait_name(name, secs=20):
    end = time.time() + secs
    while time.time() < end:
        if name in sess.list_names():
            return True
        time.sleep(0.2)
    return False


if not wait_name("org.gnome.Terminal"):
    teardown()
    sys.exit("gnome-terminal-server never took its name")
client = subprocess.Popen(["gnome-terminal", "--app-id", "org.gnome.Terminal", "--window", "--geometry=100x30",
                           "--", "env", "PS1=$ ", "bash", "--norc", "--noprofile", "-i"], env=env,
                          stdout=open("/dev/null", "w"), stderr=open(OUT + ".client.log", "w"),
                          start_new_session=True)
pids.append(client.pid)

rd = subprocess.Popen(["/usr/bin/python3", os.path.join(TOOLS, "..", "web", "m7", "rd.py")],
                      env=dict(os.environ, GORCA_M7_CBUS=CBUS), stdin=subprocess.PIPE,
                      stdout=open("/dev/null", "w"), start_new_session=True, text=True)
pids.append(rd.pid)


def rd_send(line):
    rd.stdin.write(line + "\n")
    rd.stdin.flush()


# ---- find the terminal node -----------------------------------------------------------------------------
def call(bus_name, path, iface, member, sig="", args=()):
    return a11y.call_blocking(bus_name, path, iface, member, sig, args, timeout=5)


def children(name, path):
    try:
        return [(str(a), str(b)) for a, b in call(name, path, "org.a11y.atspi.Accessible", "GetChildren")]
    except dbus.DBusException:
        return []


def role(name, path):
    try:
        return int(call(name, path, "org.a11y.atspi.Accessible", "GetRole"))
    except dbus.DBusException:
        return -1


def find_terminal(secs=30):
    end = time.time() + secs
    while time.time() < end:
        for app, p in children("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root"):
            stack = [(app, p, 0)]
            seen = 0
            while stack and seen < 400:
                n, q, d = stack.pop()
                seen += 1
                if role(n, q) == 60:
                    return n, q
                if d < 8:
                    stack += [(a, b, d + 1) for a, b in children(n, q)]
        time.sleep(0.5)
    return None


found = find_terminal()
if not found:
    teardown()
    sys.exit("no TERMINAL accessible found (see the .server.log and .client.log next to the output)")
TN, TP = found
TEXT = "org.a11y.atspi.Text"
PROPS = "org.freedesktop.DBus.Properties"
emit({"ev": "found", "bus_name": TN, "path": TP})

# ---- events -------------------------------------------------------------------------------------------
reg = dbus.Interface(a11y.get_object("org.a11y.atspi.Registry", "/org/a11y/atspi/registry"),
                     "org.a11y.atspi.Registry")
for e in ("Object:TextChanged:", "Object:TextCaretMoved:", "Object:TextSelectionChanged:",
          "Object:StateChanged:", "Window:Activate:"):
    reg.RegisterEvent(e, dbus.Array([], signature="s"), "")
a11y.add_match_string("type='signal',interface='org.a11y.atspi.Event.Object'")
a11y.add_match_string("type='signal',interface='org.a11y.atspi.Event.Window'")
counts = {"text-changed": 0, "caret": 0, "bytes": 0}
RECORD = {"on": True, "payload_max": int(os.environ.get("GORCA_VTE_PAYLOAD_MAX", "4000"))}


def on_msg(bus, msg):
    if msg.get_path() != TP:
        return
    iface, member = msg.get_interface() or "", msg.get_member() or ""
    if not iface.startswith("org.a11y.atspi.Event."):
        return
    a = msg.get_args_list()
    if member == "TextChanged" and len(a) >= 4:
        text = a[3]
        text = str(text) if not isinstance(text, (dbus.Int32, dbus.Struct)) else repr(text)
        counts["text-changed"] += 1
        counts["bytes"] += len(text)
        if RECORD.get("until") is not None and counts["text-changed"] > RECORD["until"]:
            RECORD["on"] = False
            RECORD["until"] = None
        if RECORD["on"]:
            emit({"ev": "text-changed", "kind": str(a[0]), "off": int(a[1]), "len": int(a[2]),
                  "text": text[:RECORD["payload_max"]], "text_chars": len(text)})
    elif member == "TextCaretMoved" and len(a) >= 2:
        counts["caret"] += 1
        if RECORD["on"]:
            emit({"ev": "caret", "off": int(a[1])})
    elif not RECORD["on"]:
        return
    elif member == "SelectionChanged" or member == "TextSelectionChanged":
        emit({"ev": "selection"})
    elif member == "StateChanged":
        emit({"ev": "state", "state": str(a[0]), "value": int(a[1])})


a11y.add_message_filter(on_msg)
ctx = GLib.MainContext.default()


def pump(secs):
    end = time.time() + secs
    while time.time() < end:
        while ctx.pending():
            ctx.iteration(False)
        time.sleep(0.002)


def snapshot(name):
    t0 = time.monotonic()
    text = str(call(TN, TP, PROPS, "Get", "ss", (TEXT, "CharacterCount")) and "")
    try:
        text = str(call(TN, TP, TEXT, "GetText", "ii", (0, -1)))
        caret = int(call(TN, TP, PROPS, "Get", "ss", (TEXT, "CaretOffset")))
        count = int(call(TN, TP, PROPS, "Get", "ss", (TEXT, "CharacterCount")))
    except dbus.DBusException as e:
        emit({"ev": "snapshot", "name": name, "error": str(e)})
        return None
    emit({"ev": "snapshot", "name": name, "text": text, "caret": caret, "count": count, "chars": len(text),
          "rtt_ms": round((time.monotonic() - t0) * 1000, 2)})
    return text


def marker(name):
    emit({"ev": "marker", "name": name})


def press(name, evdev, gap=0.06):
    emit({"ev": "key", "name": name, "evdev": evdev})
    rd_send("key " + " ".join(str(c) for c in evdev))
    pump(gap)


def typ(s, gap=0.08):
    for c in s:
        if c == "|":
            press("|", SPECIAL["pipe"], gap)
        elif c in KEYS and KEYS[c]:
            press(c, [KEYS[c]], gap)
        else:
            raise ValueError("no evdev code for %r" % c)


pump(1.5)
snapshot("idle")
rd_send("click")
pump(1.0)
press("home", SPECIAL["home"], 0.5)   # the first key after the click can be lost: spend it here
snapshot("focused")

marker("type echo hi")
typ("echo hi")
pump(0.5)
snapshot("typed")
marker("backspace x2")
press("backspace", SPECIAL["backspace"], 0.15)
press("backspace", SPECIAL["backspace"], 0.15)
pump(0.4)
snapshot("after backspace")
marker("left x2, home, end")
press("left", SPECIAL["left"], 0.15)
press("left", SPECIAL["left"], 0.15)
press("home", SPECIAL["home"], 0.2)
press("end", SPECIAL["end"], 0.2)
pump(0.4)
snapshot("after motions")
marker("enter")
press("enter", SPECIAL["enter"], 0.6)
snapshot("after enter")
marker("ec tab")
typ("ec")
press("tab", SPECIAL["tab"], 0.3)
pump(0.5)
snapshot("after tab")
marker("ctrl-u")
press("ctrl-u", SPECIAL["ctrl-u"], 0.4)
snapshot("after kill")
marker("history up")
press("up", SPECIAL["up"], 0.5)
snapshot("after up")
press("down", SPECIAL["down"], 0.5)
snapshot("after down")
marker("seq 1 40")
typ("seq 1 40")
press("enter", SPECIAL["enter"], 1.0)
snapshot("after seq")

# floods: events per second and payload per second, as a screen reader would receive them
for name, cmd, secs in (("seq flood", "seq 1 100000000", 6), ("yes flood", "yes", 4)):
    marker(name)
    counts.update({"text-changed": 0, "caret": 0, "bytes": 0})
    typ(cmd)
    # The first 24 events of the flood are recorded in full (their shapes), the rest only counted.
    RECORD["on"] = True
    RECORD["until"] = 24
    press("enter", SPECIAL["enter"], 0.05)
    t0 = time.monotonic()
    # sample the server's CPU and count events without recording each payload
    cpu0 = sum(int(x) for x in open("/proc/%d/stat" % server.pid).read().split(")")[1].split()[11:13])
    pump(secs)
    cpu1 = sum(int(x) for x in open("/proc/%d/stat" % server.pid).read().split(")")[1].split()[11:13])
    dt = time.monotonic() - t0
    emit({"ev": "flood", "name": name, "secs": round(dt, 2), "text_changed": counts["text-changed"],
          "caret": counts["caret"], "payload_bytes": counts["bytes"],
          "events_per_s": round((counts["text-changed"] + counts["caret"]) / dt, 1),
          "payload_mb_per_s": round(counts["bytes"] / dt / 1e6, 3),
          "server_cpu_pct": round((cpu1 - cpu0) / os.sysconf("SC_CLK_TCK") / dt * 100, 1)})
    RECORD["on"] = True
    RECORD["until"] = None
    if name == "yes flood":
        marker("ctrl-c during yes")
        press("ctrl-c", SPECIAL["ctrl-c"], 0.6)
    else:
        press("ctrl-c", SPECIAL["ctrl-c"], 0.6)
    pump(1.0)
    snapshot("after " + name)
    press("ctrl-l", SPECIAL["ctrl-l"], 0.6)
    snapshot("after clear")

marker("done")
pump(0.3)
rd.stdin.close()
teardown()
