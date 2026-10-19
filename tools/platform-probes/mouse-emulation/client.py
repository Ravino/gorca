#!/usr/bin/python3
# GTK4 click recorder for the private shell. Prints "EV kind button x y" (surface-local logical px).
import sys, gi
gi.require_version('Gtk','4.0')
from gi.repository import Gtk, GLib
def on_press(g, n, x, y): print('EV press', g.get_current_button(), round(x), round(y), flush=True)
def on_rel(g, n, x, y): print('EV release', g.get_current_button(), round(x), round(y), flush=True)
def on_motion(c, x, y): print('EV motion', round(x), round(y), flush=True)
def on_enter(c, x, y): print('EV enter', round(x), round(y), flush=True)
def activate(app):
    w = Gtk.ApplicationWindow(application=app, title=sys.argv[1] if len(sys.argv)>1 else 'clicker')
    w.set_default_size(500, 400)
    da = Gtk.DrawingArea(); da.set_hexpand(True); da.set_vexpand(True)
    g = Gtk.GestureClick(); g.set_button(0); g.connect('pressed', on_press); g.connect('released', on_rel); da.add_controller(g)
    m = Gtk.EventControllerMotion(); m.connect('motion', on_motion); m.connect('enter', on_enter); da.add_controller(m)
    w.set_child(da); w.present()
app = Gtk.Application(application_id='org.gorca.ClickProbe%d' % (hash(sys.argv[1] if len(sys.argv)>1 else '') % 1000)); app.connect('activate', activate); app.run([])
