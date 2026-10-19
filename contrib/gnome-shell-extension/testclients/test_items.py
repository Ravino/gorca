#!/usr/bin/python3
# MO-5b item clicks end to end (also the resolver): the connector's resolver (locate_cli) on a real client of a real widget -> ToplevelHint + point -> the Rust helper client's window matcher
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
    errs = []

    def run_loc(mode):
        p = subprocess.run([LOC, A11Y, str(c.p.pid), 'Target', mode], capture_output=True, text=True, timeout=30)
        errs.append(p.stderr.strip())
        return p.stdout.strip()
    # 1. item click, action path: the application's own click action, no pointer involved
    out = ''
    for _ in range(40):
        time.sleep(1.5)
        out = run_loc('locate')
        if out.startswith('clickwin '):
            break
    R.check(f'{kind}: resolver found "Target"', out.startswith('clickwin '), out)
    r = run_loc('act-click')
    time.sleep(1.2)
    ok = wait_for(lambda: fired(kind, c), 3)
    tries = [r]
    for _ in range(2):
        if ok:
            break
        tries.append(run_loc('act-click'))
        time.sleep(1.2)
        ok = wait_for(lambda: fired(kind, c), 3)
    R.check(f'{kind}: left item click by the click/press action fires the button (no pointer)', r == 'acted' and ok, f'{tries} {errs[-1][:300]} titles={[w["title"] for w in windows()]}')
    # 2. right item click: the context-menu action if there is one, else the pointer (route + right click)
    r = run_loc('act-menu')
    if r == 'no action' and kind in ('firefox', 'chromium', 'electron'):
        spec = out[len('clickwin '):].replace('|L|', '|R|')
        pr = ptr.cmd('clickwin ' + spec)
        time.sleep(1.2)
        ok = wait_for(lambda: any(w['title'].startswith('CTX') for w in windows()), 3)
        R.check(f'{kind}: no context-menu action -> the pointer right-clicks exactly the item (page saw contextmenu)', ok and pr.startswith('Ok'), f'{pr}')
    else:
        R.check(f'{kind}: right item click: menu action result is "acted" or "no action"', r in ('acted', 'no action'), r)
    c.kill()
    time.sleep(1)
ptr.quit()
sys.exit(R.summary())
