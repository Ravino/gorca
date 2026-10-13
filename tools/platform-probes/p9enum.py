#!/usr/bin/env python3
"""Read-only enumeration of AT-SPI apps on the live a11y bus (P-9 / P2P probe).
Only calls Properties.Get, GetChildren, GetApplicationBusAddress. No registration, no settings writes."""
import sys, os, subprocess
import gi
gi.require_version("Gio","2.0")
from gi.repository import Gio, GLib
sess=Gio.bus_get_sync(Gio.BusType.SESSION)
addr=sess.call_sync("org.a11y.Bus","/org/a11y/bus","org.a11y.Bus","GetAddress",None,None,0,2000,None).unpack()[0]
c=Gio.DBusConnection.new_for_address_sync(addr, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(dest,path,iface,m,args=None,sig=None,timeout=2000,conn=c):
    return conn.call_sync(dest,path,iface,m,args,GLib.VariantType(sig) if sig else None,Gio.DBusCallFlags.NONE,timeout,None)
def prop(dest,path,iface,name,timeout=2000,conn=c):
    return call(dest,path,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",(iface,name)),"(v)",timeout,conn).unpack()[0]
kids=call("org.a11y.atspi.Registry","/org/a11y/atspi/accessible/root","org.a11y.atspi.Accessible","GetChildren",None,"(a(so))").unpack()[0]
for (n,p) in kids:
    try:
        pid=call("org.freedesktop.DBus","/org/freedesktop/DBus","org.freedesktop.DBus","GetConnectionUnixProcessID",GLib.Variant("(s)",(n,)),"(u)").unpack()[0]
        comm=open(f"/proc/{pid}/comm").read().strip()
    except Exception as e:
        pid=-1; comm="?"
    try: nm=prop(n,p,"org.a11y.atspi.Accessible","Name")
    except Exception as e: nm="ERR "+str(e)[:60]
    try: cc=prop(n,p,"org.a11y.atspi.Accessible","ChildCount")
    except Exception as e: cc="ERR"
    try: tk=prop(n,p,"org.a11y.atspi.Application","ToolkitName")+" "+prop(n,p,"org.a11y.atspi.Application","Version")
    except Exception as e: tk="?"
    try: ab=call(n,p,"org.a11y.atspi.Application","GetApplicationBusAddress",None,"(s)").unpack()[0]
    except Exception as e: ab="ERR "+str(e)[:80]
    try:
        r=call(n,"/org/a11y/atspi/cache","org.a11y.atspi.Cache","GetItems",None,"(a((so)(so)(so)iiassusau))",10000)
        items=r.unpack()[0]
        ni=len(items)
    except Exception as e: ni="ERR "+str(e)[:60]
    print(f"{n:8} pid={pid:<7} comm={comm:<22} name={nm!r:28} toolkit={tk!r:22} children={cc} cache_items={ni} p2p={ab}")
