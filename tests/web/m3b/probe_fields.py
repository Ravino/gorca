#!/usr/bin/python3
"""probe_fields.py - what a private Firefox emits and answers for text fields (M3b, V-T3).

Run inside the M7 private sessions:

    tests/web/m7/run_m7.sh --probe /usr/bin/python3 tests/web/m3b/probe_fields.py [scenario]

It launches its own Firefox (fresh profile, own process group), focuses fields through
Component.GrabFocus, presses keys through the compositor's RemoteDesktop session (rd.py) and
prints, for every step, the AT-SPI text events Firefox sent and what the Text interface answers.
Read-only toward the host: private buses, private compositor.
"""
import json
import os
import signal
import subprocess
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
M7 = os.path.join(HERE, "..", "m7")
ROOT_DIR = os.environ["GORCA_HARNESS_DIR"]
ADDR = os.environ.get("AT_SPI_BUS_ADDRESS") or os.environ.get("GORCA_A11Y_BUS_ADDRESS")
if not ADDR or "/gorca-h." not in ADDR:
    sys.exit("refusing: not a harness a11y bus")

USER_JS = open(os.path.join(M7, "..", "..", "..", "crates/gorca-bench/src/e2e/web.rs")).read()
USER_JS = USER_JS.split('const USER_JS: &str = r#"')[1].split('"#;')[0]

