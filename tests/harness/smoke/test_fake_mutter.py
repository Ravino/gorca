"""fake_mutter.py grab-semantics tests (M1 test support, ADR-0001 section 7.3 K-1/K-2/K-6).

Every expectation below is traced to real Mutter code, see the fake_mutter.py docstring
(refs/mutter/src/backends/meta-a11y-manager.c and refs/mutter/src/backends/native/meta-seat-impl.c).

Run inside the isolated session:  tests/harness/run-smoke.sh
"""
import json
import os
import subprocess
import sys
import time
import unittest

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

import dbus  # noqa: E402
import dbus.bus  # noqa: E402
from dbus.mainloop.glib import DBusGMainLoop  # noqa: E402
from gi.repository import GLib  # noqa: E402

DBusGMainLoop(set_as_default=True)

NAME = "org.freedesktop.a11y.Manager"
PATH = "/org/freedesktop/a11y/Manager"
IFACE = "org.freedesktop.a11y.KeyboardMonitor"
ORCA = "org.gnome.Orca.KeyboardMonitor"

LOCK, CTRL, MOD2 = 0x02, 0x04, 0x10
KEY_A, KEY_B = 0x61, 0x62
INSERT = 0xFF63  # a typical screen reader modifier keysym
CAPS = 0xFFE5
T0 = 100_000  # event times (ms) are explicit so the first-click rule is deterministic


