#!/usr/bin/python3
"""sem-capture: record the raw AT-SPI semantic surface of one page / one GTK4 gallery page.

Runs INSIDE a private world (tests/shell/run_shell.sh or tests/harness/run-isolated.sh); it talks
to the private a11y bus named by AT_SPI_BUS_ADDRESS and never to the live one. Use the system
Python (PyGObject + Atspi typelib), not the project venv.

  capture.py --engine firefox  --page forms --html tests/fixtures/sem/pages/forms.html --out DIR
  capture.py --engine chromium --page forms --html ... --out DIR
  capture.py --engine gtk4     --page inputs --out DIR

Output: DIR/<page>.jsonl. Line 1 is a header object; every other line is one accessible, in
pre-order. All data is read with plain org.a11y.atspi.* D-Bus calls (the same calls the gorca
connector issues), so the record is the engine's own answer, not a toolkit-library reinterpretation.
The only Atspi-typelib use is the enum nick tables (role/state/relation names).
"""
import argparse
import datetime
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, Gio, GLib  # noqa: E402

ROLE_DOCUMENT_WEB = 95
TIMEOUT_MS = 3000
ACC = "org.a11y.atspi.Accessible"


def nick_table(enum_cls, n):
    out = {}
    for i in range(n):
        try:
            out[i] = enum_cls(i).value_nick
        except Exception:
            pass
    return out


ROLE_NAMES = nick_table(Atspi.Role, 140)
STATE_NAMES = nick_table(Atspi.StateType, 64)
REL_NAMES = nick_table(Atspi.RelationType, 24)


def is_err(r):
    return isinstance(r, dict) and "__error__" in r


class Bus:
    def __init__(self, addr):
        self.c = Gio.DBusConnection.new_for_address_sync(
            addr,
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
            None, None)

    def call(self, dest, path, iface, method, args=None, timeout=TIMEOUT_MS):
        try:
            v = self.c.call_sync(dest, path, iface, method, args, None,
                                 Gio.DBusCallFlags.NONE, timeout, None)
            u = v.unpack()
            return u[0] if len(u) == 1 else u
        except GLib.Error as e:
            return {"__error__": e.message[:160]}

    def prop(self, dest, path, iface, name):
        r = self.call(dest, path, "org.freedesktop.DBus.Properties", "Get",
                      GLib.Variant("(ss)", (iface, name)))
        return None if is_err(r) else r

    def props(self, dest, path, iface):
        r = self.call(dest, path, "org.freedesktop.DBus.Properties", "GetAll", GLib.Variant("(s)", (iface,)))
        return {} if is_err(r) else r


def record(bus, d, p, parent_i, depth, idx_of, nodes, pending_rel, limit):
    if len(nodes) >= limit:
        return
    i = len(nodes)
    idx_of[(d, p)] = i
    ap = bus.props(d, p, ACC)
    role = bus.call(d, p, ACC, "GetRole")
    rolename = bus.call(d, p, ACC, "GetRoleName")
    st = bus.call(d, p, ACC, "GetState")
    attrs = bus.call(d, p, ACC, "GetAttributes")
    rels = bus.call(d, p, ACC, "GetRelationSet")
    ifs = bus.call(d, p, ACC, "GetInterfaces")
    ifs = [] if is_err(ifs) else sorted(x.replace("org.a11y.atspi.", "") for x in ifs)
    st = list(st) if isinstance(st, (list, tuple)) and len(st) == 2 else [0, 0]
    bits = (st[0] & 0xFFFFFFFF) | ((st[1] & 0xFFFFFFFF) << 32)
    node = {
        "i": i, "parent": parent_i, "depth": depth,
        "role": role if isinstance(role, int) else None,
        "role_name": ROLE_NAMES.get(role, "?") if isinstance(role, int) else None,
        "raw_role_name": rolename if isinstance(rolename, str) else None,
        "state_u32": st,
        "states": [STATE_NAMES.get(b, "bit%d" % b) for b in range(64) if bits >> b & 1],
        "attrs": {} if is_err(attrs) or not isinstance(attrs, dict) else attrs,
        "ifaces": ifs,
        "name": ap.get("Name"), "desc": ap.get("Description"),
        "nchildren": ap.get("ChildCount"),
        "bus_name": d, "path": p,
    }
    node["fixture_id"] = node["attrs"].get("id")
    d0 = node["desc"]
    if isinstance(d0, str) and d0.startswith("fx:"):  # GTK4 gallery: id carried in the description
        head, _, rest = d0[3:].partition("|")
        node["fixture_id"] = head
        node["desc_clean"] = rest
    if "Text" in ifs:
        n = bus.prop(d, p, "org.a11y.atspi.Text", "CharacterCount")
        node["text_len"] = n
        if isinstance(n, int) and 0 < n <= 4000:
            t = bus.call(d, p, "org.a11y.atspi.Text", "GetText", GLib.Variant("(ii)", (0, min(n, 400))))
            node["text"] = t if isinstance(t, str) else None
    if "Value" in ifs:
        vv = bus.props(d, p, "org.a11y.atspi.Value")
        node["value"] = {k: vv.get(k) for k in
                         ("MinimumValue", "MaximumValue", "MinimumIncrement", "CurrentValue", "Text") if k in vv}
    if "Action" in ifs:
        n = bus.prop(d, p, "org.a11y.atspi.Action", "NActions")
        acts = []
        for k in range(n if isinstance(n, int) and n < 8 else 0):
            nm = bus.call(d, p, "org.a11y.atspi.Action", "GetName", GLib.Variant("(i)", (k,)))
            acts.append(nm if isinstance(nm, str) else None)
        node["actions"] = acts
    if "TableCell" in ifs:
        tc = bus.props(d, p, "org.a11y.atspi.TableCell")
        node["cell"] = {k: tc.get(k) for k in ("RowSpan", "ColumnSpan", "Position")}
        rc = bus.call(d, p, "org.a11y.atspi.TableCell", "GetRowColumnSpan")
        node["cell"]["row_col_span"] = list(rc) if isinstance(rc, (tuple, list)) else None
    if "Table" in ifs:
        tp = bus.props(d, p, "org.a11y.atspi.Table")
        node["table"] = {k: tp.get(k) for k in ("NRows", "NColumns")}
    if "Selection" in ifs:
        node["nselected"] = bus.prop(d, p, "org.a11y.atspi.Selection", "NSelectedChildren")
    node["relations"] = []
    if isinstance(rels, list):
        for rt, targets in rels:
            node["relations"].append({"type": rt, "name": REL_NAMES.get(rt, "?"),
                                      "targets": [list(t) for t in targets]})
            pending_rel.append((i, len(node["relations"]) - 1))
    nodes.append(node)
    kids = bus.call(d, p, ACC, "GetChildren")
    if isinstance(kids, list):
        for (kd, kp) in kids:
            if kp == "/org/a11y/atspi/null":
                continue
            record(bus, kd, kp, i, depth + 1, idx_of, nodes, pending_rel, limit)


