#!/usr/bin/python3
"""fake_mutter.py - emulates Mutter's org.freedesktop.a11y.KeyboardMonitor on the PRIVATE session bus.

Contract (docs/platform/org.freedesktop.a11y.KeyboardMonitor.md, GNOME 48.7):
  * name `org.freedesktop.a11y.Manager`, path `/org/freedesktop/a11y/Manager`
  * methods WatchKeyboard, UnwatchKeyboard, GrabKeyboard, UngrabKeyboard, SetKeyGrabs(au a(uu))
  * every method is access-checked: the caller's unique name must currently own
    `org.gnome.Orca.KeyboardMonitor`, otherwise `org.freedesktop.DBus.Error.AccessDenied`
  * signal KeyEvent(b u u u q) = released, state, keysym, unichar, keycode, UNICAST to each
    client that called WatchKeyboard or GrabKeyboard (destination = its unique name)

Run ONLY inside tests/harness/run-isolated.sh (refuses to start when the session bus is not
under a gorca-h.* temp dir). Control channel like fakeapp.py: JSON lines on stdin, replies and
notifications on stdout, EOF on stdin quits.

  {"cmd":"key","keycode":38,"keysym":97,"unichar":97,"state":0,"released":false,"id":1}
      -> {"event":"reply","id":1,"ok":true,"t_emit_ns":<CLOCK_MONOTONIC ns just before send>,"sent":<clients>}
  {"cmd":"stats"} -> watchers, grabbers, calls, denied

Grab semantics (M1 test support; ADR-0001 section 7.3 K-1/K-2/K-6). A port of real Mutter code
(paths relative to refs/mutter):
  * src/backends/meta-a11y-manager.c: handle_set_key_grabs, should_grab_keypress,
    should_watch_keypress, meta_a11y_manager_notify_clients,
    key_grabber_bus_name_vanished_callback, rebuild_all_grabbed_modifiers
  * src/backends/native/meta-seat-impl.c: is_a11y_modifier_first_click (called from
    meta_seat_impl_notify_key_in_impl), set_a11y_modifiers

  * SetKeyGrabs(au modifiers, a(uu) keystrokes) REPLACES that caller's whole grab set. A keystroke
    (keysym, mask) matches only when `keysym == k.keysym && state == k.mask`: exact equality with
    no NumLock/CapsLock masking. state is the event modifier state minus mouse buttons
    (MOUSE_BUTTONS_MASK 0x1f00), so NumLock (MOD2, 0x10) and CapsLock (LOCK, 0x02) are part of the
    mask and a client needs the 4 variants {0, LOCK, MOD2, LOCK|MOD2} per key (K-6).
  * A key event is NOTIFIED (KeyEvent unicast) to a client that watches (WatchKeyboard) or would
    grab it. It is CONSUMED (never reaches the app) only when a client grabs it: GrabKeyboard
    (everything), a keystroke match, the grabbed modifier itself, or any key pressed while a
    grabbed modifier is in `grabbed_keypresses`. A consumed press makes the matching release
    consumed too. Watched-but-not-grabbed keys are notified and pass through.
  * A11y-modifier first-click rule: the first press of a grabbed modifier is consumed (and
    notified), and so is its release; a second press of the same keysym within repeat_delay ms
    of the first release is NOT consumed, so the app sees the modifier (double-tap passes it
    through) and keys typed while that second press is held pass through as well.
  * Owner loss: the grab set belongs to the caller's unique name and vanishes with it
    (NameOwnerChanged); the modifier set is recomputed. Nothing is restored for a new owner, it
    must call SetKeyGrabs again. `grabbed_keypresses` is NOT cleared on owner loss, like Mutter
    (a held grabbed key stays consumed until its release).
  * Not emulated: autorepeat (never ignored, no grab logic differences), xkb state tracking (the
    test supplies `state`), duplicate-press dropping (button_count), and Mutter's NULL
    dereference in rebuild_all_grabbed_modifiers when a second client that never called
    SetKeyGrabs exists (only one client may own org.gnome.Orca.KeyboardMonitor anyway).

  {"cmd":"key", ..., "time_ms":<optional event time, default CLOCK_MONOTONIC ms>}
      reply adds notified, consumed, first_click, passed (the app received it)
  {"cmd":"app_log","clear":false} -> {"keys":[[keycode,keysym,released],...]} delivered to the app
  {"cmd":"grabs"} -> per-grabber state, all_grabbed_modifiers, grabbed_keypresses
  {"cmd":"set_repeat_delay","ms":500}   (Mutter uses the GNOME keyboard repeat delay, default 500)
  {"cmd":"set_latency","max_ms":8,"seed":1}
      grab calls (SetKeyGrabs, GrabKeyboard, UngrabKeyboard) are applied after a random delay in
      [0, max_ms] and replied to afterwards, so calls sent concurrently can be applied (and
      replied to) out of order, like a busy Mutter input thread. 0 = apply at once (default).
  {"cmd":"grab_state"} -> {"grab_all":0|1,"n_modifiers":N,"applied":N,"held":N,"xkb_down":N}
      the first grabber's state, the number of grab calls applied so far, and the number of keys
      Mutter currently holds as grabbed, and the keys XKB believes are down (both must be 0 when
      no key is down; a non-zero xkb_down is a stuck key).
  {"cmd":"drop_name","name":":1.7"}     forget a grabber as if its bus name had vanished
"""
import json
import os
import random
import sys
import time

