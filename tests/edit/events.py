#!/usr/bin/python3
"""Which AT-SPI events does an editor send for a key sequence? (editing matrix diagnosis)

    tests/edit/run_edit.sh /usr/bin/python3 tests/edit/events.py <app> <keys...>

<app> is gedit, text-editor, or a command line. Each key is a space-free chord of evdev codes joined by
`+` (`29+33` is Ctrl+F) or `wait:<ms>`; text can be typed with `t:<letters>`. Prints one line per
focus, state, caret and selection event (role, name, detail) and the focused object after each key.
Private session only (run_edit.sh); it never touches the live bus.
"""
import os
import subprocess
import sys
import threading
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib  # noqa: E402

app = sys.argv[1]
keys = sys.argv[2:]
cbus = os.environ["GORCA_VTE_CBUS"]
wayland = os.environ["GORCA_VTE_WAYLAND"]
tools = os.environ["GORCA_VTE_TOOLS"]
LETTERS = {c: code for code, c in zip(
    list(range(16, 26)) + list(range(30, 39)) + list(range(44, 51)), "qwertyuiopasdfghjklzxcvbnm")}


def desc(obj):
    try:
        role = obj.get_role_name()
        name = obj.get_name()
        app_name = obj.get_application().get_name() if obj.get_application() else "?"
        return f"{role!r} {name!r} app={app_name}"
    except GLib.Error as e:
        return f"<gone: {e.message}>"


T0 = time.monotonic()


def log(msg):
    print(f"{(time.monotonic() - T0) * 1000:8.0f} ms  {msg}", flush=True)


def on_event(ev):
    if ev.type.startswith("object:state-changed:focused") and ev.detail1 == 0:
        return
    log(f"{ev.type} d1={ev.detail1} d2={ev.detail2} {desc(ev.source)}")


listener = Atspi.EventListener.new(on_event)
for t in ("object:state-changed:focused", "object:state-changed:active", "object:text-caret-moved",
          "object:text-selection-changed", "window:activate", "window:deactivate",
          "object:property-change:accessible-name", "object:children-changed:add"):
    listener.register(t)

env = dict(os.environ, GDK_BACKEND="wayland", WAYLAND_DISPLAY=wayland, GSETTINGS_BACKEND="memory", LANG="C.UTF-8")
env.pop("DISPLAY", None)
rd = subprocess.Popen(["/usr/bin/python3", os.path.join(tools, "../web/m7/rd.py")],
                      env=dict(env, GORCA_M7_CBUS=cbus), stdin=subprocess.PIPE, text=True, start_new_session=True)
time.sleep(1.5)


def rd_line(s):
    rd.stdin.write(s + "\n")
    rd.stdin.flush()


rd_line("key 42")
time.sleep(0.5)
cmd = {"gedit": ["/usr/bin/gedit", "--standalone"],
       "text-editor": ["/usr/bin/gnome-text-editor", "--standalone"]}.get(app, app.split())
proc = subprocess.Popen(cmd, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                        start_new_session=True)
loop = GLib.MainLoop()


def drive():
    time.sleep(5)
    rd_line("click")
    time.sleep(1.2)
    for k in keys:
        log(f"=== key {k}")
        if k.startswith("wait:"):
            time.sleep(int(k[5:]) / 1000)
            continue
        if k.startswith("t:"):
            for c in k[2:]:
                rd_line(f"key {LETTERS[c]}")
                time.sleep(0.07)
        else:
            rd_line("key " + " ".join(k.split("+")))
        time.sleep(0.9)
    log("=== done")
    loop.quit()


threading.Thread(target=drive, daemon=True).start()
try:
    loop.run()
finally:
    rd.stdin.close()
    for p in (proc, rd):
        try:
            os.killpg(p.pid, 15)
        except ProcessLookupError:
            pass