def walk_subtree(bus, d, p, limit=20000):
    nodes, idx_of, pend = [], {}, []
    record(bus, d, p, -1, 0, idx_of, nodes, pend, limit)
    for (ni, ri) in pend:  # relation targets -> node index (null = outside the recorded subtree)
        rel = nodes[ni]["relations"][ri]
        rel["targets"] = [idx_of.get((t[0], t[1])) for t in rel["targets"]]
    return nodes


def find_app(bus, want):
    kids = bus.call("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root", ACC, "GetChildren")
    for (d, p) in (kids if isinstance(kids, list) else []):
        nm = bus.prop(d, p, ACC, "Name")
        if isinstance(nm, str) and want.lower() in nm.lower():
            return d, p, nm
    return None


def find_doc(bus, d, p, title, depth=0):
    """First DOCUMENT_WEB node (pre-order) whose Name == title."""
    role = bus.call(d, p, ACC, "GetRole")
    if role == ROLE_DOCUMENT_WEB:
        return (d, p) if bus.prop(d, p, ACC, "Name") == title else None
    if depth > 12:
        return None
    kids = bus.call(d, p, ACC, "GetChildren")
    if not isinstance(kids, list):
        return None
    for (kd, kp) in kids:
        r = find_doc(bus, kd, kp, title, depth + 1)
        if r:
            return r
    return None


def quiesce(bus, d, p, need=3, step=0.7, deadline=45):
    """Wait until the subtree node count is stable for `need` consecutive polls."""
    end = time.time() + deadline
    last, same = -1, 0
    while time.time() < end:
        n = len(walk_subtree(bus, d, p, limit=6000))
        same = same + 1 if n == last and n > 1 else 0
        last = n
        if same >= need:
            return n
        time.sleep(step)
    return last


def launch(engine, html, root):
    env = dict(os.environ)
    prof = tempfile.mkdtemp(prefix="semprof.", dir=root)
    if engine == "firefox":
        with open(os.path.join(prof, "user.js"), "w") as f:
            f.write('user_pref("browser.shell.checkDefaultBrowser", false);\n'
                    'user_pref("browser.startup.homepage_override.mstone","ignore");\n'
                    'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
                    'user_pref("browser.aboutwelcome.enabled", false);\n'
                    'user_pref("browser.tabs.warnOnClose", false);\n')
        env["MOZ_ENABLE_WAYLAND"] = "1"
        cmd = ["firefox-esr", "--no-remote", "--new-instance", "-profile", prof,
               "--width", "900", "--height", "700", "file://" + os.path.abspath(html)]
    else:
        cmd = ["chromium", "--force-renderer-accessibility", "--ozone-platform=wayland",
               "--user-data-dir=" + prof, "--no-first-run", "--no-default-browser-check",
               "--disable-gpu", "--disable-extensions", "--password-store=basic",
               "--window-size=900,700", "file://" + os.path.abspath(html)]
    p = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         start_new_session=True)
    return p, prof


