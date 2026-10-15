#!/usr/bin/python3
"""ax.py - a tiny AT-SPI driver for the M7a end-to-end scenarios (private session only).

It stands in for the user's hands where the emulated Mutter cannot reach the browser: it finds
Firefox's documents on the PRIVATE a11y bus, moves focus (Component.GrabFocus, the same call a
Tab key press ends in), switches tabs (Action.DoAction on a tab) and follows links.

It talks raw D-Bus through Gio, never libatspi: a libatspi client changes what a browser answers
(web source-layer report 2.1), and this process must not disturb the reader under test.

Control channel like fakeapp.py and fake_mutter.py: JSON lines on stdin, replies on stdout,
EOF on stdin quits. Every reply carries the request id. `ax.py --once <json>` runs one request.

  {"cmd":"find_doc","title":"M7 Article","timeout_ms":20000}
      -> {"ok":true,"bus":":1.9","path":"/org/a11y/atspi/accessible/12","name":"M7 Article"}
  {"cmd":"find","bus":":1.9","root":"/org/a11y/atspi/accessible/12","role":"link","name":"first",
   "timeout_ms":5000}                     (BFS under root; role names: link, entry, page tab, ...)
      -> {"ok":true,"path":"...","name":"first link","role":"link"}
  {"cmd":"focus","bus":":1.9","path":"..."}
      -> {"ok":true,"t_emit_ns":<CLOCK_MONOTONIC ns just before the call>}
  {"cmd":"action","bus":":1.9","path":"...","index":0}
      -> {"ok":true,"t_emit_ns":...}
  {"cmd":"tabs","bus":":1.9"}             -> {"ok":true,"tabs":[["name","path"],...]}

Refuses to start unless the a11y bus address is a harness bus (a /gorca-h.* temp dir).
"""
import json
import os
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

ACC = "org.a11y.atspi.Accessible"
COMP = "org.a11y.atspi.Component"
ACTION = "org.a11y.atspi.Action"
REG = "org.a11y.atspi.Registry"
REG_ROOT = "/org/a11y/atspi/accessible/root"

# AtspiRole numbers (stable ABI)
ROLES = {
    "link": 88, "entry": 79, "page tab": 37, "document web": 95, "heading": 83,
    "push button": 43, "combo box": 11, "application": 75, "frame": 23, "section": 85,
}
DOC_WEB = 95


def die(msg):
    sys.stdout.write(json.dumps({"event": "error", "error": msg}) + "\n")
    sys.stdout.flush()
    sys.exit(1)


addr = os.environ.get("AT_SPI_BUS_ADDRESS") or os.environ.get("GORCA_A11Y_BUS_ADDRESS")
if not addr or "/gorca-h." not in addr:
    die("refusing to run: the a11y bus is not a harness bus (tests/harness/run-isolated.sh)")

conn = Gio.DBusConnection.new_for_address_sync(
    addr,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)


def call(bus, path, iface, member, args=None, sig=None, timeout=3000):
    v = GLib.Variant(sig, args) if sig else None
    r = conn.call_sync(bus, path, iface, member, v, None, Gio.DBusCallFlags.NONE, timeout, None)
    return r.unpack() if r is not None else None


def prop(bus, path, iface, name):
    r = call(bus, path, "org.freedesktop.DBus.Properties", "Get", (iface, name), "(ss)")
    return r[0]


def children(bus, path):
    try:
        return call(bus, path, ACC, "GetChildren")[0]
    except GLib.Error:
        return []


def role_of(bus, path):
    try:
        return call(bus, path, ACC, "GetRole")[0]
    except GLib.Error:
        return -1


def name_of(bus, path):
    try:
        return prop(bus, path, ACC, "Name")
    except GLib.Error:
        return ""


def firefox_buses():
    found = []
    try:
        kids = call(REG, REG_ROOT, ACC, "GetChildren")[0]
    except GLib.Error:
        return found
    for (b, p) in kids:
        nm = name_of(b, p).lower()
        if "firefox" in nm:
            found.append(b)
    return found


def bfs(bus, root, pred, max_nodes=60000):
    queue = [root]
    seen = 0
    while queue and seen < max_nodes:
        p = queue.pop(0)
        seen += 1
        r = role_of(bus, p)
        n = name_of(bus, p)
        if pred(r, n, p):
            return p, n, r
        for (b, c) in children(bus, p):
            if b == bus:
                queue.append(c)
    return None


