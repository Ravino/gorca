#!/usr/bin/python3
"""probe_shell.py - what does a REAL GNOME Shell 48 emit over AT-SPI? (M4b ground truth)

Run it INSIDE the private world (never against the live session):

    tests/shell/run_shell.sh -- /usr/bin/python3 tests/shell/probe_shell.py --out DIR [--only a,b]

It refuses to run unless AT_SPI_BUS_ADDRESS / DBUS_SESSION_BUS_ADDRESS are under a /gorca-h.*
harness dir. Everything is raw D-Bus through Gio (no libatspi: a libatspi client changes what an
application emits and answers, see CLAUDE.md memory "cache/p2p facts"):

  * a capture thread registers for AT-SPI events with the registry and records EVERY signal on
    the private a11y bus with a monotonic ms timestamp (type, sender, path, detail, detail1/2,
    any_data, properties);
  * an enrich thread resolves each event's source (role, name, description, states, parent chain,
    Value/Text/Relations) a few ms after arrival (a snapshot, NOT the state at emit time);
  * before/after each scenario the whole Shell tree is read with Cache.GetItems and diffed: the
    nodes that became showing and carry a name or text are "what a reader can read".

Scenario triggers: org.freedesktop.Notifications.Notify (gdbus), org.gnome.Shell.ShowOSD (the
real path gsd-media-keys uses), org.gnome.Shell.Eval (needs --unsafe-mode), the Shell's
EndSessionDialog D-Bus interface, and Mutter RemoteDesktop virtual-keyboard key presses.
Teardown signals only the client PIDs started here.
"""
import argparse
import collections
import json
import os
import queue
import re
import signal
import subprocess
import sys
import threading
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
A11Y = os.environ.get("AT_SPI_BUS_ADDRESS", "")
SESSION = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
SHELL_PID = int(os.environ.get("GORCA_SHELL_PID", "0") or 0)
if "/gorca-h." not in A11Y or "/gorca-h." not in SESSION:
    sys.exit("probe_shell: refusing to run outside a tests/harness private session")

ACC = "org.a11y.atspi.Accessible"
STATE_NAMES = (
    "invalid active armed busy checked collapsed defunct editable enabled expandable expanded "
    "focusable focused has-tooltip horizontal iconified modal multi-line multiselectable opaque "
    "pressed resizable selectable selected sensitive showing single-line stale transient vertical "
    "visible manages-descendants indeterminate required truncated animated invalid-entry "
    "supports-autocompletion selectable-text is-default visited checkable has-popup read-only"
).split()

# evdev key codes
K_ESC, K_TAB, K_ENTER, K_A, K_D, K_E, K_G, K_ALT, K_F2, K_META = 1, 15, 28, 30, 32, 18, 34, 56, 60, 125
K_DOWN, K_RIGHT, K_SPACE = 108, 106, 57


def now_ms():
    return time.monotonic_ns() / 1e6


def new_conn(addr):
    return Gio.DBusConnection.new_for_address_sync(
        addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)


def states_of(words):
    out = []
    for i, w in enumerate(words):
        for b in range(32):
            if w >> b & 1:
                idx = i * 32 + b
                out.append(STATE_NAMES[idx] if idx < len(STATE_NAMES) else f"state{idx}")
    return out


def jsonable(o):
    if isinstance(o, (bytes, bytearray)):
        return o.hex()
    if isinstance(o, dict):
        return {str(k): jsonable(v) for k, v in o.items()}
    if isinstance(o, (list, tuple)):
        return [jsonable(v) for v in o]
    return o


class Peer:
    """Synchronous D-Bus caller with a deadline; errors come back as ('ERR', text)."""

    def __init__(self, addr):
        self.c = new_conn(addr)

    def call(self, dest, path, iface, method, args=None, sig=None, timeout=3000):
        try:
            v = GLib.Variant(sig, args) if sig else None
            r = self.c.call_sync(dest, path, iface, method, v, None, Gio.DBusCallFlags.NONE, timeout, None)
            return r.unpack()
        except GLib.Error as e:
            return ("ERR", e.message)

    def prop(self, dest, path, iface, name, timeout=3000):
        r = self.call(dest, path, "org.freedesktop.DBus.Properties", "Get", (iface, name), "(ss)", timeout)
        if isinstance(r, tuple) and len(r) == 2 and r[0] == "ERR":
            return r
        return r[0]


