"""Python helpers to drive tests/harness/fakeapp.py from a test (system python3 only).

    app = FakeApp.start("--scenario", "form")      # blocks until the app is registered
    app.cmd(cmd="focus", node="email")
    app.stop()

Also: `A11yWatcher` (collect AT-SPI event signals from the a11y bus) and `a11y_connection()`.
"""
import json
import os
import select
import subprocess
import sys
import time

import dbus
import dbus.bus
import dbus.mainloop.glib
from gi.repository import GLib

HERE = os.path.dirname(os.path.abspath(__file__))
FAKEAPP = os.path.join(HERE, "fakeapp.py")
dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)


def a11y_address():
    return os.environ["AT_SPI_BUS_ADDRESS"]


def a11y_connection():
    return dbus.bus.BusConnection(a11y_address())


class FakeApp:
    def __init__(self, proc, ready):
        self.proc, self.ready = proc, ready
        self.bus_name = ready["bus_name"] if ready else None
        self.paths = ready.get("paths", {}) if ready else {}
        self._next = 1
        self.extra = []      # non-reply lines (action events, storm_done...)

    @classmethod
    def start(cls, *args, timeout=60):
        proc = subprocess.Popen(["/usr/bin/python3", FAKEAPP, *args],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)
        app = cls(proc, None)
        ready = app._read_until(lambda o: o.get("event") in ("ready", "error"), timeout)
        if ready.get("event") != "ready":
            raise RuntimeError("fakeapp failed to start: %r" % (ready,))
        app.ready, app.bus_name, app.paths = ready, ready["bus_name"], ready.get("paths", {})
        return app

    def _read_until(self, pred, timeout):
        end = time.monotonic() + timeout
        while True:
            left = end - time.monotonic()
            if left <= 0 or self.proc.poll() is not None and not select.select([self.proc.stdout], [], [], 0)[0]:
                raise TimeoutError("fakeapp: no matching line (exit=%r)" % self.proc.poll())
            if not select.select([self.proc.stdout], [], [], min(left, 0.5))[0]:
                continue
            line = self.proc.stdout.readline()
            if not line:
                raise TimeoutError("fakeapp: stdout closed")
            obj = json.loads(line)
            if pred(obj):
                return obj
            self.extra.append(obj)

    def cmd(self, timeout=30, **c):
        rid = self._next
        self._next += 1
        c["id"] = rid
        self.proc.stdin.write(json.dumps(c) + "\n")
        self.proc.stdin.flush()
        r = self._read_until(lambda o: o.get("event") == "reply" and o.get("id") == rid, timeout)
        if not r.get("ok"):
            raise RuntimeError("fakeapp cmd %r failed: %s" % (c, r.get("error")))
        return r

    def path(self, name):
        return self.paths[name]

    def stop(self):
        try:
            self.proc.stdin.close()
        except OSError:
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.proc.stdout.close()

    def __enter__(self):
        return self

    def __exit__(self, *a):
        self.stop()


class A11yWatcher:
    """Collects org.a11y.atspi.Event.* signals (optionally from one sender)."""

    def __init__(self, sender=None, interfaces=("Object", "Window", "Focus"), conn=None):
        self.conn = conn or a11y_connection()
        self.events = []
        for i in interfaces:
            rule = "type='signal',interface='org.a11y.atspi.Event.%s'" % i
            if sender:
                rule += ",sender='%s'" % sender
            self.conn.add_match_string(rule)
        self.conn.add_message_filter(self._filter)

    def _filter(self, conn, msg):
        if isinstance(msg, dbus.lowlevel.SignalMessage) and \
                msg.get_interface().startswith("org.a11y.atspi.Event."):
            self.events.append((time.monotonic(), msg.get_sender(), msg.get_path(),
                                msg.get_interface().rsplit(".", 1)[1], msg.get_member(),
                                tuple(msg.get_args_list())))
        return dbus.lowlevel.HANDLER_RESULT_NOT_YET_HANDLED

    def pump(self, seconds):
        ctx = GLib.MainContext.default()
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            while ctx.pending():
                ctx.iteration(False)
            time.sleep(0.0005)

    def wait_for(self, pred, timeout=5.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            self.pump(0.02)
            hit = [e for e in self.events if pred(e)]
            if hit:
                return hit
        return []
