#!/usr/bin/python3
"""Orca vs gorca on a Signal-like Electron/Chromium page, inside a private session.

Runs ONLY under tests/harness/run-isolated.sh (see run.sh): private session bus, private a11y bus,
registryd, a private headless compositor reached through GORCA_M7_WAYLAND / GORCA_M7_CBUS.
Nothing touches the user's live session; every child is started by this script and is killed by
its recorded process group.

env: SC_READER=orca|gorca|none  SC_APP=chromium|chrome|electron  SC_PAGE=<file path>  SC_OUT=<dir>
     SC_SCENARIO=<name from SCENARIOS>  SC_GORCA_BIN=<gorca binary>  SC_APPNAME=<electron app name>
Output (SC_OUT): speech.log (the mock speech server: time, verb, text), steps.tsv (one row per key:
step, key, what the compositor did with it, FOCUSED element after the step, speech after the step),
reader.log, browser.log.
"""
import json, os, signal, socket, subprocess, sys, threading, time, hashlib

import gi
gi.require_version('Atspi', '2.0')
from gi.repository import Atspi, GLib  # noqa: E402

ROOT = os.environ.get('SC_ROOT', os.getcwd())
OUT = os.environ['SC_OUT']
READER = os.environ.get('SC_READER', 'none')
APP = os.environ.get('SC_APP', 'chromium')
PAGE = os.environ['SC_PAGE']
SCEN = os.environ.get('SC_SCENARIO', 'chat')
GORCA = os.environ.get('SC_GORCA_BIN', '<repo>/target/release/gorca')
RT = os.environ['XDG_RUNTIME_DIR']
HOME = os.environ['HOME']
os.makedirs(OUT, exist_ok=True)
T0 = time.monotonic()
PGIDS = []


def ms():
    return int((time.monotonic() - T0) * 1000)


# ---------------------------------------------------------------- mock speech-dispatcher (SSIP)
SPEECH = open(os.path.join(OUT, 'speech.log'), 'w', buffering=1)
SPEECH_LOCK = threading.Lock()
SPEECH_ITEMS = []  # (ms, verb, text)


def log_speech(verb, text):
    with SPEECH_LOCK:
        SPEECH_ITEMS.append((ms(), verb, text))
        SPEECH.write('%d\t%s\t%s\n' % (ms(), verb, text.replace('\n', ' | ')))


def serve_client(c):
    f = c.makefile('rwb', buffering=0)
    speaking = False
    buf = []
    msgid = [0]

    def w(s):
        c.sendall(s.encode())
    try:
        while True:
            line = f.readline()
            if not line:
                return
            s = line.decode('utf8', 'replace').rstrip('\r\n')
            if speaking:
                if s == '.':
                    speaking = False
                    msgid[0] += 1
                    log_speech('SPEAK', '\n'.join(buf))
                    buf = []
                    w('225-%d\r\n225 OK MESSAGE QUEUED\r\n' % msgid[0])
                else:
                    buf.append(s[1:] if s.startswith('..') else s)
                continue
            u = s.upper().split()
            verb = u[0] if u else ''
            if verb == 'SPEAK':
                speaking = True
                w('230 OK RECEIVING DATA\r\n')
            elif verb in ('CHAR', 'KEY'):
                log_speech(verb, s.partition(' ')[2])
                msgid[0] += 1
                w('225-%d\r\n225 OK MESSAGE QUEUED\r\n' % msgid[0])
            elif verb in ('STOP', 'CANCEL', 'PAUSE', 'RESUME'):
                log_speech(verb, s.partition(' ')[2])
                w('210 OK STOPPED\r\n' if verb == 'STOP' else '211 OK CANCELED\r\n')
            elif verb == 'QUIT':
                w('231 HAPPY HACKING\r\n')
                return
            elif verb == 'LIST':
                w('250-mock\r\n250 OK LIST SENT\r\n' if 'OUTPUT_MODULES' in u else '249-mock\r\n249 OK VOICE LIST SENT\r\n')
            elif verb == 'SET':
                w('203 OK SET\r\n')
            elif verb == 'GET':
                w('251-1\r\n251 OK GET RETURNED\r\n')
            elif verb == 'BLOCK':
                w('260 OK INSIDE BLOCK\r\n')
            elif verb == 'SOUND_ICON':
                w('225-1\r\n225 OK MESSAGE QUEUED\r\n')
            elif verb == 'HISTORY':
                w('200 OK\r\n')
            else:
                w('200 OK\r\n')
    except OSError:
        return