def iserr(r):
    return isinstance(r, tuple) and len(r) == 2 and r[0] == "ERR"


class Enricher(threading.Thread):
    """Resolves the source of each event: what a screen reader could ask right after the event."""

    def __init__(self):
        super().__init__(daemon=True)
        self.q = queue.PriorityQueue()   # key events (state/name/value/window) first, bulk churn after
        self.peer = None
        self.stop = False
        self.known = {}      # (bus, path) -> (role, name): last good description, for sources that died
        self.pending = 0
        self.lock = threading.Lock()

    @staticmethod
    def priority(ev):
        d = ev.get("detail", "")
        if ev["iface"] in ("Window", "Focus", "Document") or ev["member"] in ("Announcement", "TextChanged", "TextCaretMoved"):
            return 0
        if ev["member"] == "StateChanged" and d in ("showing", "focused", "active", "selected", "checked", "expanded", "defunct"):
            return 0 if d != "showing" else 1
        if ev["member"] == "PropertyChange" and d in ("accessible-name", "accessible-value"):
            return 1
        if ev["member"] == "ChildrenChanged":
            return 2
        return 3

    def submit(self, ev):
        with self.lock:
            self.pending += 1
        self.q.put((self.priority(ev), ev["seq"], ev))

    def idle(self):
        with self.lock:
            return self.pending == 0

    def describe(self, bus, path, depth=3):
        p = self.peer
        info = {"path": path.rsplit("/", 1)[-1]}
        r = p.call(bus, path, ACC, "GetRoleName", timeout=800)
        if iserr(r):
            info["role"] = "<gone>" if "does not exist" in r[1] else "<err " + r[1][:60] + ">"
            info["name"] = None
            if (bus, path) in self.known:
                info["last_known"] = "%s:%r" % self.known[(bus, path)]
            return info
        info["role"] = r[0]
        info["name"] = p.prop(bus, path, ACC, "Name", 800)
        self.known[(bus, path)] = (info["role"], info["name"])
        d = p.prop(bus, path, ACC, "Description", 800)
        if d:
            info["desc"] = d
        st = p.call(bus, path, ACC, "GetState", timeout=800)
        info["states"] = states_of(st[0]) if not iserr(st) else str(st)
        info["nchildren"] = p.prop(bus, path, ACC, "ChildCount", 800)
        ifs = p.call(bus, path, ACC, "GetInterfaces", timeout=800)
        ifs = [i.rsplit(".", 1)[-1] for i in ifs[0]] if not iserr(ifs) else []
        info["ifaces"] = ifs
        if "Value" in ifs:
            for pr in ("CurrentValue", "MinimumValue", "MaximumValue"):
                info["value." + pr] = p.prop(bus, path, "org.a11y.atspi.Value", pr, 800)
        if "Text" in ifs:
            n = p.prop(bus, path, "org.a11y.atspi.Text", "CharacterCount", 800)
            if isinstance(n, int):
                t = p.call(bus, path, "org.a11y.atspi.Text", "GetText", (0, min(n, 300)), "(ii)", 800)
                info["text"] = t[0] if not iserr(t) else str(t)
        rel = p.call(bus, path, ACC, "GetRelationSet", timeout=800)
        if not iserr(rel) and rel[0]:
            info["relations"] = [(r[0], [x[1].rsplit("/", 1)[-1] for x in r[1]]) for r in rel[0]]
        at = p.call(bus, path, ACC, "GetAttributes", timeout=800)
        if not iserr(at) and at[0]:
            info["attrs"] = at[0]
        if depth > 0:
            par = p.prop(bus, path, ACC, "Parent", 800)
            if not iserr(par) and par and par[1] != "/org/a11y/atspi/null":
                if par[1].endswith("/root"):
                    info["parent"] = "root"
                else:
                    info["parent"] = self.describe(par[0], par[1], depth - 1)
        return info

    def run(self):
        self.peer = Peer(A11Y)
        while not self.stop:
            try:
                ev = self.q.get(timeout=0.2)[2]
            except queue.Empty:
                continue
            try:
                t = now_ms()
                if ev.get("src_bus") and ev.get("src_path"):
                    ev["src"] = self.describe(ev["src_bus"], ev["src_path"])
                    # a ChildrenChanged any_data is the child reference: describe it too
                    ad = ev.get("any_ref")
                    if ad:
                        ev["any_src"] = self.describe(ad[0], ad[1], 1)
                ev["enrich_lag_ms"] = round(now_ms() - ev["t_ms"], 1)
                ev["enrich_cost_ms"] = round(now_ms() - t, 1)
            except Exception as e:  # noqa: BLE001 - a probe must record, not die
                ev["enrich_error"] = repr(e)
            finally:
                with self.lock:
                    self.pending -= 1