class Mutter:
    """fake_mutter.py as a child process with a JSON-lines control channel."""

    def __init__(self):
        self.p = subprocess.Popen([sys.executable, os.path.join(HERE, "fake_mutter.py")],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.n = 0
        ready = json.loads(self.p.stdout.readline())
        assert ready["event"] == "ready", ready

    def cmd(self, **kw):
        self.n += 1
        kw["id"] = self.n
        self.p.stdin.write(json.dumps(kw) + "\n")
        self.p.stdin.flush()
        r = json.loads(self.p.stdout.readline())
        assert r["ok"], r
        return r

    def key(self, keysym, state=0, released=False, t=None, keycode=0):
        return self.cmd(cmd="key", keycode=keycode or keysym & 0xFF, keysym=keysym, unichar=0,
                        state=state, released=released, **({} if t is None else {"time_ms": t}))

    def tap(self, keysym, t, state=0, hold_ms=10):
        a = self.key(keysym, state, False, t)
        b = self.key(keysym, state, True, t + hold_ms)
        return a, b

    def close(self):
        try:
            self.p.stdin.close()
            self.p.wait(5)
        except Exception:  # noqa: BLE001
            self.p.kill()


class Client:
    """A would-be screen reader: owns the Orca name, collects unicast KeyEvent signals."""

    def __init__(self, own_name=True):
        self.conn = dbus.bus.BusConnection(os.environ["DBUS_SESSION_BUS_ADDRESS"])
        if own_name:
            self.conn.request_name(ORCA, dbus.bus.NAME_FLAG_DO_NOT_QUEUE)
        self.events = []
        self.conn.add_signal_receiver(self.on_key, "KeyEvent", IFACE, NAME, PATH)
        self.mgr = self.conn.get_object(NAME, PATH, introspect=False)

    def on_key(self, released, state, keysym, unichar, keycode):
        self.events.append((bool(released), int(state), int(keysym), int(keycode)))

    def call(self, m, *a, sig=None):
        f = self.mgr.get_dbus_method(m, IFACE)
        if sig:
            return f(*a, signature=sig)
        return f(*a)

    def set_grabs(self, mods, keystrokes):
        self.mgr.SetKeyGrabs(dbus.Array(mods, signature="u"),
                             dbus.Array([dbus.Struct((dbus.UInt32(k), dbus.UInt32(m)), signature="uu")
                                         for k, m in keystrokes], signature="(uu)"),
                             dbus_interface=IFACE)

    def pump(self, until=lambda: False, timeout=1.0):
        ctx = GLib.MainContext.default()
        end = time.monotonic() + timeout
        while time.monotonic() < end and not until():
            while ctx.iteration(False):
                pass
            time.sleep(0.002)

    def settle(self, n, timeout=2.0):
        self.pump(lambda: len(self.events) >= n, timeout)

    def close(self):
        self.conn.close()


class Base(unittest.TestCase):
    def setUp(self):
        self.m = Mutter()
        self.c = Client()
        self.addCleanup(self.m.close)
        self.addCleanup(self.c.close)


class ExactMask(Base):
    def test_exact_match_consumes_and_notifies(self):
        self.c.set_grabs([], [(KEY_A, CTRL)])
        r = self.m.key(KEY_A, CTRL)
        self.assertEqual((r["consumed"], r["notified"], r["passed"]), (True, 1, False))
        r = self.m.key(KEY_A, CTRL, released=True)  # the release of a consumed press is consumed
        self.assertEqual((r["consumed"], r["notified"]), (True, 1))
        self.c.settle(2)
        self.assertEqual(self.c.events, [(False, CTRL, KEY_A, KEY_A & 0xFF),
                                         (True, CTRL, KEY_A, KEY_A & 0xFF)])

    def test_lock_masks_are_not_normalised(self):
        """K-6: with NumLock or CapsLock on, state differs, so only registered variants match."""
        self.c.set_grabs([], [(KEY_A, CTRL)])
        for extra in (LOCK, MOD2, LOCK | MOD2):
            r = self.m.key(KEY_A, CTRL | extra)
            self.assertEqual((r["consumed"], r["notified"], r["passed"]), (False, 0, True), hex(extra))
            self.m.key(KEY_A, CTRL | extra, released=True)
        # all four variants registered: every lock combination is consumed
        self.c.set_grabs([], [(KEY_A, CTRL | v) for v in (0, LOCK, MOD2, LOCK | MOD2)])
        for extra in (0, LOCK, MOD2, LOCK | MOD2):
            r = self.m.key(KEY_A, CTRL | extra)
            self.assertTrue(r["consumed"], hex(extra))
            self.m.key(KEY_A, CTRL | extra, released=True)

    def test_other_keysym_or_subset_mask_passes(self):
        self.c.set_grabs([], [(KEY_A, CTRL)])
        self.assertTrue(self.m.key(KEY_B, CTRL)["passed"])
        self.assertTrue(self.m.key(KEY_A, 0)["passed"])
        self.assertTrue(self.m.key(KEY_A, CTRL | 0x08)["passed"])  # extra Alt: not equal

    def test_mouse_buttons_are_removed_from_state(self):
        self.c.set_grabs([], [(KEY_A, CTRL)])
        self.assertTrue(self.m.key(KEY_A, CTRL | 0x100)["consumed"])  # BUTTON1_MASK

    def test_set_key_grabs_replaces_the_previous_set(self):
        self.c.set_grabs([], [(KEY_A, CTRL)])
        self.c.set_grabs([], [(KEY_B, CTRL)])
        self.assertTrue(self.m.key(KEY_A, CTRL)["passed"])
        self.assertTrue(self.m.key(KEY_B, CTRL)["consumed"])

    def test_pass_through_vs_watch(self):
        """Watched keys are notified but still reach the app; grabbed keys do not."""
        self.c.call("WatchKeyboard")
        r = self.m.key(KEY_B, 0)
        self.assertEqual((r["notified"], r["consumed"], r["passed"]), (1, False, True))
        self.c.set_grabs([], [(KEY_A, CTRL)])
        r = self.m.key(KEY_A, CTRL)
        self.assertEqual((r["notified"], r["consumed"]), (1, True))
        self.c.settle(2)
        self.assertEqual(len(self.c.events), 2)
        self.assertEqual(self.m.cmd(cmd="app_log")["keys"], [[KEY_B & 0xFF, KEY_B, False]])

    def test_grab_keyboard_consumes_everything_until_ungrab(self):
        self.c.call("GrabKeyboard")
        self.assertTrue(self.m.key(KEY_B, 0)["consumed"])
        self.m.key(KEY_B, 0, released=True)
        self.c.call("UngrabKeyboard")
        self.assertTrue(self.m.key(KEY_B, 0)["passed"])

    def test_no_grabs_means_everything_passes_silently(self):
        r = self.m.key(KEY_A, 0)
        self.assertEqual((r["notified"], r["consumed"], r["passed"]), (0, False, True))

    def test_access_denied_for_non_owner(self):
        other = Client(own_name=False)
        self.addCleanup(other.close)
        with self.assertRaises(dbus.DBusException) as cm:
            other.set_grabs([], [(KEY_A, 0)])
        self.assertEqual(cm.exception.get_dbus_name(), "org.freedesktop.DBus.Error.AccessDenied")


class ModifierFirstClick(Base):
    def setUp(self):
        super().setUp()
        self.c.set_grabs([INSERT], [])

    def test_single_tap_is_swallowed_and_notified(self):
        a, b = self.m.tap(INSERT, T0)
        self.assertEqual((a["first_click"], a["consumed"], a["notified"]), (True, True, 1))
        self.assertEqual((b["first_click"], b["consumed"], b["notified"]), (True, True, 1))
        self.assertEqual(self.m.cmd(cmd="app_log")["keys"], [])

    def test_chord_with_held_modifier_is_consumed(self):
        self.m.key(INSERT, 0, False, T0)
        r = self.m.key(KEY_A, 0, False, T0 + 20)
        self.assertEqual((r["consumed"], r["notified"]), (True, 1))
        self.assertTrue(self.m.key(KEY_A, 0, True, T0 + 40)["consumed"])
        self.assertTrue(self.m.key(INSERT, 0, True, T0 + 60)["consumed"])
        self.assertEqual(self.m.cmd(cmd="app_log")["keys"], [])
        # after the release, typing the same key passes again
        self.assertTrue(self.m.key(KEY_A, 0, False, T0 + 5000)["passed"])

    def test_double_tap_passes_modifier_to_the_app(self):
        self.m.tap(INSERT, T0)  # swallowed: press T0, release T0+10
        p2 = self.m.key(INSERT, 0, False, T0 + 100)  # second press, on time
        self.assertEqual((p2["first_click"], p2["consumed"], p2["passed"], p2["notified"]),
                         (False, False, True, 1))
        # keys typed while the second press is held reach the app
        self.assertTrue(self.m.key(KEY_A, 0, False, T0 + 120)["passed"])
        self.assertTrue(self.m.key(KEY_A, 0, True, T0 + 130)["passed"])
        r = self.m.key(INSERT, 0, True, T0 + 140)  # release of a normally pressed modifier
        self.assertEqual((r["first_click"], r["passed"]), (False, True))
        self.assertEqual(self.m.cmd(cmd="app_log")["keys"],
                         [[INSERT & 0xFF, INSERT, False], [KEY_A & 0xFF, KEY_A, False],
                          [KEY_A & 0xFF, KEY_A, True], [INSERT & 0xFF, INSERT, True]])

    def test_late_second_press_is_a_new_first_click(self):
        self.m.tap(INSERT, T0)
        p2 = self.m.key(INSERT, 0, False, T0 + 10 + 500)  # exactly repeat_delay: not "soon"
        self.assertEqual((p2["first_click"], p2["consumed"]), (True, True))

    def test_repeat_delay_is_configurable(self):
        self.m.cmd(cmd="set_repeat_delay", ms=100)
        self.m.tap(INSERT, T0)
        p2 = self.m.key(INSERT, 0, False, T0 + 10 + 150)
        self.assertTrue(p2["consumed"])
        self.m.key(INSERT, 0, True, T0 + 200)
        self.m.tap(INSERT, T0 + 300)
        self.assertTrue(self.m.key(INSERT, 0, False, T0 + 300 + 10 + 50)["passed"])

    def test_different_modifier_between_taps_resets_the_rule(self):
        self.c.set_grabs([INSERT, CAPS], [])
        self.m.tap(INSERT, T0)
        self.m.tap(CAPS, T0 + 30)
        p = self.m.key(INSERT, 0, False, T0 + 60)
        self.assertTrue(p["consumed"] and p["first_click"])

    def test_non_grabbed_key_between_taps_breaks_the_double_tap(self):
        self.m.tap(INSERT, T0)
        self.m.tap(KEY_B, T0 + 20)  # updates last_keysym, so the next press is not "same keysym"
        p = self.m.key(INSERT, 0, False, T0 + 60)
        self.assertTrue(p["consumed"])

    def test_modifier_not_grabbed_is_never_special(self):
        r = self.m.key(CAPS, 0, False, T0)
        self.assertEqual((r["first_click"], r["passed"], r["notified"]), (False, True, 0))


class OwnerLoss(Base):
    def wait_grabbers(self, n):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if len(self.m.cmd(cmd="grabs")["grabbers"]) == n:
                return
            time.sleep(0.01)
        self.fail("grabbers never reached %d: %r" % (n, self.m.cmd(cmd="grabs")))

    def test_loss_drops_grabs_and_restore_needs_set_key_grabs(self):
        self.c.set_grabs([INSERT], [(KEY_A, CTRL)])
        self.assertEqual(self.m.cmd(cmd="grabs")["all_grabbed_modifiers"], [INSERT])
        self.assertTrue(self.m.key(KEY_A, CTRL)["consumed"])
        self.m.key(KEY_A, CTRL, released=True)

        self.c.close()  # owner loss: unique name vanishes
        self.wait_grabbers(0)
        g = self.m.cmd(cmd="grabs")
        self.assertEqual(g["all_grabbed_modifiers"], [])
        r = self.m.key(KEY_A, CTRL)
        self.assertEqual((r["consumed"], r["notified"], r["passed"]), (False, 0, True))
        self.m.key(KEY_A, CTRL, released=True)
        # the modifier is an ordinary key again
        self.assertTrue(self.m.key(INSERT, 0, False, T0)["passed"])
        self.m.key(INSERT, 0, True, T0 + 10)

        # restore: a new owner has no grabs until it calls SetKeyGrabs again
        c2 = Client()
        self.addCleanup(c2.close)
        self.assertTrue(self.m.key(KEY_A, CTRL)["passed"])
        self.m.key(KEY_A, CTRL, released=True)
        c2.set_grabs([INSERT], [(KEY_A, CTRL)])
        self.assertEqual(self.m.cmd(cmd="grabs")["all_grabbed_modifiers"], [INSERT])
        self.assertTrue(self.m.key(KEY_A, CTRL)["consumed"])
        c2.settle(1)
        self.assertEqual(len(c2.events), 1)

    def test_held_grabbed_key_stays_consumed_across_owner_loss(self):
        """Mutter never clears grabbed_keypresses on owner loss, so the release is still eaten."""
        self.c.set_grabs([], [(KEY_A, CTRL)])
        self.assertTrue(self.m.key(KEY_A, CTRL)["consumed"])
        self.c.close()
        self.wait_grabbers(0)
        r = self.m.key(KEY_A, CTRL, released=True)
        self.assertEqual((r["consumed"], r["notified"]), (True, 0))

    def test_drop_name_command_emulates_owner_loss_without_disconnect(self):
        self.c.set_grabs([INSERT], [])
        name = self.c.conn.get_unique_name()
        self.m.cmd(cmd="drop_name", name=name)
        self.assertEqual(self.m.cmd(cmd="grabs")["grabbers"], {})
        self.assertTrue(self.m.key(INSERT, 0, False, T0)["passed"])


if __name__ == "__main__":
    unittest.main()
