#!/usr/bin/python3
"""fakeapp.py - a scriptable fake AT-SPI application for the gorca test harness.

It speaks the AT-SPI2 D-Bus protocol directly (no toolkit) on the a11y bus, so every
byte on the wire is under test control: Accessible, Component, Text, EditableText, Action,
Application and Cache interfaces, the Registry `Socket.Embed` handshake and the
`org.a11y.atspi.Event.*` signals.

Run it ONLY inside run-isolated.sh, with the system interpreter (needs python3-dbus + PyGObject):

    tests/harness/run-isolated.sh /usr/bin/python3 tests/harness/fakeapp.py --scenario form

Control channel: JSON lines on stdin (one command per line, optional "id" echoed back);
JSON lines on stdout ("event": ready | reply | action | ...). EOF on stdin terminates the app.

Scenarios: form, tree, storm, hung, terminal, malformed, switcher, review (see README.md and `--help`).
"""
import argparse
import json
import os
import random
import re
import sys
import time

import dbus
import dbus.bus
import dbus.exceptions
import dbus.lowlevel
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

BASE = "/org/a11y/atspi/accessible"
ROOT = BASE + "/root"
CACHE_PATH = "/org/a11y/atspi/cache"
NULL_REF = ("", "/org/a11y/atspi/null")
I = "org.a11y.atspi."
IF_ACC, IF_COMP, IF_TEXT, IF_EDIT, IF_ACT, IF_APP, IF_CACHE = (
    I + "Accessible", I + "Component", I + "Text", I + "EditableText", I + "Action",
    I + "Application", I + "Cache")
IF_COLL, IF_SEL = I + "Collection", I + "Selection"
IF_VALUE = I + "Value"
IF_PROPS = "org.freedesktop.DBus.Properties"
EV_OBJ, EV_WIN, EV_FOCUS = I + "Event.Object", I + "Event.Window", I + "Event.Focus"

# Role numbers: Atspi.Role (verified against pyatspi in the smoke test).
R_APP, R_FRAME, R_PANEL, R_LABEL, R_BUTTON, R_ENTRY, R_TERMINAL, R_TEXT, R_CHECK, R_LIST_ITEM = (
    75, 23, 39, 29, 43, 79, 60, 61, 7, 32)
# Review trees (ADR-0006): roles by name, Atspi.Role numbers.
ROLE_BY_NAME = {"application": R_APP, "frame": R_FRAME, "window": 69, "dialog": 16, "alert": 2,
                "panel": R_PANEL, "filler": 20, "label": R_LABEL, "push button": R_BUTTON,
                "button": R_BUTTON, "entry": R_ENTRY, "text": R_TEXT, "check box": R_CHECK,
                "list": 31, "list item": R_LIST_ITEM, "status bar": 54, "menu": 33,
                "menu item": 35, "static": 116, "paragraph": 73, "heading": 83, "section": 85,
                "terminal": R_TERMINAL, "notification": 101, "info bar": 102, "level bar": 103,
                "slider": 51, "toggle button": 62, "tool tip": 64}
ROLE_NAMES = {R_APP: "application", R_FRAME: "frame", R_PANEL: "panel", R_LABEL: "label",
              R_BUTTON: "push button", R_ENTRY: "entry", R_TERMINAL: "terminal", R_TEXT: "text",
              R_CHECK: "check box", R_LIST_ITEM: "list item"}
# Atspi.StateType numbers.
S_ACTIVE, S_CHECKED, S_EDITABLE, S_ENABLED, S_FOCUSABLE, S_FOCUSED, S_MULTI_LINE = 1, 4, 7, 8, 11, 12, 17
S_PRESSED, S_SENSITIVE, S_SHOWING, S_SINGLE_LINE, S_VISIBLE = 20, 24, 25, 26, 30
STATE_NAMES = {"active": S_ACTIVE, "checked": S_CHECKED, "editable": S_EDITABLE, "enabled": S_ENABLED,
               "focusable": S_FOCUSABLE, "focused": S_FOCUSED, "pressed": S_PRESSED,
               "sensitive": S_SENSITIVE, "showing": S_SHOWING, "visible": S_VISIBLE,
               "busy": 3, "defunct": 6, "expanded": 10, "selected": 23, "selectable": 22, "modal": 16}
BASE_STATES = (S_ENABLED, S_SENSITIVE, S_SHOWING, S_VISIBLE)
COORD_MODES = ("default", "window", "global")
MALFORMED_MODES = ("bad_refs", "neg_offsets", "bad_payload", "count_mismatch", "bad_role_state", "huge")