class Capture(threading.Thread):
    EVENTS = [
        "object:state-changed", "object:children-changed", "object:property-change",
        "object:announcement", "object:text-changed", "object:text-caret-moved",
        "object:active-descendant-changed", "object:selection-changed", "object:visible-data-changed",
        "object:bounds-changed", "object:link-selected", "object:row-reordered",
        "window", "focus", "document",
    ]

    def __init__(self, enricher):
        super().__init__(daemon=True)
        self.events = []
        self.enricher = enricher
        self.ready = threading.Event()
        self.seq = 0
        self.reg_log = []
        self.last_ms = now_ms()

    def run(self):
        ctx = GLib.MainContext()
        ctx.push_thread_default()
        self.conn = new_conn(A11Y)
        self.conn.signal_subscribe(None, None, None, None, None, Gio.DBusSignalFlags.NONE, self.on_sig, None)
        reg = "org.a11y.atspi.Registry"
        for ev in self.EVENTS:
            # at-spi2-core >= 2.46: RegisterEvent(s event, as properties, s app_bus_name)
            try:
                self.conn.call_sync(reg, "/org/a11y/atspi/registry", reg, "RegisterEvent",
                                    GLib.Variant("(sass)", (ev, [], "")), None, Gio.DBusCallFlags.NONE, 5000, None)
                self.reg_log.append((ev, "ok(sass)"))
            except GLib.Error as e:
                try:
                    self.conn.call_sync(reg, "/org/a11y/atspi/registry", reg, "RegisterEvent",
                                        GLib.Variant("(sas)", (ev, [])), None, Gio.DBusCallFlags.NONE, 5000, None)
                    self.reg_log.append((ev, "ok(sas)"))
                except GLib.Error as e2:
                    self.reg_log.append((ev, f"FAILED {e.message} / {e2.message}"))
        self.loop = GLib.MainLoop(ctx)
        self.ready.set()
        self.loop.run()

    def on_sig(self, conn, sender, path, iface, member, params, _ud):
        t = now_ms()
        if not iface.startswith("org.a11y.atspi.Event."):
            return
        self.last_ms = t
        self.seq += 1
        try:
            body = params.unpack()
        except Exception:  # noqa: BLE001
            body = None
        ev = {"seq": self.seq, "t_ms": round(t, 2), "sender": sender, "iface": iface[len("org.a11y.atspi.Event."):],
              "member": member, "src_bus": sender, "src_path": path}
        if isinstance(body, tuple) and len(body) >= 4:
            ev["detail"], ev["d1"], ev["d2"] = body[0], body[1], body[2]
            any_d = body[3]
            ev["any"] = jsonable(any_d)
            if isinstance(any_d, tuple) and len(any_d) == 2 and isinstance(any_d[0], str) and str(any_d[1]).startswith("/org/a11y"):
                ev["any_ref"] = (any_d[0], any_d[1])
            if len(body) > 4 and body[4]:
                ev["props"] = jsonable(body[4])
        else:
            ev["body"] = jsonable(body)
        self.events.append(ev)
        self.enricher.submit(ev)


