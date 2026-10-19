#!/usr/bin/python3
# S-1 (browsers) / S-2 (Electron) / S-4 (Chromium at fractional scale): context-menu popup windows, AT-SPI extents of
# the menu item vs Meta rects, a real click on "Select All", and the page Target button via frame + child / scale.
# Usage: spike_s1_web.py firefox|chromium|electron [MONITOR_SCALE_FOR_CHILD_DIV]
import sys
from spike_lib import *

which = sys.argv[1]
ITEM = 'select all' if sys.argv[1] == 'firefox' else 'reload'
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
c = Client(argv, env_extra=env, name='web-' + which)
print('launched', which, c.p.pid)
def _find():
    for x in windows():
        if x['title'].startswith(('WAIT', 'L1')) and x['type'] == 0:
            return x
    return None


w = wait_for(_find, 60)
print('window', w and brief(w))
if not w:
    print(c.text()[-600:]); c.kill(); sys.exit(1)
probe('HideOverview'); time.sleep(0.5)
probe('MoveFrame', w['id'], 100, 60); time.sleep(2.5)
w = [x for x in windows() if x['id'] == w['id']][0]
print('moved', brief(w))
mons = monitors(); sc = [m for m in mons if m['index'] == w['monitor']][0]['scale']
print('monitor scale', sc, 'x-scale', mons[0]['x-scale'])
app = None
for _ in range(30):
    app = app_for_pid(c.p.pid)
    if app and find(app, name='Target'):
        break
    time.sleep(1)
if not app:
    # browsers spawn helper processes: look by name
    d = Atspi.get_desktop(0)
    for i in range(d.get_child_count()):
        a = d.get_child_at_index(i)
        if a and any(k in (a.get_name() or '').lower() for k in (which, 'chrom', 'electron')):
            app = a
print('atspi app', app and (app.get_name(), app.get_process_id()))
tgt = find(app, name='Target') if app else None
te = tgt and extents(tgt)
print('Target extents', te)
if te:
    div = sc if which in ('chromium', 'electron') else 1.0
    cx, cy = center(te['WINDOW'])
    print(f'child divisor {div}: Target centre in frame coords = ({cx / div:.1f}, {cy / div:.1f})  (page expects CSS (380, 230) + toolbar offset)')
# right-click on the paragraph to open the context menu
para = find(app, pred=lambda n: 'Select this sentence' in (n.get_name() or '') or 'Select this sentence' in (lambda t: t)((lambda: (n.get_text_iface().get_text(0, 30) if n.get_text_iface() else ''))())) if app else None
pe = para and extents(para)
print('paragraph extents', pe)
if pe:
    div = sc if which in ('chromium', 'electron') else 1.0
    px, py = center(pe['WINDOW'])
    px, py = px / div, py / div
else:
    px, py = 150, 200
before = {x['id'] for x in windows()}
r = click_at(w['id'], 'frame', px, py, 3)
print('right click at frame', (px, py), '->', r['outcome'], 'window-at', r['window-at'])
time.sleep(2)
new = [x for x in windows() if x['id'] not in before]
print('popup windows listed by get_window_actors():', len(new))
for x in new:
    print('  ', brief(x))
item = None
for _ in range(10):
    item = find(app, pred=lambda n: (n.get_name() or '').lower().startswith(ITEM) and n.get_role_name() in ('menu item', 'menuitem', 'push button'))
    if item:
        break
    time.sleep(0.5)
ie = item and extents(item)
if not item and len(sys.argv) > 2:
    for n in walk(app):
        try:
            if n.get_role_name() in ('menu item', 'menu', 'popup menu', 'check menu item'):
                print('   menu node', n.get_role_name(), repr(n.get_name()), extents(n)['WINDOW'])
        except Exception:
            pass
print('menu item "Select All"', item and item.get_role_name(), ie)
done = False
if ie and new:
    pop = new[0]
    div = sc if which in ('chromium', 'electron') else 1.0
    wc = center(ie['WINDOW'])
    sc_ = center(ie['SCREEN'])
    cands = {'toplevel.frame+WINDOW': (w['frame-x'] + wc[0], w['frame-y'] + wc[1]),
             'toplevel.frame+WINDOW/div': (w['frame-x'] + wc[0] / div, w['frame-y'] + wc[1] / div),
             'popup.frame+WINDOW': (pop['frame-x'] + wc[0], pop['frame-y'] + wc[1]),
             'popup.frame+WINDOW/div': (pop['frame-x'] + wc[0] / div, pop['frame-y'] + wc[1] / div),
             'SCREEN': sc_, 'SCREEN/div': (sc_[0] / div, sc_[1] / div)}
    print('stage candidates', {k: (round(a, 1), round(b, 1)) for k, (a, b) in cands.items()})
    for nm, (sx, sy) in cands.items():
        cur = [x for x in windows() if x['id'] not in before]
        if not cur:
            click_at(w['id'], 'frame', px, py, 3); time.sleep(1.5)
            cur = [x for x in windows() if x['id'] not in before]
        inside = any(p['frame-x'] <= sx < p['frame-x'] + p['frame-w'] and p['frame-y'] <= sy < p['frame-y'] + p['frame-h'] for p in cur)
        if not inside:
            print(f'  {nm}: ({sx:.1f},{sy:.1f}) is outside the popup frame(s) {[(p["frame-x"], p["frame-y"], p["frame-w"], p["frame-h"]) for p in cur]} -> skipped'); continue
        m = move(w['id'], 'stage', sx, sy)
        time.sleep(0.4)
        r = click_at(w['id'], 'stage', sx, sy, 1)
        time.sleep(1.5)
        ws = windows()
        title = [x['title'] for x in ws if x['id'] == w['id']][0]
        print(f'  click via {nm}: move={m["outcome"]} click={r["outcome"]} window-at={r["window-at"]} title now {title!r}; windows now {[brief(x) for x in ws if x["id"] != w["id"]]}')
        if title.startswith('SEL') or title.startswith('L2'):
            done = True
            break
else:
    print('no item/popup to click')
print('RESULT', which, 'activated' if done else 'NOT activated')
c.kill()