def out(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def path_of(nid):
    return ROOT if nid == 0 else "%s/%d" % (BASE, nid)


class Node:
    __slots__ = ("id", "parent", "children", "role", "name", "desc", "states", "text", "caret",
                 "actions", "editable", "extents", "no_text_extents", "hang", "sel", "value")

    def __init__(self, nid, parent, role, name, text=None, editable=False, actions=(), states=(),
                 extents=None, no_text_extents=False, value=None):
        self.id = nid
        self.parent = parent
        self.children = []
        self.role = role
        self.name = name
        self.desc = ""
        self.states = set(BASE_STATES) | set(states)
        self.text = text
        self.caret = 0
        self.actions = list(actions)
        self.editable = editable
        self.extents = tuple(extents) if extents else None   # (x, y, w, h), see App.coord
        self.no_text_extents = no_text_extents                # GTK4 editables: F-3
        self.hang = False                                     # never answer any call (OB-4)
        self.sel = []                                         # selected ranges [(start, end)], M3
        self.value = tuple(value) if value else None          # (current, min, max): Value interface, M4b


class ApiError(Exception):
    def __init__(self, name, msg):
        super().__init__(msg)
        self.dbus_name = name


class HangPolicy:
    """never: swallow the call (no reply); delay: reply after `ms`; off."""

    def __init__(self):
        self.mode = "off"
        self.ms = 0
        self.methods = None   # None = every method, else a set of member names
        self.after = 0        # serve this many calls normally before hanging

    def configure(self, mode, ms=0, methods=None, after=0):
        self.mode, self.ms, self.after = mode, int(ms), int(after)
        self.methods = set(methods) if methods else None

    def applies(self, member):
        if self.mode == "off":
            return False
        if self.methods is not None and member not in self.methods:
            return False
        if self.after > 0:
            self.after -= 1
            return False
        return True


class App:
    def __init__(self, args, conn):
        self.args = args
        self.conn = conn
        self.unique = conn.get_unique_name()
        self.rng = random.Random(args.seed)
        self.nodes = []              # id -> Node (None = removed)
        self.by_name = {}
        self.focus_id = None
        self.hang = HangPolicy()
        self.malformed = set()
        self.seq = 0
        self.t_focus_ns = 0
        self.desktop_ref = NULL_REF
        self.stats = {"calls": 0, "hung": 0, "delayed": 0, "events": 0, "storm_events": 0,
                      "term_frames": 0, "term_bytes": 0, "bad_values": 0}
        self.storm = None
        self.term = None
        self.term_source = None
        self.term_node = None
        self.term_timer = None
        self.cache_signals = True
        self.coord = "default"       # Component.GetExtents flavour: default | window | global
        self.review = False          # a review tree was built: Collection/Selection exist
        self.member_calls = {}       # "Interface.Member" -> calls served since the last reset
        self.first_emit_ns = 0       # CLOCK_MONOTONIC ns of the first signal of a text command (M3)
        self.t_caret_set_ns = 0      # when Text.SetCaretOffset last moved a caret (say-all stop)
        self.caret_sets = 0

    # ---- tree construction -------------------------------------------------------------------
    def add(self, parent, role, name, **kw):
        nid = len(self.nodes)
        n = Node(nid, parent, role, name, **kw)
        self.nodes.append(n)
        if parent is not None:
            self.nodes[parent].children.append(nid)
        if name:
            self.by_name.setdefault(name, nid)
        return n

    def node(self, ref):
        """Resolve a name, an int id or a path to a Node, or None."""
        if isinstance(ref, str):
            if ref in self.by_name:
                ref = self.by_name[ref]
            elif ref == ROOT:
                ref = 0
            elif ref.startswith(BASE + "/") and ref[len(BASE) + 1:].isdigit():
                ref = int(ref[len(BASE) + 1:])
            else:
                return None
        if isinstance(ref, int) and 0 <= ref < len(self.nodes):
            return self.nodes[ref]
        return None

    def node_for_path(self, path):
        if path == ROOT:
            return self.nodes[0]
        if path.startswith(BASE + "/"):
            tail = path[len(BASE) + 1:]
            if tail.isdigit():
                return self.node(int(tail))
        return None

    def ifaces(self, n):
        r = [IF_ACC, IF_COMP]
        if n.id == 0:
            r.append(IF_APP)
        if n.text is not None:
            r.append(IF_TEXT)
        if n.editable:
            r.append(IF_EDIT)
        if n.actions:
            r.append(IF_ACT)
        if n.value is not None:
            r.append(IF_VALUE)
        if self.review and n.children:
            r.extend((IF_COLL, IF_SEL))
        return r

    def ref(self, n):
        return (self.unique, path_of(n.id))

    def state_words(self, n):
        lo = hi = 0
        for s in n.states:
            if s < 32:
                lo |= 1 << s
            else:
                hi |= 1 << (s - 32)
        return [dbus.UInt32(lo), dbus.UInt32(hi)]

    # ---- malformed-data helpers --------------------------------------------------------------
    def bad(self, mode):
        if mode in self.malformed:
            self.stats["bad_values"] += 1
            return True
        return False

    def corrupt_ref(self, n):
        """A syntactically valid (so) that points nowhere useful."""
        k = self.rng.randrange(7)
        self_ref = self.ref(n)
        return [NULL_REF, (":1.999999", BASE + "/1"), ("!!not a bus name!!", BASE + "/1"),
                ("", "/"), self_ref, (self.unique, BASE + "/2147483647"),
                (self.unique, BASE + "/0")][k]

    def child_ref(self, n, cid):
        c = self.nodes[cid]
        if self.bad("bad_refs") and self.rng.random() < 0.5:
            return self.corrupt_ref(n)
        return self.ref(c)

    # ---- event emission ----------------------------------------------------------------------
    def emit(self, path, iface, member, detail, d1, d2, data, props=None):
        if self.first_emit_ns == 0:
            self.first_emit_ns = time.monotonic_ns()
        self.seq += 1
        p = dbus.Dictionary({"seq": dbus.UInt64(self.seq, variant_level=1)}, signature="sv")
        if props:
            p.update(props)
        msg = dbus.lowlevel.SignalMessage(path, iface, member)
        msg.append(detail, dbus.Int32(d1), dbus.Int32(d2), data, p, signature="siiva{sv}")
        self.conn.send_message(msg)
        self.stats["events"] += 1

    def ev_state(self, n, state, value):
        name = {v: k for k, v in STATE_NAMES.items()}.get(state, str(state))
        self.emit(path_of(n.id), EV_OBJ, "StateChanged", name, 1 if value else 0, 0, dbus.Int32(0))

    def ev_text(self, n, kind, offset, length, text):
        data = dbus.String(text)
        d1, d2 = offset, length
        if self.bad("neg_offsets"):
            d1, d2 = self.rng.choice([(-1, 0), (0, -5), (2**31 - 1, 2**31 - 1), (-2**31, 2**31 - 1),
                                      (10**6, 3), (offset, length + 1000)])
        if self.bad("bad_payload"):
            data = self.rng.choice([dbus.Int32(7), dbus.Struct(("x", "/y"), signature="so"),
                                    dbus.Array([], signature="s"), dbus.String("")])
        self.emit(path_of(n.id), EV_OBJ, "TextChanged", kind, d1, d2, data)

    def ev_caret(self, n):
        off = n.caret
        if self.bad("neg_offsets"):
            off = self.rng.choice([-1, -2**31, 2**31 - 1, 10**7])
        self.emit(path_of(n.id), EV_OBJ, "TextCaretMoved", "", off, 0, dbus.Int32(0))

    def ev_children(self, parent, kind, index, child):
        ref = dbus.Struct((self.unique, path_of(child.id)), signature="so")
        data = ref
        detail = kind
        if self.bad("bad_payload"):
            data = self.rng.choice([dbus.Int32(7), dbus.String("not a ref"),
                                    dbus.Struct(("", ""), signature="ss")])
            detail = self.rng.choice([kind, "", "x" * 300, "ADD"])
        if self.bad("neg_offsets"):
            index = self.rng.choice([-1, 2**31 - 1, -2**31])
        self.emit(path_of(parent.id), EV_OBJ, "ChildrenChanged", detail, index, 0, data)

    def ev_cache(self, member, n):
        if member == "AddAccessible":
            msg = dbus.lowlevel.SignalMessage(CACHE_PATH, IF_CACHE, "AddAccessible")
            msg.append(self.cache_item(n), signature="((so)(so)(so)iiassusau)")
        else:
            msg = dbus.lowlevel.SignalMessage(CACHE_PATH, IF_CACHE, "RemoveAccessible")
            msg.append(self.ref(n), signature="(so)")
        self.conn.send_message(msg)

    def cache_item(self, n):
        parent = self.nodes[n.parent] if n.parent is not None else None
        pref = self.ref(parent) if parent else (self.desktop_ref if n.id == 0 else NULL_REF)
        idx = parent.children.index(n.id) if parent else -1
        return (self.ref(n), (self.unique, ROOT), pref, dbus.Int32(idx), dbus.Int32(len(n.children)),
                dbus.Array(self.ifaces(n), signature="s"), n.name, dbus.UInt32(n.role), n.desc,
                dbus.Array(self.state_words(n), signature="u"))

    # ---- mutations (shared by control channel and D-Bus calls) -------------------------------
    def set_focus(self, n):
        old = self.nodes[self.focus_id] if self.focus_id is not None else None
        if old is n:
            return
        if old is not None:
            old.states.discard(S_FOCUSED)
            self.ev_state(old, S_FOCUSED, False)
        self.focus_id = n.id
        n.states.add(S_FOCUSED)
        # Latency probes (crates/gorca-bench e2e): CLOCK_MONOTONIC ns taken right before the
        # focused=1 signal is queued, then flushed so it leaves this process now, not at the
        # next main-loop iteration.
        self.t_focus_ns = time.monotonic_ns()
        self.ev_state(n, S_FOCUSED, True)
        self.conn.flush()
        self.emit(path_of(n.id), EV_FOCUS, "Focus", "", 0, 0, dbus.Int32(0))

    def text_insert(self, n, pos, s):
        pos = max(0, min(pos, len(n.text)))
        n.text = n.text[:pos] + s + n.text[pos:]
        self.ev_text(n, "insert", pos, len(s), s)
        n.caret = pos + len(s)
        self.ev_caret(n)

    def text_delete(self, n, start, end):
        end = len(n.text) if end < 0 else min(end, len(n.text))
        start = max(0, start)
        if start >= end:
            return False
        gone = n.text[start:end]
        n.text = n.text[:start] + n.text[end:]
        self.ev_text(n, "delete", start, len(gone), gone)
        n.caret = start
        self.ev_caret(n)
        return True

    def do_action(self, n, idx):
        if not (0 <= idx < len(n.actions)):
            return False
        act = n.actions[idx]
        out({"event": "action", "node": n.name, "path": path_of(n.id), "action": act})
        if n.role == R_CHECK:
            if S_CHECKED in n.states:
                n.states.discard(S_CHECKED)
            else:
                n.states.add(S_CHECKED)
            self.ev_state(n, S_CHECKED, S_CHECKED in n.states)
        else:
            n.states.add(S_PRESSED)
            self.ev_state(n, S_PRESSED, True)
            n.states.discard(S_PRESSED)
            self.ev_state(n, S_PRESSED, False)
        return True

    def add_child(self, parent, role, name, **kw):
        n = self.add(parent.id, role, name, **kw)
        self.ev_cache("AddAccessible", n)
        self.ev_children(parent, "add", len(parent.children) - 1, n)
        return n

    def remove_node(self, n):
        for c in list(n.children):
            self.remove_node(self.nodes[c])
        p = self.nodes[n.parent]
        idx = p.children.index(n.id)
        p.children.remove(n.id)
        self.ev_children(p, "remove", idx, n)
        self.ev_cache("RemoveAccessible", n)
        self.nodes[n.id] = None
        if self.by_name.get(n.name) == n.id:
            del self.by_name[n.name]
        if self.focus_id == n.id:
            self.focus_id = None


# ---- D-Bus method implementations: (iface, member) -> (in_sig, out_sig, fn(app, node, *args)) ---
TABLE = {}


def impl(iface, member, in_sig, out_sig):
    def deco(fn):
        TABLE[(iface, member)] = (in_sig, out_sig, fn)
        return fn
    return deco


def _text_at(n, off, gran):
    t = n.text
    off = max(0, min(off, len(t)))
    if gran == 0:                                  # char
        return (t[off:off + 1], off, min(off + 1, len(t)))
    if gran == 3 or gran == 4:                     # line / paragraph
        s = t.rfind("\n", 0, off) + 1
        e = t.find("\n", off)
        e = len(t) if e < 0 else e + 1
        return (t[s:e], s, e)
    if gran == 1:                                  # word
        for m in re.finditer(r"\S+\s*", t):
            if m.start() <= off < m.end():
                return (m.group(), m.start(), m.end())
        return ("", off, off)
    return (t, 0, len(t))                          # sentence: whole text


@impl(IF_ACC, "GetChildAtIndex", "i", "(so)")
def _(app, n, i):
    if app.bad("bad_refs") and app.rng.random() < 0.3:
        return (app.corrupt_ref(n),)
    if not 0 <= i < len(n.children):
        raise ApiError("org.freedesktop.DBus.Error.InvalidArgs", "index out of range")
    return (app.child_ref(n, n.children[i]),)


@impl(IF_ACC, "GetChildren", "", "a(so)")
def _(app, n):
    refs = [app.child_ref(n, c) for c in n.children]
    if app.bad("count_mismatch"):
        refs = refs[:-1] if refs and app.rng.random() < 0.5 else refs + [app.ref(n)]
    return (dbus.Array(refs, signature="(so)"),)


@impl(IF_ACC, "GetIndexInParent", "", "i")
def _(app, n):
    if n.parent is None:
        return (-1,)
    if app.bad("count_mismatch"):
        return (len(app.nodes[n.parent].children) + 5,)
    return (app.nodes[n.parent].children.index(n.id),)


@impl(IF_ACC, "GetRelationSet", "", "a(ua(so))")
def _(app, n):
    return (dbus.Array([], signature="(ua(so))"),)


@impl(IF_ACC, "GetRole", "", "u")
def _(app, n):
    return (dbus.UInt32(9999 if app.bad("bad_role_state") else n.role),)


@impl(IF_ACC, "GetRoleName", "", "s")
def _(app, n):
    return (ROLE_NAMES.get(n.role, "unknown"),)


@impl(IF_ACC, "GetLocalizedRoleName", "", "s")
def _(app, n):
    return (ROLE_NAMES.get(n.role, "unknown"),)


@impl(IF_ACC, "GetState", "", "au")
def _(app, n):
    if app.bad("bad_role_state"):
        k = app.rng.randrange(3)
        return (dbus.Array([dbus.UInt32(0xFFFFFFFF)] * (0, 1, 5)[k], signature="u"),)
    return (dbus.Array(app.state_words(n), signature="u"),)


@impl(IF_ACC, "GetAttributes", "", "a{ss}")
def _(app, n):
    return (dbus.Dictionary({"id": str(n.id)}, signature="ss"),)


@impl(IF_ACC, "GetApplication", "", "(so)")
def _(app, n):
    if app.bad("bad_refs") and app.rng.random() < 0.5:
        return (app.corrupt_ref(n),)
    return ((app.unique, ROOT),)


@impl(IF_ACC, "GetInterfaces", "", "as")
def _(app, n):
    r = app.ifaces(n)
    if app.bad("bad_role_state"):
        r = r + ["org.example.Bogus", ""]
    return (dbus.Array(r, signature="s"),)


@impl(IF_COMP, "Contains", "iiu", "b")
def _(app, n, x, y, t):
    return (False,)


@impl(IF_COMP, "GetAccessibleAtPoint", "iiu", "(so)")
def _(app, n, x, y, t):
    return (NULL_REF,)


def _extents(n):
    if n.extents:
        return n.extents
    return (n.id % 100 * 10, n.id // 100 % 100 * 10, 80, 20)


def _coord_extents(app, n, t):
    """GetExtents by coordinate flavour (ADR-0006 F-1, F-5): `window` is GTK4 (SCREEN has no
    origin), `global` is St (ignores the coordinate type), `default` the pre-review behaviour."""
    e = _extents(n)
    if app.coord == "window" and t == 0:
        return (0, 0, e[2], e[3])
    return e


@impl(IF_COMP, "GetExtents", "u", "(iiii)")
def _(app, n, t):
    return (dbus.Struct(tuple(map(dbus.Int32, _coord_extents(app, n, t))), signature="iiii"),)


@impl(IF_COMP, "GetPosition", "u", "ii")
def _(app, n, t):
    e = _coord_extents(app, n, t)
    return (e[0], e[1])


@impl(IF_COMP, "GetSize", "", "ii")
def _(app, n):
    return (80, 20)


@impl(IF_COMP, "GetLayer", "", "u")
def _(app, n):
    return (dbus.UInt32(3),)


@impl(IF_COMP, "GetMDIZOrder", "", "n")
def _(app, n):
    return (dbus.Int16(0),)


@impl(IF_COMP, "GrabFocus", "", "b")
def _(app, n):
    out({"event": "grab_focus", "node": n.name, "path": path_of(n.id)})
    if S_FOCUSABLE in n.states:
        app.set_focus(n)
        return (True,)
    return (False,)


@impl(IF_COMP, "GetAlpha", "", "d")
def _(app, n):
    return (1.0,)


@impl(IF_TEXT, "GetText", "ii", "s")
def _(app, n, start, end):
    t = n.text
    if app.term_node is n and (start < 0 or end < -1):
        # VTE: g_assert((start_offset >= 0) && (end_offset >= -1)) aborts the whole terminal server.
        app.stats["vte_assert"] = app.stats.get("vte_assert", 0) + 1
    if end < 0:
        end = len(t)
    if app.bad("huge"):
        return (t + "x" * 2_000_000,)
    return (t[max(0, start):max(0, end)] if start <= end else "",)


@impl(IF_TEXT, "SetCaretOffset", "i", "b")
def _(app, n, off):
    if not 0 <= off <= len(n.text):
        return (False,)
    n.caret = off
    app.t_caret_set_ns = time.monotonic_ns()
    app.caret_sets += 1
    app.ev_caret(n)
    return (True,)


@impl(IF_TEXT, "GetCharacterAtOffset", "i", "i")
def _(app, n, off):
    return (ord(n.text[off]) if 0 <= off < len(n.text) else -1,)


def _tx(app, n, off, gran):
    s, a, b = _text_at(n, off, gran)
    if app.bad("neg_offsets"):
        a, b = app.rng.choice([(-1, -1), (b, a), (0, 2**31 - 1), (-2**31, 0)])
    return (s, a, b)


for _m in ("GetTextAtOffset", "GetStringAtOffset", "GetTextBeforeOffset", "GetTextAfterOffset"):
    impl(IF_TEXT, _m, "iu", "sii")(lambda app, n, off, gran: _tx(app, n, off, gran))


@impl(IF_TEXT, "GetNSelections", "", "i")
def _(app, n):
    return (len(n.sel),)


@impl(IF_TEXT, "GetSelection", "i", "ii")
def _(app, n, i):
    if not 0 <= i < len(n.sel):
        raise ApiError("org.freedesktop.DBus.Error.InvalidArgs", "no such selection")
    return n.sel[i]


@impl(IF_TEXT, "GetAttributes", "i", "a{ss}ii")
def _(app, n, off):
    return (dbus.Dictionary({}, signature="ss"), 0, len(n.text))


@impl(IF_TEXT, "GetAttributeRun", "ib", "a{ss}ii")
def _(app, n, off, defaults):
    return (dbus.Dictionary({}, signature="ss"), 0, len(n.text))


@impl(IF_TEXT, "GetDefaultAttributes", "", "a{ss}")
def _(app, n):
    return (dbus.Dictionary({}, signature="ss"),)


def _line_starts(n):
    """Offsets where each line of the text starts (a trailing newline ends the last line)."""
    starts, pos = [0], 0
    for part in n.text.split("\n")[:-1]:
        pos += len(part) + 1
        starts.append(pos)
    return starts


def _pos_of(n, off):
    """(line index, column) of an offset."""
    starts = _line_starts(n)
    off = max(0, min(off, len(n.text)))
    li = max(i for i, s0 in enumerate(starts) if s0 <= off)
    return li, off - starts[li]


def _no_ext(n):
    if n.no_text_extents:
        raise ApiError("org.freedesktop.DBus.Error.NotSupported", "extents on an editable")


@impl(IF_TEXT, "GetCharacterExtents", "iu", "iiii")
def _(app, n, off, t):
    _no_ext(n)
    if n.extents:
        li, col = _pos_of(n, off)
        return (n.extents[0] + col * 8, n.extents[1] + li * 16, 8, 16)
    return (off * 8, 0, 8, 16)


@impl(IF_TEXT, "GetRangeExtents", "iiu", "iiii")
def _(app, n, a, b, t):
    _no_ext(n)
    if n.extents:
        # Window-relative (or stage) rectangle of a range that lies on one line: first line of
        # the range decides y; the width counts characters up to the end of that line.
        la, ca = _pos_of(n, a)
        lb, cb = _pos_of(n, b)
        end_col = cb if lb == la else len(n.text.split("\n")[la])
        return (n.extents[0] + ca * 8, n.extents[1] + la * 16, max(1, end_col - ca) * 8, 16)
    return (a * 8, 0, max(0, b - a) * 8, 16)


@impl(IF_TEXT, "GetOffsetAtPoint", "iiu", "i")
def _(app, n, x, y, t):
    if n.extents:
        lines = n.text.split("\n")
        starts = _line_starts(n)
        li = max(0, min(len(lines) - 1, (y - n.extents[1]) // 16))
        col = max(0, min(len(lines[li]), (x - n.extents[0]) // 8))
        return (starts[li] + col,)
    return (max(0, min(len(n.text), x // 8)),)


def _within(app, root, n):
    while n is not None:
        if n is root:
            return True
        n = app.nodes[n.parent] if n.parent is not None else None
    return False


@impl(IF_COLL, "GetMatches", "(aiia{ss}iaiiasib)uib", "a(so)")
def _(app, n, rule, sortby, count, traverse):
    states, smt, _attrs, _amt, roles, rmt, _ifaces, _imt, invert = rule
    want = (int(states[0]) & 0xFFFFFFFF) | ((int(states[1]) & 0xFFFFFFFF) << 32 if len(states) > 1 else 0)
    role_set = {int(r) for r in roles if int(r) != 0}
    hits = []
    for x in app.nodes:
        if x is None or x is n or not _within(app, n, x):
            continue
        if not traverse and x.parent != n.id:
            continue
        bits = 0
        for s0 in x.states:
            bits |= 1 << s0
        if smt == 1:
            ok_s = (bits & want) == want
        elif smt == 2:
            ok_s = bool(bits & want)
        elif smt == 3:
            ok_s = not (bits & want)
        else:
            ok_s = True
        if rmt == 2:
            ok_r = x.role in role_set
        elif rmt == 1:
            ok_r = bool(role_set) and role_set == {x.role}
        elif rmt == 3:
            ok_r = x.role not in role_set
        else:
            ok_r = True
        if (ok_s and ok_r) != bool(invert):
            hits.append(app.ref(x))
        if count > 0 and len(hits) >= count:
            break
    return (dbus.Array(hits, signature="(so)"),)


@impl(IF_SEL, "SelectChild", "i", "b")
def _(app, n, i):
    if not 0 <= i < len(n.children):
        return (False,)
    c = app.nodes[n.children[i]]
    out({"event": "select_child", "parent": n.name, "node": c.name, "index": i})
    c.states.add(STATE_NAMES["selected"])
    app.ev_state(c, STATE_NAMES["selected"], True)
    return (True,)


@impl(IF_EDIT, "SetTextContents", "s", "b")
def _(app, n, s):
    if n.text:
        app.text_delete(n, 0, -1)
    if s:
        app.text_insert(n, 0, s)
    return (True,)


@impl(IF_EDIT, "InsertText", "isi", "b")
def _(app, n, pos, s, ln):
    app.text_insert(n, pos, s[:ln] if ln >= 0 else s)
    return (True,)


@impl(IF_EDIT, "DeleteText", "ii", "b")
def _(app, n, a, b):
    return (app.text_delete(n, a, b),)


@impl(IF_ACT, "GetName", "i", "s")
def _(app, n, i):
    return (n.actions[i] if 0 <= i < len(n.actions) else "",)


@impl(IF_ACT, "GetLocalizedName", "i", "s")
def _(app, n, i):
    return (n.actions[i] if 0 <= i < len(n.actions) else "",)


@impl(IF_ACT, "GetDescription", "i", "s")
def _(app, n, i):
    return ("",)


@impl(IF_ACT, "GetKeyBinding", "i", "s")
def _(app, n, i):
    return ("",)


@impl(IF_ACT, "GetActions", "", "a(sss)")
def _(app, n):
    return (dbus.Array([(a, "", "") for a in n.actions], signature="(sss)"),)


@impl(IF_ACT, "DoAction", "i", "b")
def _(app, n, i):
    return (app.do_action(n, i),)


@impl(IF_APP, "GetLocale", "u", "s")
def _(app, n, t):
    return ("C",)


@impl(IF_APP, "GetApplicationBusAddress", "", "s")
def _(app, n):
    return ("",)


@impl(IF_CACHE, "GetItems", "", "a((so)(so)(so)iiassusau)")
def _(app, n):
    items = [app.cache_item(x) for x in app.nodes if x is not None]
    return (dbus.Array(items, signature="((so)(so)(so)iiassusau)"),)


def _prop_table(app, n):
    """(iface, prop) -> value for the node."""
    p = {}
    parent = app.nodes[n.parent] if n.parent is not None else None
    pref = app.ref(parent) if parent else (app.desktop_ref if n.id == 0 else NULL_REF)
    if app.bad("bad_refs") and app.rng.random() < 0.5:
        pref = app.corrupt_ref(n)
    cc = len(n.children)
    if app.bad("count_mismatch"):
        cc += app.rng.choice([-3, 3, 100000])
    name = n.name
    if app.bad("huge"):
        name = name + "N" * 1_000_000
    p[(IF_ACC, "Name")] = dbus.String(name)
    p[(IF_ACC, "Description")] = dbus.String(n.desc)
    p[(IF_ACC, "Parent")] = dbus.Struct(pref, signature="so")
    p[(IF_ACC, "ChildCount")] = dbus.Int32(cc)
    p[(IF_ACC, "Locale")] = dbus.String("C")
    p[(IF_ACC, "AccessibleId")] = dbus.String("fake-%d" % n.id)
    p[(IF_ACC, "HelpText")] = dbus.String("")
    if n.text is not None:
        count = len(n.text)
        if app.term_node is n and app.args.term_charcount_quirk:
            count = int(count * 1.5)      # VTE: CharacterCount disagrees with GetText (F-4)
        caret = n.caret
        if app.bad("count_mismatch"):
            count = app.rng.choice([-1, count + 50, 2**31 - 1])
        if app.bad("neg_offsets"):
            caret = app.rng.choice([-5, 2**31 - 1])
        p[(IF_TEXT, "CharacterCount")] = dbus.Int32(count)
        p[(IF_TEXT, "CaretOffset")] = dbus.Int32(caret)
    if n.actions:
        p[(IF_ACT, "NActions")] = dbus.Int32(len(n.actions))
    if n.value is not None:
        p[(IF_VALUE, "CurrentValue")] = dbus.Double(n.value[0])
        p[(IF_VALUE, "MinimumValue")] = dbus.Double(n.value[1])
        p[(IF_VALUE, "MaximumValue")] = dbus.Double(n.value[2])
    if n.id == 0:
        p[(IF_APP, "ToolkitName")] = dbus.String("gorca-fakeapp")
        p[(IF_APP, "Version")] = dbus.String("0")
        p[(IF_APP, "ToolkitVersion")] = dbus.String("0")
        p[(IF_APP, "AtspiVersion")] = dbus.String("2.1")
        p[(IF_APP, "InterfaceVersion")] = dbus.UInt32(1)
        p[(IF_APP, "Id")] = dbus.Int32(os.getpid())
    return p


@impl(IF_PROPS, "Get", "ss", "v")
def _(app, n, iface, prop):
    t = _prop_table(app, n)
    if (iface, prop) in t:
        return (t[(iface, prop)],)
    raise ApiError("org.freedesktop.DBus.Error.UnknownProperty", "%s.%s" % (iface, prop))


@impl(IF_PROPS, "GetAll", "s", "a{sv}")
def _(app, n, iface):
    d = {k[1]: v for k, v in _prop_table(app, n).items() if k[0] == iface}
    return (dbus.Dictionary(d, signature="sv"),)


@impl(IF_PROPS, "Set", "ssv", "")
def _(app, n, iface, prop, val):
    raise ApiError("org.freedesktop.DBus.Error.PropertyReadOnly", "read-only")


def build_service_class(app):
    # dbus-python resolves a method by *attribute name* and then checks the interface, so members
    # with the same name on different interfaces (GetAttributes, GetName...) need separate
    # classes. One mixin class per interface, merged through multiple inheritance.
    mixins = {}
    for (iface, member), (in_sig, out_sig, _fn) in TABLE.items():
        ns = mixins.setdefault(iface, {"__module__": __name__})
        nargs = len(list(dbus.Signature(in_sig)))
        params = "".join("a%d, " % i for i in range(nargs))
        argtuple = "(" + "".join("a%d, " % i for i in range(nargs)) + ")"
        src = ("def f(self, %spath=None, _r=None, _e=None):\n"
               "    return self._app_call(%r, %r, path, %s, _r, _e)\n") % (params, iface, member, argtuple)
        g = {}
        exec(src, g)    # noqa: S102 - generated from the static TABLE above
        f = g["f"]
        f.__name__ = member
        ns[member] = dbus.service.method(
            iface, in_signature=in_sig, out_signature=out_sig,
            async_callbacks=("_r", "_e"), path_keyword="path")(f)

    def _app_call(self, iface, member, path, args, r, e):
        app.stats["calls"] += 1
        key = "%s.%s" % (iface.rsplit(".", 1)[1], member)
        app.member_calls[key] = app.member_calls.get(key, 0) + 1
        try:
            node = None
            if iface != IF_CACHE:
                node = app.node_for_path(path)
                if node is None:
                    raise ApiError("org.freedesktop.DBus.Error.UnknownObject", "no object at " + path)
                if iface != IF_PROPS and iface not in app.ifaces(node):
                    raise ApiError("org.freedesktop.DBus.Error.UnknownMethod", iface + " not on " + path)
            if node is not None and node.hang:
                app.stats["hung"] += 1
                return
            result = TABLE[(iface, member)][2](app, node, *args)
        except ApiError as ex:
            e(dbus.exceptions.DBusException(str(ex), name=ex.dbus_name))
            return
        except Exception as ex:   # noqa: BLE001 - a fake app must answer, not die
            e(dbus.exceptions.DBusException("fakeapp internal error: %r" % (ex,),
                                            name="org.freedesktop.DBus.Error.Failed"))
            return
        if isinstance(result, tuple) is False:
            result = (result,)
        if app.hang.applies(member):
            if app.hang.mode == "never":
                app.stats["hung"] += 1
                return
            app.stats["delayed"] += 1
            GLib.timeout_add(app.hang.ms, lambda: (r(*result), False)[1])
            return
        r(*result)

    bases = tuple(dbus.service.InterfaceType("Mixin_" + i.replace(".", "_"), (dbus.service.FallbackObject,), ns)
                  for i, ns in mixins.items())
    return type("AtspiService", bases, {"_app_call": _app_call})


# ---- terminal ------------------------------------------------------------------------------------
class Term:
    def __init__(self, rows, cols, trim=False):
        self.rows, self.cols = rows, cols
        self.lines = [""] * rows
        self.r = self.c = 0
        self.trim = trim        # VTE-like text: rows right-trimmed, trailing empty rows dropped
        self.esc = None         # pending escape sequence (M6)

    def _csi(self, params, final):
        n = [int(x) if x.isdigit() else 0 for x in params.split(";")] if params else []
        a = n[0] if n else 0
        if final == "K":
            if a in (0, 2):
                self.lines[self.r] = self.lines[self.r][:self.c] if a == 0 else ""
            elif a == 1:
                self.lines[self.r] = " " * self.c + self.lines[self.r][self.c:]
        elif final == "J":
            if a == 2 or a == 3:
                self.lines = [""] * self.rows
            elif a == 0:
                self.lines[self.r] = self.lines[self.r][:self.c]
                for i in range(self.r + 1, self.rows):
                    self.lines[i] = ""
        elif final == "H" or final == "f":
            self.r = min(max((n[0] if n else 1) - 1, 0), self.rows - 1)
            self.c = min(max((n[1] if len(n) > 1 else 1) - 1, 0), self.cols - 1)
        elif final == "A":
            self.r = max(0, self.r - max(a, 1))
        elif final == "B":
            self.r = min(self.rows - 1, self.r + max(a, 1))
        elif final == "C":
            self.c = min(self.cols - 1, self.c + max(a, 1))
        elif final == "D":
            self.c = max(0, self.c - max(a, 1))
        elif final == "G":
            self.c = min(max(a - 1, 0), self.cols - 1)

    def feed(self, s):
        for ch in s:
            if self.esc is not None:
                self.esc += ch
                if len(self.esc) == 1:
                    if ch != "[":
                        self.esc = None
                elif ch.isalpha() or ch in "@`~":
                    self._csi(self.esc[1:-1], ch)
                    self.esc = None
                elif len(self.esc) > 24:
                    self.esc = None
                continue
            if ch == "\x1b":
                self.esc = ""
                continue
            if ch == "\b":
                self.c = max(0, self.c - 1)
            elif ch == "\n":
                self.r += 1
                self.c = 0
            elif ch == "\r":
                self.c = 0
            else:
                if self.c >= self.cols:
                    self.r += 1
                    self.c = 0
                if self.r >= self.rows:
                    self._scroll()
                line = self.lines[self.r]
                self.lines[self.r] = line[:self.c].ljust(self.c) + ch + line[self.c + 1:]
                self.c += 1
            if self.r >= self.rows:
                self._scroll()

    def _scroll(self):
        self.lines = self.lines[1:] + [""]
        self.r = self.rows - 1

    def _trimmed(self):
        rows = [l.rstrip() for l in self.lines]
        while rows and not rows[-1] and len(rows) > self.r + 1:
            rows.pop()
        return rows

    def snapshot(self):
        if self.trim:
            return "\n".join(self._trimmed())
        return "".join(l + "\n" for l in self.lines)

    def caret(self):
        if self.trim:
            rows = self._trimmed()
            before = sum(len(rows[i]) + 1 if i < len(rows) else 1 for i in range(self.r))
            row = rows[self.r] if self.r < len(rows) else ""
            return before + min(self.c, len(row))
        return sum(len(l) + 1 for l in self.lines[:self.r]) + self.c


def term_frame(app):
    """One VTE-style frame: rebuild the screen, diff by common head/tail, emit delete+insert+caret."""
    n, t = app.term_node, app.term
    if app.term_source:
        app.term_source()
    new = t.snapshot()
    old = n.text
    if new == old:
        # Only the cursor moved (readline Left / Right): VTE signals the caret alone.
        c = t.caret()
        if c != n.caret:
            n.caret = c
            app.ev_caret(n)
            app.stats["term_frames"] += 1
        return True
    head = 0
    m = min(len(old), len(new))
    while head < m and old[head] == new[head]:
        head += 1
    tail = 0
    while tail < m - head and old[len(old) - 1 - tail] == new[len(new) - 1 - tail]:
        tail += 1
    old_mid, new_mid = old[head:len(old) - tail], new[head:len(new) - tail]
    n.text = new
    n.caret = t.caret()
    if old_mid:
        app.ev_text(n, "delete", head, len(old_mid), old_mid)
        app.stats["term_bytes"] += len(old_mid)
    if new_mid:
        app.ev_text(n, "insert", head, len(new_mid), new_mid)
        app.stats["term_bytes"] += len(new_mid)
    app.ev_caret(n)
    app.stats["term_frames"] += 1
    return True


def term_start(app, fps, lines_per_frame, kind="seq"):
    if app.term_timer:
        GLib.source_remove(app.term_timer)
        app.term_timer = None
    app.term_source = None
    if lines_per_frame > 0:
        counter = [0]

        def gen():
            for _ in range(lines_per_frame):
                counter[0] += 1
                if kind == "yes":
                    app.term.feed("y\n")
                elif kind == "build":
                    app.term.feed("[%3d%%] Building CXX object src/mod%05d.o\n" % (counter[0] % 100, counter[0]))
                else:
                    body = "".join(app.rng.choice("abcdefghij klmnopqrst") for _ in range(app.term.cols - 12))
                    app.term.feed("%08d %s\n" % (counter[0], body))
        app.term_source = gen
    if fps > 0:
        app.term_timer = GLib.timeout_add(max(1, int(1000 / fps)), lambda: term_frame(app))


# ---- event storm ---------------------------------------------------------------------------------
class Storm:
    def __init__(self, app, rate, mix, duration):
        self.app, self.rate, self.mix = app, float(rate), mix
        self.kinds = [k for k, w in mix.items() for _ in range(int(w))] or ["state"]
        self.t0 = time.monotonic()
        self.emitted = 0
        self.until = self.t0 + duration if duration else None
        self.max_lag = 0.0
        self.pool = [x for x in app.nodes if x is not None and x.id != 0]
        self.tick_id = GLib.timeout_add(2, self.tick)
        self.ci = 0

    def one(self):
        a = self.app
        n = a.rng.choice(self.pool) if self.pool else a.nodes[0]
        k = a.rng.choice(self.kinds)
        if k == "children":
            self.ci += 1
            a.ev_children(n, "add" if self.ci % 2 else "remove", self.ci % 8, n if not n.children
                          else a.nodes[n.children[0]])
        elif k == "text":
            a.ev_text(n, "insert", self.ci % 50, 1, "x")
            self.ci += 1
        elif k == "focus":
            a.emit(path_of(n.id), EV_OBJ, "StateChanged", "focused", self.ci & 1, 0, dbus.Int32(0))
            self.ci += 1
        else:
            a.ev_state(n, S_SHOWING, self.ci & 1)
            self.ci += 1
        a.stats["storm_events"] += 1

    def tick(self):
        now = time.monotonic()
        if self.until and now >= self.until:
            self.app.storm = None
            out({"event": "storm_done", "emitted": self.emitted,
                 "rate": self.emitted / max(1e-9, now - self.t0)})
            return False
        due = int((now - self.t0) * self.rate) - self.emitted
        self.max_lag = max(self.max_lag, due / self.rate if self.rate else 0)
        for _ in range(min(due, 5000)):      # bounded per tick so control input stays responsive
            self.one()
            self.emitted += 1
        return True

    def stop(self):
        GLib.source_remove(self.tick_id)
        el = time.monotonic() - self.t0
        return {"emitted": self.emitted, "seconds": el, "achieved_rate": self.emitted / max(1e-9, el),
                "max_lag_s": self.max_lag}


# ---- scenarios -----------------------------------------------------------------------------------
def build_form(app):
    app.add(None, R_APP, app.args.name)
    frame = app.add(0, R_FRAME, "Form", states=(S_ACTIVE,))
    panel = app.add(frame.id, R_PANEL, "panel")
    app.add(panel.id, R_LABEL, "Name:")
    app.add(panel.id, R_ENTRY, "name", text="", editable=True,
            states=(S_FOCUSABLE, S_EDITABLE, S_SINGLE_LINE))
    app.add(panel.id, R_LABEL, "Email:")
    app.add(panel.id, R_ENTRY, "email", text="", editable=True,
            states=(S_FOCUSABLE, S_EDITABLE, S_SINGLE_LINE))
    app.add(panel.id, R_CHECK, "Subscribe", actions=("toggle",), states=(S_FOCUSABLE,))
    app.add(panel.id, R_BUTTON, "Submit", actions=("click",), states=(S_FOCUSABLE,))
    app.add(panel.id, R_BUTTON, "Cancel", actions=("click",), states=(S_FOCUSABLE,))
    app.add(panel.id, R_TEXT, "notes", text="line one\nline two\n", editable=True,
            states=(S_FOCUSABLE, S_EDITABLE, S_MULTI_LINE))


SWITCHER_ITEMS = ("Terminal", "Files", "Firefox", "Settings", "Calculator", "Text Editor")


def build_switcher(app):
    """GNOME Shell's Alt+Tab popup as AT-SPI sees it (St widgets, no Selection interface).

    Run with `--name gnome-shell`. Items are push buttons that never take key focus; moving the
    selection is `select`: StateChanged selected=0 on the old item, then selected=1 on the new
    one, and nothing else (refs/gnome-shell/src/st/st-widget.c check_pseudo_class; the switcher
    only toggles the `selected` style pseudo-class, switcherPopup.js highlight()).
    """
    app.add(None, R_APP, app.args.name)
    popup = app.add(0, R_PANEL, "Window switcher")
    for name in SWITCHER_ITEMS:
        app.add(popup.id, R_BUTTON, name, actions=("click",), states=(S_FOCUSABLE,))
    app.switcher_selected = None


def build_tree(app):
    """Balanced tree with `fanout` children per node: nodes=100000,fanout=1 is a 100k-deep chain."""
    a = app.args
    app.add(None, R_APP, a.name)
    fan = max(1, a.fanout)
    for i in range(1, max(2, a.nodes)):
        parent = (i - 1) // fan if fan > 1 else i - 1
        leaf_text = a.tree_text and i % 10 == 0
        app.add(parent, R_TEXT if leaf_text else R_LIST_ITEM, "node-%d" % i,
                text=("text of node %d" % i) if leaf_text else None, states=(S_FOCUSABLE,))
    app.nodes[1].states.add(S_ACTIVE)


LONG_LINE = "".join(chr(ord("a") + i % 26) for i in range(1500))


def build_text(app):
    """The M3 text form (ADR-0007 test support). Node ids: 1 frame, 2 `name` (single-line entry),
    3 `notes` (multi-line text view), 4 `secret` (password entry), 5 `long` (single-line entry
    of 1500 letters). Every entry is focusable and editable; the control channel drives the
    caret, typing, selections and text edits the way a toolkit would (`caret`, `type`,
    `backspace`, `insert_text`, `select_all`, `load_text`, `get_caret`)."""
    app.add(None, R_APP, app.args.name)
    frame = app.add(0, R_FRAME, "Text Form", states=(S_ACTIVE,))
    single = (S_FOCUSABLE, S_EDITABLE, S_SINGLE_LINE)
    multi = (S_FOCUSABLE, S_EDITABLE, S_MULTI_LINE)
    app.add(frame.id, R_ENTRY, "name", text="hello world", editable=True, states=single)
    app.add(frame.id, R_ENTRY, "notes", text="".join("Line %04d text\n" % i for i in range(1500)),
            editable=True, states=multi)
    app.add(frame.id, 40, "secret", text="", editable=True, states=single)
    app.add(frame.id, R_ENTRY, "long", text=LONG_LINE, editable=True, states=single)


def build_review(app):
    """Root only; the test sends a `build` command with a whole tree (ADR-0006 test trees)."""
    app.add(None, R_APP, app.args.name)
    app.review = True


def _role_of(v):
    if isinstance(v, int):
        return v
    if v not in ROLE_BY_NAME:
        raise ValueError("unknown role name %r (known: %s)" % (v, sorted(ROLE_BY_NAME)))
    return ROLE_BY_NAME[v]


def _build_below(app, parent, spec):
    states = [STATE_NAMES[x] if isinstance(x, str) else int(x) for x in spec.get("states", ())]
    if spec.get("focusable"):
        states.append(S_FOCUSABLE)
    editable = bool(spec.get("editable"))
    if editable:
        states.append(S_EDITABLE)
    text = spec.get("text")
    n = app.add(parent.id, _role_of(spec.get("role", "panel")), spec.get("name", ""), text=text,
                editable=editable, actions=tuple(spec.get("actions", ())), states=states,
                extents=spec.get("extents"), value=spec.get("value"),
                no_text_extents=bool(spec.get("no_text_extents", editable and app.coord == "window")))
    if spec.get("hang"):
        n.hang = True
    for child in spec.get("children", ()):
        _build_below(app, n, child)
    return n


def _walk(app, n):
    """`n` and its descendants, parents first."""
    yield n
    for c in n.children:
        yield from _walk(app, app.nodes[c])


def build_tree_cmd(app, cmd):
    """`{"cmd":"build","coord":"window|global|default","tree":{...}}`: replace everything below
    the application root. A tree node is `{"name","role","extents":[x,y,w,h],"text","editable",
    "focusable","states":[names],"actions":[names],"hang","no_text_extents","children":[...]}`;
    `role` is a name from ROLE_BY_NAME or a number. The reply lists `paths` of named nodes."""
    coord = cmd.get("coord", "window")
    if coord not in COORD_MODES:
        raise ValueError("coord must be one of %s" % (COORD_MODES,))
    app.coord = coord
    app.review = True
    root = app.nodes[0]
    keep_name = root.name
    del app.nodes[1:]
    root.children = []
    app.by_name = {keep_name: 0} if keep_name else {}
    app.focus_id = None
    top = cmd["tree"]
    tops = top if isinstance(top, list) else [top]
    for spec in tops:
        _build_below(app, root, spec)
    return {"paths": {k: path_of(v) for k, v in app.by_name.items()}, "nodes": len(app.nodes)}


def build_terminal(app):
    a = app.args
    app.add(None, R_APP, a.name)
    frame = app.add(0, R_FRAME, "Terminal", states=(S_ACTIVE,))
    app.term = Term(a.term_rows, a.term_cols, trim=a.term_trim)
    app.term_node = app.add(frame.id, R_TERMINAL, "term", text=app.term.snapshot(),
                            states=(S_FOCUSABLE, S_MULTI_LINE))
    # A second focusable node, so a test can move the focus away from the terminal and back.
    app.add(frame.id, R_ENTRY, "other", text="", editable=True, states=(S_FOCUSABLE,))


# ---- control channel -----------------------------------------------------------------------------
def handle(app, cmd):
    c = cmd.get("cmd")
    if c == "focus":
        n = app.node(cmd["node"])
        app.t_focus_ns = 0
        app.set_focus(n)
        return {"t_emit_ns": app.t_focus_ns}
    elif c == "select":
        n = app.node(cmd["node"])
        old = app.nodes[app.switcher_selected] if app.switcher_selected is not None else None
        if old is n:
            return {}
        if old is not None:
            old.states.discard(STATE_NAMES["selected"])
            app.ev_state(old, STATE_NAMES["selected"], False)
        app.switcher_selected = n.id
        n.states.add(STATE_NAMES["selected"])
        app.t_focus_ns = time.monotonic_ns()
        app.ev_state(n, STATE_NAMES["selected"], True)
        app.conn.flush()
        return {"t_emit_ns": app.t_focus_ns}
        return {"t_emit_ns": app.t_focus_ns}
    elif c == "activate":
        # M4b: a window becomes the active one, and (after `gap_ms`, GTK sends it a few
        # milliseconds later) its focus widget takes focus. Returns both emit times.
        w = app.node(cmd["node"])
        w.states.add(S_ACTIVE)
        t_act = time.monotonic_ns()
        app.emit(path_of(w.id), EV_WIN, "Activate", "", 0, 0, dbus.Int32(0))
        app.conn.flush()
        res = {"t_act_ns": t_act}
        if "focus" in cmd:
            gap = float(cmd.get("gap_ms", 2))
            if gap > 0:
                time.sleep(gap / 1000.0)
            app.t_focus_ns = 0
            app.set_focus(app.node(cmd["focus"]))
            res["t_focus_ns"] = app.t_focus_ns
        return res
    elif c == "popup":
        # M4b: a subtree appears under `parent` (a notification banner, an OSD) and its root
        # reports `showing`, which is all St sets when an actor is mapped.
        parent = app.node(cmd.get("parent", 0))
        n = _build_below(app, parent, cmd["tree"])
        for x in _walk(app, n):
            app.ev_cache("AddAccessible", x)
        app.ev_children(parent, "add", len(parent.children) - 1, n)
        t = time.monotonic_ns()
        app.ev_state(n, S_SHOWING, True)
        app.conn.flush()
        return {"t_emit_ns": t, "id": n.id, "path": path_of(n.id)}
    elif c == "set_value":
        # M4b: a Value interface changes (the OSD level bar animates); `accessible-value` is
        # signalled like a toolkit does.
        n = app.node(cmd["node"])
        n.value = (float(cmd["value"]), n.value[1], n.value[2])
        t = time.monotonic_ns()
        app.emit(path_of(n.id), EV_OBJ, "PropertyChange", "accessible-value", 0, 0, dbus.Int32(0))
        app.conn.flush()
        return {"t_emit_ns": t}
    elif c == "announce":
        # Object:Announcement: `d1` is the politeness (1 polite, 2 assertive), any_data the text.
        n = app.node(cmd.get("node", 0))
        t = time.monotonic_ns()
        app.emit(path_of(n.id), EV_OBJ, "Announcement", "", int(cmd.get("politeness", 1)), 0,
                 dbus.String(cmd["text"]))
        app.conn.flush()
        return {"t_emit_ns": t}
    elif c == "type":
        n = app.node(cmd["node"])
        app.first_emit_ns = 0
        for ch in cmd["text"]:
            app.text_insert(n, n.caret, ch)
        app.conn.flush()
        return {"t_emit_ns": app.first_emit_ns}
    elif c == "backspace":
        n = app.node(cmd["node"])
        app.first_emit_ns = 0
        app.text_delete(n, n.caret - 1, n.caret)
        app.conn.flush()
        return {"t_emit_ns": app.first_emit_ns}
    elif c == "delete_fwd":
        n = app.node(cmd["node"])
        app.first_emit_ns = 0
        pos = n.caret
        if app.text_delete(n, pos, pos + 1):
            n.caret = pos
        app.conn.flush()
        return {"t_emit_ns": app.first_emit_ns}
    elif c == "insert_text":
        # A paste or an input-method commit: one insert of the whole text at the caret.
        n = app.node(cmd["node"])
        app.first_emit_ns = 0
        app.text_insert(n, n.caret if cmd.get("at") is None else int(cmd["at"]), cmd["text"])
        app.conn.flush()
        return {"t_emit_ns": app.first_emit_ns}
    elif c == "select_all":
        n = app.node(cmd["node"])
        app.first_emit_ns = 0
        n.sel = [(0, len(n.text))]
        n.caret = len(n.text)
        app.ev_caret(n)
        app.emit(path_of(n.id), EV_OBJ, "TextSelectionChanged", "", 0, 0, dbus.Int32(0))
        app.conn.flush()
        return {"t_emit_ns": app.first_emit_ns}
    elif c == "load_text":
        n = app.node(cmd["node"])
        n.text = cmd["text"]
        n.caret = int(cmd.get("caret", 0))
        n.sel = []
    elif c == "get_caret":
        n = app.node(cmd["node"])
        return {"caret": n.caret, "t_set_ns": app.t_caret_set_ns, "sets": app.caret_sets,
                "len": len(n.text or "")}
    elif c == "set_text":
        n = app.node(cmd["node"])
        if n.text:
            app.text_delete(n, 0, -1)
        app.text_insert(n, 0, cmd["text"])
    elif c == "caret":
        # `sel`: [[start, end], ...] also changes the selection, signalled after the caret like
        # GTK4 does (ADR-0007 T-F2).
        n = app.node(cmd["node"])
        app.first_emit_ns = 0
        n.caret = int(cmd["offset"])
        if "sel" in cmd:
            n.sel = [(int(a), int(b)) for a, b in cmd["sel"]]
        app.ev_caret(n)
        if "sel" in cmd:
            app.emit(path_of(n.id), EV_OBJ, "TextSelectionChanged", "", 0, 0, dbus.Int32(0))
        app.conn.flush()
        return {"t_emit_ns": app.first_emit_ns}
    elif c == "action":
        app.do_action(app.node(cmd["node"]), int(cmd.get("index", 0)))
    elif c == "emit":
        n = app.node(cmd["node"])
        iface = {"Object": EV_OBJ, "Window": EV_WIN, "Focus": EV_FOCUS}[cmd.get("class", "Object")]
        data = cmd.get("data")
        if isinstance(data, list) and len(data) == 2:
            data = dbus.Struct(tuple(data), signature="so")
        elif isinstance(data, int):
            data = dbus.Int32(data)
        else:
            data = dbus.String(data or "")
        app.emit(path_of(n.id), iface, cmd["member"], cmd.get("detail", ""), cmd.get("d1", 0),
                 cmd.get("d2", 0), data)
    elif c == "add_child":
        p = app.node(cmd["parent"])
        n = app.add_child(p, int(cmd.get("role", R_LIST_ITEM)), cmd["name"], text=cmd.get("text"))
        return {"path": path_of(n.id), "id": n.id}
    elif c == "remove":
        app.remove_node(app.node(cmd["node"]))
    elif c == "hang":
        app.hang.configure(cmd.get("mode", "off"), cmd.get("ms", 0), cmd.get("methods"), cmd.get("after", 0))
    elif c == "storm":
        if app.storm:
            app.storm.stop()
            app.storm = None
        if cmd.get("rate", 0) > 0:
            app.storm = Storm(app, cmd["rate"], cmd.get("mix", {"children": 1, "text": 1, "state": 1,
                                                                 "focus": 1}), cmd.get("duration", 0))
    elif c == "storm_stop":
        if app.storm:
            r = app.storm.stop()
            app.storm = None
            return r
    elif c == "malformed":
        modes = cmd.get("modes", [])
        app.malformed = set(MALFORMED_MODES if modes == "all" or "all" in modes else modes)
        bad = app.malformed - set(MALFORMED_MODES)
        if bad:
            raise ValueError("unknown malformed mode(s): %s" % sorted(bad))
    elif c == "term_feed":
        # The text is what a program writes to the pty (escape sequences included). One VTE frame
        # follows unless `defer`; the reply carries the emit time of its first signal.
        app.first_emit_ns = 0
        app.term.feed(cmd["text"])
        if not cmd.get("defer"):
            term_frame(app)
        app.conn.flush()
        return {"t_emit_ns": app.first_emit_ns, "caret": app.term_node.caret}
    elif c == "term_gen":
        term_start(app, float(cmd.get("fps", 60)), int(cmd.get("lines", 1)), cmd.get("kind", "seq"))
    elif c == "term_stop":
        term_start(app, 0, 0)
        app.conn.flush()
        return {"frames": app.stats["term_frames"]}
    elif c == "build":
        return build_tree_cmd(app, cmd)
    elif c == "calls":
        res = {"calls": dict(app.member_calls), "total": sum(app.member_calls.values())}
        if cmd.get("reset"):
            app.member_calls.clear()
        return res
    elif c == "hang_node":
        app.node(cmd["node"]).hang = bool(cmd.get("on", True))
    elif c == "stats":
        return dict(app.stats, nodes=sum(1 for x in app.nodes if x is not None), seq=app.seq)
    elif c == "paths":
        return {"paths": {k: path_of(v) for k, v in app.by_name.items()}}
    elif c == "quit":
        app.loop.quit()
    else:
        raise ValueError("unknown cmd %r" % c)
    return {}


class Stdin:
    def __init__(self, app):
        self.app, self.buf = app, b""
        os.set_blocking(0, False)
        GLib.io_add_watch(0, GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, self.ready)

    def ready(self, fd, cond):
        try:
            data = os.read(0, 65536)
        except BlockingIOError:
            return True
        if not data:
            self.app.loop.quit()
            return False
        self.buf += data
        while b"\n" in self.buf:
            line, self.buf = self.buf.split(b"\n", 1)
            if not line.strip():
                continue
            rid = None
            try:
                cmd = json.loads(line)
                rid = cmd.get("id")
                res = handle(self.app, cmd)
                out(dict(res or {}, event="reply", id=rid, ok=True))
            except Exception as ex:   # noqa: BLE001
                out({"event": "reply", "id": rid, "ok": False, "error": repr(ex)})
        return True


def a11y_address():
    addr = os.environ.get("AT_SPI_BUS_ADDRESS")
    if addr:
        return addr
    sb = dbus.bus.BusConnection(os.environ["DBUS_SESSION_BUS_ADDRESS"])
    return str(sb.call_blocking("org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus", "GetAddress", "", (),
                                timeout=5))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--scenario", default="form",
                    choices=["form", "tree", "storm", "hung", "terminal", "malformed", "switcher", "review",
                             "text"])
    ap.add_argument("--name", default="fakeapp")
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--nodes", type=int, default=1000, help="tree: node count (up to 100k+)")
    ap.add_argument("--fanout", type=int, default=10, help="tree: children per node; 1 = deep chain")
    ap.add_argument("--tree-text", action="store_true", help="tree: every 10th node is a Text node")
    ap.add_argument("--storm-rate", type=float, default=0, help="events/s (storm scenario default 5000)")
    ap.add_argument("--storm-mix", default="children=1,text=1,state=1,focus=1")
    ap.add_argument("--storm-duration", type=float, default=0, help="seconds, 0 = until told to stop")
    ap.add_argument("--hang", default="", help="never | delay:MS (hung scenario default: never)")
    ap.add_argument("--hang-methods", default="", help="comma list of member names; default all")
    ap.add_argument("--hang-after", type=int, default=0, help="serve N calls normally first")
    ap.add_argument("--malformed", default="", help="comma list of %s or all" % ",".join(MALFORMED_MODES))
    ap.add_argument("--term-rows", type=int, default=24)
    ap.add_argument("--term-cols", type=int, default=80)
    ap.add_argument("--term-fps", type=float, default=60)
    ap.add_argument("--term-gen", type=int, default=0, help="auto-generate N lines per frame")
    ap.add_argument("--term-trim", action="store_true",
                    help="VTE-like text: rows right-trimmed, trailing empty rows dropped (M6)")
    ap.add_argument("--term-charcount-quirk", action=argparse.BooleanOptionalAction, default=True)
    ap.add_argument("--no-register", action="store_true", help="do not call Registry Socket.Embed")
    ap.add_argument("--own-shell-name", action="store_true",
                    help="own org.gnome.Shell on the session bus from this process, so the daemon "
                         "verifies it as the Shell (the connector never trusts self-reported names)")
    args = ap.parse_args()

    DBusGMainLoop(set_as_default=True)
    shell_bus = None
    if args.own_shell_name:
        shell_bus = dbus.bus.BusConnection(os.environ["DBUS_SESSION_BUS_ADDRESS"])
        shell_bus.request_name("org.gnome.Shell")   # held for the process lifetime
    conn = dbus.bus.BusConnection(a11y_address())
    app = App(args, conn)
    app.loop = GLib.MainLoop()

    sc = args.scenario
    if sc == "storm" and args.nodes == 1000:
        args.nodes = 200
    if sc == "malformed" and args.nodes == 1000:
        args.nodes = 50
    if sc == "form":
        build_form(app)
    elif sc == "terminal":
        build_terminal(app)
    elif sc == "switcher":
        build_switcher(app)
    elif sc == "review":
        build_review(app)
    elif sc == "text":
        build_text(app)
    else:
        build_tree(app)
    if args.malformed:
        app.malformed = set(MALFORMED_MODES if args.malformed == "all" else args.malformed.split(","))
        unknown = app.malformed - set(MALFORMED_MODES)
        if unknown:
            ap.error("unknown malformed mode(s): %s" % sorted(unknown))
    elif sc == "malformed":
        app.malformed = set(MALFORMED_MODES)
    if sc == "hung" and not args.hang:
        args.hang = "never"
    if args.hang:
        mode, _, ms = args.hang.partition(":")
        app.hang.configure(mode, ms or 0, [m for m in args.hang_methods.split(",") if m], args.hang_after)

    cls = build_service_class(app)
    cls(conn, BASE)
    cls(conn, CACHE_PATH)
    Stdin(app)

    def ready():
        info = {"event": "ready", "scenario": sc, "bus_name": app.unique, "root": ROOT,
                "nodes": len(app.nodes), "pid": os.getpid()}
        if len(app.nodes) <= 64:
            info["paths"] = {k: path_of(v) for k, v in app.by_name.items()}
        out(info)
        if args.term_gen:
            term_start(app, args.term_fps, args.term_gen)
        rate = args.storm_rate or (5000 if sc == "storm" else 0)
        if rate:
            mix = {k: float(v) for k, v in (kv.split("=") for kv in args.storm_mix.split(","))}
            app.storm = Storm(app, rate, mix, args.storm_duration)
        if sc in ("form", "terminal"):
            f = app.nodes[1]
            app.emit(path_of(f.id), EV_WIN, "Activate", "", 0, 0, dbus.Int32(0))
        return False

    def on_embed(ret):
        app.desktop_ref = (str(ret[0]), str(ret[1]))
        ready()

    def on_embed_err(err):
        out({"event": "error", "where": "Socket.Embed", "error": str(err)})
        ready()

    def start():
        if args.no_register:
            ready()
        else:
            conn.call_async("org.a11y.atspi.Registry", ROOT, I + "Socket", "Embed", "(so)",
                            (dbus.Struct((app.unique, ROOT), signature="so"),),
                            on_embed, on_embed_err, timeout=10)
        return False

    GLib.idle_add(start)
    try:
        app.loop.run()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
