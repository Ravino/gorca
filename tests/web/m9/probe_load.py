#!/usr/bin/python3
"""probe_load.py - Chromium tree arrival and event rate while a page loads (M9, private session)

  tests/web/m9/run_m9.sh probe-load <page.html|URL> [title-fragment]   (M9_AT as in run_m9.sh)

Launches its own Chromium (throw-away profile, own process group), registers for the AT-SPI event
classes gorca listens to, and reports as one JSON line:
  t_app_s / t_doc_s     browser start to the application / the document appearing on the bus
  events                counts by "iface.member" from the browser start to quiet (no event for 1.5 s)
  peak_events_per_s     busiest 100 ms bin, scaled to events/s
  t_quiet_s             time of the last event
  getree                {nodes, ms, bytes} of one Collection.GetTree on the document (the gorca fill call)
  getree_ms_second      the same call again (steady state)
  doc_child_count       children of the document node
The browser is killed through its own process group and its profile deleted at exit.
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
REG_ROOT = "/org/a11y/atspi/accessible/root"
DOC_WEB = 95

target = sys.argv[1]
frag = sys.argv[2] if len(sys.argv) > 2 else ""
addr = os.environ["AT_SPI_BUS_ADDRESS"]
if "/gorca-h." not in addr:
    sys.exit("refusing: not a harness bus")
url = target if "://" in target else "file://" + os.path.abspath(target)
chrome = os.environ.get("GORCA_M7_CHROMIUM") or "chromium"
mode = os.environ.get("GORCA_M9_AT", "poke")
conn = Gio.DBusConnection.new_for_address_sync(
    addr,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)
events = []  # (t, "iface.member")


def cb(c, sender, path, iface, member, params, ud):
    if iface.startswith("org.a11y.atspi.Event"):
        det = params.unpack()[0] if params.n_children() else ""
        events.append((time.monotonic(), "%s.%s:%s" % (iface.split(".")[-1], member, det)))


conn.signal_subscribe(None, None, None, None, None, Gio.DBusSignalFlags.NONE, cb, None)
for ev in ("object:", "window:", "document:", "focus:"):
    try:
        conn.call_sync("org.a11y.atspi.Registry", "/org/a11y/atspi/registry",
                       "org.a11y.atspi.Registry", "RegisterEvent",
                       GLib.Variant("(sas)", (ev, [])), None, Gio.DBusCallFlags.NONE, 3000, None)
    except GLib.Error as e:
        print("register", ev, e, file=sys.stderr)


def call(bus, path, iface, member, args=None, sig=None, timeout=20000):
    v = GLib.Variant(sig, args) if sig else None
    r = conn.call_sync(bus, path, iface, member, v, None, Gio.DBusCallFlags.NONE, timeout, None)
    return r.unpack() if r is not None else None


def safe(f, d=None):
    try:
        return f()
    except GLib.Error:
        return d


def pump(sec):
    end = time.monotonic() + sec
    ctx = GLib.MainContext.default()
    while time.monotonic() < end:
        while ctx.iteration(False):
            pass
        time.sleep(0.005)


profile = tempfile.mkdtemp(prefix="m9load-", dir=os.environ.get("GORCA_HARNESS_DIR"))
args = [chrome, f"--user-data-dir={profile}", "--no-first-run", "--no-default-browser-check",
        "--ozone-platform=wayland", "--window-size=1200,800", "--password-store=basic",
        "--disable-sync", "--disable-background-networking", "--disable-component-update",
        "--no-proxy-server"]
if mode == "force":
    args.append("--force-renderer-accessibility")
args.append(url)
env = dict(os.environ, WAYLAND_DISPLAY=os.environ["GORCA_M7_WAYLAND"])
env.pop("DISPLAY", None)
procs = []
if mode == "orca":
    procs.append(subprocess.Popen(["/bin/bash", "-c", "exec -a orca sleep 600"], start_new_session=True))
    time.sleep(0.3)
res = {"url": url, "mode": mode}
t0 = time.monotonic()
br = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                      start_new_session=True)
procs.append(br)
try:
    app = None
    while time.monotonic() < t0 + 60 and not app:
        for (b, p) in safe(lambda: call("org.a11y.atspi.Registry", REG_ROOT, ACC, "GetChildren")[0], []):
            if "chrom" in (safe(lambda: call(b, p, "org.freedesktop.DBus.Properties", "Get",
                                              (ACC, "Name"), "(ss)")[0], "") or "").lower():
                app = (b, p)
        pump(0.1)
    if not app:
        res["error"] = "no application"
    else:
        bus, root = app
        res["t_app_s"] = round(time.monotonic() - t0, 2)
        if mode in ("poke", "orca"):
            safe(lambda: call(bus, root, ACC, "GetAttributes"))
            safe(lambda: call(bus, root, ACC, "GetRelationSet"))
        doc = None
        end = time.monotonic() + 120
        while time.monotonic() < end and not doc:
            queue = [root]
            seen = 0
            while queue and seen < 400 and not doc:
                p = queue.pop(0)
                seen += 1
                if safe(lambda: call(bus, p, ACC, "GetRole")[0], -1) == DOC_WEB and \
                        frag in (safe(lambda: call(bus, p, "org.freedesktop.DBus.Properties", "Get",
                                                   (ACC, "Name"), "(ss)")[0], "") or ""):
                    doc = p
                    break
                queue.extend(c for (b2, c) in safe(lambda: call(bus, p, ACC, "GetChildren")[0], []) if b2 == bus)
            pump(0.2)
        if not doc:
            res["error"] = "no document"
        else:
            res["t_doc_s"] = round(time.monotonic() - t0, 2)
            res["doc_child_count"] = safe(lambda: call(bus, doc, ACC, "GetChildren")[0] and len(call(bus, doc, ACC, "GetChildren")[0]), -1)
            # quiet = no event for 1.5 s
            while True:
                pump(0.25)
                last = events[-1][0] if events else t0
                if time.monotonic() - last > 1.5 or time.monotonic() - t0 > 240:
                    break
            counts = {}
            for t, k in events:
                key = k.split(":")[0]
                counts[key] = counts.get(key, 0) + 1
            res["events"] = dict(sorted(counts.items(), key=lambda kv: -kv[1])[:16])
            res["events_total"] = len(events)
            bins = {}
            for t, k in events:
                bins[int((t - t0) * 10)] = bins.get(int((t - t0) * 10), 0) + 1
            res["peak_events_per_s"] = max(bins.values()) * 10 if bins else 0
            res["t_last_event_s"] = round(events[-1][0] - t0, 2) if events else None
            body = (([], 0, {}, 0, [], 0, [], 0, False), ["Name", "ChildCount", "Attributes"])
            for nm in ("getree", "getree_second"):
                t1 = time.monotonic()
                try:
                    r = conn.call_sync(bus, doc, "org.a11y.atspi.Collection", "GetTree",
                                       GLib.Variant("((aiia{ss}iaiiasib)as)", body), None,
                                       Gio.DBusCallFlags.NONE, 120000, None)
                    n = r.get_child_value(0).n_children()
                    res[nm] = {"nodes": n, "ms": round((time.monotonic() - t1) * 1000, 1)}
                except GLib.Error as e:
                    res[nm] = {"error": str(e)[:120]}
finally:
    for pr in procs:
        try:
            os.killpg(pr.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    time.sleep(1.5)
    for pr in procs:
        try:
            os.killpg(pr.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        pr.wait()
    shutil.rmtree(profile, ignore_errors=True)
print(json.dumps(res))
