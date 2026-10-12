"""AT-SPI side of the SPA characterization: raw event capture + tree walker on a (private) a11y bus.

Runs under /usr/bin/python3 (gi + Atspi typelib). Events are captured as raw D-Bus signals so the sender object PATH
(the identity the mirror will see) is preserved; libatspi is only used for enum names.
"""
import time, threading, collections
import gi
gi.require_version('Atspi', '2.0')
from gi.repository import Gio, GLib, Atspi

REG = 'org.a11y.atspi.Registry'
ACC = 'org.a11y.atspi.Accessible'
COMP = 'org.a11y.atspi.Component'
PROPS = 'org.freedesktop.DBus.Properties'
ROOT_PATH = '/org/a11y/atspi/accessible/root'

ROLE = {int(r): r.value_nick for r in Atspi.Role.__enum_values__.values()} if hasattr(Atspi.Role, '__enum_values__') else {}
STATE = {int(s): s.value_nick for s in Atspi.StateType.__enum_values__.values()} if hasattr(Atspi.StateType, '__enum_values__') else {}


class Bus:
    def __init__(self, addr):
        self.conn = Gio.DBusConnection.new_for_address_sync(
            addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
        self.events = []          # (t_monotonic, sender, path, iface, member, kind, d1, d2, data)
        self.lock = threading.Lock()
        self.loop = GLib.MainLoop()
        self.t = threading.Thread(target=self.loop.run, daemon=True)
        self.calls = 0
        self.fallback_doc = False

    def start_events(self, names=('object:', 'window:', 'document:')):
        self.conn.signal_subscribe(None, None, None, None, None, Gio.DBusSignalFlags.NONE, self._on_sig)
        self.t.start()
        for n in names:
            self.conn.call_sync(REG, '/org/a11y/atspi/registry', REG, 'RegisterEvent', GLib.Variant('(s)', (n,)), None,
                                Gio.DBusCallFlags.NONE, 3000, None)

    def _on_sig(self, conn, sender, path, iface, member, params):
        if not iface.startswith('org.a11y.atspi.Event.'):
            return
        t = time.monotonic()
        try:
            v = params.unpack()
            kind, d1, d2, data = v[0], v[1], v[2], v[3]
        except Exception:
            kind, d1, d2, data = '', 0, 0, None
        ref = None
        if isinstance(data, tuple) and len(data) == 2 and all(isinstance(x, str) for x in data):
            ref = data[1]; data = None            # (bus, path) child reference
        elif isinstance(data, str):
            data = data[:60]
        else:
            data = None if data is None else str(data)[:60]
        with self.lock:
            self.events.append((t, sender, path, iface.rsplit('.', 1)[1], member, kind, d1, d2, data, ref))

    def mark(self):
        with self.lock:
            return len(self.events)

    def since(self, m):
        with self.lock:
            return list(self.events[m:])

    def wait_quiet(self, m, quiet=0.7, maxwait=6.0, first_wait=0.5):
        """Block until no event arrived for `quiet` s, or maxwait s elapsed. Returns (ms of last event after start, settled)."""
        t0 = time.monotonic()
        while True:
            time.sleep(0.02)
            now = time.monotonic()
            with self.lock:
                last = self.events[-1][0] if len(self.events) > m else None
            if now - t0 >= maxwait:
                return ((last - t0) if last else None), False
            if now - (last if last else t0) >= (quiet if last else max(quiet, first_wait)):
                return ((last - t0) if last else 0.0), True

    # ---- synchronous accessible calls ----
    def call(self, bus, path, iface, method, args=None, timeout=4000):
        self.calls += 1
        r = self.conn.call_sync(bus, path, iface, method, args, None, Gio.DBusCallFlags.NONE, timeout, None)
        return r.unpack()

    def apps(self):
        return self.call(REG, ROOT_PATH, ACC, 'GetChildren')[0]

    def prop_all(self, bus, path, iface=ACC):
        return self.call(bus, path, PROPS, 'GetAll', GLib.Variant('(s)', (iface,)))[0]

    def find_app(self, name_sub, wait=30):
        t0 = time.time()
        while time.time() - t0 < wait:
            try:
                for b, p in self.apps():
                    try:
                        n = self.prop_all(b, p).get('Name', '')
                    except GLib.Error:
                        continue
                    if name_sub.lower() in n.lower():
                        return b, p
            except GLib.Error:
                pass
            time.sleep(0.3)
        return None

    def find_document(self, app, wait=20):
        """The SHOWING document-web under app (does not descend into documents)."""
        t0 = time.time()
        while time.time() - t0 < wait:
            found = self._find_doc(app[0], app[1], 0)
            if found:
                return found
            if time.time() - t0 > 6:      # fallback: window not "showing" (e.g. deactivated/occluded): take any web document
                found = self._find_doc(app[0], app[1], 0, any_state=True)
                if found:
                    self.fallback_doc = True
                    return found
            time.sleep(0.3)
        return None

    def _find_doc(self, b, p, depth, any_state=False):
        if depth > 16:
            return None
        try:
            kids = self.call(b, p, ACC, 'GetChildren')[0]
        except GLib.Error:
            return None
        for cb, cp in kids:
            try:
                role = self.call(cb, cp, ACC, 'GetRole')[0]
                if role == 95:    # ATSPI_ROLE_DOCUMENT_WEB
                    st = self.call(cb, cp, ACC, 'GetState')[0]
                    if any_state or (st[0] >> 25) & 1:    # SHOWING
                        return cb, cp
                    continue
            except GLib.Error:
                continue          # one stale/erroring child must not hide its siblings
            r = self._find_doc(cb, cp, depth + 1, any_state)
            if r:
                return r
        return None

    def snapshot(self, doc, bounds=True, max_nodes=60000):
        """DFS of the document subtree. Returns list of node dicts in document order; p = parent index."""
        nodes = []
        stack = [(doc[0], doc[1], -1, 0)]
        while stack and len(nodes) < max_nodes:
            b, p, par, idx = stack.pop()
            try:
                pr = self.prop_all(b, p)
                role = self.call(b, p, ACC, 'GetRole')[0]
                st = self.call(b, p, ACC, 'GetState')[0]
                attrs = self.call(b, p, ACC, 'GetAttributes')[0]
                ch = self.call(b, p, ACC, 'GetChildren')[0] if pr.get('ChildCount', 0) else []
            except GLib.Error:
                continue
            ext = None
            if bounds:
                try:
                    ext = list(self.call(b, p, COMP, 'GetExtents', GLib.Variant('(u)', (0,)))[0])
                except GLib.Error:
                    ext = None
            states = []
            for w in range(2):
                for bit in range(32):
                    if st[w] >> bit & 1:
                        states.append(STATE.get(w * 32 + bit, str(w * 32 + bit)))
            i = len(nodes)
            name = pr.get('Name', '')
            nodes.append({'i': i, 'p': par, 'path': p, 'role': ROLE.get(role, str(role)), 'name': name, 'nl': len(name),
                          'states': states, 'attrs': attrs, 'b': ext, 'nc': len(ch), 'idx': idx})
            for k in range(len(ch) - 1, -1, -1):
                stack.append((ch[k][0], ch[k][1], i, k))
        return nodes