class Snap:
    """The Shell's whole tree through the Cache interface (one call)."""

    def __init__(self, peer, bus):
        r = peer.call(bus, "/org/a11y/atspi/cache", "org.a11y.atspi.Cache", "GetItems", timeout=20000)
        self.err = None
        self.nodes = {}
        if iserr(r):
            self.err = r
            return
        for n in r[0]:
            (nb, npath), _app, (pb, ppath), idx, nch, ifs, name, role, desc, st = n
            self.nodes[npath] = {
                "path": npath.rsplit("/", 1)[-1], "bus": nb, "parent": ppath.rsplit("/", 1)[-1],
                "idx": idx, "nch": nch, "ifaces": [i.rsplit(".", 1)[-1] for i in ifs], "name": name,
                "role": role_name(role), "desc": desc, "states": states_of(st),
            }

    def chain(self, path, n=9):
        out = []
        while path in self.nodes and n > 0:
            nd = self.nodes[path]
            out.append(f"{nd['role']}:{nd['name'] or '-'}")
            path = "/org/a11y/atspi/accessible/" + nd["parent"]
            n -= 1
        return " < ".join(out)


ROLE_NAMES = (
    "invalid|accelerator-label|alert|animation|arrow|calendar|canvas|check-box|check-menu-item|color-chooser|"
    "column-header|combo-box|date-editor|desktop-icon|desktop-frame|dial|dialog|directory-pane|drawing-area|"
    "file-chooser|filler|focus-traversable|font-chooser|frame|glass-pane|html-container|icon|image|internal-frame|"
    "label|layered-pane|list|list-item|menu|menu-bar|menu-item|option-pane|page-tab|page-tab-list|panel|"
    "password-text|popup-menu|progress-bar|push-button|radio-button|radio-menu-item|root-pane|row-header|"
    "scroll-bar|scroll-pane|separator|slider|spin-button|split-pane|status-bar|table|table-cell|"
    "table-column-header|table-row-header|tearoff-menu-item|terminal|text|toggle-button|tool-bar|tool-tip|tree|"
    "tree-table|unknown|viewport|window|extended|header|footer|paragraph|ruler|application|autocomplete|"
    "edit-bar|embedded|entry|chart|caption|document-frame|heading|page|section|redundant-object|form|link|"
    "input-method-window|table-row|tree-item|document-spreadsheet|document-presentation|document-text|"
    "document-web|document-email|comment|list-box|grouping|image-map|notification|info-bar|level-bar|"
    "title-bar|block-quote|audio|video|definition|article|landmark|log|marquee|math|rating|timer|static|"
    "math-fraction|math-root|subscript|superscript|description-list|description-term|description-value|"
    "footnote|content-deletion|content-insertion|mark|suggestion|push-button-menu"
).split("|")


def role_name(n):
    return ROLE_NAMES[n] if isinstance(n, int) and 0 <= n < len(ROLE_NAMES) else str(n)


