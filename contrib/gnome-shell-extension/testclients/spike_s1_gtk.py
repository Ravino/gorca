#!/usr/bin/python3
# S-1 (GTK part): popup windows in get_window_actors(), popup type / transient-for, AT-SPI coordinates of popup items
# vs the Meta popup rect, and a real click through ClickAt(window = parent, origin = stage) on a popover/menu item.
import sys
from spike_lib import *

start_a11y()
probe('HideOverview'); time.sleep(0.5)
for toolkit, script in (('GTK4 popover', 'gtk4_clicker.py'), ('GTK3 menu/popover', 'gtk3_clicker.py')):
    print(f'===== {toolkit}')
    c = Client(['/usr/bin/python3', os.path.join(HERE, script), 'S1', 'menu'], env_extra={'GTK_A11Y': 'atspi', 'GNOME_ACCESSIBILITY': '1'}, name='s1')
    wait_for(lambda: 'READY' in c.text(), 20); time.sleep(2)
    w = find_windows(title='S1')[0]
    probe('MoveFrame', w['id'], 300, 200); time.sleep(1.5)
    w = find_windows(title='S1')[0]
    print('toplevel', brief(w))
    app = app_for_pid(c.p.pid)
    print('atspi app', app and app.get_name())
    before = {x['id'] for x in windows()}
    mb = find(app, name='Menu') if app else None
    print('Menu button extents', mb and extents(mb))
    mc = center(extents(mb)['WINDOW'])
    OPEN = (lambda: click_at(w['id'], 'frame', mc[0], mc[1], 1)) if toolkit.startswith('GTK4') else (lambda: click_at(w['id'], 'buffer', mc[0], mc[1], 1))
    print('open ->', OPEN()['outcome'])
    time.sleep(1.5)
    after = windows()
    new = [x for x in after if x['id'] not in before]
    print('popup windows listed by get_window_actors():', len(new))
    for x in new:
        print('  ', brief(x))
    if app:
        item = find(app, name='Item A') or find(app, name='Pop item')
        ie = item and extents(item)
        print('item', item and item.get_name(), ie)
        if item and new:
            pop = new[0]
            # candidates: AT-SPI SCREEN, popup frame + WINDOW, toplevel frame + WINDOW
            cands = {'SCREEN': center(ie['SCREEN']),
                     'popup.frame+WINDOW': (pop['frame-x'] + center(ie['WINDOW'])[0], pop['frame-y'] + center(ie['WINDOW'])[1]),
                     'popup.buffer+WINDOW': (pop['buffer-x'] + center(ie['WINDOW'])[0], pop['buffer-y'] + center(ie['WINDOW'])[1]),
                     'toplevel.frame+WINDOW': (w['frame-x'] + center(ie['WINDOW'])[0], w['frame-y'] + center(ie['WINDOW'])[1]),
                     'toplevel.buffer+WINDOW': (w['buffer-x'] + center(ie['WINDOW'])[0], w['buffer-y'] + center(ie['WINDOW'])[1])}
            print('stage candidates', cands)
            for nm, (sx, sy) in cands.items():
                n0 = len(c.events())
                txt0 = c.text().count('EV activate')
                try:
                    r = click_at(w['id'], 'stage', sx, sy, 1)
                except HelperError as e:
                    print(nm, 'ERR', e); continue
                time.sleep(0.8)
                act = c.text().count('EV activate') > txt0
                print(f'  click via {nm} -> stage ({sx:.1f},{sy:.1f}) outcome={r["outcome"]} window-at={r["window-at"]} (popup id {pop["id"]}) activated={act}')
                if act:
                    break
                # popup may have closed; reopen
                if not [x for x in windows() if x['id'] not in before]:
                    OPEN(); time.sleep(1.2)
    # occlusion/guard on the popup point without the family rule is exercised in test_pointer; here show the ADR family
    c.kill(); time.sleep(1)
