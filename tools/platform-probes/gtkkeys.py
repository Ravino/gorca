#!/usr/bin/python3
"""Tiny GTK4 key logger for the PRIVATE headless gnome-shell. Prints one line per key event:
KEY <monotonic_ns> press|release <keyval> ; and FOCUS when the window becomes active.
Run with the private session's DBUS_SESSION_BUS_ADDRESS/XDG_RUNTIME_DIR and GDK_BACKEND=wayland."""
import sys, time, gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk, Gdk, GLib

def out(s):
    sys.stdout.write(s + "\n"); sys.stdout.flush()

def on_app(app):
    w = Gtk.ApplicationWindow(application=app, title="gtkkeys")
    w.set_default_size(300, 200)
    kc = Gtk.EventControllerKey()
    kc.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    kc.connect("key-pressed", lambda c, v, k, s: out("KEY %d press %d" % (time.monotonic_ns(), v)) or False)
    kc.connect("key-released", lambda c, v, k, s: out("KEY %d release %d" % (time.monotonic_ns(), v)))
    w.add_controller(kc)
    w.connect("notify::is-active", lambda *a: w.is_active() and out("FOCUS"))
    w.present()
    out("READY")

app = Gtk.Application(application_id="org.gorca.probe.gtkkeys")
app.connect("activate", on_app)
app.run([])
