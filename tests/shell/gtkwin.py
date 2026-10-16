#!/usr/bin/python3
"""A tiny GTK3 client window for the Shell probe (title from argv[1]; an entry and a button)."""
import sys

import gi

gi.require_version("Gtk", "3.0")
from gi.repository import Gtk  # noqa: E402

title = sys.argv[1] if len(sys.argv) > 1 else "Probe Window"
w = Gtk.Window(title=title)
w.set_default_size(500, 300)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=6)
box.pack_start(Gtk.Entry(), False, False, 0)
box.pack_start(Gtk.Button(label="Probe button"), False, False, 0)
w.add(box)
w.connect("destroy", Gtk.main_quit)
w.show_all()
Gtk.main()
