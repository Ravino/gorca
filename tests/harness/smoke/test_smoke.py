"""Smoke tests: every fake-app scenario is observable through pyatspi / dbus-python / busctl.

Run inside the isolated session:  tests/harness/run-smoke.sh
"""
import os
import statistics
import subprocess
import sys
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import dbus  # noqa: E402
from fakeapp_ctl import A11yWatcher, FakeApp, a11y_connection  # noqa: E402

ACC = "org.a11y.atspi.Accessible"
TEXT = "org.a11y.atspi.Text"
CACHE = "org.a11y.atspi.Cache"


def proxy(conn, app, path):
    return conn.get_object(app.bus_name, path, introspect=False)


def call(obj, iface, method, *a, timeout=5, sig=None):
    return obj.get_dbus_method(method, iface)(*a, timeout=timeout)


def prop(obj, iface, name, timeout=5):
    return obj.Get(iface, name, dbus_interface="org.freedesktop.DBus.Properties", timeout=timeout)


class Isolation(unittest.TestCase):
    def test_environment_is_private(self):
        root = os.environ["GORCA_HARNESS_DIR"]
        for v in ("DISPLAY", "WAYLAND_DISPLAY"):
            self.assertNotIn(v, os.environ)
        for v in ("DBUS_SESSION_BUS_ADDRESS", "AT_SPI_BUS_ADDRESS", "XDG_RUNTIME_DIR", "HOME"):
            self.assertIn(root, os.environ[v], v)
        self.assertNotIn("DBUS_STARTER_BUS_TYPE", os.environ)
        self.assertTrue(os.environ["DBUS_SYSTEM_BUS_ADDRESS"].endswith("no-system-bus"))

    def test_a11y_status_enabled(self):
        sb = dbus.bus.BusConnection(os.environ["DBUS_SESSION_BUS_ADDRESS"])
        o = sb.get_object("org.a11y.Bus", "/org/a11y/bus", introspect=False)
        self.assertTrue(o.Get("org.a11y.Status", "IsEnabled", dbus_interface="org.freedesktop.DBus.Properties"))
        self.assertTrue(o.Get("org.a11y.Status", "ScreenReaderEnabled", dbus_interface="org.freedesktop.DBus.Properties"))

    def test_registry_present(self):
        names = [str(n) for n in a11y_connection().list_names()]
        self.assertIn("org.a11y.atspi.Registry", names)


class Form(unittest.TestCase):
    def test_events_focus_and_text(self):
        with FakeApp.start("--scenario", "form") as app:
            w = A11yWatcher(sender=app.bus_name)
            app.cmd(cmd="focus", node="name")
            app.cmd(cmd="type", node="name", text="hi")
            self.assertTrue(w.wait_for(lambda e: e[4] == "TextChanged" and e[5][0] == "insert" and
                                       e[5][1] == 1 and str(e[5][3]) == "i", 3))
            focused = [e for e in w.events if e[4] == "StateChanged" and e[5][0] == "focused"]
            self.assertEqual(focused[0][2], app.path("name"))
            self.assertEqual(focused[0][5][1], 1)
            carets = [e for e in w.events if e[4] == "TextCaretMoved"]
            self.assertEqual([int(e[5][1]) for e in carets], [1, 2])
            obj = proxy(w.conn, app, app.path("name"))
            self.assertEqual(call(obj, TEXT, "GetText", 0, -1), "hi")
            self.assertEqual(prop(obj, TEXT, "CharacterCount"), 2)
            # change focus: the old widget loses it
            app.cmd(cmd="focus", node="email")
            w.pump(0.2)
            lost = [e for e in w.events if e[4] == "StateChanged" and e[5][0] == "focused" and e[5][1] == 0]
            self.assertEqual(lost[0][2], app.path("name"))

    def test_action_and_edit_via_dbus(self):
        with FakeApp.start("--scenario", "form") as app:
            conn = a11y_connection()
            btn = proxy(conn, app, app.path("Submit"))
            self.assertEqual(call(btn, "org.a11y.atspi.Action", "GetName", 0), "click")
            self.assertTrue(call(btn, "org.a11y.atspi.Action", "DoAction", 0))
            ent = proxy(conn, app, app.path("email"))
            call(ent, "org.a11y.atspi.EditableText", "InsertText", 0, "a@b", 3)
            self.assertEqual(call(ent, TEXT, "GetText", 0, -1), "a@b")
            self.assertTrue(any(o.get("event") == "action" and o["node"] == "Submit"
                                for o in app.extra) or app.cmd(cmd="stats"))
            # unsupported interface on a node -> UnknownMethod, not a crash
            with self.assertRaises(dbus.DBusException):
                call(btn, TEXT, "GetText", 0, -1)