def start_speech():
    d = os.path.join(RT, 'speech-dispatcher')
    os.makedirs(d, exist_ok=True)
    p = os.path.join(d, 'speechd.sock')
    if os.path.exists(p):
        os.unlink(p)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(p)
    s.listen(8)

    def loop():
        while True:
            try:
                c, _ = s.accept()
            except OSError:
                return
            threading.Thread(target=serve_client, args=(c,), daemon=True).start()
    threading.Thread(target=loop, daemon=True).start()
    return p


# ---------------------------------------------------------------- processes
def spawn(argv, log, env_extra=None, stdin=None):
    env = dict(os.environ)
    env.update(env_extra or {})
    f = open(os.path.join(OUT, log), 'w')
    p = subprocess.Popen(argv, env=env, stdout=f, stderr=subprocess.STDOUT, stdin=stdin,
                         start_new_session=True)
    PGIDS.append(p.pid)
    return p


def killall():
    for pg in reversed(PGIDS):
        try:
            os.killpg(pg, signal.SIGTERM)
        except OSError:
            pass
    time.sleep(1.0)
    for pg in reversed(PGIDS):
        try:
            os.killpg(pg, signal.SIGKILL)
        except OSError:
            pass


class JsonProc:
    def __init__(self, argv, log):
        self.p = subprocess.Popen(argv, env=dict(os.environ), stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=open(os.path.join(OUT, log + '.err'), 'w'), start_new_session=True)
        PGIDS.append(self.p.pid)
        self.n = 0

    def call(self, obj, timeout=5.0):
        self.n += 1
        obj['id'] = self.n
        self.p.stdin.write((json.dumps(obj) + '\n').encode())
        self.p.stdin.flush()
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            line = self.p.stdout.readline()
            if not line:
                return None
            try:
                r = json.loads(line)
            except ValueError:
                continue
            if r.get('id') == self.n:
                return r
        return None


# ---------------------------------------------------------------- keys
KEYS = {  # name: (xkb keycode, keysym, evdev)
    'Down': (116, 0xff54, 108), 'Up': (111, 0xff52, 103), 'Left': (113, 0xff51, 105), 'Right': (114, 0xff53, 106),
    'Tab': (23, 0xff09, 15), 'Return': (36, 0xff0d, 28), 'Escape': (9, 0xff1b, 1), 'Space': (65, 0x20, 57),
    'Home': (110, 0xff50, 102), 'End': (115, 0xff57, 107), 'Insert': (118, 0xff63, 110), 'Shift': (50, 0xffe1, 42),
    'ShiftTab': (23, 0xff09, 15), 'h': (43, 0x68, 35), 'b': (56, 0x62, 48), 'k': (45, 0x6b, 37), 'a': (38, 0x61, 30),
}
FM = None
RD = None


def press(name, mods=()):
    """Send a key through the emulated Mutter; deliver it to the browser when nothing consumed it."""
    kc, ks, ev = KEYS[name]
    r1 = FM.call({'cmd': 'key', 'keycode': kc, 'keysym': ks, 'unichar': 0, 'state': 0, 'released': False}) or {}
    r2 = FM.call({'cmd': 'key', 'keycode': kc, 'keysym': ks, 'unichar': 0, 'state': 0, 'released': True}) or {}
    passed = bool(r1.get('passed', True))
    if passed:
        RD.stdin.write(('key %d\n' % ev).encode())
        RD.stdin.flush()
    return {'notified': r1.get('notified'), 'consumed': r1.get('consumed'), 'passed': passed}


def chord(mod, name):
    if READER == 'orca' and name == 'Space':
        name = 'a'  # Orca toggles sticky focus mode with Orca+A
    mk, ms_, _ = KEYS[mod]
    kk, ks, _ = KEYS[name]
    FM.call({'cmd': 'key', 'keycode': mk, 'keysym': ms_, 'unichar': 0, 'state': 0, 'released': False})
    r = FM.call({'cmd': 'key', 'keycode': kk, 'keysym': ks, 'unichar': 0, 'state': 0, 'released': False}) or {}
    FM.call({'cmd': 'key', 'keycode': kk, 'keysym': ks, 'unichar': 0, 'state': 0, 'released': True})
    FM.call({'cmd': 'key', 'keycode': mk, 'keysym': ms_, 'unichar': 0, 'state': 0, 'released': True})
    return {'consumed': r.get('consumed'), 'passed': bool(r.get('passed', False))}


