#!/usr/bin/python3
"""probe_composer.py - raw AT-SPI events for keys typed into a re-rendering contenteditable composer (private session)

  tests/web/m9/run_m9.sh probe-composer <page> <editor-name> [keyspec...]   (backspace=14)
  keyspec: evdev codes separated by commas, one key press each (t=20 space=57 home=102 left=105
           backspace=14 o=24); default: 20,24,57,102,20,105,14

Launches Chromium on tests/web/m3b/pages/fields.html, focuses the text input labelled
`input-name` (default "Typing") through Component.GrabFocus, types the keys through the private
compositor's virtual keyboard (tests/web/m7/rd.py) and prints, per key, every Object event the
page sent for that entry: member:detail(d1,d2) plus the text of a text-changed. No gorca
involved: this is the raw event shape the reader's typing echo has to interpret.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

ACC = "org.a11y.atspi.Accessible"
COMP = "org.a11y.atspi.Component"
here = os.path.dirname(os.path.abspath(__file__))
page = os.path.abspath(sys.argv[1])
want = sys.argv[2]
keys = [int(k) for k in (sys.argv[3] if len(sys.argv) > 3 else "35,18,38,38,24,14,14").split(",")]
addr = os.environ["AT_SPI_BUS_ADDRESS"]
if "/gorca-h." not in addr:
    sys.exit("refusing: not a harness bus")
chrome = os.environ.get("GORCA_M7_CHROMIUM", "chromium")
mode = os.environ.get("GORCA_M9_AT", "poke")
conn = Gio.DBusConnection.new_for_address_sync(
    addr,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)
ev = []


def cb(c, sender, path, iface, member, params, ud):
    if not iface.startswith("org.a11y.atspi.Event"):
        return
    try:
        det, d1, d2, any_, _ = params.unpack()
    except Exception:
        return
    if isinstance(any_, tuple):
        any_ = "<obj>"
    ev.append((time.monotonic(), sender, path, iface.split(".")[-1] + "." + member, det, d1, d2, any_))


conn.signal_subscribe(None, None, None, None, None, Gio.DBusSignalFlags.NONE, cb, None)
for e in ("object:", "window:", "document:", "focus:"):
    try:
        conn.call_sync("org.a11y.atspi.Registry", "/org/a11y/atspi/registry", "org.a11y.atspi.Registry",
                       "RegisterEvent", GLib.Variant("(sas)", (e, [])), None, Gio.DBusCallFlags.NONE, 3000, None)
    except GLib.Error:
        pass


def call(bus, path, iface, member, args=None, sig=None):
    v = GLib.Variant(sig, args) if sig else None
    r = conn.call_sync(bus, path, iface, member, v, None, Gio.DBusCallFlags.NONE, 5000, None)
    return r.unpack() if r is not None else None


def safe(f, d=None):
    try:
        return f()
    except GLib.Error:
        return d


def prop(b, p, i, n):
    return call(b, p, "org.freedesktop.DBus.Properties", "Get", (i, n), "(ss)")[0]


def pump(sec):
    end = time.monotonic() + sec
    ctx = GLib.MainContext.default()
    while time.monotonic() < end:
        while ctx.iteration(False):
            pass
        time.sleep(0.003)


profile = tempfile.mkdtemp(prefix="m9typ-", dir=os.environ.get("GORCA_HARNESS_DIR"))
env = dict(os.environ, WAYLAND_DISPLAY=os.environ["GORCA_M7_WAYLAND"])
env.pop("DISPLAY", None)
args = [chrome, f"--user-data-dir={profile}", "--no-first-run", "--no-default-browser-check",
        "--ozone-platform=wayland", "--window-size=1200,800", "--password-store=basic",
        "--disable-sync", "--disable-background-networking", "--no-proxy-server"]
if mode == "force":
    args.append("--force-renderer-accessibility")
args.append("file://" + os.path.abspath(page))
rd = subprocess.Popen(["/usr/bin/python3", os.path.join(here, "..", "m7", "rd.py")], stdin=subprocess.PIPE,
                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=os.environ)
br = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
out = {"keys": []}
try:
    app = None
    end = time.monotonic() + 40
    while time.monotonic() < end and not app:
        for (b, p) in safe(lambda: call("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root", ACC, "GetChildren")[0], []):
            if "chrom" in (safe(lambda: prop(b, p, ACC, "Name"), "") or "").lower():
                app = (b, p)
        pump(0.2)
    bus, root = app
    if mode in ("poke", "orca"):
        safe(lambda: call(bus, root, ACC, "GetAttributes"))
        safe(lambda: call(bus, root, ACC, "GetRelationSet"))
    ent = None
    end = time.monotonic() + 40
    while time.monotonic() < end and not ent:
        queue = [root]
        n = 0
        while queue and n < 3000 and not ent:
            p = queue.pop(0)
            n += 1
            if safe(lambda: call(bus, p, ACC, "GetRole")[0], -1) in (79, 85, 40) and (safe(lambda: prop(bus, p, ACC, "Name"), "") == want):
                ent = p
                break
            queue.extend(c for (b2, c) in safe(lambda: call(bus, p, ACC, "GetChildren")[0], []) if b2 == bus)
        pump(0.3)
    out["entry"] = ent
    rd.stdin.write(b"click\n")
    rd.stdin.flush()
    pump(0.5)
    safe(lambda: call(bus, ent, COMP, "GrabFocus"))
    pump(1.0)
    ev.clear()
    for k in keys:
        t0 = time.monotonic()
        rd.stdin.write(("key %d\n" % k).encode())
        rd.stdin.flush()
        pump(0.45)
        row = []
        for (t, s, path, name, det, d1, d2, any_) in ev:
            if t < t0:
                continue
            if name.startswith("Object.") or name.startswith("Focus") or name.startswith("Window"):
                a = repr(any_)[:40] if name.startswith("Object.text-changed") or name.endswith("TextChanged") else ""
                row.append("+%dms %s %s:%s(%s,%s)%s" % ((t - t0) * 1000, path.rsplit("/", 1)[-1], name.split(".")[-1], det, d1, d2, (" " + a) if a else ""))
        out["keys"].append({"code": k, "events": row})
        ev.clear()
finally:
    try:
        rd.stdin.close()
    except Exception:
        pass
    try:
        os.killpg(br.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    time.sleep(1.5)
    try:
        os.killpg(br.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    br.wait()
    rd.wait(timeout=5)
    shutil.rmtree(profile, ignore_errors=True)
print(json.dumps(out, indent=1))
