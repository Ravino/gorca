import sys, gi, threading
gi.require_version("Gtk","4.0")
from gi.repository import Gtk, GLib, Gio
class A(Gtk.Application):
    def __init__(s): super().__init__(application_id="pw.gorca.P9Gtk4")
    def do_activate(s):
        w=Gtk.ApplicationWindow(application=s,title="P9 GTK4")
        s.w=w
        box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=4); w.set_child(box)
        s.label=Gtk.Label(label="static label"); box.append(s.label)
        s.btn=Gtk.Button(label="Btn"); box.append(s.btn)
        s.chk=Gtk.CheckButton(label="Chk"); box.append(s.chk)
        s.lab4e=Gtk.Label(label="Name:"); box.append(s.lab4e)
        s.entry=Gtk.Entry(); s.entry.update_relation([Gtk.AccessibleRelation.LABELLED_BY],[[s.lab4e]]); box.append(s.entry)
        s.scale=Gtk.Scale.new_with_range(Gtk.Orientation.HORIZONTAL,0,100,1); box.append(s.scale)
        s.lb=Gtk.ListBox(); box.append(s.lb)
        for i in range(3): s.lb.append(Gtk.Label(label=f"row {i}"))
        s.stack=Gtk.Stack(); s.stack.add_titled(Gtk.Label(label="page A"),"a","A"); s.stack.add_titled(Gtk.Label(label="page B"),"b","B"); box.append(s.stack)
        s.exp=Gtk.Expander(label="Exp"); s.exp.set_child(Gtk.Label(label="inside")); box.append(s.exp)
        s.sw=Gtk.Switch(); box.append(s.sw)
        s.extra=Gtk.Button(label="Extra"); box.append(s.extra)
        w.present()
        threading.Thread(target=s.reader,daemon=True).start()
    def reader(s):
        for line in sys.stdin:
            GLib.idle_add(s.cmd,line.strip())
    def cmd(s,c):
        try:
            if c=="m_button_label": s.btn.set_label("Renamed")
            elif c=="m_label_text": s.label.set_text("changed label")
            elif c=="m_check_on": s.chk.set_active(True)
            elif c=="m_labelfor_text": s.lab4e.set_text("Full name:")
            elif c=="m_scale": s.scale.set_value(42)
            elif c=="m_entry_text": s.entry.set_text("hello")
            elif c=="m_lb_append":
                s.newrow=Gtk.Label(label="row new"); s.lb.append(s.newrow)
            elif c=="m_lb_remove": s.lb.remove(s.lb.get_row_at_index(0))
            elif c=="m_hide_extra": s.extra.set_visible(False)
            elif c=="m_show_extra": s.extra.set_visible(True)
            elif c=="m_hide_then_destroy": s.extra.set_visible(False); s.extra.get_parent().remove(s.extra); s.extra=None
            elif c=="m_stack_b": s.stack.set_visible_child_name("b")
            elif c=="m_expand": s.exp.set_expanded(True)
            elif c=="m_insensitive": s.btn.set_sensitive(False)
            elif c=="m_switch": s.sw.set_active(True)
            elif c=="m_tooltip": s.btn.set_tooltip_text("tip")
            elif c=="m_a11y_label": s.btn.update_property([Gtk.AccessibleProperty.LABEL],["A11yLabel"])
            elif c=="m_add_new_subtree":
                b=Gtk.Box(orientation=Gtk.Orientation.VERTICAL); b.append(Gtk.Label(label="n1")); b.append(Gtk.Button(label="n2")); s.sub=b; s.w.get_child().append(b)
            elif c=="m_rm_subtree": s.w.get_child().remove(s.sub)
            elif c=="quit": s.quit()
        except Exception as e: print("ERR",c,e,flush=True)
        print("done",c,flush=True)
A().run(None)