class Probe:
    def __init__(self, outdir):
        self.out = outdir
        os.makedirs(outdir, exist_ok=True)
        self.peer = Peer(A11Y)
        self.sess = Peer(SESSION)
        self.enr = Enricher()
        self.enr.start()
        self.cap = Capture(self.enr)
        self.cap.start()
        if not self.cap.ready.wait(60):
            sys.exit("capture thread never became ready")
        self.shell_bus = None
        self.clients = []   # [Popen]
        self.rd = None
        self.summary = []
        self.results = {}

    # ---- identity ---------------------------------------------------------------------
    def find_shell(self):
        reg = self.peer.call("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root", ACC, "GetChildren")
        apps = []
        if not iserr(reg):
            for bus, path in reg[0]:
                pid = self.peer.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                                     "GetConnectionUnixProcessID", (bus,), "(s)")
                pid = pid[0] if not iserr(pid) else None
                nm = self.peer.prop(bus, path, ACC, "Name")
                apps.append({"bus": bus, "path": path, "pid": pid, "name": nm})
                if pid == SHELL_PID:
                    self.shell_bus = bus
        return apps

    def identity(self):
        apps = self.find_shell()
        res = {"apps_on_a11y_bus": apps, "expected_shell_pid": SHELL_PID, "shell_bus": self.shell_bus}
        if not self.shell_bus:
            return res
        b = self.shell_bus
        root = "/org/a11y/atspi/accessible/root"
        res["root_role"] = self.peer.call(b, root, ACC, "GetRoleName")
        res["root_name"] = self.peer.prop(b, root, ACC, "Name")
        res["root_id"] = self.peer.prop(b, root, ACC, "Id") if False else None
        for pr in ("ToolkitName", "Version", "AtspiVersion", "Id"):
            res["app." + pr] = self.peer.prop(b, root, "org.a11y.atspi.Application", pr)
        res["root_children"] = self.peer.call(b, root, ACC, "GetChildren")
        res["root_interfaces"] = self.peer.call(b, root, ACC, "GetInterfaces")
        # the credentials path gorca's connector uses (GetConnectionCredentials on both buses)
        def creds(peer, name):
            r = peer.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                          "GetConnectionCredentials", (name,), "(s)")
            return r[0] if not iserr(r) else r
        res["a11y_bus_credentials"] = jsonable(creds(self.peer, b))
        own = self.sess.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner",
                             ("org.gnome.Shell",), "(s)")
        res["session_bus_org.gnome.Shell_owner"] = own
        if not iserr(own):
            res["session_bus_credentials"] = jsonable(creds(self.sess, own[0]))
        # what proc.rs reads
        try:
            stat = open(f"/proc/{SHELL_PID}/stat").read()
            f = stat[stat.rindex(")") + 2:].split()
            res["proc"] = {"pgrp": int(f[2]), "start_ticks": int(f[19])}
            cg = open(f"/proc/{SHELL_PID}/cgroup").read().strip()
            res["proc"]["cgroup"] = cg
        except Exception as e:  # noqa: BLE001
            res["proc"] = repr(e)
        res["shell_pid_matches_session_owner"] = (
            isinstance(res.get("session_bus_credentials"), dict)
            and res["session_bus_credentials"].get("ProcessID") == SHELL_PID)
        return res

    # ---- helpers ----------------------------------------------------------------------
    def eval(self, js, timeout=5000):
        r = self.sess.call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "Eval", (js,), "(s)", timeout)
        return r

    def sess_lockhint(self):
        """READ-ONLY: the host logind session's LockedHint (to prove the lock test did not change it)."""
        try:
            r = subprocess.run(["busctl", "--address=unix:path=/run/dbus/system_bus_socket", "get-property",
                                "org.freedesktop.login1", "/org/freedesktop/login1/session/auto",
                                "org.freedesktop.login1.Session", "LockedHint"], capture_output=True, text=True, timeout=10)
            return r.stdout.strip() or r.stderr.strip()
        except Exception as e:  # noqa: BLE001
            return repr(e)

    def gdbus(self, *args):
        return subprocess.run(["gdbus", "call", "--session", *args], capture_output=True, text=True, timeout=30)

    def notify(self, summary, body, actions="[]", hints="{}", replaces=0, timeout=-1, app="probe-app"):
        r = self.gdbus("--dest", "org.freedesktop.Notifications", "--object-path", "/org/freedesktop/Notifications",
                       "--method", "org.freedesktop.Notifications.Notify", "--", app, str(replaces), "dialog-information",
                       summary, body, actions, hints, str(timeout))
        m = re.search(r"uint32 (\d+)", r.stdout)
        return int(m.group(1)) if m else r.stderr.strip() or r.stdout.strip()

    def close_notification(self, nid):
        self.gdbus("--dest", "org.freedesktop.Notifications", "--object-path", "/org/freedesktop/Notifications",
                   "--method", "org.freedesktop.Notifications.CloseNotification", "--", str(nid))

    def rd_start(self):
        r = self.sess.call("org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop",
                           "org.gnome.Mutter.RemoteDesktop", "CreateSession")
        if iserr(r):
            self.rd = None
            return r
        self.rd = r[0]
        s = self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, "org.gnome.Mutter.RemoteDesktop.Session", "Start")
        return s

    def key(self, code, down):
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, "org.gnome.Mutter.RemoteDesktop.Session",
                       "NotifyKeyboardKeycode", (code, down), "(ub)")

    def tap(self, *codes, gap=0.03):
        for c in codes:
            self.key(c, True)
            time.sleep(gap)
        for c in reversed(codes):
            self.key(c, False)
            time.sleep(gap)

    def click(self):
        S = "org.gnome.Mutter.RemoteDesktop.Session"
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S, "NotifyPointerMotionAbsolute",
                       ("", 400.0, 400.0), "(sdd)") if False else None
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S, "NotifyPointerMotionRelative", (50.0, 50.0), "(dd)")
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S, "NotifyPointerButton", (272, True), "(ib)")
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S, "NotifyPointerButton", (272, False), "(ib)")

    def spawn_client(self, argv, extra_env=None):
        env = {k: os.environ[k] for k in ("PATH", "HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS",
                                          "AT_SPI_BUS_ADDRESS", "GSETTINGS_BACKEND", "LANG", "XDG_CONFIG_HOME",
                                          "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME", "WAYLAND_DISPLAY",
                                          "GORCA_HARNESS_DIR") if k in os.environ}
        env["GDK_BACKEND"] = "wayland"
        env.update(extra_env or {})
        log = open(os.path.join(self.out, "client-" + os.path.basename(argv[0]) + ".log"), "ab")
        p = subprocess.Popen(argv, env=env, stdout=log, stderr=log, stdin=subprocess.DEVNULL)
        self.clients.append(p)
        return p

    SHELL_STATE_JS = ("JSON.stringify({overview: Main.overview.visible, modalCount: Main.modalCount, "
                      "trayState: Main.messageTray._notificationState, banner: !!Main.messageTray._banner, "
                      "queue: Main.messageTray._notificationQueue.length, mode: Main.sessionMode.currentMode, "
                      "keyFocus: global.stage.key_focus ? global.stage.key_focus.constructor.name : null, "
                      "focusWindow: global.display.focus_window ? global.display.focus_window.get_title() : null, "
                      "qsOpen: Main.panel.statusArea.quickSettings.menu.isOpen})")

    def shell_state(self):
        r = self.eval(self.SHELL_STATE_JS)
        try:
            return json.loads(r[1]) if r and r[0] else r
        except Exception:  # noqa: BLE001
            return r

    def activity(self):
        """A harmless Shift tap: user activity (the banner timeout waits for it) without a visible effect."""
        self.tap(42)

    def windows(self):
        r = self.eval("JSON.stringify(global.get_window_actors().map(a=>({t:a.meta_window.get_title(),"
                      "app:a.meta_window.get_wm_class(),f:a.meta_window.has_focus()})))")
        try:
            return json.loads(r[1]) if r and r[0] else r
        except Exception:  # noqa: BLE001
            return r

    def wait_for(self, what, pred, timeout=60.0, every=0.4):
        end = time.time() + timeout
        while time.time() < end:
            try:
                v = pred()
            except Exception:  # noqa: BLE001
                v = None
            if v:
                return v
            time.sleep(every)
        print(f"  (timeout waiting for {what})", file=sys.stderr)
        return None

    def quiet(self, min_s=1.0, quiet_s=1.2, max_s=10.0):
        """Wait until no event arrived for quiet_s (after at least min_s), at most max_s."""
        t0 = time.time()
        while True:
            time.sleep(0.1)
            el = time.time() - t0
            if el >= max_s:
                return
            if el >= min_s and (now_ms() - self.cap.last_ms) / 1000.0 >= quiet_s:
                return

    def settle(self, quiet_s=4.0, max_s=90.0):
        """Wait until the Shell stops emitting startup noise."""
        t0 = time.time()
        while time.time() - t0 < max_s:
            time.sleep(0.25)
            if (now_ms() - self.cap.last_ms) / 1000.0 >= quiet_s:
                break
        return round(time.time() - t0, 1)

    # ---- scenario runner ---------------------------------------------------------------
    def scenario(self, name, trigger, min_s=1.0, quiet_s=1.2, max_s=10.0, note=""):
        print(f"== {name}", file=sys.stderr, flush=True)
        self.quiet(0.2, 0.8, 5.0)
        self.enr_drain()
        s0 = Snap(self.peer, self.shell_bus)
        for p_, n_ in s0.nodes.items():
            self.enr.known.setdefault((n_["bus"], p_), (n_["role"], n_["name"]))
        state0 = self.shell_state()
        start = len(self.cap.events)
        self._s0 = s0
        self._peaks = {}
        load = open("/proc/loadavg").read().split()[0]
        t0 = now_ms()
        marks = []

        def mark(label):
            marks.append({"t_rel_ms": round(now_ms() - t0, 1), "mark": label})

        try:
            detail = trigger(mark)
        except Exception as e:  # noqa: BLE001
            detail = f"TRIGGER EXCEPTION {e!r}"
        mark("trigger returned")
        self.quiet(min_s, quiet_s, max_s)
        self.enr_drain()
        s1 = Snap(self.peer, self.shell_bus)
        state1 = self.shell_state()
        evs = self.cap.events[start:]
        for ev in evs:
            ev["t_rel_ms"] = round(ev["t_ms"] - t0, 1)
        new_showing = self.diff(s0, s1)
        rec = {"scenario": name, "note": note, "load": load, "trigger_detail": jsonable(detail), "marks": marks,
               "peaks": self._peaks, "shell_state_before": state0, "shell_state_after": state1, "n_events": len(evs), "events": evs, "diff": new_showing,
               "snap_err": [s0.err, s1.err]}
        with open(os.path.join(self.out, f"{name}.json"), "w") as f:
            json.dump(jsonable(rec), f, indent=1, default=str)
        with open(os.path.join(self.out, f"{name}.events.txt"), "w") as f:
            f.write(self.render(rec))
        self.results[name] = rec
        print(self.render(rec, brief=True), file=sys.stderr, flush=True)
        return rec

    def peak(self, label):
        """Snapshot the Shell tree NOW and keep its diff against the scenario's start (what is on screen
        at the peak: transient UI such as a banner is gone by the end-of-scenario diff)."""
        snap = Snap(self.peer, self.shell_bus)
        self._peaks[label] = self.diff(self._s0, snap)
        self._peaks[label]["state"] = self.shell_state()
        return self._peaks[label]["stats"]

    def enr_drain(self, timeout=30.0):
        end = time.time() + timeout
        while not self.enr.idle() and time.time() < end:
            time.sleep(0.05)

    def diff(self, a, b):
        """Nodes that appeared / were removed / changed name / changed showing, with text."""
        out = {"added_showing": [], "removed": [], "renamed": [], "showing_changed": [], "stats": {
            "before": len(a.nodes), "after": len(b.nodes)}}
        if a.err or b.err:
            return out
        for p, n in b.nodes.items():
            o = a.nodes.get(p)
            show = "showing" in n["states"]
            if o is None:
                if show and (n["name"] or "Text" in n["ifaces"] or n["role"] not in ("panel", "filler", "text")):
                    out["added_showing"].append(self.node_view(n, b))
            else:
                if o["name"] != n["name"] and (n["name"] or o["name"]):
                    out["renamed"].append({"path": n["path"], "role": n["role"], "old": o["name"], "new": n["name"],
                                           "showing": show, "chain": b.chain(p)})
                if ("showing" in o["states"]) != show and (n["name"] or n["role"] in ("notification", "dialog", "alert", "window", "menu", "popup menu")):
                    out["showing_changed"].append({"path": n["path"], "role": n["role"], "name": n["name"],
                                                   "now_showing": show, "chain": b.chain(p)})
        for p, o in a.nodes.items():
            if p not in b.nodes and (o["name"] or o["role"] in ("notification", "dialog")):
                out["removed"].append({"path": o["path"], "role": o["role"], "name": o["name"], "states": o["states"]})
        # text for new showing nodes without a name but with Text
        for v in out["added_showing"]:
            if not v["name"] and "Text" in v["ifaces"]:
                full = "/org/a11y/atspi/accessible/" + v["path"]
                n = self.peer.prop(b.nodes[full]["bus"], full, "org.a11y.atspi.Text", "CharacterCount")
                if isinstance(n, int) and n:
                    t = self.peer.call(b.nodes[full]["bus"], full, "org.a11y.atspi.Text", "GetText", (0, min(n, 300)), "(ii)")
                    v["text"] = t[0] if not iserr(t) else str(t)
        for lst in ("added_showing", "removed", "renamed", "showing_changed"):
            out[lst] = out[lst][:200]
        return out

    def node_view(self, n, snap):
        return {"path": n["path"], "role": n["role"], "name": n["name"], "desc": n["desc"], "ifaces": n["ifaces"],
                "states": n["states"], "chain": snap.chain("/org/a11y/atspi/accessible/" + n["path"])}

    def render(self, rec, brief=False):
        L = [f"# {rec['scenario']}  (load {rec['load']}; {rec['n_events']} events)  {rec['note']}",
             f"trigger: {json.dumps(rec['trigger_detail'], default=str)[:400]}",
             f"shell state before: {json.dumps(rec.get('shell_state_before'))}",
             f"shell state after : {json.dumps(rec.get('shell_state_after'))}"]
        for m in rec["marks"]:
            L.append(f"  [{m['t_rel_ms']:>9.1f}ms] MARK {m['mark']}")
        evs = rec["events"]
        cnt = collections.Counter((e["iface"], e["member"], e.get("detail", "")) for e in evs)
        L.append("event counts: " + "; ".join(f"{k[0]}.{k[1]}[{k[2]}] x{v}" for k, v in cnt.most_common(25)))
        lim = 40 if brief else 10 ** 9
        for e in evs[:lim]:
            src = e.get("src", {})
            par = src.get("parent")
            line = (f"  [{e['t_rel_ms']:>9.1f}ms] #{e['seq']} {e['iface']}.{e['member']} '{e.get('detail', '')}' "
                    f"d1={e.get('d1')} d2={e.get('d2')} any={json.dumps(e.get('any'), default=str)[:120]} "
                    f"src={e['sender']}{e['src_path'].rsplit('/', 1)[-1]} {src.get('role')}:{src.get('name')!r}")
            if src.get("text"):
                line += f" text={src['text']!r}"
            if "value.CurrentValue" in src:
                line += f" value={src['value.CurrentValue']}"
            if not brief:
                line += f" states={','.join(src.get('states', [])) if isinstance(src.get('states'), list) else src.get('states')}"
                if e.get("any_src"):
                    a = e["any_src"]
                    line += f" | child={a.get('role')}:{a.get('name')!r}"
                if isinstance(par, dict):
                    line += f" | parent={par.get('role')}:{par.get('name')!r}"
                if e.get("props"):
                    line += f" props={json.dumps(e['props'], default=str)[:100]}"
                if "relations" in src:
                    line += f" rel={src['relations']}"
            L.append(line)
        if brief and len(evs) > lim:
            L.append(f"  ... {len(evs) - lim} more")
        d = rec["diff"]
        L.append(f"tree diff: nodes {d['stats']['before']} -> {d['stats']['after']}")
        for k in ("added_showing", "renamed", "showing_changed", "removed"):
            for v in d[k][: (12 if brief else 200)]:
                L.append(f"  {k}: " + json.dumps(v, default=str)[:300])
        return "\n".join(L) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    only = set(filter(None, a.only.split(",")))
    pr = Probe(a.out)
    try:
        import scenarios
        scenarios.run(pr, only)
    finally:
        pr.enr.stop = True
        for p in pr.clients:
            if p.poll() is None:
                p.send_signal(signal.SIGTERM)
        time.sleep(1)
        for p in pr.clients:
            if p.poll() is None:
                p.kill()
        with open(os.path.join(a.out, "_summary.json"), "w") as f:
            json.dump(jsonable({k: {"n_events": v["n_events"], "load": v["load"]} for k, v in pr.results.items()}), f, indent=1)
        sys.stdout.flush()
        os._exit(0)


if __name__ == "__main__":
    sys.path.insert(0, HERE)
    sys.modules["probe_shell"] = sys.modules["__main__"]
    main()
