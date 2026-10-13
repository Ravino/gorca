"""libadwaita test app: AdwApplicationWindow + AdwAlertDialog; stdin: dialog | truth | quit."""
import sys, json, threading, gi
gi.require_version("Gtk","4.0"); gi.require_version("Adw","1")
from gi.repository import Gtk, Adw, GLib
class App(Adw.Application):
    def __init__(s): super().__init__(application_id="pw.gorca.VAdw")
    def do_activate(s):
        s.w=w=Adw.ApplicationWindow(application=s,title="V Adw"); w.set_default_size(520,420)
        tv=Adw.ToolbarView(); tv.add_top_bar(Adw.HeaderBar())
        s.btn=Gtk.Button(label="Main button"); s.btn.set_halign(Gtk.Align.START); s.btn.set_valign(Gtk.Align.START); tv.set_content(s.btn); w.set_content(tv)
        w.present(); print("READY",flush=True)
        threading.Thread(target=s.rd,daemon=True).start()
    def rd(s):
        for l in sys.stdin: GLib.idle_add(s.cmd,l.split())
    def cmd(s,a):
        if a[0]=="dialog":
            d=Adw.AlertDialog(heading="Dialog heading",body="Dialog body text that explains something to the user at length.")
            d.add_response("cancel","Cancel"); d.add_response("ok","OK"); d.present(s.w); print("DIALOG",flush=True)
        elif a[0]=="truth":
            ok,r=s.btn.compute_bounds(s.w); print("TRUTH "+json.dumps({"btn":[r.get_x(),r.get_y(),r.get_width(),r.get_height()]}),flush=True)
        elif a[0]=="quit": s.quit()
App().run([])
