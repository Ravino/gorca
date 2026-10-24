#!/usr/bin/python3
"""GTK 3 / GTK 4 app whose widgets change state when a trigger button is pressed (AT-SPI
Action "click"). Used by capture_more.py to record real state changes made by the toolkit.

  change_gallery.py 3|4

Each case has a trigger button named "do:<case>" and one target widget. Pressing the trigger
runs the toolkit's own setter, so the state set, name and value the accessibility tree shows
afterwards are what a user's action (or an application update) produces.
"""
import sys

import gi

MAJOR = sys.argv[1] if len(sys.argv) > 1 else "3"
gi.require_version("Gtk", MAJOR + ".0")
from gi.repository import GLib, Gtk  # noqa: E402

G4 = MAJOR == "4"


def add(box, w):
    if G4:
        box.append(w)
    else:
        box.add(w)
    return w


def label_of(w, name):
    """Give a widget without its own text an accessible name (GTK3 Atk, GTK4 property)."""
    if G4:
        w.update_property([Gtk.AccessibleProperty.LABEL], [name])
    else:
        w.get_accessible().set_name(name)


box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)

check = add(box, Gtk.CheckButton(label="Subscribe"))
mixed = add(box, Gtk.CheckButton(label="Partial"))
toggle = add(box, Gtk.Switch())
label_of(toggle, "Wi-Fi")
press = add(box, Gtk.ToggleButton(label="Bold"))
expander = Gtk.Expander(label="Details")
if G4:
    expander.set_child(Gtk.Label(label="inside"))
else:
    expander.add(Gtk.Label(label="inside"))
add(box, expander)
save = add(box, Gtk.Button(label="Save"))
lb = add(box, Gtk.ListBox())
rows = []
for t in ("One", "Two", "Three"):
    r = Gtk.ListBoxRow()
    lab = Gtk.Label(label=t)
    if G4:
        r.set_child(lab)
        lb.append(r)
    else:
        r.add(lab)
        lb.add(r)
    rows.append(r)
rename = add(box, Gtk.Button(label="Old name"))

ACTIONS = {
    "check": lambda: check.set_active(True),
    "mixed": lambda: mixed.set_inconsistent(True),
    "toggle": lambda: toggle.set_active(True),
    "press": lambda: press.set_active(True),
    "expand": lambda: expander.set_expanded(True),
    "disable": lambda: save.set_sensitive(False),
    "select": lambda: lb.select_row(rows[1]),
    "rename": lambda: rename.set_label("New name"),
}
for case, fn in ACTIONS.items():
    b = add(box, Gtk.Button(label="do:" + case))
    b.connect("clicked", lambda _b, f=fn: f())

if G4:
    GLib.set_prgname("change_gallery")
    app = Gtk.Application(application_id="org.gorca.ChangeGallery")

    def activate(a):
        win = Gtk.ApplicationWindow(application=a, title="Change gallery", default_width=500, default_height=600)
        win.set_child(box)
        win.present()

    app.connect("activate", activate)
    app.run([])
else:
    win = Gtk.Window(title="Change gallery")
    win.add(box)
    win.connect("destroy", Gtk.main_quit)
    win.show_all()
    Gtk.main()