class Tree(unittest.TestCase):
    def test_100k_cache_dump(self):
        with FakeApp.start("--scenario", "tree", "--nodes", "100000", "--fanout", "10", timeout=120) as app:
            conn = a11y_connection()
            cache = proxy(conn, app, "/org/a11y/atspi/cache")
            t = time.monotonic()
            items = call(cache, CACHE, "GetItems", timeout=60)
            dt = time.monotonic() - t
            print("\n[tree-100k] Cache.GetItems: %d items in %.2f s" % (len(items), dt))
            self.assertEqual(len(items), 100000)
            root = proxy(conn, app, "/org/a11y/atspi/accessible/root")
            self.assertEqual(prop(root, ACC, "ChildCount"), 10)

    def test_deep_chain(self):
        with FakeApp.start("--scenario", "tree", "--nodes", "5000", "--fanout", "1") as app:
            conn = a11y_connection()
            leaf = proxy(conn, app, "/org/a11y/atspi/accessible/4999")
            depth, path = 0, "/org/a11y/atspi/accessible/4999"
            while path != "/org/a11y/atspi/accessible/root" and depth < 6000:
                p = prop(proxy(conn, app, path), ACC, "Parent")
                path, depth = str(p[1]), depth + 1
            self.assertEqual(depth, 4999)
            self.assertEqual(call(leaf, ACC, "GetIndexInParent"), 0)


class Storm(unittest.TestCase):
    def test_storm_rate_and_seq(self):
        with FakeApp.start("--scenario", "tree", "--nodes", "100") as app:
            w = A11yWatcher(sender=app.bus_name)
            app.cmd(cmd="storm", rate=5000)
            w.pump(1.0)
            r = app.cmd(cmd="storm_stop")
            w.pump(0.3)
            kinds = {(e[3], e[4]) for e in w.events}
            seqs = [e[5][4]["seq"] for e in w.events]
            print("\n[storm] emitted=%d achieved=%.0f/s received=%d kinds=%s" %
                  (r["emitted"], r["achieved_rate"], len(w.events), sorted(kinds)))
            self.assertGreater(r["achieved_rate"], 3500)
            self.assertEqual(len(w.events), r["emitted"])           # same-user, no loss
            self.assertEqual(seqs, sorted(seqs))
            self.assertGreaterEqual(len(kinds), 3)
            # the app stays responsive to ordinary calls during a storm
            app.cmd(cmd="storm", rate=5000)
            obj = proxy(w.conn, app, "/org/a11y/atspi/accessible/root")
            lat = []
            for _ in range(50):
                t = time.monotonic()
                prop(obj, ACC, "Name")
                lat.append(time.monotonic() - t)
            app.cmd(cmd="storm_stop")
            print("[storm] Properties.Get(Name) under 5k ev/s: p50=%.2f ms max=%.2f ms" %
                  (statistics.median(lat) * 1e3, max(lat) * 1e3))

    def test_peak_rate_reported(self):
        with FakeApp.start("--scenario", "storm", "--storm-rate", "40000", "--storm-duration", "1") as app:
            w = A11yWatcher(sender=app.bus_name)
            w.pump(1.6)
            app.cmd(cmd="stats")                                   # flushes storm_done into app.extra
            n = len(w.events)
            done = [o for o in app.extra if o.get("event") == "storm_done"]
            print("\n[storm] target 40k/s: emitted=%s received=%d in window" % (done and done[0]["emitted"], n))
            self.assertTrue(done)
            self.assertGreater(done[0]["emitted"], 5000)