def kill_group(p):
    """Kill ONLY the process group we started (setsid => pgid == pid)."""
    try:
        os.killpg(p.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    for _ in range(30):
        if p.poll() is not None:
            break
        time.sleep(0.2)
    if p.poll() is None:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def title_of(html):
    m = re.search(r"<title>(.*?)</title>", open(html, encoding="utf-8").read(), re.S)
    return m.group(1).strip() if m else ""


def version_of(engine):
    if engine == "gtk4":
        code = ("import gi;gi.require_version('Gtk','4.0');from gi.repository import Gtk;"
                "print('GTK %d.%d.%d'%(Gtk.get_major_version(),Gtk.get_minor_version(),Gtk.get_micro_version()))")
        return subprocess.run(["/usr/bin/python3", "-c", code], capture_output=True, text=True).stdout.strip()
    exe = {"firefox": "firefox-esr", "chromium": "chromium"}[engine]
    return subprocess.run([exe, "--version"], capture_output=True, text=True).stdout.strip()


def pkg_version(pkg):
    r = subprocess.run(["dpkg-query", "-W", "-f=${Version}", pkg], capture_output=True, text=True)
    return pkg + " " + (r.stdout.strip() or "?")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine", required=True, choices=["firefox", "chromium", "gtk4"])
    ap.add_argument("--page", required=True)
    ap.add_argument("--html")
    ap.add_argument("--out", required=True)
    ap.add_argument("--settle", type=float, default=3.0, help="extra seconds after the tree is stable")
    a = ap.parse_args()
    addr = os.environ.get("AT_SPI_BUS_ADDRESS") or os.environ.get("GORCA_A11Y_BUS_ADDRESS")
    if not addr or not os.environ.get("GORCA_HARNESS_DIR") or "/run/user/" in addr:
        sys.exit("refusing to run outside a private world (need GORCA_HARNESS_DIR and a private "
                 "AT_SPI_BUS_ADDRESS, got %r)" % addr)
    os.makedirs(a.out, exist_ok=True)
    root = os.environ["GORCA_HARNESS_DIR"]
    bus = Bus(addr)
    if a.engine == "gtk4":
        gallery = os.path.join(os.path.dirname(os.path.abspath(__file__)), "gtk4_gallery.py")
        proc = subprocess.Popen(["/usr/bin/python3", gallery, a.page], stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, start_new_session=True)
        prof, title, want = None, "", "sem-gallery"
    else:
        proc, prof = launch(a.engine, a.html, root)
        title = title_of(a.html)
        want = "firefox" if a.engine == "firefox" else "chrom"
    try:
        found = None
        for _ in range(120):
            found = find_app(bus, want)
            if found:
                break
            time.sleep(0.5)
        if not found:
            sys.exit("app %r never appeared on the private a11y bus" % want)
        d, p, appname = found
        if a.engine == "gtk4":
            sub = (d, p)
            time.sleep(2)
        else:
            sub = None
            for _ in range(120):
                sub = find_doc(bus, d, p, title)
                if sub:
                    break
                time.sleep(0.5)
            if not sub:
                sys.exit("document %r never appeared" % title)
        n = quiesce(bus, sub[0], sub[1])
        time.sleep(a.settle)
        nodes = walk_subtree(bus, sub[0], sub[1])
        header = {"kind": "header", "schema": 1, "engine": a.engine, "engine_version": version_of(a.engine),
                  "app_name": appname, "page": a.page, "title": title,
                  "date": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                  "atspi_lib": pkg_version("libatspi2.0-0t64"), "at_spi2_core": pkg_version("at-spi2-core"),
                  "settled_nodes": n, "nodes": len(nodes),
                  "unreadable": bool(nodes and nodes[0]["role"] is None),
                  "note": "bus_name/path are per-run and not stable; compare by fixture_id / document order"}
        with open(os.path.join(a.out, a.page + ".jsonl"), "w", encoding="utf-8") as f:
            f.write(json.dumps(header, sort_keys=True) + "\n")
            for nd in nodes:
                f.write(json.dumps(nd, sort_keys=True, ensure_ascii=False) + "\n")
        if nodes and nodes[0]["role"] is None:
            print("WARNING %s/%s: document root unreadable (every call errored/timed out); the engine "
                  "hung or crashed on this page" % (a.engine, a.page), file=sys.stderr)
        print("%s/%s: %d nodes, %d with fixture id" % (a.engine, a.page, len(nodes),
              sum(1 for x in nodes if x.get("fixture_id"))), file=sys.stderr)
    finally:
        kill_group(proc)
        if prof:
            shutil.rmtree(prof, ignore_errors=True)


if __name__ == "__main__":
    main()
