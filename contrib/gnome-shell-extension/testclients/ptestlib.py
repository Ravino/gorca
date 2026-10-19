#!/usr/bin/python3
# Test library for org.gorca.ShellHelper v2 (run with /usr/bin/python3 as the rig's allow-listed caller binary, see run-as.sh).
# Environment: BUS (private session bus address), WL (wayland display), RT (runtime dir), W (rig workdir).
import json, os, subprocess, sys, time
import gi
gi.require_version('Gio', '2.0')
from gi.repository import Gio, GLib

BUS = os.environ['BUS']


def new_conn():
    return Gio.DBusConnection.new_for_address_sync(
        BUS, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)


conn = new_conn()


class HelperError(Exception):
    def __init__(self, name, msg):
        super().__init__(f'{name}: {msg}')
        self.name, self.msg = name, msg


def mono_us():
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1000


def V(sig, val):
    return GLib.Variant(sig, val)


def opts(not_after_ms=500, **kw):
    """a{sv} options with a not-after deadline `not_after_ms` from now (None = omit); kw values are Variants."""
    d = {}
    if not_after_ms is not None:
        d['not-after'] = V('x', mono_us() + int(not_after_ms * 1000))
    d.update(kw)
    return d


def call(method, sig=None, args=None, rsig='', timeout=3000, c=None):
    params = GLib.Variant(sig, args) if sig else None
    try:
        r = (c or conn).call_sync('org.gorca.ShellHelper', '/org/gorca/ShellHelper', 'org.gorca.ShellHelper', method,
                           params, GLib.VariantType(rsig) if rsig else None, 0, timeout, None)
    except GLib.Error as e:
        name = Gio.DBusError.get_remote_error(e) or str(e.code)
        msg = Gio.DBusError.strip_remote_error(e) if Gio.DBusError.get_remote_error(e) else e.message
        raise HelperError(name.replace('org.gorca.ShellHelper.Error.', ''), e.message.split(': ', 1)[-1]) from None
    return r.unpack()[0] if r.n_children() == 1 else r.unpack()


def ping(): return call('Ping', rsig='(u)')
def caps(): return call('Capabilities', rsig='(as)')
def windows(): return call('GetWindows', rsig='(aa{sv})')
def monitors(): return call('GetMonitors', rsig='(aa{sv})')
def get_pointer(): return call('GetPointer', rsig='(a{sv})')
def move(win, origin, x, y, o=None, c=None): return call('PointerMove', '(tsdda{sv})', (win, origin, x, y, o if o is not None else opts()), '(a{sv})', c=c)
def click_at(win, origin, x, y, button=1, o=None, c=None): return call('PointerClickAt', '(tsddua{sv})', (win, origin, x, y, button, o if o is not None else opts()), '(a{sv})', c=c)
def button(b, action, o=None, c=None): return call('PointerButton', '(usa{sv})', (b, action, o if o is not None else opts()), '(a{sv})', c=c)
def release(c=None): return call('PointerRelease', rsig='(u)', c=c)


def find_windows(**kw):
    out = []
    for w in windows():
        if all(w.get(k) == v for k, v in kw.items()):
            out.append(w)
    return out


def wait_for(fn, timeout=10.0, step=0.1):
    t0 = time.time()
    while time.time() - t0 < timeout:
        r = fn()
        if r:
            return r
        time.sleep(step)
    return None


class Client:
    """A spawned test application; its stdout is parsed for EV/READY lines. Killed with kill() (own Popen only)."""
    def __init__(self, argv, env_extra=None, name='client'):
        env = dict(os.environ)
        env.update({'WAYLAND_DISPLAY': os.environ['WL'], 'XDG_RUNTIME_DIR': os.environ['RT'],
                    'DBUS_SESSION_BUS_ADDRESS': BUS, 'GTK_A11Y': 'none'})
        env.update(env_extra or {})
        self.log = open(os.path.join(os.environ['W'], f'{name}.log'), 'w')
        self.p = subprocess.Popen(argv, env=env, stdout=self.log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
        self.name = name
        with open(os.path.join(os.environ['W'], 'pids'), 'a') as f:
            f.write(f'{self.p.pid}\n')
        self.path = self.log.name

    def events(self):
        with open(self.path) as f:
            return [l.split()[1:] for l in f if l.startswith('EV ')]

    def text(self):
        with open(self.path) as f:
            return f.read()

    def kill(self):
        if self.p.poll() is None:
            self.p.terminate()
            try:
                self.p.wait(3)
            except subprocess.TimeoutExpired:
                self.p.kill()


PROBE = ['gdbus', 'call', '--session', '-d', 'org.gorca.MouseProbe', '-o', '/org/gorca/MouseProbe', '-m']


def probe(method, *args):
    """Test-only org.gorca.MouseProbe (tools/platform-probes/mouse-emulation/ext): HideOverview, ShowOverview,
    Notify, MoveFrame, Frames."""
    env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS=BUS)
    return subprocess.run(PROBE + ['org.gorca.MouseProbe.' + method] + [str(a) for a in args], env=env,
                          capture_output=True, text=True, timeout=10).stdout.strip()


class Results:
    def __init__(self):
        self.rows = []

    def check(self, name, ok, detail=''):
        self.rows.append((name, bool(ok), detail))
        print(('PASS ' if ok else 'FAIL ') + name + (f'  [{detail}]' if detail else ''), flush=True)
        return ok

    def summary(self):
        bad = [r for r in self.rows if not r[1]]
        print(f'SUMMARY {len(self.rows) - len(bad)}/{len(self.rows)} passed', flush=True)
        return 1 if bad else 0
