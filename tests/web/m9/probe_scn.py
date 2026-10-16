#!/usr/bin/python3
"""probe_scn.py - Chromium behaviour probes for M9 (private session only).

  tests/web/m9/run_m9.sh probe-scn anchor <page> <title> <link-name>
  tests/web/m9/run_m9.sh probe-scn frames <page> <title>
  tests/web/m9/run_m9.sh probe-scn click <page> <title> <role-name> <node-name>

Launches its own Chromium (throw-away profile in the harness dir, own process group, deleted at
exit), wakes the tree the way gorca does (GetAttributes + GetRelationSet on the application root),
registers for the event classes gorca listens to and prints one JSON object.

  anchor   DoAction(0) on the named link, then every event of the next 2 s with its source path,
           role, name and (for text-caret-moved) the offset
  frames   every `internal frame` with child count, states and attributes; the whole subtree of
           each frame that has children (depth 4)
  click    DoAction(0) on the first node with that role name and name; the events of the next 2 s,
           the focus after it and the ancestors of the focus
Extra Chromium arguments: M9_ARGS (space separated).
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
ACT = "org.a11y.atspi.Action"
REG_ROOT = "/org/a11y/atspi/accessible/root"
DOC_WEB = 95

kind = sys.argv[1]
page = os.path.abspath(sys.argv[2])
title = sys.argv[3] if len(sys.argv) > 3 else ""
addr = os.environ["AT_SPI_BUS_ADDRESS"]
if "/gorca-h." not in addr:
    sys.exit("refusing: not a harness bus")
chrome = os.environ.get("GORCA_M7_CHROMIUM") or "chromium"
conn = Gio.DBusConnection.new_for_address_sync(
    addr,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)
events = []


def cb(c, sender, path, iface, member, params, ud):
    if iface.startswith("org.a11y.atspi.Event"):
        u = params.unpack()
        events.append((time.monotonic(), "%s.%s" % (iface.split(".")[-1], member), path,
                       u[0] if u else "", u[1] if len(u) > 1 else None))


conn.signal_subscribe(None, None, None, None, None, Gio.DBusSignalFlags.NONE, cb, None)
for ev in ("object:", "window:", "document:", "focus:"):
    try:
        conn.call_sync("org.a11y.atspi.Registry", "/org/a11y/atspi/registry",
                       "org.a11y.atspi.Registry", "RegisterEvent",
                       GLib.Variant("(sas)", (ev, [])), None, Gio.DBusCallFlags.NONE, 3000, None)
    except GLib.Error as e:
        print("register", ev, e, file=sys.stderr)


def call(bus, path, iface, member, args=None, sig=None, timeout=5000):
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


def prop(b, p, i, n):
    return call(b, p, "org.freedesktop.DBus.Properties", "Get", (i, n), "(ss)")[0]


def role(b, p):
    return safe(lambda: call(b, p, ACC, "GetRoleName")[0], "?")


def name(b, p):
    return safe(lambda: prop(b, p, ACC, "Name"), "") or ""


def kids(b, p):
    return safe(lambda: call(b, p, ACC, "GetChildren")[0], [])


def states(b, p):
    s = safe(lambda: call(b, p, ACC, "GetState")[0], [0, 0])
    names = ["invalid", "active", "armed", "busy", "checked", "collapsed", "defunct", "editable",
             "enabled", "expandable", "expanded", "focusable", "focused", "has-tooltip",
             "horizontal", "iconified", "modal", "multi-line", "multiselectable", "opaque",
             "pressed", "resizable", "selectable", "selected", "sensitive", "showing",
             "single-line", "stale", "transient", "vertical", "visible", "manages-descendants",
             "indeterminate", "required", "truncated", "animated", "invalid-entry",
             "supports-autocompletion", "selectable-text", "is-default", "visited"]
    return [names[i] for i in range(min(len(names), 32)) if s[0] >> i & 1]


def find(b, root, rn, nm, limit=4000):
    q = [root]
    n = 0
    while q and n < limit:
        p = q.pop(0)
        n += 1
        if (not rn or role(b, p) == rn) and (not nm or nm in name(b, p)):
            return p
        q.extend(c for (b2, c) in kids(b, p) if b2 == b)
    return None


def dump(b, p, depth, maxd=4):
    ifs = safe(lambda: call(b, p, ACC, "GetInterfaces")[0], [])
    d = {"role": role(b, p), "name": name(b, p)[:40], "path": p.rsplit("/", 1)[-1]}
    if "org.a11y.atspi.Text" in ifs:
        d["text"] = (safe(lambda: call(b, p, "org.a11y.atspi.Text", "GetText", (0, 40), "(ii)")[0], "") or "")[:40]
    if depth < maxd:
        d["kids"] = [dump(b, c, depth + 1, maxd) for (b2, c) in kids(b, p) if b2 == b][:12]
    return d


def describe(b, p):
    return {"path": p, "role": role(b, p), "name": name(b, p)[:50], "states": states(b, p)}


profile = tempfile.mkdtemp(prefix="m9scn-", dir=os.environ.get("GORCA_HARNESS_DIR"))
args = [chrome, f"--user-data-dir={profile}", "--no-first-run", "--no-default-browser-check",
        "--ozone-platform=wayland", "--window-size=1200,800", "--password-store=basic",
        "--disable-sync", "--disable-background-networking", "--disable-component-update",
        "--no-proxy-server"]
# probe options ride on M9_ARGS (the harness passes no other variable through): --m9-scroll,
# --m9-wait=SECONDS; everything else goes to Chromium
OPTS = {}
for a in os.environ.get("GORCA_M9_CHROMIUM_ARGS", "").split():
    if a.startswith("--m9-"):
        k, _, v = a[5:].partition("=")
        OPTS[k] = v or "1"
    else:
        args.append(a)
args.append("file://" + page)
env = dict(os.environ, WAYLAND_DISPLAY=os.environ["GORCA_M7_WAYLAND"])
env.pop("DISPLAY", None)
br = subprocess.Popen(args, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                      start_new_session=True)
res = {"kind": kind}
try:
    t0 = time.monotonic()
    app = None
    while time.monotonic() < t0 + 45 and not app:
        for (b, p) in safe(lambda: call("org.a11y.atspi.Registry", REG_ROOT, ACC, "GetChildren")[0], []):
            if "chrom" in name(b, p).lower():
                app = (b, p)
        pump(0.2)
    if not app:
        sys.exit("no application")
    bus, root = app
    safe(lambda: call(bus, root, ACC, "GetAttributes"))
    safe(lambda: call(bus, root, ACC, "GetRelationSet"))
    doc = None
    end = time.monotonic() + 40
    while time.monotonic() < end and not doc:
        q = [root]
        n = 0
        while q and n < 400 and not doc:
            p = q.pop(0)
            n += 1
            if safe(lambda: call(bus, p, ACC, "GetRole")[0], -1) == DOC_WEB and title in name(bus, p):
                doc = p
            q.extend(c for (b2, c) in kids(bus, p) if b2 == bus)
        pump(0.2)
    if not doc:
        sys.exit("no document")
    pump(float(OPTS.get("wait", "3")))
    res["doc"] = doc
    if kind == "frames":
        frames = []
        q = [doc]
        n = 0
        while q and n < 3000:
            p = q.pop(0)
            n += 1
            if role(bus, p) == "internal frame":
                ks = [c for (b2, c) in kids(bus, p) if b2 == bus]
                other = [(b2, c) for (b2, c) in kids(bus, p) if b2 != bus]
                f = describe(bus, p)
                f["kids"] = len(ks)
                f["foreign_kids"] = other[:2]
                f["attrs"] = safe(lambda: call(bus, p, ACC, "GetAttributes")[0], {})
                if ks:
                    f["tree"] = dump(bus, ks[0], 0, 3)
                frames.append(f)
            q.extend(c for (b2, c) in kids(bus, p) if b2 == bus)
        res["frames"] = frames
        if OPTS.get("scroll"):
            # bring each empty frame into view (Component.ScrollTo ANYWHERE) and look again
            after = {}
            for f in frames:
                if f["kids"] == 0:
                    ok = safe(lambda: call(bus, f["path"], "org.a11y.atspi.Component", "ScrollTo", (6,), "(u)"))
                    pump(2.0)
                    after[f["name"]] = {"scrolled": ok, "kids": len([1 for (b2, c) in kids(bus, f["path"]) if b2 == bus]),
                                        "states": states(bus, f["path"])}
            res["after_scroll"] = after
    else:
        if kind == "anchor":
            target = find(bus, doc, "link", sys.argv[4])
            rn = "link"
        else:
            target = find(bus, doc, sys.argv[4], sys.argv[5])
        if not target:
            sys.exit("target not found")
        res["target"] = describe(bus, target)
        events.clear()
        t1 = time.monotonic()
        res["do_action"] = safe(lambda: call(bus, target, ACT, "DoAction", (0,), "(i)"))
        pump(2.0)
        evs = []
        for (t, k, path, det, a1) in events:
            if k.startswith("Object.PropertyChange") or k.startswith("Object.BoundsChanged"):
                continue
            evs.append({"dt_ms": round((t - t1) * 1000), "ev": k + ":" + str(det), "src": describe(bus, path)
                        if (bus and path.startswith("/org/a11y/atspi/accessible")) else path, "d1": a1})
        res["events"] = evs[:60]
finally:
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
    shutil.rmtree(profile, ignore_errors=True)
print(json.dumps(res, indent=1))
