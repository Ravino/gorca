import gi
gi.require_version("Gtk", "3.0")
from gi.repository import Gtk
w = Gtk.Window(title="gtk3probe")
w.add(Gtk.Button(label="hello"))
w.show_all()
Gtk.main()