conn = Gio.DBusConnection.new_for_address_sync(
    ADDR, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
seen = []
names = {}

ACC = "org.a11y.atspi.Accessible"
TEXT = "org.a11y.atspi.Text"


def call(bus, path, iface, member, args=None, sig=None, timeout=3000):
    v = GLib.Variant(sig, args) if sig else None
    r = conn.call_sync(bus, path, iface, member, v, None, Gio.DBusCallFlags.NONE, timeout, None)
    return r.unpack() if r is not None else None


def prop(bus, path, iface, name):
    return call(bus, path, "org.freedesktop.DBus.Properties", "Get", (iface, name), "(ss)")[0]


def cb(c, sender, path, iface, member, params, ud):
    if iface != "org.a11y.atspi.Event.Object":
        return
    p = params.unpack()
    seen.append((time.monotonic(), sender, path, member, p[0], p[1], p[2], p[3] if len(p) > 3 else None))


conn.signal_subscribe(None, "org.a11y.atspi.Event.Object", None, None, None, Gio.DBusSignalFlags.NONE, cb, None)
for ev in ("object:text-caret-moved", "object:text-changed", "object:text-selection-changed",
           "object:state-changed:focused", "object:text-attributes-changed"):
    try:
        conn.call_sync("org.a11y.atspi.Registry", "/org/a11y/atspi/registry", "org.a11y.atspi.Registry",
                       "RegisterEvent", GLib.Variant("(sas)", (ev, [])), None, Gio.DBusCallFlags.NONE, 3000, None)
    except GLib.Error as e:
        print("register", ev, e)


def pump(sec):
    end = time.time() + sec
    ctx = GLib.MainContext.default()
    while time.time() < end:
        while ctx.iteration(False):
            pass
        time.sleep(0.01)


def ax(req):
    out = subprocess.run(["/usr/bin/python3", os.path.join(M7, "ax.py"), "--once", json.dumps(req)],
                         capture_output=True, text=True).stdout
    return json.loads(out)


def role_name(bus, path):
    try:
        return call(bus, path, ACC, "GetRoleName")[0]
    except GLib.Error:
        return "?"


def label(bus, path):
    try:
        n = prop(bus, path, ACC, "Name")
    except GLib.Error:
        n = "?"
    return "%s %r" % (role_name(bus, path), n)


def text_of(bus, path):
    try:
        n = prop(bus, path, TEXT, "CharacterCount")
        t = call(bus, path, TEXT, "GetText", (0, n), "(ii)")[0]
        return n, t
    except GLib.Error as e:
        return None, "ERR %s" % e


def caret_of(bus, path):
    try:
        return prop(bus, path, TEXT, "CaretOffset")
    except GLib.Error:
        return None


def esc(s):
    return s.replace("﻿", "<BOM>").replace("\n", "\\n")


def dump_events(tag):
    pump(0.35)
    print("  [%s] events:" % tag)
    for (t, sender, path, member, kind, d1, d2, dv) in seen:
        node = path.split("/")[-1]
        extra = ""
        if member == "TextChanged":
            extra = " text=%r" % esc(str(dv))
        print("    %s %s on %s %s d1=%s d2=%s%s" % (member, kind, node, label(sender, path), d1, d2, extra))
    seen.clear()


def start_firefox(url):
    profile = os.path.join(ROOT_DIR, "ffprofile-probe")
    os.makedirs(profile, exist_ok=True)
    open(os.path.join(profile, "user.js"), "w").write(USER_JS)
    env = dict(os.environ)
    env.pop("DISPLAY", None)
    env["WAYLAND_DISPLAY"] = os.environ["GORCA_M7_WAYLAND"]
    env["MOZ_ENABLE_WAYLAND"] = "1"
    env["GNOME_ACCESSIBILITY"] = "1"
    log = open(os.path.join(ROOT_DIR, "ff-probe.log"), "w")
    return subprocess.Popen(["firefox", "--no-remote", "--new-instance", "--profile", profile,
                             "--width", "1200", "--height", "800", url],
                            env=env, stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)


def main():
    scen = sys.argv[1] if len(sys.argv) > 1 else "all"
    url = "file://" + os.path.realpath(os.path.join(HERE, "pages", "fields.html"))
    ff = start_firefox(url)
    rd = subprocess.Popen(["/usr/bin/python3", os.path.join(M7, "rd.py")], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, text=True)
    try:
        d = ax({"cmd": "find_doc", "title": "M3b Fields", "timeout_ms": 60000})
        print("doc", d)
        bus, doc = d["bus"], d["path"]
        pump(2.0)

        def keys(*codes):
            rd.stdin.write("key " + " ".join(str(c) for c in codes) + "\n")
            rd.stdin.flush()

        rd.stdin.write("click\n")
        rd.stdin.flush()
        pump(0.8)
        # tree summary
        def walk(path, depth, out):
            if depth > 9 or len(out) > 80:
                return
            out.append("  " * depth + label(bus, path) + " " + path.split("/")[-1])
            try:
                for (b, c) in call(bus, path, ACC, "GetChildren")[0]:
                    walk(c, depth + 1, out)
            except GLib.Error:
                pass
        out = []
        walk(doc, 0, out)
        print("\n".join(out))
        fields = {}
        for name in ("Single line", "Emoji line", "Multi line", "Password", "Editable region"):
            role = 40 if name == "Password" else "entry"
            r = ax({"cmd": "find", "bus": bus, "root": doc, "role": role, "name": name, "timeout_ms": 3000})
            print("find", name, r)
            fields[name] = r.get("path")
        # also the contenteditable by role name
        ce = ax({"cmd": "find", "bus": bus, "root": doc, "role": "", "name": "Editable", "timeout_ms": 3000})
        print("ce", ce)

        LEFT, RIGHT, UP, DOWN, HOME, END = 105, 106, 103, 108, 102, 107
        SHIFT, CTRL, BKSP, DEL, A = 42, 29, 14, 111, 30
        F9, F10 = 67, 68
        for name in ("Single line", "Emoji line", "Multi line", "Password", "Editable region"):
            path = fields.get(name)
            if not path:
                continue
            print("=== %s %s" % (name, path))
            seen.clear()
            ax({"cmd": "focus", "bus": bus, "path": path})
            dump_events("focus")
            n, t = text_of(bus, path)
            print("  text n=%s %r caret=%s" % (n, esc(str(t)), caret_of(bus, path)))
            keys(HOME)
            dump_events("home")
            for i, k in enumerate((RIGHT, RIGHT, RIGHT, RIGHT, RIGHT)):
                keys(k)
                dump_events("right%d" % (i + 1))
            if name in ("Multi line", "Editable region"):
                keys(DOWN)
                dump_events("down")
                keys(END)
                dump_events("end")
            keys(LEFT)
            dump_events("left")
            keys(SHIFT, RIGHT)
            dump_events("shift+right")
            keys(F9)
            dump_events("insert emoji (F9)")
            keys(F10)
            dump_events("insert cyrillic (F10)")
            keys(BKSP)
            dump_events("backspace")
            keys(BKSP)
            dump_events("backspace 2")
            keys(CTRL, A)
            dump_events("ctrl+a")
            n, t = text_of(bus, path)
            print("  text n=%s %r caret=%s" % (n, esc(str(t)), caret_of(bus, path)))
            # GetStringAtOffset at a few offsets
            for off in (0, 1, 2):
                for gran, gname in ((0, "char"), (1, "word"), (3, "line")):
                    try:
                        r = call(bus, path, TEXT, "GetStringAtOffset", (off, gran), "(iu)")
                        print("  GetStringAtOffset(%d,%s) = %r" % (off, gname, (esc(r[0]), r[1], r[2])))
                    except GLib.Error as e:
                        print("  GetStringAtOffset(%d,%s) ERR %s" % (off, gname, e))
            try:
                attrs = call(bus, path, TEXT, "GetAttributeRun", (0, True), "(ib)")
                print("  GetAttributeRun(0) =", attrs)
                print("  GetDefaultAttributes =", call(bus, path, TEXT, "GetDefaultAttributes"))
            except GLib.Error as e:
                print("  attrs ERR", e)
        print("done")
    finally:
        try:
            rd.stdin.close()
        except Exception:
            pass
        try:
            os.killpg(ff.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        time.sleep(1)
        try:
            os.killpg(ff.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


main()
