#!/usr/bin/python3
"""probe_growth.py - CHR-0 deciding experiment: when does a Chromium/Electron document announce its
growth? Like tools/electron-probe/probe_app.py, plus timestamps (seconds since the browser start) of
every document:load-complete, state-changed:busy and children-changed:add, and a per-0.5 s node count.
Summary line: adds before/after the first load-complete, nodes at that moment and at the end.

  tests/web/m7/run_m7.sh --probe /usr/bin/python3 tests/web/chr0/probe_growth.py FRAG poke -- CMD ARGS
  (@PROFILE@ in ARGS is replaced by a throw-away profile directory)

mode: poke (GetAttributes/GetRelationSet on the app root once, as gorca does) | none | force
(adds --force-renderer-accessibility to cmd). Prints JSON lines: timeline of {t, nodes, doc_children,
events} until the tree is stable for 4 s or 90 s pass. Never touches the live session.
"""
import json, os, shutil, signal, subprocess, sys, tempfile, time
# The harness drops custom environment variables: options come from a key=value file in the
# private probe directory (never sourced), PROBE_CLICK, PROBE_CLICK_AT, PROBE_MIN_T.
import pwd
_conf = os.path.join(pwd.getpwuid(os.getuid()).pw_dir, ".cache/electron-probe/probe.conf")
if os.path.isfile(_conf) and os.stat(_conf).st_uid == os.getuid():
    for _l in open(_conf):
        if "=" in _l:
            _k, _v = _l.rstrip("\n").split("=", 1)
            os.environ.setdefault(_k, _v)
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
stamped = []  # (t, key) for the three interesting kinds
T0 = [None]
def cb(c, sender, path, iface, member, params, ud):
    if iface.startswith("org.a11y.atspi.Event"):
        det = params.unpack()[0] if params.n_children() else ""
        k = "%s.%s:%s" % (iface.split(".")[-1], member, det)
        events[k] = events.get(k, 0) + 1
        if T0[0] is not None and (k.startswith("Document.LoadComplete") or k.startswith("Object.StateChanged:busy")
                or k.startswith("Object.ChildrenChanged:add")):
            stamped.append((round(time.monotonic() - T0[0], 2), k))
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
T0[0] = t0
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
    stable = 0; last = None; doc = None; timeline = []; clicked = False
    while time.monotonic() < t0 + 90:
        pump(0.5)
        # PROBE_CLICK="Render app": at PROBE_CLICK_AT seconds press the push button of that name
        # with Action.DoAction(0), as the e2e step does (an AT-driven click, not a gesture).
        if os.environ.get("PROBE_CLICK") and not clicked and time.monotonic() - t0 > float(os.environ.get("PROBE_CLICK_AT", "8")) and doc:
            queue = [doc]; seen = 0
            while queue and seen < 2000 and not clicked:
                p = queue.pop(0); seen += 1
                nm = safe(lambda: call(bus, p, "org.freedesktop.DBus.Properties", "Get", ("org.a11y.atspi.Accessible", "Name"), "(ss)")[0], "") or ""
                if os.environ["PROBE_CLICK"] in nm and safe(lambda: call(bus, p, ACC, "GetRole")[0], -1) == 43:
                    safe(lambda: call(bus, p, "org.a11y.atspi.Action", "DoAction", (0,), "(i)"))
                    clicked = True
                    print(json.dumps({"clicked": nm, "t": round(time.monotonic() - t0, 1)}), flush=True)
                queue.extend(c for (b2, c) in safe(lambda: call(bus, p, ACC, "GetChildren")[0], []) if b2 == bus)
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
        timeline.append((round(time.monotonic() - t0, 2), n))
        if n == last: stable += 1
        else: stable = 0
        last = n
        if stable >= 8 and n > 0 and time.monotonic() - t0 > float(os.environ.get('PROBE_MIN_T', '35')): break
    print(json.dumps({"events": dict(sorted(events.items(), key=lambda kv: -kv[1])[:14])}))
    lc = [t for t, k in stamped if k.startswith("Document.LoadComplete")]
    t_lc = lc[0] if lc else None
    adds = [t for t, k in stamped if k.startswith("Object.ChildrenChanged:add")]
    n_at_lc = None
    if t_lc is not None:
        before = [n for t, n in timeline if t <= t_lc]
        n_at_lc = before[-1] if before else None
    print(json.dumps({"summary": {
        "load_complete_at": lc, "busy_events": [x for x in stamped if "busy" in x[1]],
        "adds_total": len(adds), "adds_before_first_load_complete": len([t for t in adds if t_lc is None or t <= t_lc]),
        "adds_after_first_load_complete": len([t for t in adds if t_lc is not None and t > t_lc]),
        "nodes_at_load_complete": n_at_lc, "nodes_final": last, "first_adds": adds[:6]}}))
finally:
    try: os.killpg(br.pid, signal.SIGTERM)
    except ProcessLookupError: pass
    time.sleep(1.5)
    try: os.killpg(br.pid, signal.SIGKILL)
    except ProcessLookupError: pass
    br.wait()
    shutil.rmtree(profile, ignore_errors=True)