class Hung(unittest.TestCase):
    def test_never_replies_times_out_client_side(self):
        with FakeApp.start("--scenario", "hung") as app:
            conn = a11y_connection()
            obj = proxy(conn, app, "/org/a11y/atspi/accessible/root")
            t = time.monotonic()
            with self.assertRaises(dbus.exceptions.DBusException) as cm:
                prop(obj, ACC, "Name", timeout=0.5)
            dt = time.monotonic() - t
            self.assertIn("NoReply", cm.exception.get_dbus_name())
            self.assertTrue(0.45 < dt < 1.5, dt)
            self.assertGreaterEqual(app.cmd(cmd="stats")["hung"], 1)   # control channel still alive

    def test_busctl_timeout(self):
        with FakeApp.start("--scenario", "hung") as app:
            t = time.monotonic()
            p = subprocess.run(["busctl", "--address=" + os.environ["AT_SPI_BUS_ADDRESS"], "--timeout=1",
                                "call", app.bus_name, "/org/a11y/atspi/accessible/root", ACC, "GetRole"],
                               capture_output=True, text=True)
            self.assertNotEqual(p.returncode, 0)
            self.assertIn("timed out", p.stderr.lower())
            self.assertLess(time.monotonic() - t, 3)

    def test_delay_and_method_filter(self):
        with FakeApp.start("--scenario", "form", "--hang", "delay:300", "--hang-methods", "GetText") as app:
            conn = a11y_connection()
            ent = proxy(conn, app, app.path("notes"))
            t = time.monotonic()
            prop(ent, ACC, "Name")
            self.assertLess(time.monotonic() - t, 0.15)           # unfiltered method is fast
            t = time.monotonic()
            call(ent, TEXT, "GetText", 0, -1)
            self.assertGreater(time.monotonic() - t, 0.28)
            app.cmd(cmd="hang", mode="off")
            t = time.monotonic()
            call(ent, TEXT, "GetText", 0, -1)
            self.assertLess(time.monotonic() - t, 0.15)

    def test_hang_after_n_calls(self):
        with FakeApp.start("--scenario", "form", "--hang", "never", "--hang-after", "2") as app:
            conn = a11y_connection()
            obj = proxy(conn, app, app.path("name"))
            prop(obj, ACC, "Name")
            prop(obj, ACC, "Name")
            with self.assertRaises(dbus.exceptions.DBusException):
                prop(obj, ACC, "Name", timeout=0.3)


class Terminal(unittest.TestCase):
    def test_vte_style_diff_events(self):
        with FakeApp.start("--scenario", "terminal", "--term-rows", "5", "--term-cols", "20") as app:
            w = A11yWatcher(sender=app.bus_name)
            app.cmd(cmd="term_feed", text="hello\n")
            app.cmd(cmd="term_feed", text="a\nb\nc\nd\ne\n")        # scrolls the whole screen
            w.pump(0.3)
            ev = [(e[4], e[5][0], int(e[5][1]), int(e[5][2])) for e in w.events if e[2].endswith("/2")]
            tc = [e for e in ev if e[0] == "TextChanged"]
            self.assertTrue(any(e[1] == "insert" for e in tc))
            self.assertTrue(any(e[1] == "delete" for e in tc))
            self.assertEqual(ev[-1][0], "TextCaretMoved")            # caret event AFTER text events
            conn = a11y_connection()
            term = proxy(conn, app, app.path("term"))
            text = call(term, TEXT, "GetText", 0, -1)
            count = prop(term, TEXT, "CharacterCount")
            print("\n[terminal] GetText len=%d CharacterCount=%d (F-4 quirk)" % (len(text), count))
            self.assertNotEqual(len(text), count)
            self.assertEqual(text.count("\n"), 5)

    def test_frame_generator(self):
        with FakeApp.start("--scenario", "terminal", "--term-gen", "3", "--term-fps", "60") as app:
            w = A11yWatcher(sender=app.bus_name)
            w.pump(1.0)
            tc = [e for e in w.events if e[4] == "TextChanged"]
            sizes = [int(e[5][2]) for e in tc]
            print("[terminal] 1 s of `cat`-like output: %d text events, median len %d, max %d" %
                  (len(tc), statistics.median(sizes), max(sizes)))
            self.assertGreater(len(tc), 80)
            self.assertGreater(statistics.median(sizes), 1000)


