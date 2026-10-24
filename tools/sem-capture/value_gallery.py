#!/usr/bin/python3
"""GTK 3 / GTK 4 app for the value, selection, expand and disable changes the cut-over gate needs
(second gallery next to change_gallery.py).

  value_gallery.py 3|4

Each case has a trigger button named "do:<case>" and one target widget. Pressing the trigger runs
the toolkit's own setter, so the tree the accessibility layer shows afterwards is what an
application update produces. capture_more.py `change --page values` records before and after.

  value-scale     Gtk.Scale (slider)            set_value
  value-spin      Gtk.SpinButton                set_value
  value-progress  Gtk.ProgressBar               set_fraction
  value-level     Gtk.LevelBar                  set_value
  select-combo    collapsed combo / drop down   set_active / set_selected
  expand-tree     tree rows, shape changes      expand_row / TreeListRow.set_expanded
  disable-row     list box row                  set_sensitive(False)
  disable-tab     notebook tab                  tab label set_sensitive(False)
"""
import sys

import gi

MAJOR = sys.argv[1] if len(sys.argv) > 1 else "3"
gi.require_version("Gtk", MAJOR + ".0")
from gi.repository import GLib, GObject, Gtk  # noqa: E402

G4 = MAJOR == "4"


def add(box, w):
    if G4:
        box.append(w)
    else:
        box.add(w)
    return w


def label_of(w, name):
    if G4:
        w.update_property([Gtk.AccessibleProperty.LABEL], [name])
    else:
        w.get_accessible().set_name(name)


box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=4)

scale = add(box, Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL, 0, 100, 1))
scale.set_value(30)
label_of(scale, "Volume")
spin = add(box, Gtk.SpinButton.new_with_range(0, 10, 1))
spin.set_value(3)
label_of(spin, "Quantity")
progress = add(box, Gtk.ProgressBar())
progress.set_fraction(0.25)
label_of(progress, "Download")
level = add(box, Gtk.LevelBar.new_for_interval(0, 10))
level.set_value(2)
label_of(level, "Battery")

if G4:
    combo = add(box, Gtk.DropDown.new_from_strings(["Red", "Green", "Blue"]))
    label_of(combo, "Colour")
else:
    combo = add(box, Gtk.ComboBoxText())
    for t in ("Red", "Green", "Blue"):
        combo.append_text(t)
    combo.set_active(0)  # no explicit name: GTK3 names the combo box after its active text

# ---- tree rows whose number changes when one is expanded ----
if G4:
    from gi.repository import Gio

    class Item(GObject.Object):
        def __init__(self, name, kids=()):
            super().__init__()
            self.name = name
            self.kids = kids

    def children(item):
        if not item.kids:
            return None
        s = Gio.ListStore.new(Item)
        for k in item.kids:
            s.append(k)
        return s

    top = Gio.ListStore.new(Item)
    top.append(Item("Documents", [Item("report.txt"), Item("notes.txt")]))
    top.append(Item("Pictures", [Item("cat.png")]))
    tree_model = Gtk.TreeListModel.new(top, False, False, children)
    factory = Gtk.SignalListItemFactory()

    def setup(_f, li):
        li.set_child(Gtk.TreeExpander())
        li.get_child().set_child(Gtk.Label())

    def bind(_f, li):
        row = li.get_item()
        exp = li.get_child()
        exp.set_list_row(row)
        exp.get_child().set_label(row.get_item().name)

    factory.connect("setup", setup)
    factory.connect("bind", bind)
    tree = Gtk.ListView.new(Gtk.SingleSelection.new(tree_model), factory)
    label_of(tree, "Files")
    sw = Gtk.ScrolledWindow()
    sw.set_min_content_height(120)
    sw.set_child(tree)
    box.append(sw)
else:
    store = Gtk.TreeStore(str)
    p1 = store.append(None, ["Documents"])
    store.append(p1, ["report.txt"])
    store.append(p1, ["notes.txt"])
    p2 = store.append(None, ["Pictures"])
    store.append(p2, ["cat.png"])
    tree = Gtk.TreeView(model=store)
    tree.append_column(Gtk.TreeViewColumn("Name", Gtk.CellRendererText(), text=0))
    label_of(tree, "Files")
    sw = Gtk.ScrolledWindow()
    sw.set_min_content_height(120)
    sw.add(tree)
    box.add(sw)

lb = add(box, Gtk.ListBox())
rows = []
for t in ("Alpha", "Beta", "Gamma"):
    r = Gtk.ListBoxRow()
    lab = Gtk.Label(label=t)
    if G4:
        r.set_child(lab)
        lb.append(r)
    else:
        r.add(lab)
        lb.add(r)
    rows.append(r)

nb = add(box, Gtk.Notebook())
tabs = []
for t in ("General", "Billing"):
    page = Gtk.Label(label=t + " page")
    tab = Gtk.Label(label=t)
    nb.append_page(page, tab)
    tabs.append(tab)


def expand_tree():
    if G4:
        tree_model.get_row(0).set_expanded(True)
    else:
        tree.expand_row(Gtk.TreePath.new_first(), False)


ACTIONS = {
    "value-scale": lambda: scale.set_value(70),
    "value-spin": lambda: spin.set_value(4),
    "value-progress": lambda: progress.set_fraction(0.8),
    "value-level": lambda: level.set_value(7),
    "select-combo": lambda: combo.set_selected(2) if G4 else combo.set_active(2),
    "expand-tree": expand_tree,
    "disable-row": lambda: rows[1].set_sensitive(False),
    "disable-tab": lambda: tabs[1].set_sensitive(False),
}
for case, fn in ACTIONS.items():
    b = add(box, Gtk.Button(label="do:" + case))
    b.connect("clicked", lambda _b, f=fn: f())

if G4:
    GLib.set_prgname("value_gallery")
    app = Gtk.Application(application_id="org.gorca.ValueGallery")

    def activate(a):
        win = Gtk.ApplicationWindow(application=a, title="Value gallery", default_width=500, default_height=700)
        win.set_child(box)
        win.present()

    app.connect("activate", activate)
    app.run([])
else:
    win = Gtk.Window(title="Value gallery")
    win.add(box)
    win.connect("destroy", Gtk.main_quit)
    win.show_all()
    Gtk.main()