# ---------------------------------------------------------------- AT-SPI observation (our own page: names are fine)
S = Atspi.StateType


def find_app(pat):
    d = Atspi.get_desktop(0)
    for i in range(d.get_child_count()):
        a = d.get_child_at_index(i)
        if a and pat in (a.get_name() or '').lower():
            return a
    return None


def walk(n, fn, depth=0, maxn=[4000]):
    if maxn[0] <= 0:
        return
    maxn[0] -= 1
    fn(n, depth)
    for i in range(n.get_child_count()):
        c = n.get_child_at_index(i)
        if c:
            walk(c, fn, depth + 1)


def focused_desc(app):
    found = []
    maxn = [4000]

    def f(n, d):
        try:
            if n.get_state_set().contains(S.FOCUSED) and n.get_role_name() not in ('application', 'frame', 'document web'):
                chain = []
                x = n
                for _ in range(5):
                    chain.append(x.get_role_name())
                    x = x.get_parent()
                    if x is None:
                        break
                found.append('%s name=%r desc=%s' % (n.get_role_name(), (n.get_name() or '')[:50], '<'.join(chain)))
        except Exception:
            pass
    try:
        walk(app, f, 0, maxn)
    except Exception as e:
        return 'ERR %s' % e
    return ' ; '.join(found) or '-'


EVENTS = []


def on_event(ev):
    try:
        src = ev.source
        t = ev.type
        if t == 'object:state-changed:focused' and not ev.detail1:
            return
        chain = []
        x = src
        for _ in range(4):
            chain.append(x.get_role_name())
            x = x.get_parent()
            if x is None:
                break
        EVENTS.append((ms(), t, ev.detail1, '%s name=%r %s' % (src.get_role_name(), (src.get_name() or '')[:40], '<'.join(chain))))
    except Exception as e:
        EVENTS.append((ms(), 'ERR', 0, str(e)))


def start_listener():
    L = Atspi.EventListener.new(on_event)
    for t in ('object:state-changed:focused', 'object:active-descendant-changed', 'object:children-changed:add',
              'object:children-changed:remove'):
        L.register(t)
    return L


def pump(secs):
    end = time.monotonic() + secs
    ctx = GLib.MainContext.default()
    while time.monotonic() < end:
        while ctx.iteration(False):
            pass
        time.sleep(0.01)


def wait_doc(pat, timeout=40):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        a = find_app(pat)
        if a:
            cnt = [0]

            def f(n, d):
                if n.get_role_name() == 'document web' and n.get_child_count() > 0:
                    cnt[0] += 1
            try:
                walk(a, f, 0, [3000])
            except Exception:
                pass
            if cnt[0]:
                return a
        time.sleep(0.5)
    return None


def count_nodes(app):
    c = [0]

    def f(n, d):
        c[0] += 1
    walk(app, f, 0, [20000])
    return c[0]


# ---------------------------------------------------------------- scenarios: (label, kind, arg)
SCENARIOS = {
    'probe': [('settle', 'wait', 2.0), ('prime', 'key', 'Shift'), ('up1', 'key', 'Up'), ('probe', 'probe', None)],
    # gorca+Space (sticky focus mode, DP-3) first, then the same arrows as 'chat'
    'chat_focus': [('settle', 'wait', 2.0), ('prime', 'key', 'Shift'), ('toggle', 'chord', ('Insert', 'Space')),
                   ('up1', 'key', 'Up'), ('up2', 'key', 'Up'), ('up3', 'key', 'Up'), ('down1', 'key', 'Down'),
                   ('home', 'key', 'Home'), ('end', 'key', 'End'), ('toggle2', 'chord', ('Insert', 'Space')),
                   ('up4', 'key', 'Up'), ('flush', 'wait', 4.0)],
    'list_focus': [('settle', 'wait', 2.0), ('prime', 'key', 'Shift'), ('toggle', 'chord', ('Insert', 'Space')),
                   ('down1', 'key', 'Down'), ('down2', 'key', 'Down'), ('up1', 'key', 'Up'), ('flush', 'wait', 4.0)],
    # control: arrows and quick-nav letters on a plain article page (browse mode, no roving widgets)
    'doc': [('settle', 'wait', 2.0), ('prime', 'key', 'Shift'), ('down1', 'key', 'Down'), ('down2', 'key', 'Down'),
            ('h1', 'key', 'h'), ('h2', 'key', 'h'), ('down3', 'key', 'Down'), ('up1', 'key', 'Up'), ('flush', 'wait', 4.0)],
    # the page focuses its first conversation button (?focus=grid) or the newest message (?focus=timeline)
    'list': [('settle', 'wait', 2.0), ('prime', 'key', 'Shift'), ('down1', 'key', 'Down'), ('down2', 'key', 'Down'), ('down3', 'key', 'Down'),
             ('up1', 'key', 'Up'), ('up2', 'key', 'Up'), ('flush', 'wait', 4.0)],
    'chat': [('settle', 'wait', 2.0), ('prime', 'key', 'Shift'), ('up1', 'key', 'Up'), ('up2', 'key', 'Up'), ('up3', 'key', 'Up'), ('up4', 'key', 'Up'),
             ('down1', 'key', 'Down'), ('down2', 'key', 'Down'), ('home', 'key', 'Home'), ('end', 'key', 'End'),
             ('tab', 'key', 'Tab'), ('enter', 'key', 'Return'), ('flush', 'wait', 4.0)],
}