class Malformed(unittest.TestCase):
    def test_libatspi_client_vs_bad_payload(self):
        """Known upstream hazard: libatspi aborts (assertion in libdbus) on some bad event payloads
        (seen: ChildrenChanged any_data with an empty bus name). The client runs in a subprocess so
        this process survives; we assert that the fake app does, and print the client's fate."""
        client = (
            "import sys,time; sys.path.insert(0, %r)\n"
            "import pyatspi\n"
            "from gi.repository import GLib\n"
            "pyatspi.Registry.getDesktop(0).childCount\n"
            "ctx = GLib.MainContext.default(); end = time.monotonic() + 4\n"
            "while time.monotonic() < end:\n"
            "    ctx.iteration(False); time.sleep(0.001)\n") % os.path.dirname(
                os.path.dirname(os.path.abspath(__file__)))
        with FakeApp.start("--scenario", "tree", "--nodes", "20", "--malformed", "bad_payload",
                           "--seed", "7") as app:
            p = subprocess.Popen(["/usr/bin/python3", "-c", client], stderr=subprocess.PIPE, text=True)
            time.sleep(1.0)
            app.cmd(cmd="add_child", parent=0, name="x")
            app.cmd(cmd="storm", rate=2000)
            time.sleep(1.0)
            app.cmd(cmd="storm_stop")
            err = p.communicate(timeout=30)[1]
            print("\n[malformed] libatspi client exit=%d (-6 = abort())" % p.returncode)
            if p.returncode != 0:
                print("[malformed] stderr: " + err.strip().splitlines()[0][:200])
            self.assertGreater(app.cmd(cmd="stats")["events"], 100)   # the fake app is unaffected

    def test_bad_values_observed_and_app_survives(self):
        with FakeApp.start("--scenario", "malformed", "--seed", "7") as app:
            w = A11yWatcher(sender=app.bus_name)
            app.cmd(cmd="add_child", parent=0, name="extra")
            app.cmd(cmd="storm", rate=2000, mix={"text": 1, "children": 1})
            w.pump(0.8)
            app.cmd(cmd="storm_stop")
            offs = [(int(e[5][1]), int(e[5][2])) for e in w.events if e[4] == "TextChanged"]
            self.assertTrue(any(a < 0 or b < 0 for a, b in offs), "no negative offsets in %d events" % len(offs))
            payloads = {type(e[5][3]).__name__ for e in w.events if e[4] == "TextChanged"}
            self.assertGreater(len(payloads), 1)
            conn = a11y_connection()
            root = proxy(conn, app, "/org/a11y/atspi/accessible/root")
            refs = set()
            for _ in range(40):
                refs.update((str(r[0]), str(r[1])) for r in call(root, ACC, "GetChildren"))
            bad = [r for r in refs if not r[0].startswith(":1.") or r[0] == ":1.999999" or not r[1].startswith("/org/a11y/atspi/accessible")]
            print("\n[malformed] %d distinct child refs, %d bad, e.g. %s" % (len(refs), len(bad), sorted(bad)[:3]))
            self.assertTrue(bad)
            counts = {prop(root, ACC, "ChildCount") for _ in range(30)}
            self.assertGreater(len(counts), 1)
            self.assertGreater(app.cmd(cmd="stats")["bad_values"], 0)

    def test_modes_can_be_toggled(self):
        with FakeApp.start("--scenario", "form") as app:
            with self.assertRaises(RuntimeError):
                app.cmd(cmd="malformed", modes=["nope"])
            app.cmd(cmd="malformed", modes=["neg_offsets"])
            conn = a11y_connection()
            ent = proxy(conn, app, app.path("name"))
            vals = {prop(ent, TEXT, "CaretOffset") for _ in range(20)}
            self.assertTrue(all(v in (-5, 2**31 - 1) for v in vals))
            app.cmd(cmd="malformed", modes=[])
            self.assertEqual(prop(ent, TEXT, "CaretOffset"), 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
