import sys, time, gi
gi.require_version("Gio","2.0")
from gi.repository import Gio, GLib
addr=open(sys.argv[1]).read().strip(); app=sys.argv[2]
c=Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(path,iface,m,args=None,sig=None):
    t=time.perf_counter()
    r=c.call_sync(app,path,iface,m,args,GLib.VariantType(sig) if sig else None,Gio.DBusCallFlags.NONE,5000,None)
    return r,(time.perf_counter()-t)*1e3
r,_=call("/org/a11y/atspi/cache","org.a11y.atspi.Cache","GetItems",None,"(a((so)(so)(so)iiassusau))")
items=r.get_child_value(0)
for i in range(items.n_children()):
    it=items.get_child_value(i)
    path=it.get_child_value(0).get_child_value(1).get_string()
    ifs=[it.get_child_value(5).get_child_value(j).get_string() for j in range(it.get_child_value(5).n_children())]
    role=it.get_child_value(7).get_uint32()
    if "org.a11y.atspi.Text" in ifs and role==60:  # ROLE_TERMINAL
        print("terminal",path,"interfaces:",ifs)
        for p in ("CharacterCount","CaretOffset"):
            v,dt=call(path,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",("org.a11y.atspi.Text",p)),"(v)")
            print(p,v.get_child_value(0).get_variant().unpack(),f"{dt:.2f}ms")
        n=None
        v,dt=call(path,"org.a11y.atspi.Text","GetText",GLib.Variant("(ii)",(0,-1)),"(s)"); s=v.unpack()[0]; print("GetText(0,-1): chars",len(s),"lines",s.count("\n"),f"{dt:.2f}ms")
        v,dt=call(path,"org.a11y.atspi.Text","GetAttributeRun",GLib.Variant("(ib)",(0,True)),"(a{ss}ii)"); print("GetAttributeRun", v.unpack()[1:], f"{dt:.2f}ms")
        v,dt=call(path,"org.a11y.atspi.Text","GetTextAtOffset",GLib.Variant("(iu)",(0,2)),"(sii)"); print("GetTextAtOffset line0 range", v.unpack()[1:], f"{dt:.2f}ms")
        ts=[]
        for k in range(50):
            _,d=call(path,"org.a11y.atspi.Text","GetText",GLib.Variant("(ii)",(0,-1)),"(s)"); ts.append(d)
        ts.sort(); print(f"GetText full-screen x50 p50={ts[25]:.2f} max={ts[-1]:.2f} ms")
        break
