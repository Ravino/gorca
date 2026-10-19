#!/usr/bin/python3
# S-4 / S-2: web clients at a fractional or 2x monitor scale. Finds the page "Target" button through AT-SPI, tries
# stage = frame + WINDOW / divisor for divisor in (1, monitor scale) and reports which one makes the page button fire
# (title CLICKED). Also prints the helper's x-scale and monitor scales.  Usage: spike_s4_scale.py firefox|chromium|electron
import sys
from spike_lib import *

which = sys.argv[1]
PAGE = 'file://' + os.path.join(HERE, 'page_menu.html')
EP = os.path.expanduser('~/.cache/electron-probe')
start_a11y()
os.makedirs(os.path.join(W, 'ffprof'), exist_ok=True)
home = os.path.join(W, 'home-' + which)
os.makedirs(home, exist_ok=True)
env = {'HOME': home, 'GNOME_ACCESSIBILITY': '1', 'GDK_BACKEND': 'wayland', 'MOZ_ENABLE_WAYLAND': '1', 'GTK_A11Y': 'atspi'}
if which == 'firefox':
    argv = ['/usr/bin/firefox-esr', '--no-remote', '--profile', os.path.join(W, 'ffprof'), '--new-window', PAGE]
elif which == 'chromium':
    argv = ['/usr/bin/chromium', '--ozone-platform=wayland', '--force-renderer-accessibility', '--user-data-dir=' + os.path.join(W, 'crprof'),
            '--no-first-run', '--no-default-browser-check', PAGE]
else:
    argv = [EP + '/dist2/chromelectron', '--no-sandbox', '--disable-gpu', '--ozone-platform=wayland', '--force-renderer-accessibility', EP + '/app', PAGE]
probe('HideOverview')
c = Client(argv, env_extra=env, name='s4-' + which)
w = wait_for(lambda: next((x for x in windows() if x['title'].startswith(('WAIT', 'L1')) and x['type'] == 0), None), 60)
if not w:
    print('no window'); c.kill(); sys.exit(1)
time.sleep(3)
w = [x for x in windows() if x['id'] == w['id']][0]
mons = monitors()
print('monitors', [(m['index'], m['w'], m['h'], m['scale'], m['x-scale']) for m in mons])
sc = [m for m in mons if m['index'] == w['monitor']][0]['scale']
print('window', brief(w), 'monitor scale', sc)
app = None
for _ in range(40):
    app = app_for_pid(c.p.pid)
    if app and find(app, name='Target'):
        break
    time.sleep(1)
te = None
for _ in range(30):
    tgt = find(app, name='Target') if app else None
    te = tgt and extents(tgt)
    if te and te['WINDOW'][2] > 0:
        break
    time.sleep(1)
print('Target extents', te)
res = 'NO-TARGET'
if te:
    cx, cy = center(te['WINDOW'])
    for div in (1.0, sc):
        sx, sy = w['frame-x'] + cx / div, w['frame-y'] + cy / div
        move(w['id'], 'stage', sx, sy); time.sleep(0.4)
        r = click_at(w['id'], 'stage', sx, sy, 1)
        time.sleep(1.2)
        title = [x['title'] for x in windows() if x['id'] == w['id']][0]
        ok = title.startswith('CLICKED')
        print(f'  divisor {div}: stage ({sx:.1f},{sy:.1f}) outcome={r["outcome"]} title={title!r} -> {"FIRED" if ok else "no"}')
        if ok:
            res = f'divisor {div}'
            break
        time.sleep(0.5)
print('RESULT', which, 'scale', sc, res)
c.kill()
