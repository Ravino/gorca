#!/usr/bin/python3
"""probe_at.py - how does Chromium find out that an assistive technology runs? (M9, private session)

Run inside the private worlds of tests/web/m7/run_m7.sh --probe (compositor + harness a11y bus):

  GORCA_HARNESS_SCREEN_READER=0 tests/web/m7/run_m7.sh --probe \
      /usr/bin/python3 tests/web/m9/probe_at.py <mode> <page.html> [title]

  mode = isenabled   nothing but org.a11y.Status IsEnabled (what gorca writes; run the probe with
                     GORCA_HARNESS_SCREEN_READER=0 so ScreenReaderEnabled stays false too)
       | force       isenabled + --force-renderer-accessibility
       | poke        isenabled + one GetAttributes/GetRelationSet call on the application node
       | orca        poke + a decoy process whose argv[0] is "orca" (Chromium's /proc scan,
                     content/browser/accessibility/browser_accessibility_state_impl_auralinux.cc)
       | none        no a11y at all (a11y bus IsEnabled is forced false first): control

It reads the tree raw over D-Bus (no libatspi) and prints one JSON object: whether the application
registered, how long until the document appeared, node counts by role, whether static text nodes
exist, whether the Text interface answers line boundaries, and the interface set of the document.
The browser is this script's own child in its own process group; its profile is deleted at exit.
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
TEXT = "org.a11y.atspi.Text"
REG_ROOT = "/org/a11y/atspi/accessible/root"
DOC_WEB = 95
STATIC = 61  # ATSPI_ROLE_STATIC? (checked below by name, the number is only a hint)

mode = sys.argv[1]
page = os.path.abspath(sys.argv[2])
title = sys.argv[3] if len(sys.argv) > 3 else ""
addr = os.environ["AT_SPI_BUS_ADDRESS"]
if "/gorca-h." not in addr:
    sys.exit("refusing: not a harness bus")
wl = os.environ["GORCA_M7_WAYLAND"]
chrome = os.environ.get("GORCA_M7_CHROMIUM") or shutil.which("chromium") or "chromium"

conn = Gio.DBusConnection.new_for_address_sync(
    addr,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)
sess = Gio.DBusConnection.new_for_address_sync(
    os.environ["DBUS_SESSION_BUS_ADDRESS"],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)


def call(bus, path, iface, member, args=None, sig=None, timeout=3000):
    v = GLib.Variant(sig, args) if sig else None
    r = conn.call_sync(bus, path, iface, member, v, None, Gio.DBusCallFlags.NONE, timeout, None)
    return r.unpack() if r is not None else None


def prop(bus, path, iface, name):
    return call(bus, path, "org.freedesktop.DBus.Properties", "Get", (iface, name), "(ss)")[0]


def safe(f, default=None):
    try:
        return f()
    except GLib.Error:
        return default


def set_is_enabled(v):
    sess.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Set",
                   GLib.Variant("(ssv)", ("org.a11y.Status", "IsEnabled", GLib.Variant("b", v))),
                   None, Gio.DBusCallFlags.NONE, 3000, None)


status = {
    "is_enabled": sess.call_sync("org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties",
                                 "Get", GLib.Variant("(ss)", ("org.a11y.Status", "IsEnabled")),
                                 None, Gio.DBusCallFlags.NONE, 3000, None).unpack()[0],
    "screen_reader_enabled": sess.call_sync("org.a11y.Bus", "/org/a11y/bus",
                                            "org.freedesktop.DBus.Properties", "Get",
                                            GLib.Variant("(ss)", ("org.a11y.Status", "ScreenReaderEnabled")),
                                            None, Gio.DBusCallFlags.NONE, 3000, None).unpack()[0],
}
if mode == "none":
    set_is_enabled(False)

profile = tempfile.mkdtemp(prefix="m9probe-", dir=os.environ.get("GORCA_HARNESS_DIR"))
args = [chrome, f"--user-data-dir={profile}", "--no-first-run", "--no-default-browser-check",
        "--ozone-platform=wayland", "--window-size=1200,800", "--password-store=basic",
        "--disable-sync", "--disable-background-networking", "--disable-component-update",
        "--no-proxy-server"]
if mode == "force":
    args.append("--force-renderer-accessibility")
args.append("file://" + page)
env = dict(os.environ, WAYLAND_DISPLAY=wl)
env.pop("DISPLAY", None)
log = open(os.path.join(profile, "..", "chrome-probe-%s.log" % mode), "w")
procs = []
if mode == "orca":
    procs.append(subprocess.Popen(["/bin/bash", "-c", "exec -a orca sleep 600"], start_new_session=True))
    time.sleep(0.3)
t0 = time.monotonic()
br = subprocess.Popen(args, env=env, stdout=log, stderr=log, start_new_session=True)
procs.append(br)


def role_of(b, p):
    return safe(lambda: call(b, p, ACC, "GetRole")[0], -1)


def name_of(b, p):
    return safe(lambda: prop(b, p, ACC, "Name"), "")


def kids(b, p):
    return safe(lambda: call(b, p, ACC, "GetChildren")[0], [])


def role_name(b, p):
    return safe(lambda: call(b, p, ACC, "GetRoleName")[0], "?")


def find_app():
    for (b, p) in safe(lambda: call("org.a11y.atspi.Registry", REG_ROOT, ACC, "GetChildren")[0], []):
        n = name_of(b, p).lower()
        if "chrom" in n:
            return b, p, name_of(b, p)
    return None


res = {"mode": mode, "status_before": status, "chrome": chrome}
try:
    app = None
    end = t0 + 45
    while time.monotonic() < end and not app:
        app = find_app()
        time.sleep(0.2)
    if not app:
        res["app_registered"] = False
    else:
        res["app_registered"] = True
        res["t_app_s"] = round(time.monotonic() - t0, 2)
        bus, root, appname = app
        if mode in ("poke", "orca"):
            # What Orca does first: Chromium's ATK GetAttributes / ref_relation_set hooks enable
            # kAXModeBasic|kExtendedProperties and start the /proc scan for "orca"
            # (ax_platform_node_auralinux.cc AtkGetAttributes).
            safe(lambda: call(bus, root, ACC, "GetAttributes"))
            safe(lambda: call(bus, root, ACC, "GetRelationSet"))
            res["poked"] = True
        res["app_name"] = appname
        res["toolkit"] = safe(lambda: prop(bus, root, "org.a11y.atspi.Application", "ToolkitName"), "?")
        # find the document
        doc = None
        end = time.monotonic() + 40
        queue_limit = 6000
        while time.monotonic() < end and not doc:
            queue = [root]
            seen = 0
            while queue and seen < queue_limit:
                p = queue.pop(0)
                seen += 1
                if role_of(bus, p) == DOC_WEB and (not title or title in name_of(bus, p)):
                    doc = p
                    break
                queue.extend(c for (b2, c) in kids(bus, p) if b2 == bus)
            if not doc:
                time.sleep(0.3)
        if not doc:
            res["document"] = False
            counts = {}
            queue = [root]
            seen = 0
            while queue and seen < 3000:
                p = queue.pop(0)
                seen += 1
                rn = role_name(bus, p)
                counts[rn] = counts.get(rn, 0) + 1
                queue.extend(c for (b2, c) in kids(bus, p) if b2 == bus)
            res["app_nodes"] = seen
            res["app_roles"] = dict(sorted(counts.items(), key=lambda kv: -kv[1])[:14])
        else:
            res["document"] = True
            res["t_doc_s"] = round(time.monotonic() - t0, 2)
            res["doc_ifaces"] = safe(lambda: call(bus, doc, ACC, "GetInterfaces")[0], [])
            time.sleep(float(os.environ.get("M9_WAIT", "0")))
            # walk the document
            counts = {}
            n = 0
            static_nodes = 0
            queue = [doc]
            sample_text = None
            t1 = time.monotonic()
            while queue:
                p = queue.pop(0)
                n += 1
                rn = role_name(bus, p)
                counts[rn] = counts.get(rn, 0) + 1
                ifs = safe(lambda: call(bus, p, ACC, "GetInterfaces")[0], [])
                if rn in ("static", "text") or rn.startswith("static"):
                    static_nodes += 1
                if "org.a11y.atspi.Text" in ifs and sample_text is None and rn in ("paragraph", "heading"):
                    sample_text = (p, rn)
                queue.extend(c for (b2, c) in kids(bus, p) if b2 == bus)
            res["walk_s"] = round(time.monotonic() - t1, 3)
            res["nodes"] = n
            res["roles"] = dict(sorted(counts.items(), key=lambda kv: -kv[1])[:14])
            if sample_text:
                p, rn = sample_text
                cc = safe(lambda: prop(bus, p, TEXT, "CharacterCount"), -1)
                res["sample"] = {"role": rn, "char_count": cc}
                for nm, bt in (("line", 3), ("word", 1), ("sentence", 2)):
                    r = safe(lambda: call(bus, p, TEXT, "GetTextAtOffset", (0, bt), "(iu)"), None)
                    res["sample"][nm] = r if r is None else [r[0][:40], r[1], r[2]]
                attrs = safe(lambda: call(bus, p, ACC, "GetAttributes")[0], {})
                res["sample"]["attrs"] = attrs
            # name, text and children of every non-static node of the first 12 (what the mirror reads)
            dump = []
            queue = [doc]
            while queue and len(dump) < int(os.environ.get("M9_DUMP", "24")):
                p = queue.pop(0)
                rn = role_name(bus, p)
                ifs = safe(lambda: call(bus, p, ACC, "GetInterfaces")[0], [])
                cc = safe(lambda: prop(bus, p, TEXT, "CharacterCount"), None) if TEXT in ifs else None
                txt = safe(lambda: call(bus, p, TEXT, "GetText", (0, min(cc, 60)), "(ii)")[0], None) if cc else None
                dump.append({"role": rn, "name": name_of(bus, p)[:40], "text": txt, "chars": cc,
                             "kids": len(kids(bus, p))})
                queue.extend(c for (b2, c) in kids(bus, p) if b2 == bus)
            res["dump"] = dump
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
print(json.dumps(res, indent=1, default=str))
