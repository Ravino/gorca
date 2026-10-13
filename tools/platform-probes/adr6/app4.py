"""GTK4 test app for ADR-0006 V-2/V-3/V-4 (plain Gtk, Adw dialog when 'adw' arg). stdin: truth | resize W H | scroll N | dialog | quit."""
import sys, json, threading, gi
gi.require_version("Gtk","4.0")
from gi.repository import Gtk, GLib
ADW=None
try:
    gi.require_version("Adw","1"); from gi.repository import Adw; ADW=Adw
except Exception: pass
LONG="\n".join("Line %d of the long text buffer used for scrolling checks"%i for i in range(1,80))
class App(Gtk.Application):
    def __init__(s): super().__init__(application_id="pw.gorca.V4")
    def do_activate(s):
        s.w=w=Gtk.ApplicationWindow(application=s,title="V GTK4"); w.set_default_size(520,420)
        hb=Gtk.HeaderBar(); w.set_titlebar(hb)
        box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=6); box.set_margin_start(12); box.set_margin_top(12); box.set_margin_end(12); box.set_margin_bottom(12); w.set_child(box)
        s.label=Gtk.Label(label="Wrapped label with several words that will wrap across lines when the window is narrow enough to force it"); s.label.set_wrap(True); s.label.set_xalign(0); box.append(s.label)
        s.btn=Gtk.Button(label="Button"); box.append(s.btn)
        s.entry=Gtk.Entry(text="entry text"); box.append(s.entry)
        s.sw=Gtk.ScrolledWindow(); s.sw.set_min_content_height(160); s.sw.set_vexpand(True); box.append(s.sw)
        s.tv=Gtk.TextView(); s.tv.get_buffer().set_text(LONG); s.tv.set_editable(False); s.sw.set_child(s.tv)
        s.sw2=Gtk.ScrolledWindow(); s.sw2.set_min_content_height(100); box.append(s.sw2)
        s.lb=Gtk.ListBox(); s.sw2.set_child(s.lb)
        for i in range(60): s.lb.append(Gtk.Label(label="row %d"%i))
        s.mb=Gtk.MenuButton(label="Menu"); s.pop=Gtk.Popover(); pl=Gtk.Label(label="Popover content label"); pl.set_margin_start(20); pl.set_margin_top(20); pl.set_margin_bottom(20); pl.set_margin_end(20); s.pop.set_child(pl); s.pl=pl; s.mb.set_popover(s.pop); box.append(s.mb)
        w.present(); print("READY",flush=True)
        threading.Thread(target=s.rd,daemon=True).start()
    def bb(s,wd):
        ok,r=wd.compute_bounds(s.w)
        return [r.get_x(),r.get_y(),r.get_width(),r.get_height()] if ok else None
    def truth(s):
        d={"win_size":[s.w.get_width(),s.w.get_height()],"label":s.bb(s.label),"button":s.bb(s.btn),"entry":s.bb(s.entry),"sw":s.bb(s.sw),"tv":s.bb(s.tv),
           "tv_scroll_y":s.sw.get_vadjustment().get_value()}
        vr=s.tv.get_visible_rect(); d["tv_visible"]=[vr.x,vr.y,vr.width,vr.height]
        bx,by=s.tv.window_to_buffer_coords(Gtk.TextWindowType.TEXT,0,0)
        r=s.tv.get_iter_at_location(bx,by)
        it=r[1] if isinstance(r,tuple) else r
        d["tv_first_visible_offset"]=it.get_offset(); d["tv_first_visible_line"]=it.get_line()
        tx,ty=s.w.get_surface_transform() if hasattr(s.w,"get_surface_transform") else (None,None)
        d["surface_transform"]=[tx,ty]; d["surface_size"]=[s.w.get_surface().get_width(),s.w.get_surface().get_height()]
        d["menubtn"]=s.bb(s.mb)
        if s.pop.get_visible():
            ok,r=s.pl.compute_bounds(s.pop); d["popover_label_in_popover"]=[r.get_x(),r.get_y(),r.get_width(),r.get_height()] if ok else None
            d["popover_surface_size"]=[s.pop.get_surface().get_width(),s.pop.get_surface().get_height()]
            d["popover_surface_transform"]=list(s.pop.get_surface_transform())
        print("TRUTH "+json.dumps(d),flush=True)
    def rd(s):
        for l in sys.stdin: GLib.idle_add(s.cmd,l.split())
    def cmd(s,a):
        if a[0]=="truth": s.truth()
        elif a[0]=="resize": s.w.set_default_size(int(a[1]),int(a[2]))
        elif a[0]=="scroll": s.sw.get_vadjustment().set_value(float(a[1]))
        elif a[0]=="popup": s.pop.popup()
        elif a[0]=="lbscroll": s.sw2.get_vadjustment().set_value(float(a[1]))
        elif a[0]=="grow": s.btn.set_size_request(-1,int(a[1]))
        elif a[0]=="quit": s.quit()
App().run([])
