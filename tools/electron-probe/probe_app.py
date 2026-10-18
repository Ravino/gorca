#!/usr/bin/python3
"""probe_app.py - tree growth timeline of an arbitrary Chromium/Electron app in a PRIVATE harness
session (tests/web/m7/run_m7.sh --probe /usr/bin/python3 THIS name_fragment mode -- cmd args...).

mode: poke (GetAttributes/GetRelationSet on the app root once, as gorca does) | none | force
(adds --force-renderer-accessibility to cmd). Prints JSON lines: timeline of {t, nodes, doc_children,
events} until the tree is stable for 4 s or 90 s pass. Never touches the live session.
"""
import json, os, shutil, signal, subprocess, sys, tempfile, time
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

ACC = "org.a11y.atspi.Accessible"
REG_ROOT = "/org/a11y/atspi/accessible/root"
DOC_WEB = 95
frag, mode = sys.argv[1], sys.argv[2]
cmd = sys.argv[sys.argv.index("--") + 1:]
addr = os.environ["AT_SPI_BUS_ADDRESS"]
if "/gorca-h." not in addr:
    sys.exit("refusing: not a harness bus")
conn = Gio.DBusConnection.new_for_address_sync(
    addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
events = {}
def cb(c, sender, path, iface, member, params, ud):
    if iface.startswith("org.a11y.atspi.Event"):
        det = params.unpack()[0] if params.n_children() else ""
        k = "%s.%s:%s" % (iface.split(".")[-1], member, det)
        events[k] = events.get(k, 0) + 1
conn.signal_subscribe(None, None, None, None, None, Gio.DBusSignalFlags.NONE, cb, None)
for ev in ("object:", "window:", "document:", "focus:"):
    conn.call_sync("org.a11y.atspi.Registry", "/org/a11y/atspi/registry", "org.a11y.atspi.Registry",
                   "RegisterEvent", GLib.Variant("(sas)", (ev, [])), None, Gio.DBusCallFlags.NONE, 3000, None)

def call(bus, path, iface, member, args=None, sig=None, timeout=20000):
    v = GLib.Variant(sig, args) if sig else None
    r = conn.call_sync(bus, path, iface, member, v, None, Gio.DBusCallFlags.NONE, timeout, None)
    return r.unpack() if r is not None else None
def safe(f, d=None):
    try: return f()
    except GLib.Error: return d
def pump(sec):
    end = time.monotonic() + sec
    ctx = GLib.MainContext.default()
    while time.monotonic() < end:
        while ctx.iteration(False): pass
        time.sleep(0.005)

profile = tempfile.mkdtemp(prefix="eprobe-", dir=os.environ.get("GORCA_HARNESS_DIR"))
args = [a.replace("@PROFILE@", profile) for a in cmd]
if mode == "force": args.append("--force-renderer-accessibility")
env = dict(os.environ, WAYLAND_DISPLAY=os.environ["GORCA_M7_WAYLAND"])
env.pop("DISPLAY", None)
t0 = time.monotonic()
br = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
try:
    app = None
    while time.monotonic() < t0 + 60 and not app:
        for (b, p) in safe(lambda: call("org.a11y.atspi.Registry", REG_ROOT, ACC, "GetChildren")[0], []):
            nm = safe(lambda: call(b, p, "org.freedesktop.DBus.Properties", "Get", (ACC, "Name"), "(ss)")[0], "") or ""
            if frag in nm.lower(): app = (b, p)
        pump(0.1)
    if not app:
        print(json.dumps({"error": "no application"})); sys.exit(0)
    bus, root = app
    print(json.dumps({"t_app_s": round(time.monotonic() - t0, 2), "mode": mode}), flush=True)
    if mode == "poke":
        safe(lambda: call(bus, root, ACC, "GetAttributes")); safe(lambda: call(bus, root, ACC, "GetRelationSet"))
    body = (([], 0, {}, 0, [], 0, [], 0, False), ["Name", "ChildCount", "Attributes"])
    stable = 0; last = None; doc = None
    while time.monotonic() < t0 + 90:
        pump(0.5)
        if doc is not None:
            # re-validate: the document may have been replaced
            cur = None
            queue = [root]; seen = 0
            while queue and seen < 600:
                p = queue.pop(0); seen += 1
                if safe(lambda: call(bus, p, ACC, "GetRole")[0], -1) == DOC_WEB: cur = p; break
                queue.extend(c for (b2, c) in safe(lambda: call(bus, p, ACC, "GetChildren")[0], []) if b2 == bus)
            if cur != doc:
                print(json.dumps({"doc_replaced": [doc, cur], "t": round(time.monotonic() - t0, 1)}), flush=True)
                doc = cur
        if doc is None:
            queue = [root]; seen = 0
            while queue and seen < 600 and doc is None:
                p = queue.pop(0); seen += 1
                if safe(lambda: call(bus, p, ACC, "GetRole")[0], -1) == DOC_WEB: doc = p; break
                queue.extend(c for (b2, c) in safe(lambda: call(bus, p, ACC, "GetChildren")[0], []) if b2 == bus)
        n = -1
        if doc:
            r = safe(lambda: conn.call_sync(bus, doc, "org.a11y.atspi.Collection", "GetTree", GLib.Variant("((aiia{ss}iaiiasib)as)", body), None, Gio.DBusCallFlags.NONE, 20000, None))
            n = r.get_child_value(0).n_children() if r else -2
        ev = sum(v for k, v in events.items() if k.startswith("Object.ChildrenChanged"))
        print(json.dumps({"t": round(time.monotonic() - t0, 1), "doc": doc, "nodes": n, "children_changed": ev, "events_total": sum(events.values())}), flush=True)
        if n == last: stable += 1
        else: stable = 0
        last = n
        if stable >= 8 and n > 0: break
    print(json.dumps({"events": dict(sorted(events.items(), key=lambda kv: -kv[1])[:14])}))
finally:
    try: os.killpg(br.pid, signal.SIGTERM)
    except ProcessLookupError: pass
    time.sleep(1.5)
    try: os.killpg(br.pid, signal.SIGKILL)
    except ProcessLookupError: pass
    br.wait()
    shutil.rmtree(profile, ignore_errors=True)
