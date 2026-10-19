#!/usr/bin/python3
"""A real GTK4 window for the M22a C1a cold-focus measurement (private session only).

150 labelled entries in a scrolled box. The harness focuses them one by one through AT-SPI, so
every focus lands on a node the reader has never fetched.
"""
import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

GLib.set_prgname("c1a-gtk4")
GLib.set_application_name("c1a-gtk4")


def activate(app):
    win = Gtk.ApplicationWindow(application=app, title="C1a Cold")
    win.set_default_size(600, 700)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)
    for i in range(150):
        e = Gtk.Entry()
        e.update_property([Gtk.AccessibleProperty.LABEL], [f"Field {i}"])
        box.append(e)
    sc = Gtk.ScrolledWindow()
    sc.set_child(box)
    win.set_child(sc)
    win.present()


app = Gtk.Application(application_id="pw.nekrasov.c1a")
app.connect("activate", activate)
app.run(None)
