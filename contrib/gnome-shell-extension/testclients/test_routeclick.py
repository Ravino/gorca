#!/usr/bin/python3
# Route (movewin) then click at the pointer (click L): MO-5a (resolver) end to end: the connector's resolver (locate_cli) on a real client of a real widget -> ToplevelHint + point -> the Rust helper client's window matcher
# -> the real helper -> the click must activate exactly that widget (the page button, the GTK button).
# Usage: test_mapping.py TOOLKIT...   (gtk4 gtk3 gtk3x firefox chromium electron)   in a rig started by test-mapping.sh
import os, re, subprocess, sys
from spike_lib import *

W = os.environ['W']
HERE = os.path.dirname(os.path.abspath(__file__))
RS = os.path.join(W, 'rs-a')
LOC = os.path.join(W, 'locate-cli')
A11Y = subprocess.run(['gdbus', 'call', '--session', '-d', 'org.a11y.Bus', '-o', '/org/a11y/bus', '-m', 'org.a11y.Bus.GetAddress'], capture_output=True, text=True, env=dict(os.environ, DBUS_SESSION_BUS_ADDRESS=os.environ['BUS'])).stdout.split("'")[1]
EP = os.path.expanduser('~/.cache/electron-probe')
PAGE = 'file://' + os.path.join(HERE, 'page_menu.html')
R = Results()


class Pointer:
    def __init__(self):
        self.p = subprocess.Popen([RS, BUS], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        with open(os.path.join(W, 'pids'), 'a') as f:
            f.write(f'{self.p.pid}\n')
        self.first = self.p.stdout.readline().strip()

    def cmd(self, line):
        self.p.stdin.write(line + '\n')
        self.p.stdin.flush()
        return self.p.stdout.readline().strip()

    def quit(self):
        self.p.stdin.write('quit\n')
        self.p.stdin.flush()
        self.p.stdout.read()
        self.p.wait(5)


def launch(kind):
    home = os.path.join(W, 'home-' + kind)
    os.makedirs(home, exist_ok=True)
    os.makedirs(os.path.join(W, 'ffprof'), exist_ok=True)
    env = {'HOME': home, 'GNOME_ACCESSIBILITY': '1', 'GTK_A11Y': 'atspi', 'MOZ_ENABLE_WAYLAND': '1'}
    if kind == 'gtk4':
        return Client(['/usr/bin/python3', os.path.join(HERE, 'gtk4_clicker.py'), 'MAP', 'target'], env_extra=env, name='map-gtk4'), 'MAP'
    if kind == 'gtk3':
        return Client(['/usr/bin/python3', os.path.join(HERE, 'gtk3_clicker.py'), 'MAP', 'target'], env_extra=env, name='map-gtk3'), 'MAP'
    if kind == 'gtk3x':
        log = open(os.path.join(W, 'shell.log')).read()
        disp = re.search(r'Using public X11 display (:\d+)', log).group(1)
        rt = os.environ['RT']
        xauth = sorted(f for f in os.listdir(rt) if f.startswith('.mutter-Xwaylandauth.'))[0]
        env.update({'GDK_BACKEND': 'x11', 'DISPLAY': disp, 'XAUTHORITY': os.path.join(rt, xauth)})
        return Client(['/usr/bin/python3', os.path.join(HERE, 'gtk3_clicker.py'), 'MAP', 'target'], env_extra=env, name='map-gtk3x'), 'MAP'
    if kind == 'firefox':
        return Client(['/usr/bin/firefox-esr', '--no-remote', '--profile', os.path.join(W, 'ffprof'), '--new-window', PAGE], env_extra=env, name='map-ff'), None
    if kind == 'chromium':
        return Client(['/usr/bin/chromium', '--ozone-platform=wayland', '--force-renderer-accessibility', '--user-data-dir=' + os.path.join(W, 'crprof'), '--no-first-run', '--no-default-browser-check', PAGE], env_extra=env, name='map-cr'), None
    return Client([EP + '/dist2/chromelectron', '--no-sandbox', '--disable-gpu', '--ozone-platform=wayland', '--force-renderer-accessibility', EP + '/app', PAGE], env_extra=env, name='map-el'), None


def fired(kind, c):
    if kind.startswith('gtk'):
        return 'EV activate Target' in c.text()
    return any(w['title'].startswith('CLICKED') for w in windows())


start_a11y()
probe('HideOverview')
time.sleep(0.5)
ptr = Pointer()
R.check('Rust client ready', 'Ready' in ptr.first, ptr.first)
mons = monitors()
scale = mons[0]['scale']
print('monitor scale', scale, 'x-scale', mons[0]['x-scale'])
for kind in sys.argv[1:]:
    c, title = launch(kind)
    wait_for(lambda: 'READY' in c.text() if kind.startswith('gtk') else find_windows() and any(w['title'].startswith(('WAIT', 'L1')) for w in windows()), 60)
    time.sleep(3)
    # the connector-side resolver (gorca-connector-atspi, the code of BackendOp::Locate) on the real client
    out = ''
    for _ in range(40):
        time.sleep(1.5)
        out = subprocess.run([LOC, A11Y, str(c.p.pid), 'Target'], capture_output=True, text=True, timeout=30).stdout.strip()
        if out.startswith('clickwin '):
            break
    if not out.startswith('clickwin '):
        R.check(f'{kind}: resolver found "Target"', False, out)
        c.kill()
        continue
    spec = out[len('clickwin '):]
    # The owner's path: gorca+KP_Divide routes (PointerMove), then KP_Divide clicks at the pointer (PointerButton, no motion).
    mv = ptr.cmd('movewin ' + spec)
    time.sleep(float(os.environ.get('ROUTE_GAP', '2')))
    print('pos', ptr.cmd('pos'), flush=True)
    r = ptr.cmd('click L')
    time.sleep(1.2)
    ok = wait_for(lambda: fired(kind, c), 3)
    R.check(f'{kind}: route then click-at-pointer (scale {scale}) clicks exactly the button', ok and r.startswith('Ok'), f'move={mv} click={r} resolver={out}')
    c.kill()
    time.sleep(1)
ptr.quit()
sys.exit(R.summary())
