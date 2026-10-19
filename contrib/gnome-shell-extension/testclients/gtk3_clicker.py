#!/usr/bin/python3
# GTK3 click recorder: window-relative coordinates to the toplevel widget origin (buffer incl. CSD shadow, see
# docs/reports/mouse-emulation-design.md section 3). Usage: gtk3_clicker.py TITLE [menu]
# With "menu": a Gtk.MenuButton "Menu" with a Gtk.Menu (xdg_popup) with items "Item A" / "Item B" and a "Pop"
# button with a Gtk.Popover; activation prints "EV activate <label>".
import sys, gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, Gdk
title = sys.argv[1] if len(sys.argv) > 1 else 'clicker'
menu = 'menu' in sys.argv[2:]
win = Gtk.Window(title=title)
win.set_default_size(500, 400)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
if menu:
    row = Gtk.Box()
    mb = Gtk.MenuButton(label='Menu'); m = Gtk.Menu()
    for lbl in ('Item A', 'Item B'):
        mi = Gtk.MenuItem(label=lbl); mi.connect('activate', lambda i: print('EV activate', i.get_label(), flush=True)); m.append(mi)
    m.show_all(); mb.set_popup(m)
    pb = Gtk.MenuButton(label='Pop'); po = Gtk.Popover()
    b = Gtk.Button(label='Pop item'); b.connect('clicked', lambda b: print('EV activate', b.get_label(), flush=True)); po.add(b); b.show()
    pb.set_popover(po)
    row.pack_start(mb, False, False, 0); row.pack_start(pb, False, False, 0)
    box.pack_start(row, False, False, 0)
ev = Gtk.EventBox(); ev.add(Gtk.DrawingArea())
ev.add_events(Gdk.EventMask.BUTTON_PRESS_MASK | Gdk.EventMask.BUTTON_RELEASE_MASK)
def rel(e):
    p = ev.translate_coordinates(win, e.x, e.y)
    return (round(p[0], 1), round(p[1], 1)) if p else (e.x, e.y)
ev.connect('button-press-event', lambda w, e: print('EV press', e.button, *rel(e), flush=True))
ev.connect('button-release-event', lambda w, e: print('EV release', e.button, *rel(e), flush=True))
box.pack_start(ev, True, True, 0)
win.add(box); win.connect('destroy', Gtk.main_quit); win.show_all()
print('READY', flush=True)
Gtk.main()
