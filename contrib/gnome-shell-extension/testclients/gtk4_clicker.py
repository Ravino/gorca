#!/usr/bin/python3
# GTK4 click recorder for the private shell. Prints "EV <kind> <detail>" lines with window-relative logical px
# (translate_coordinates to the toplevel = Meta frame origin on Wayland). Usage: gtk4_clicker.py TITLE [menu]
# With "target": a Gtk.Button "Target" 120 px from the left and 80 px from the top of the content; "EV activate Target".
# With "dialog": also opens a transient (non-popup) window over the parent.
# With "menu": a MenuButton "Menu" (top-left) opens a popover with buttons "Item A", "Item B"; activation prints
# "EV activate <label>".
import sys, gi
gi.require_version('Gtk', '4.0')
from gi.repository import Gtk, GLib
title = sys.argv[1] if len(sys.argv) > 1 else 'clicker'
menu = 'menu' in sys.argv[2:]
dialog = 'dialog' in sys.argv[2:]
target = 'target' in sys.argv[2:]
win = None
def rel(widget, x, y):
    p = widget.translate_coordinates(win, x, y)
    return (round(p[0], 1), round(p[1], 1)) if p else (x, y)
def on_press(g, n, x, y): print('EV press', g.get_current_button(), *rel(g.get_widget(), x, y), flush=True)
def on_rel(g, n, x, y): print('EV release', g.get_current_button(), *rel(g.get_widget(), x, y), flush=True)
def activate(app):
    global win
    win = Gtk.ApplicationWindow(application=app, title=title)
    win.set_default_size(500, 400)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
    if menu:
        mb = Gtk.MenuButton(label='Menu')
        pop = Gtk.Popover()
        pb = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        for lbl in ('Item A', 'Item B'):
            b = Gtk.Button(label=lbl)
            b.connect('clicked', lambda b: print('EV activate', b.get_label(), flush=True))
            pb.append(b)
        pop.set_child(pb)
        mb.set_popover(pop)
        mb.set_halign(Gtk.Align.START)
        box.append(mb)
    if target:
        tb = Gtk.Button(label='Target')
        tb.set_halign(Gtk.Align.START); tb.set_margin_start(120); tb.set_margin_top(80)
        tb.connect('clicked', lambda b: print('EV activate Target', flush=True))
        box.append(tb)
    da = Gtk.DrawingArea(); da.set_hexpand(True); da.set_vexpand(True)
    g = Gtk.GestureClick(); g.set_button(0); g.connect('pressed', on_press); g.connect('released', on_rel); da.add_controller(g)
    box.append(da)
    win.set_child(box); win.present()
    if dialog:
        d = Gtk.Window(title=title + ' dialog', transient_for=win)
        d.set_default_size(240, 160)
        d.set_child(Gtk.Label(label='attached dialog'))
        d.present()
    print('READY', flush=True)
app = Gtk.Application(application_id='org.gorca.PClient4x%d' % abs(hash(title) % 100000))
app.connect('activate', activate)
app.run([])