READER_LOG = None


def file_size(p):
    try:
        return os.path.getsize(p) if p else 0
    except OSError:
        return 0


def reader_speech(path, off):
    """What the reader itself says it presented since byte offset off (Orca debug / gorca present log)."""
    if not path or not os.path.exists(path):
        return []
    with open(path, 'rb') as f:
        f.seek(off)
        data = f.read().decode('utf8', 'replace')
    out = []
    for line in data.splitlines():
        if READER == 'orca':
            if 'SPEECH OUTPUT:' in line:
                out.append((0, 'ORCA', line.split('SPEECH OUTPUT:', 1)[1].strip().rsplit(" {", 1)[0]))
        elif READER == 'gorca':
            out.append((0, 'GORCA', line.strip()))
    return out


def probe_focus(app):
    """What the focused node answers on the two name paths (python Atspi, raw Cache.GetItems)."""
    out = {}
    found = []

    def f(n, d):
        try:
            if n.get_state_set().contains(S.FOCUSED) and n.get_role_name() not in ('application', 'frame', 'document web'):
                found.append(n)
        except Exception:
            pass
    walk(app, f, 0, [4000])
    for n in found:
        out['role'] = n.get_role_name()
        out['name'] = n.get_name()
        out['desc'] = n.get_description()
        out['text_children'] = [(c.get_role_name(), c.get_name()) for c in [n.get_child_at_index(i) for i in range(min(n.get_child_count(), 6))] if c]
        try:
            out['attrs'] = dict(n.get_attributes() or {})
        except Exception as e:
            out['attrs'] = str(e)
        try:
            out['relations'] = [(r.get_relation_type().value_nick, r.get_n_targets()) for r in n.get_relation_set()]
        except Exception as e:
            out['relations'] = str(e)
    return out


