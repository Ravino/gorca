#!/usr/bin/python3
"""dump_tree.py - what does the browser expose for a page? (M8b probe, private session only)

  tests/web/m8/run_m8.sh --probe /usr/bin/python3 tests/web/m8b/dump_tree.py <page.html> <title> [firefox|chromium]

Starts its own browser (own process group, throw-away profile deleted at exit), waits for the
document, then prints one line per node of the document: depth, role name, name, description,
state names, object attributes and relations (target role + name). Raw D-Bus, no libatspi.
"""
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
STATE_NAMES = {1: "active", 3: "busy", 4: "checked", 5: "collapsed", 7: "editable", 8: "enabled",
               9: "expandable", 10: "expanded", 11: "focusable", 12: "focused", 16: "modal",
               17: "multi-line", 18: "multiselectable", 20: "pressed", 22: "selectable",
               23: "selected", 24: "sensitive", 25: "showing", 30: "visible", 33: "required",
               36: "invalid-entry", 32: "indeterminate", 31: "manages-desc", 40: "visited", 41: "checkable",
               42: "has-popup", 43: "read-only", 6: "defunct", 26: "single-line"}
REL = {1: "label-for", 2: "labelled-by", 3: "controller-for", 4: "controlled-by", 5: "member-of",
       6: "tooltip-for", 7: "node-child-of", 8: "node-parent-of", 9: "extended", 10: "flows-to",
       11: "flows-from", 12: "subwindow-of", 13: "embeds", 14: "embedded-by", 15: "popup-for",
       16: "parent-window-of", 17: "description-for", 18: "described-by", 19: "details",
       20: "details-for", 21: "error-message", 22: "error-for"}

page = os.path.abspath(sys.argv[1])
title = sys.argv[2]
engine = sys.argv[3] if len(sys.argv) > 3 else os.environ.get("GORCA_M7_BROWSER", "firefox")
addr = os.environ["AT_SPI_BUS_ADDRESS"]
if "/gorca-h." not in addr:
    sys.exit("refusing: not a harness bus")
wl = os.environ["GORCA_M7_WAYLAND"]
conn = Gio.DBusConnection.new_for_address_sync(
    addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)


def call(bus, path, iface, member, args=None, sig=None, timeout=4000):
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


profile = tempfile.mkdtemp(prefix="m8bdump-", dir=os.environ.get("GORCA_HARNESS_DIR"))
env = dict(os.environ, WAYLAND_DISPLAY=wl, MOZ_ENABLE_WAYLAND="1", GNOME_ACCESSIBILITY="1")
env.pop("DISPLAY", None)
if engine == "chromium":
    binp = os.environ.get("GORCA_M7_CHROMIUM") or shutil.which("chromium") or "chromium"
    args = [binp, f"--user-data-dir={profile}", "--no-first-run", "--no-default-browser-check",
            "--ozone-platform=wayland", "--window-size=1200,800", "--password-store=basic",
            "--disable-sync", "--disable-background-networking", "--disable-component-update",
            "--no-proxy-server", "file://" + page]
    words = ("chrom",)
else:
    binp = os.environ.get("GORCA_M7_FIREFOX", "firefox")
    with open(os.path.join(profile, "user.js"), "w") as f:
        f.write('user_pref("browser.shell.checkDefaultBrowser", false);\n'
                'user_pref("browser.startup.homepage_override.mstone", "ignore");\n'
                'user_pref("startup.homepage_welcome_url", "");\n'
                'user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);\n'
                'user_pref("browser.aboutwelcome.enabled", false);\n'
                'user_pref("browser.sessionstore.resume_from_crash", false);\n')
    args = [binp, "--no-remote", "--new-instance", "--profile", profile, "--width", "1200",
            "--height", "800", "file://" + page]
    words = ("firefox",)
log = open(os.path.join(profile, "..", "dump-browser.log"), "w")
br = subprocess.Popen(args, env=env, stdout=log, stderr=log, start_new_session=True)


def name_of(b, p):
    return safe(lambda: prop(b, p, ACC, "Name"), "")


def kids(b, p):
    return safe(lambda: call(b, p, ACC, "GetChildren")[0], [])


def states_of(b, p):
    bits = safe(lambda: call(b, p, ACC, "GetState")[0], [0, 0])
    v = bits[0] | (bits[1] << 32)
    return [n for i, n in sorted(STATE_NAMES.items()) if v & (1 << i)]


def dump(bus, p, depth, out, limit=900):
    if len(out) > limit:
        return
    rn = safe(lambda: call(bus, p, ACC, "GetRoleName")[0], "?")
    role = safe(lambda: call(bus, p, ACC, "GetRole")[0], -1)
    nm = name_of(bus, p)
    desc = safe(lambda: prop(bus, p, ACC, "Description"), "")
    attrs = safe(lambda: call(bus, p, ACC, "GetAttributes")[0], {})
    attrs = {k: v for k, v in attrs.items() if k in (
        "xml-roles", "level", "current", "haspopup", "tag", "live", "container-live", "busy",
        "posinset", "setsize", "invalid", "readonly", "autocomplete", "checkable", "errormessage",
        "description", "explicit-name", "sort", "placeholder", "details", "roledescription",
        "valuetext", "multiselectable", "orientation", "required", "disabled", "pressed",
        "expanded", "selected", "checked", "controls", "colindex", "rowindex")}
    rels = []
    for (rt, tg) in safe(lambda: call(bus, p, ACC, "GetRelationSet")[0], []):
        for (b2, t2) in tg:
            rels.append("%s->%s:%s" % (REL.get(rt, rt), safe(lambda: call(b2, t2, ACC, "GetRoleName")[0], "?"),
                                       name_of(b2, t2)[:24]))
    st = [s for s in states_of(bus, p) if s not in ("enabled", "sensitive", "visible", "showing")]
    out.append("%s[%d]%s name=%r desc=%r st=%s attrs=%s rel=%s" % (
        "  " * depth, role, rn, nm[:50], desc[:50], ",".join(st), attrs, rels))
    for (b2, c) in kids(bus, p):
        if b2 == bus:
            dump(bus, c, depth + 1, out, limit)


try:
    doc = None
    end = time.monotonic() + 60
    while time.monotonic() < end and not doc:
        for (b, p) in safe(lambda: call("org.a11y.atspi.Registry", REG_ROOT, ACC, "GetChildren")[0], []):
            if any(w in name_of(b, p).lower() for w in words):
                if engine == "chromium":
                    for m in ("GetAttributes", "GetRelationSet"):
                        safe(lambda: call(b, p, ACC, m))
                queue = [p]
                seen = 0
                while queue and seen < 4000 and not doc:
                    q = queue.pop(0)
                    seen += 1
                    if safe(lambda: call(b, q, ACC, "GetRole")[0], -1) == DOC_WEB and title in name_of(b, q):
                        doc = (b, q)
                    queue.extend(c for (b2, c) in kids(b, q) if b2 == b)
        time.sleep(0.4)
    if not doc:
        print("no document found")
    else:
        time.sleep(float(os.environ.get("DUMP_WAIT", "2")))
        out = []
        dump(doc[0], doc[1], 0, out)
        print("\n".join(out))
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
