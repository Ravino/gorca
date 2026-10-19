#!/usr/bin/python3
# client_btn.py GTKMAJOR : window with one button at a known place; prints "CLICKED" on activation and "EV press x y" (button-local).
import sys, gi
major = sys.argv[1]
if major == '4':
    gi.require_version('Gtk','4.0'); from gi.repository import Gtk
    def act(app):
        w = Gtk.ApplicationWindow(application=app, title='BTN4'); w.set_default_size(500,400)
        fx = Gtk.Fixed(); b = Gtk.Button(label='Target'); b.set_size_request(120,50); b.connect('clicked', lambda *_: print('CLICKED', flush=True))
        fx.put(b, 200, 150); w.set_child(fx); w.present()
    app = Gtk.Application(application_id='org.gorca.Btn4'); app.connect('activate', act); app.run([])
else:
    gi.require_version('Gtk','3.0'); from gi.repository import Gtk
    w = Gtk.Window(title='BTN3'); w.set_default_size(500,400)
    fx = Gtk.Fixed(); b = Gtk.Button(label='Target'); b.set_size_request(120,50); b.connect('clicked', lambda *_: print('CLICKED', flush=True))
    fx.put(b, 200, 150); w.add(fx); w.connect('destroy', Gtk.main_quit); w.show_all(); Gtk.main()
