#!/usr/bin/python3
"""A one-widget GTK window for the editing matrix (docs/reports/unified-editing.md).

    gtkedit.py <3|4> <entry|textview> [--title T] [--text TEXT] [--width W]

The widget has the keyboard focus from the start. A GtkTextView wraps at words, so wrapped
lines can be tested with the window width (--width). No other widget, no menus: the keys of the
matrix go to the editable and nowhere else.
"""
import argparse

import gi

p = argparse.ArgumentParser()
p.add_argument("gtk", choices=["3", "4"])
p.add_argument("widget", choices=["entry", "textview"])
p.add_argument("--title", default="Edit Probe")
p.add_argument("--text", default="")
p.add_argument("--width", type=int, default=600)
a = p.parse_args()

if a.gtk == "3":
    gi.require_version("Gtk", "3.0")
else:
    gi.require_version("Gtk", "4.0")
from gi.repository import Gtk  # noqa: E402

GTK4 = a.gtk == "4"


def activate(app=None):
    if GTK4:
        win = Gtk.ApplicationWindow(application=app, title=a.title)
    else:
        win = Gtk.Window(title=a.title)
    win.set_default_size(a.width, 300)
    if a.widget == "entry":
        w = Gtk.Entry()
        w.set_text(a.text)
        w.set_position(-1)
    else:
        w = Gtk.TextView()
        w.set_wrap_mode(Gtk.WrapMode.WORD)
        w.get_buffer().set_text(a.text)
    if GTK4:
        win.set_child(w)
        win.present()
    else:
        win.add(w)
        win.connect("destroy", Gtk.main_quit)
        win.show_all()
    w.grab_focus()


if GTK4:
    app = Gtk.Application(application_id="org.gorca.EditProbe")
    app.connect("activate", activate)
    app.run(None)
else:
    activate()
    Gtk.main()
