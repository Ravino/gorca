#!/usr/bin/python3
"""A small real GTK4 window for the P1 wired-path check (private Shell world only).

Tab order: check button, expander, scale, toggle button. The harness presses Tab / Space / Right and
reads what gorca says through its SSIP mock. Use the system Python (PyGObject).
"""
import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gtk  # noqa: E402


def activate(app):
    win = Gtk.ApplicationWindow(application=app, title="P1 Wired")
    win.set_default_size(480, 360)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
    box.set_margin_top(12)
    box.set_margin_start(12)
    check = Gtk.CheckButton(label="Subscribe")
    expander = Gtk.Expander(label="Details")
    expander.set_child(Gtk.Label(label="More text"))
    scale = Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 100, 10)
    scale.set_value(50)
    scale.set_draw_value(False)
    scale.update_property([Gtk.AccessibleProperty.LABEL], ["Volume"])
    toggle = Gtk.ToggleButton(label="Mute")
    for w in (check, expander, scale, toggle):
        box.append(w)
    win.set_child(box)
    win.present()
    check.grab_focus()


app = Gtk.Application(application_id="pw.nekrasov.p1wired")
app.connect("activate", activate)
app.run(None)
