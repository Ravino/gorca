import sys, gi, threading
gi.require_version("Gtk","3.0")
from gi.repository import Gtk, GLib
class W(Gtk.Window):
    pass
w=Gtk.Window(title="P9 GTK3"); box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=4); w.add(box)
label=Gtk.Label(label="static label"); box.add(label)
btn=Gtk.Button(label="Btn"); box.add(btn)
chk=Gtk.CheckButton(label="Chk"); box.add(chk)
lab4e=Gtk.Label(label="Name:"); box.add(lab4e)
entry=Gtk.Entry(); entry.get_accessible().add_relationship  if False else None; box.add(entry)
lab4e.set_mnemonic_widget(entry)
scale=Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL,0,100,1); box.add(scale)
lb=Gtk.ListBox(); box.add(lb)
for i in range(3): lb.add(Gtk.Label(label=f"row {i}"))
nb=Gtk.Notebook(); nb.append_page(Gtk.Label(label="page A"),Gtk.Label(label="A")); nb.append_page(Gtk.Label(label="page B"),Gtk.Label(label="B")); box.add(nb)
tv_store=Gtk.ListStore(str); [tv_store.append([f"item {i}"]) for i in range(3)]
tv=Gtk.TreeView(model=tv_store); tv.append_column(Gtk.TreeViewColumn("col",Gtk.CellRendererText(),text=0)); box.add(tv)
exp=Gtk.Expander(label="Exp"); exp.add(Gtk.Label(label="inside")); box.add(exp)
extra=Gtk.Button(label="Extra"); box.add(extra)
w.connect("destroy",Gtk.main_quit); w.show_all()
st={}
def cmd(c):
    try:
        if c=="m_button_label": btn.set_label("Renamed")
        elif c=="m_label_text": label.set_text("changed label")
        elif c=="m_check_on": chk.set_active(True)
        elif c=="m_labelfor_text": lab4e.set_text("Full name:")
        elif c=="m_scale": scale.set_value(42)
        elif c=="m_entry_text": entry.set_text("hello")
        elif c=="m_lb_append": r=Gtk.Label(label="row new"); lb.add(r); r.show()
        elif c=="m_lb_remove": lb.remove(lb.get_row_at_index(0))
        elif c=="m_hide_extra": extra.hide()
        elif c=="m_show_extra": extra.show()
        elif c=="m_stack_b": nb.set_current_page(1)
        elif c=="m_expand": exp.set_expanded(True)
        elif c=="m_insensitive": btn.set_sensitive(False)
        elif c=="m_tv_append": tv_store.append(["item new"])
        elif c=="m_tv_remove": tv_store.remove(tv_store.get_iter_first())
        elif c=="m_add_new_subtree":
            b=Gtk.Box(orientation=Gtk.Orientation.VERTICAL); b.add(Gtk.Label(label="n1")); b.add(Gtk.Button(label="n2")); st['sub']=b; box.add(b); b.show_all()
        elif c=="m_rm_subtree": box.remove(st['sub'])
        elif c=="m_destroy_extra": extra.destroy()
        elif c=="quit": Gtk.main_quit()
    except Exception as e: print("ERR",c,e,flush=True)
    print("done",c,flush=True)
def reader():
    for line in sys.stdin: GLib.idle_add(cmd,line.strip())
threading.Thread(target=reader,daemon=True).start()
Gtk.main()
