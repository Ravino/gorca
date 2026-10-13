"""GTK3 test app for ADR-0006 V-2/V-3/V-4. stdin commands: truth | resize W H | scroll N | quit. Prints JSON lines."""
import sys, json, threading, gi
gi.require_version("Gtk","3.0"); gi.require_version("Gdk","3.0")
from gi.repository import Gtk, Gdk, GLib
LONG="\n".join("Line %d of the long text buffer used for scrolling checks"%i for i in range(1,80))
class W(Gtk.Window):
    def __init__(s):
        super().__init__(title="V GTK3"); s.set_default_size(520,420)
        hb=Gtk.HeaderBar(); hb.set_show_close_button(True); hb.set_title("V GTK3"); s.set_titlebar(hb)
        box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=6,margin=12); s.add(box)
        s.label=Gtk.Label(label="Wrapped label with several words that will wrap across lines when the window is narrow enough to force it"); s.label.set_line_wrap(True); s.label.set_xalign(0); box.pack_start(s.label,False,False,0)
        s.btn=Gtk.Button(label="Button"); box.pack_start(s.btn,False,False,0)
        s.entry=Gtk.Entry(text="entry text"); box.pack_start(s.entry,False,False,0)
        s.sw=Gtk.ScrolledWindow(); s.sw.set_min_content_height(160); s.sw.set_vexpand(True); box.pack_start(s.sw,True,True,0)
        s.tv=Gtk.TextView(); s.tv.get_buffer().set_text(LONG); s.tv.set_editable(False); s.sw.add(s.tv)
        s.sw2=Gtk.ScrolledWindow(); s.sw2.set_min_content_height(100); box.pack_start(s.sw2,False,False,0)
        s.lb=Gtk.ListBox(); s.sw2.add(s.lb)
        for i in range(60): s.lb.add(Gtk.Label(label="row %d"%i))
        s.connect("destroy",Gtk.main_quit)
        threading.Thread(target=s.rd,daemon=True).start()
    def box(s,w):
        top=s.get_toplevel()
        x,y=w.translate_coordinates(top,0,0)
        a=w.get_allocation()
        return [x,y,a.width,a.height]
    def truth(s):
        gw=s.get_window(); top=s
        ox,oy=gw.get_origin()[1:] if gw else (0,0)
        d={"toplevel_alloc":[s.get_allocation().x,s.get_allocation().y,s.get_allocation().width,s.get_allocation().height],
           "gdkwin_size":[gw.get_width(),gw.get_height()],"size":list(s.get_size()),
           "label":s.box(s.label),"button":s.box(s.btn),"entry":s.box(s.entry),"sw":s.box(s.sw),"tv":s.box(s.tv),
           "tv_scroll_y":s.sw.get_vadjustment().get_value(),"tv_visible":[s.tv.get_visible_rect().x,s.tv.get_visible_rect().y,s.tv.get_visible_rect().width,s.tv.get_visible_rect().height]}
        bx,by=s.tv.window_to_buffer_coords(Gtk.TextWindowType.TEXT,0,0)
        it=s.tv.get_iter_at_location(bx,by)
        it=it[1] if isinstance(it,tuple) else it
        d["tv_first_visible_offset"]=it.get_offset(); d["tv_first_visible_line"]=it.get_line()
        d["lb_scroll_y"]=s.sw2.get_vadjustment().get_value()
        print("TRUTH "+json.dumps(d),flush=True)
    def rd(s):
        for l in sys.stdin:
            GLib.idle_add(s.cmd,l.split())
    def cmd(s,a):
        if a[0]=="truth": s.truth()
        elif a[0]=="resize": s.resize(int(a[1]),int(a[2]))
        elif a[0]=="scroll": s.sw.get_vadjustment().set_value(float(a[1]))
        elif a[0]=="lbscroll": s.sw2.get_vadjustment().set_value(float(a[1]))
        elif a[0]=="grow": s.btn.set_size_request(-1,int(a[1]))
        elif a[0]=="move": s.move(int(a[1]),int(a[2]))
        elif a[0]=="quit": Gtk.main_quit()
w=W(); w.show_all(); print("READY",flush=True); Gtk.main()
