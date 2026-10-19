# Shared bits for the MO-0 spikes (AT-SPI access in the private session, window helpers).
import os, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from ptestlib import *
import gi
gi.require_version('Atspi', '2.0')
from gi.repository import Atspi

W = os.environ['W']
HERE = os.path.dirname(os.path.abspath(__file__))


def start_a11y():
    """Private a11y: enable IsEnabled on the private bus, start registryd (PID recorded in W/pids)."""
    env = dict(os.environ, DBUS_SESSION_BUS_ADDRESS=BUS)
    subprocess.run(['gdbus', 'call', '--session', '-d', 'org.a11y.Bus', '-o', '/org/a11y/bus', '-m',
                    'org.freedesktop.DBus.Properties.Set', 'org.a11y.Status', 'IsEnabled', '<true>'],
                   env=env, capture_output=True)
    p = subprocess.Popen(['/usr/libexec/at-spi2-registryd'], env=env, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    with open(os.path.join(W, 'pids'), 'a') as f:
        f.write(f'{p.pid}\n')
    time.sleep(1.5)
    Atspi.init()
    return p


def walk(a, depth=0, maxd=40):
    yield a
    if depth > maxd:
        return
    try:
        n = a.get_child_count()
    except Exception:
        return
    for i in range(n):
        try:
            c = a.get_child_at_index(i)
        except Exception:
            continue
        if c:
            yield from walk(c, depth + 1, maxd)


def app_for_pid(pid):
    d = Atspi.get_desktop(0)
    for i in range(d.get_child_count()):
        a = d.get_child_at_index(i)
        try:
            if a and a.get_process_id() == pid:
                return a
        except Exception:
            pass
    return None


def find(node, role=None, name=None, pred=None):
    for n in walk(node):
        try:
            if role and n.get_role_name() != role:
                continue
            if name is not None and n.get_name() != name:
                continue
            if pred and not pred(n):
                continue
            return n
        except Exception:
            continue
    return None


def extents(n):
    c = n.get_component_iface()
    r = {}
    for nm, t in (('SCREEN', Atspi.CoordType.SCREEN), ('WINDOW', Atspi.CoordType.WINDOW), ('PARENT', Atspi.CoordType.PARENT)):
        e = c.get_extents(t)
        r[nm] = (e.x, e.y, e.width, e.height)
    return r


def brief(w):
    return {k: w[k] for k in ('id', 'title', 'type', 'transient-for', 'client-type', 'frame-x', 'frame-y', 'frame-w', 'frame-h',
                              'buffer-x', 'buffer-y', 'buffer-w', 'buffer-h', 'monitor', 'visible')}


def center(e):
    return (e[0] + e[2] / 2, e[1] + e[3] / 2)