def find_doc(title, timeout_ms):
    end = time.monotonic() + timeout_ms / 1000.0
    while time.monotonic() < end:
        for b in firefox_buses():
            hit = bfs(b, "/org/a11y/atspi/accessible/root",
                      lambda r, n, p: r == DOC_WEB and (not title or title in n), 4000)
            if hit:
                return {"ok": True, "bus": b, "path": hit[0], "name": hit[1]}
        time.sleep(0.2)
    return {"ok": False, "error": "document not found"}


def find(bus, root, role, name, timeout_ms):
    want = ROLES.get(role, role if isinstance(role, int) else -2)
    end = time.monotonic() + timeout_ms / 1000.0
    while time.monotonic() < end:
        hit = bfs(bus, root, lambda r, n, p: (want < 0 or r == want) and (not name or name in n), 60000)
        if hit:
            return {"ok": True, "path": hit[0], "name": hit[1], "role": role}
        time.sleep(0.2)
    return {"ok": False, "error": "not found"}


def handle(req):
    cmd = req.get("cmd")
    if cmd == "find_doc":
        return find_doc(req.get("title", ""), int(req.get("timeout_ms", 20000)))
    if cmd == "find":
        return find(req["bus"], req["root"], req.get("role", ""), req.get("name", ""),
                    int(req.get("timeout_ms", 5000)))
    if cmd == "focus":
        t = time.monotonic_ns()
        r = call(req["bus"], req["path"], COMP, "GrabFocus")
        return {"ok": bool(r and r[0]), "t_emit_ns": t}
    if cmd == "action":
        t = time.monotonic_ns()
        r = call(req["bus"], req["path"], ACTION, "DoAction", (int(req.get("index", 0)),), "(i)")
        return {"ok": bool(r and r[0]), "t_emit_ns": t}
    if cmd == "text":
        # The whole Text of a node, hex of its UTF-8 (JSON escapes of astral characters are not
        # worth decoding in the harness).
        n = prop(req["bus"], req["path"], "org.a11y.atspi.Text", "CharacterCount")
        t = call(req["bus"], req["path"], "org.a11y.atspi.Text", "GetText", (0, n), "(ii)")[0]
        return {"ok": True, "hex": t.encode("utf-8").hex(), "n": n}
    if cmd == "describe":
        try:
            rn = call(req["bus"], req["path"], ACC, "GetRoleName")[0]
        except GLib.Error:
            rn = "?"
        return {"ok": True, "role": rn, "name": name_of(req["bus"], req["path"])}
    if cmd == "caret":
        return {"ok": True, "caret": prop(req["bus"], req["path"], "org.a11y.atspi.Text", "CaretOffset")}
    if cmd == "has_state":
        # AtspiStateType number (default 10, EXPANDED) in the node's state set
        bits = call(req["bus"], req["path"], ACC, "GetState")[0]
        st = int(req.get("state", 10))
        return {"ok": True, "has": (bits[st // 32] >> (st % 32)) & 1}
    if cmd == "tabs":
        root = "/org/a11y/atspi/accessible/root"
        tabs = []
        queue = [root]
        n = 0
        while queue and n < 4000:
            p = queue.pop(0)
            n += 1
            if role_of(req["bus"], p) == 37:
                tabs.append([name_of(req["bus"], p), p])
            for (b, c) in children(req["bus"], p):
                if b == req["bus"]:
                    queue.append(c)
        return {"ok": True, "tabs": tabs}
    return {"ok": False, "error": "unknown cmd"}


if len(sys.argv) >= 3 and sys.argv[1] == "--once":
    # One request from the command line (probes and shell scripts).
    sys.stdout.write(json.dumps(handle(json.loads(sys.argv[2]))) + "\n")
    sys.exit(0)

sys.stdout.write(json.dumps({"event": "ready"}, separators=(",", ":")) + "\n")
sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
        rep = handle(req)
    except Exception as e:  # report, never die: the scenario decides
        rep = {"ok": False, "error": repr(e)}
    rep["event"] = "reply"
    rep["id"] = req.get("id") if isinstance(req, dict) else None
    sys.stdout.write(json.dumps(rep, separators=(",", ":")) + "\n")
    sys.stdout.flush()