import dbus
import dbus.bus
import dbus.lowlevel
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

NAME = "org.freedesktop.a11y.Manager"
PATH = "/org/freedesktop/a11y/Manager"
IFACE = "org.freedesktop.a11y.KeyboardMonitor"
ORCA = "org.gnome.Orca.KeyboardMonitor"
MOUSE_BUTTONS_MASK = 0x1F00  # CLUTTER_BUTTON1..5_MASK


def out(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()


class Grabber:
    """MetaA11yKeyGrabber. modifiers/keystrokes stay None until SetKeyGrabs, like the C struct."""

    def __init__(self):
        self.grab_all = False
        self.watch_all = False
        self.modifiers = None
        self.keystrokes = None


class Seat:
    """The a11y part of MetaSeatImpl: is_a11y_modifier_first_click and its state."""

    def __init__(self):
        self.repeat_delay = 500
        self.grabbed_modifiers = set()
        self.pressed_modifiers = set()
        self.last_keysym = 0
        self.last_keysym_time = 0
        self.saw_first_release = False

    def first_click(self, keysym, event_time, is_press):
        is_same = keysym == self.last_keysym
        soon = ((event_time - self.last_keysym_time) & 0xFFFFFFFF) < self.repeat_delay
        is_grabbed_modifier = keysym in self.grabbed_modifiers
        self.last_keysym = keysym
        self.last_keysym_time = event_time
        if not is_grabbed_modifier:
            return False
        if not is_press and keysym in self.pressed_modifiers:
            self.pressed_modifiers.discard(keysym)
            return False  # release of a previously (normally) pressed modifier
        if is_same and soon:
            if is_press and self.saw_first_release:
                self.saw_first_release = False
                self.pressed_modifiers.add(keysym)
                return False  # second press, on time: processed normally
            self.saw_first_release = True
            return True  # first release: wait for the second press
        self.saw_first_release = False
        return True  # different modifier, first press, or too late


class Manager(dbus.service.Object):
    def __init__(self, conn):
        super().__init__(conn, PATH)
        self.conn = conn
        self.grabbers = {}  # unique name -> Grabber, in creation order
        self.seat = Seat()
        self.all_grabbed_modifiers = set()
        self.grabbed_keypresses = set()
        self.app_log = []
        self.stats = {"calls": 0, "denied": 0, "keys": 0}
        # Keys XKB believes are down. meta_seat_impl_notify_key_in_impl skips
        # xkb_state_update_key for events that is_a11y_modifier_first_click ignores, so a press
        # that passed followed by a release that is ignored leaves the key down for good (the
        # stuck-key hazard of changing a modifier grab while the modifier is held, P-7).
        self.xkb_down = set()
        self.latency_ms = 0
        self.rng = random.Random(1)
        self.applied = 0
        conn.add_signal_receiver(self.owner_changed, "NameOwnerChanged",
                                 "org.freedesktop.DBus", "org.freedesktop.DBus",
                                 "/org/freedesktop/DBus")

    @property
    def watchers(self):
        return {n for n, g in self.grabbers.items() if g.watch_all}

    def owner_changed(self, name, old, new):
        if name == old and not new:
            self.drop(name)

    def drop(self, name):
        """key_grabber_bus_name_vanished_callback"""
        if self.grabbers.pop(name, None) is not None:
            self.rebuild_modifiers()

    def rebuild_modifiers(self):
        """rebuild_all_grabbed_modifiers + set_a11y_modifiers (a11y-modifiers-changed)"""
        mods = set()
        for g in self.grabbers.values():
            mods.update(g.modifiers or ())
        self.all_grabbed_modifiers = mods
        self.seat.grabbed_modifiers = set(mods)

    def check(self, sender):
        self.stats["calls"] += 1
        try:
            owner = self.conn.get_name_owner(ORCA)
        except dbus.DBusException:
            owner = None
        if owner != sender:
            self.stats["denied"] += 1
            raise dbus.exceptions.DBusException("Access denied",
                                                name="org.freedesktop.DBus.Error.AccessDenied")

    def grabber(self, sender):  # ensure_key_grabber
        return self.grabbers.setdefault(sender, Grabber())

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s")
    def WatchKeyboard(self, s=None):
        self.check(s)
        self.grabber(s).watch_all = True

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s")
    def UnwatchKeyboard(self, s=None):
        self.check(s)
        self.grabber(s).watch_all = False

    def later(self, fn, ok, err):
        """Apply `fn` after a random delay (set_latency) and reply when it ran."""
        def run():
            try:
                fn()
                self.applied += 1
                ok()
            except Exception as ex:  # noqa: BLE001
                err(ex)
            return False
        if self.latency_ms > 0:
            GLib.timeout_add(max(1, int(self.rng.uniform(0, self.latency_ms))), run)
        else:
            run()

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s",
                         async_callbacks=("ok", "err"))
    def GrabKeyboard(self, s=None, ok=None, err=None):
        def fn():
            self.check(s)
            self.grabber(s).grab_all = True
        self.later(fn, ok, err)

    @dbus.service.method(IFACE, in_signature="", out_signature="", sender_keyword="s",
                         async_callbacks=("ok", "err"))
    def UngrabKeyboard(self, s=None, ok=None, err=None):
        def fn():
            self.check(s)
            self.grabber(s).grab_all = False
        self.later(fn, ok, err)

    @dbus.service.method(IFACE, in_signature="aua(uu)", out_signature="", sender_keyword="s",
                         async_callbacks=("ok", "err"))
    def SetKeyGrabs(self, modifiers, keystrokes, s=None, ok=None, err=None):
        def fn():
            self.check(s)
            g = self.grabber(s)
            g.modifiers = [int(m) for m in modifiers]
            g.keystrokes = [(int(k), int(m)) for k, m in keystrokes]
            self.rebuild_modifiers()
        self.later(fn, ok, err)

    def should_grab(self, g, keysym, mods):
        if g.grab_all:
            return True
        for m in g.modifiers or ():
            if keysym == m or m in self.grabbed_keypresses:
                return True
        for k, mask in g.keystrokes or ():
            if keysym == k and mods == mask:  # exact equality: no lock-mask normalisation
                return True
        return False

    def should_watch(self, g, keysym, mods):
        return g.watch_all or self.should_grab(g, keysym, mods)

    def key(self, keycode, keysym, unichar, state, released, time_ms=None):
        """meta_seat_impl_notify_key_in_impl (first-click part) + meta_a11y_manager_notify_clients."""
        t = time.monotonic_ns()
        if time_ms is None:
            time_ms = (t // 1_000_000) & 0xFFFFFFFF
        ignorable = self.seat.first_click(keysym, time_ms, not released)
        if not ignorable:
            if released:
                self.xkb_down.discard(keycode)
            else:
                self.xkb_down.add(keycode)
        mods = state & ~MOUSE_BUTTONS_MASK
        notified = 0
        for dest, g in list(self.grabbers.items()):
            if self.should_watch(g, keysym, mods):
                msg = dbus.lowlevel.SignalMessage(PATH, IFACE, "KeyEvent")
                msg.set_destination(dest)
                msg.append(dbus.Boolean(released), dbus.UInt32(mods), dbus.UInt32(keysym),
                           dbus.UInt32(unichar), dbus.UInt16(keycode & 0xFFFF), signature="buuuq")
                self.conn.send_message(msg)
                notified += 1
        self.conn.flush()
        self.stats["keys"] += 1
        consumed = False
        if keysym in self.all_grabbed_modifiers and not ignorable:
            pass  # a double-tapped grabbed modifier is processed normally
        elif released:
            if keysym in self.grabbed_keypresses:
                self.grabbed_keypresses.discard(keysym)
                consumed = True
        else:
            if keysym in self.grabbed_keypresses:
                consumed = True
            for g in list(self.grabbers.values()):
                if self.should_grab(g, keysym, mods):
                    self.grabbed_keypresses.add(keysym)
                    consumed = True
        if not consumed:
            self.app_log.append([keycode, keysym, bool(released)])
        return {"t_emit_ns": t, "sent": notified, "notified": notified, "consumed": consumed,
                "first_click": ignorable, "passed": not consumed}

    def grab_state(self):
        g = next(iter(self.grabbers.values()), None)
        return {"grab_all": int(bool(g and g.grab_all)),
                "n_modifiers": len(g.modifiers or ()) if g else 0,
                "applied": self.applied, "held": len(self.grabbed_keypresses),
                "xkb_down": len(self.xkb_down), "xkb_first": min(self.xkb_down, default=0)}

    def dump(self):
        return {
            "grabbers": {n: {"grab_all": g.grab_all, "watch_all": g.watch_all,
                             "modifiers": g.modifiers, "keystrokes": g.keystrokes}
                         for n, g in self.grabbers.items()},
            "all_grabbed_modifiers": sorted(self.all_grabbed_modifiers),
            "grabbed_keypresses": sorted(self.grabbed_keypresses),
        }


class Stdin:
    def __init__(self, mgr, loop):
        self.mgr, self.loop, self.buf = mgr, loop, b""
        os.set_blocking(0, False)
        GLib.io_add_watch(0, GLib.IO_IN | GLib.IO_HUP | GLib.IO_ERR, self.ready)

    def ready(self, fd, cond):
        try:
            data = os.read(0, 65536)
        except BlockingIOError:
            return True
        if not data:
            self.loop.quit()
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
                c = cmd.get("cmd")
                res = {}
                if c == "key":
                    tm = cmd.get("time_ms")
                    res = self.mgr.key(int(cmd.get("keycode", 38)), int(cmd.get("keysym", 97)),
                                       int(cmd.get("unichar", 97)), int(cmd.get("state", 0)),
                                       bool(cmd.get("released", False)),
                                       None if tm is None else int(tm))
                elif c == "stats":
                    res = dict(self.mgr.stats, watchers=len(self.mgr.watchers),
                               grabbers=len(self.mgr.grabbers))
                elif c == "app_log":
                    res = {"keys": list(self.mgr.app_log)}
                    if cmd.get("clear"):
                        self.mgr.app_log.clear()
                elif c == "grabs":
                    res = self.mgr.dump()
                elif c == "set_repeat_delay":
                    self.mgr.seat.repeat_delay = int(cmd["ms"])
                elif c == "set_latency":
                    self.mgr.latency_ms = float(cmd["max_ms"])
                    self.mgr.rng = random.Random(int(cmd.get("seed", 1)))
                elif c == "grab_state":
                    res = self.mgr.grab_state()
                elif c == "drop_name":
                    self.mgr.drop(cmd["name"])
                elif c == "quit":
                    self.loop.quit()
                else:
                    raise ValueError("unknown cmd %r" % c)
                out(dict(res, event="reply", id=rid, ok=True))
            except Exception as ex:  # noqa: BLE001
                out({"event": "reply", "id": rid, "ok": False, "error": repr(ex)})
        return True


def main():
    addr = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    if "/gorca-h." not in addr:
        sys.stderr.write("fake_mutter: refusing to run outside tests/harness/run-isolated.sh "
                         "(session bus %r is not a harness bus)\n" % addr)
        sys.exit(2)
    DBusGMainLoop(set_as_default=True)
    conn = dbus.bus.BusConnection(addr)
    # Take the name for real: a flapping client (name_flapper.py) may hold it for a moment.
    end = time.monotonic() + 10
    while conn.request_name(NAME, dbus.bus.NAME_FLAG_DO_NOT_QUEUE) != dbus.bus.REQUEST_NAME_REPLY_PRIMARY_OWNER:
        if time.monotonic() > end:
            sys.exit("fake_mutter: could not take %s" % NAME)
        time.sleep(0.002)
    mgr = Manager(conn)
    loop = GLib.MainLoop()
    Stdin(mgr, loop)
    out({"event": "ready", "bus_name": conn.get_unique_name()})
    try:
        loop.run()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
