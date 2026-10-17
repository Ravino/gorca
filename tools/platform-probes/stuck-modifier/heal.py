"""Which no-injection actions clear a stale grabbed_keypresses entry? PRIVATE session only.
usage: heal.py BUSADDR"""
import os, sys, time
L = "/tmp/scratch/ns"
os.environ["DBUS_SESSION_BUS_ADDRESS"] = sys.argv[1]
os.environ["XDG_RUNTIME_DIR"] = L + "/sh/run"
os.environ["WAYLAND_DISPLAY"] = "gorca-test-0"
os.environ["GDK_BACKEND"] = "wayland"
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__))+"/../adr6")
import rig
from rig import pump, call, sess
from gi.repository import GLib

INS, KC_INS = 0xff63, 110
k = rig.Keys(own=False)
g = rig.GtkKeys()
for _ in range(100):
    pump(.1); g.poll()
    if "FOCUS" in g.lines: break
print("lines",g.lines)
pump(.5)


def probe():
    n = len(g.lines)
    k.tap(15); pump(.12); k.tap(30); pump(.3)
    r = g.keys(n)
    return "PASS" if (65289 in r and (97 in r or 65 in r)) else "SWALLOWED"


def kbm(m, args=None, sig=None):
    return call(sess, "org.freedesktop.a11y.Manager", "/org/freedesktop/a11y/Manager",
                "org.freedesktop.a11y.KeyboardMonitor", m, args, sig)


for i in range(5):
    if probe() == "PASS":
        break
    k.tap(125); pump(1.5)
    print('pre', i, g.lines[-3:])
o = rig.Keys(own=True)
o.grabs([INS], [])
pump(.3)
print("baseline", probe())
k.key(KC_INS, True); pump(0.9); k.key(KC_INS, False); pump(1.0)
print("stale after 0.9 s hold:", probe())


def attempt(name, fn):
    fn(); pump(.3)
    r = probe()
    print("%-52s -> %s" % (name, r))
    return r


attempt("re-send SetKeyGrabs([Insert],[])", lambda: o.grabs([INS], []))
attempt("SetKeyGrabs([Insert, KP_Insert, Caps_Lock],[])",
        lambda: o.grabs([INS, 0xff9e, 0xffe5], []))
attempt("GrabKeyboard + UngrabKeyboard cycle",
        lambda: (kbm("GrabKeyboard"), pump(.2), kbm("UngrabKeyboard")))
attempt("UnwatchKeyboard + WatchKeyboard",
        lambda: (kbm("UnwatchKeyboard"), pump(.2), kbm("WatchKeyboard")))
attempt("SetKeyGrabs([Caps_Lock],[]) (Insert dropped): keys flow?",
        lambda: o.grabs([0xffe5], []))
attempt("then SetKeyGrabs([Insert],[]) again (entry still there?)",
        lambda: o.grabs([INS], []))
attempt("SetKeyGrabs([],[]) then GrabKeyboard/Ungrab then re-add Insert",
        lambda: (o.grabs([], []), pump(.2), kbm("GrabKeyboard"), pump(.2), kbm("UngrabKeyboard"),
                 pump(.2), o.grabs([INS], [])))
# release the name and let a second owner take it (no key events involved)
o.stop()
call(sess, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReleaseName",
     GLib.Variant("(s)", ("org.gnome.Orca.KeyboardMonitor",)), "(u)")
pump(.5)
o2 = rig.Keys(own=True)
o2.grabs([INS], [])
pump(.3)
print("%-52s -> %s" % ("owner released name, new owner grabs Insert", probe()))
# control: Insert dropped from the set, then ONE physical Insert tap clears it for good
o2.grabs([0xffe5], []); pump(.3)
print("control: Insert not grabbed, tap Insert (physical)")
k.tap(KC_INS, 0.05); pump(.4)
o2.grabs([INS], []); pump(.3)
print("%-52s -> %s" % ("control: Insert grabbed again after the tap", probe()))
k.stop(); g.p.terminate()