def main():
    global FM, RD, READER_LOG
    sock = start_speech()
    log_speech('INFO', 'mock speech at ' + sock)
    # the emulated Mutter
    FM = JsonProc(['/usr/bin/python3', os.path.join(ROOT, 'tests/harness/fake_mutter.py')], 'fake_mutter')
    time.sleep(1.0)
    # virtual keyboard on the private compositor
    rd = subprocess.Popen(['/usr/bin/python3', os.path.join(ROOT, 'tests/web/m7/rd.py')], stdin=subprocess.PIPE,
                          stdout=open(os.path.join(OUT, 'rd.log'), 'w'), stderr=subprocess.STDOUT, start_new_session=True)
    PGIDS.append(rd.pid)
    RD = rd
    time.sleep(1.5)

    senv = {'SPEECHD_ADDRESS': 'unix_socket:' + sock}
    def start_reader():
        if READER == 'orca':
            prefs = os.path.join(HOME, 'orca-prefs')
            os.makedirs(prefs, exist_ok=True)
            reader = spawn(['/usr/bin/orca', '-u', prefs, '--debug', '--debug-file=' + os.path.join(OUT, 'orca-debug.out'),
                            '--speech-system=speechdispatcherfactory'], 'reader.log',
                           dict(senv, WAYLAND_DISPLAY=os.environ['GORCA_M7_WAYLAND'], XDG_SESSION_TYPE='wayland', DISPLAY=':77'))
            time.sleep(6)
        elif READER == 'gorca':
            extra = os.environ.get('SC_GORCA_ARGS', '').split()
            reader = spawn([GORCA, '--impersonate-orca', '--speech=ssip', '--ssip-socket=' + sock,
                            '--bench-present-log=' + os.path.join(OUT, 'present.log')] + extra, 'reader.log',
                           dict(senv, RUST_LOG=os.environ.get('SC_RUST_LOG', 'warn,gorca_phase=info')))
            time.sleep(3)
    browser_first = os.environ.get('SC_BROWSER_FIRST') == '1'
    if not browser_first:
        start_reader()
    READER_LOG = os.path.join(OUT, 'orca-debug.out' if READER == 'orca' else 'present.log')
    log_speech('INFO', 'reader %s started' % READER)

    wl = os.environ['GORCA_M7_WAYLAND']
    url = 'file://' + os.path.abspath(PAGE) + os.environ.get('SC_QUERY', '')
    profile = os.path.join(HOME, 'profile')
    os.makedirs(profile, exist_ok=True)
    benv = {'WAYLAND_DISPLAY': wl, 'XDG_SESSION_TYPE': 'wayland'}
    if APP == 'electron':
        ep = os.environ['SC_ELECTRON_DIR']
        name = os.environ.get('SC_APPNAME', 'signal-desktop')
        exe = os.path.join(ep, 'dist2', name)
        pat = name
        argv = [exe, '--no-sandbox', '--disable-gpu', '--ozone-platform=wayland', '--user-data-dir=' + profile,
                os.path.join(ep, 'app'), url]
        if os.environ.get('SC_FORCE_A11Y', '1') == '1':
            argv.insert(1, '--force-renderer-accessibility')
    else:
        exe = {'chromium': '/usr/bin/chromium', 'chrome': '/opt/google/chrome/chrome'}[APP]
        pat = 'chrom'
        argv = [exe, '--user-data-dir=' + profile, '--no-first-run', '--no-default-browser-check', '--ozone-platform=wayland',
                '--window-size=1200,800', '--password-store=basic', '--disable-sync', '--disable-background-networking',
                '--disable-component-update', '--no-proxy-server', url]
        if os.environ.get('SC_FORCE_A11Y', '1') == '1':
            argv.insert(1, '--force-renderer-accessibility')
    br = spawn(argv, 'browser.log', benv)
    doc = wait_doc(pat)
    if doc is None:
        log_speech('INFO', 'document not found')
        print('FATAL no document', file=sys.stderr)
        return 3
    appnode = find_app(pat)
    log_speech('INFO', 'document up, nodes=%d' % count_nodes(appnode))
    LISTENER = start_listener()
    # click once so the window gets the keyboard focus
    RD.stdin.write(b'click\n'); RD.stdin.flush()
    time.sleep(2.0)
    if browser_first:
        start_reader()
        time.sleep(1.5)

    rows = []
    mark = len(SPEECH_ITEMS)
    for label, kind, arg in SCENARIOS[SCEN]:
        settle = float(os.environ.get('SC_SETTLE', '1.3'))
        if kind == 'wait':
            settle = arg
            kind = 'noop'
            arg = '-'
        t_before = ms()
        mark = len(SPEECH_ITEMS)
        off0 = file_size(READER_LOG)
        r = {}
        if kind == 'noop':
            pass
        elif kind == 'key':
            r = press(arg)
        elif kind == 'chord':
            r = chord(*arg)
        elif kind == 'probe':
            r = probe_focus(appnode)
        else:
            r = {}
        pump(settle)
        fd = focused_desc(appnode)
        evs = [e for e in EVENTS if e[0] >= t_before]
        fd += '  EVENTS: ' + ' || '.join('+%d %s %s' % (e[0] - t_before, e[1].replace('object:', ''), e[3]) for e in evs if 'children' not in e[1]) + (' (+%d children-changed)' % sum(1 for e in evs if 'children' in e[1]))
        with SPEECH_LOCK:
            said = [(t - t_before, v, x) for (t, v, x) in SPEECH_ITEMS[mark:]]
        said += reader_speech(READER_LOG, off0)
        rows.append((label, arg, r, fd, said))
        print('%-8s %-8s %s | focus: %s' % (label, arg if isinstance(arg, str) else ('+'.join(arg) if arg else '-'), json.dumps(r), fd))
        for t, v, x in said:
            print('            +%dms %s %s' % (t, v, x[:120]))
    with open(os.path.join(OUT, 'steps.json'), 'w') as f:
        json.dump(rows, f, indent=1)
    return 0


if __name__ == '__main__':
    rc = 1
    try:
        rc = main()
    finally:
        killall()
    sys.exit(rc)
