#!/usr/bin/python3
# S-4 (XWayland): the helper's global x-scale vs the measured ratio AT-SPI-toplevel-size / Meta-frame-size of a GTK3 X11
# window, and where a ClickAt(frame, 120, 150) lands in X coordinates. Needs a rig started with GORCA_RIG_NO_X11= (XWayland).
import re, sys
from spike_lib import *

start_a11y()
log = open(os.path.join(W, 'shell.log')).read()
m = re.search(r'Using public X11 display (:\d+)', log)
print('X display', m and m.group(1))
rt = os.environ['RT']
xauth = sorted(f for f in os.listdir(rt) if f.startswith('.mutter-Xwaylandauth.'))
env = {'GDK_BACKEND': 'x11', 'DISPLAY': m.group(1), 'XAUTHORITY': os.path.join(rt, xauth[0]), 'GTK_A11Y': 'atspi', 'GNOME_ACCESSIBILITY': '1'}
probe('HideOverview')
c = Client(['/usr/bin/python3', os.path.join(HERE, 'gtk3_clicker.py'), 'S4X'], env_extra=env, name='s4x')
wait_for(lambda: 'READY' in c.text(), 20); time.sleep(3)
w = find_windows(title='S4X')[0]
mons = monitors()
print('monitors', [(x['index'], x['w'], x['h'], x['scale'], x['x-scale']) for x in mons])
print('window', brief(w))
app = app_for_pid(c.p.pid)
fr = find(app, pred=lambda n: n.get_name() == 'S4X' and n.get_role_name() in ('frame', 'window')) if app else None
fe = fr and extents(fr)
print('AT-SPI toplevel', fe)
if fe:
    aw = fe['WINDOW'][2]
    print(f'ratio AT-SPI width / Meta frame width = {aw}/{w["frame-w"]} = {aw / w["frame-w"]:.3f}; vs buffer {aw / w["buffer-w"]:.3f}; helper x-scale = {mons[0]["x-scale"]}')
n0 = len(c.events())
r = click_at(w['id'], 'frame', 120, 150, 1)
time.sleep(1)
print('ClickAt frame (120,150) ->', r['outcome'], 'client saw', c.events()[n0:n0 + 1])
# C-5: the ratio is global, even for a window on the 1x monitor
probe('MoveFrame', w['id'], mons[1]['x'] + 150, 100); time.sleep(2)
w = find_windows(title='S4X')[0]
fe = extents(find(app, pred=lambda n: n.get_name() == 'S4X' and n.get_role_name() in ('frame', 'window')))
print('on monitor', w['monitor'], '(scale', mons[w['monitor']]['scale'] if w['monitor'] < len(mons) else '?', ') frame', (w['frame-x'], w['frame-y'], w['frame-w']),
      'AT-SPI width', fe['WINDOW'][2], 'ratio', fe['WINDOW'][2] / w['frame-w'], 'helper x-scale', monitors()[0]['x-scale'])
n0 = len(c.events())
r = click_at(w['id'], 'frame', 120, 150, 1)
time.sleep(1)
print('ClickAt frame (120,150) on the 1x monitor ->', r['outcome'], 'client saw', c.events()[n0:n0 + 1])
c.kill()
