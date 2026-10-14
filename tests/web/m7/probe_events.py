import os, sys, time, json, subprocess
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

ROOT = "<repo>/.claude/worktrees/agent-a1d08cc29c61e0bae"
AX = ROOT + "/tests/web/m7/ax.py"
addr = os.environ["AT_SPI_BUS_ADDRESS"]
conn = Gio.DBusConnection.new_for_address_sync(
    addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
seen = []


def cb(c, sender, path, iface, member, params, ud):
    seen.append((iface.split(".")[-1], member, params.unpack()[0] if params.n_children() else "", path.split("/")[-1]))


conn.signal_subscribe(None, None, None, None, None, Gio.DBusSignalFlags.NONE, cb, None)


def ax(req):
    out = subprocess.run(["/usr/bin/python3", AX, "--once", json.dumps(req)], capture_output=True, text=True).stdout
    return json.loads(out)


def pump(sec):
    end = time.time() + sec
    ctx = GLib.MainContext.default()
    while time.time() < end:
        while ctx.iteration(False):
            pass
        time.sleep(0.02)


r = ax({"cmd": "find_doc", "title": "M7 Article", "timeout_ms": 60000})
print("doc", r, file=sys.stderr)
pump(1.5)
seen.clear()
f = ax({"cmd": "focus", "bus": r["bus"], "path": r["path"]})
print("focus", f, file=sys.stderr)
pump(2)
print("events after GrabFocus:", seen[:40], file=sys.stderr)
# the frame's state
def call(bus, path, iface, m, args=None, sig=None):
    v = GLib.Variant(sig, args) if sig else None
    return conn.call_sync(bus, path, iface, m, v, None, Gio.DBusCallFlags.NONE, 3000, None).unpack()
print("doc state:", call(r["bus"], r["path"], "org.a11y.atspi.Accessible", "GetState"), file=sys.stderr)
par = call(r["bus"], r["path"], "org.freedesktop.DBus.Properties", "Get", ("org.a11y.atspi.Accessible", "Parent"), "(ss)")
print("parent", par, file=sys.stderr)

reg = 'org.a11y.atspi.Registry'
for ev in ('object:state-changed:', 'object:children-changed:', 'window:', 'focus:'):
    try:
        conn.call_sync('org.a11y.atspi.Registry', '/org/a11y/atspi/registry', reg, 'RegisterEvent', GLib.Variant('(sas)', (ev, [])), None, Gio.DBusCallFlags.NONE, 3000, None)
    except Exception as e:
        print('reg', ev, e, file=sys.stderr)
pump(1)
frame = par[0]
print('frame state:', call(frame[0], frame[1], 'org.a11y.atspi.Accessible', 'GetState'), file=sys.stderr)
seen.clear()
f = ax({'cmd': 'focus', 'bus': r['bus'], 'path': r['path']})
pump(2)
print('events after 2nd focus:', seen[:30], file=sys.stderr)

f = ax({'cmd': 'focus', 'bus': frame[0], 'path': frame[1]})
print('frame grab:', f, file=sys.stderr)
for i in range(12):
    pump(1)
    st = call(frame[0], frame[1], 'org.a11y.atspi.Accessible', 'GetState')[0][0]
    print(i, 'frame active' if st >> 1 & 1 else 'frame inactive', 'focused' if st >> 12 & 1 else '', file=sys.stderr)
print('events:', [e for e in seen if e[0] != 'DBus'][:30], file=sys.stderr)
