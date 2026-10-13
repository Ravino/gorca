"""Shared helpers for ADR-0006 probes. PRIVATE session only (refuses /run/user/1000/bus)."""
import os, sys, time, subprocess
import gi
gi.require_version("Gio","2.0")
from gi.repository import Gio, GLib
addr=os.environ["DBUS_SESSION_BUS_ADDRESS"]
assert addr.startswith("unix:path=/tmp/dbus-"), "refusing non-private bus "+addr
sess=Gio.DBusConnection.new_for_address_sync(addr,Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,None,None)
def call(conn,dest,path,iface,m,args=None,sig=None,timeout=3000):
    return conn.call_sync(dest,path,iface,m,args,GLib.VariantType(sig) if sig else None,Gio.DBusCallFlags.NONE,timeout,None)
def pump(sec):
    end=time.time()+sec; ctx=GLib.MainContext.default()
    while time.time()<end:
        while ctx.iteration(False): pass
        time.sleep(0.003)
class Keys:
    """RemoteDesktop virtual keyboard + a11y KeyboardMonitor."""
    def __init__(s, own=True):
        s.ev=[]
        if own:
            r=call(sess,"org.freedesktop.DBus","/org/freedesktop/DBus","org.freedesktop.DBus","RequestName",GLib.Variant("(su)",("org.gnome.Orca.KeyboardMonitor",0)),"(u)")
            assert r.unpack()[0]==1, r
            sess.signal_subscribe(None,"org.freedesktop.a11y.KeyboardMonitor","KeyEvent",None,None,Gio.DBusSignalFlags.NONE,s._on)
            call(sess,"org.freedesktop.a11y.Manager","/org/freedesktop/a11y/Manager","org.freedesktop.a11y.KeyboardMonitor","WatchKeyboard")
        r=call(sess,"org.gnome.Mutter.RemoteDesktop","/org/gnome/Mutter/RemoteDesktop","org.gnome.Mutter.RemoteDesktop","CreateSession",None,"(o)")
        s.sp=r.unpack()[0]
        call(sess,"org.gnome.Mutter.RemoteDesktop",s.sp,"org.gnome.Mutter.RemoteDesktop.Session","Start")
    def _on(s,conn,snd,path,iface,member,p,*a):
        rel,st,ks,uni,kc=p.unpack(); s.ev.append((time.time(),rel,st,ks,kc))
    def key(s,kc,down):
        call(sess,"org.gnome.Mutter.RemoteDesktop",s.sp,"org.gnome.Mutter.RemoteDesktop.Session","NotifyKeyboardKeycode",GLib.Variant("(ub)",(kc,down)))
    def tap(s,kc,gap=0.03): s.key(kc,True); pump(gap); s.key(kc,False); pump(gap)
    def chord(s,mods,kc,gap=0.04):
        for m in mods: s.key(m,True); pump(gap)
        s.tap(kc,gap)
        for m in reversed(mods): s.key(m,False); pump(gap)
    def grabs(s,mods,keys):
        call(sess,"org.freedesktop.a11y.Manager","/org/freedesktop/a11y/Manager","org.freedesktop.a11y.KeyboardMonitor","SetKeyGrabs",GLib.Variant("(aua(uu))",(mods,keys)))
    def stop(s):
        try: call(sess,"org.gnome.Mutter.RemoteDesktop",s.sp,"org.gnome.Mutter.RemoteDesktop.Session","Stop")
        except Exception: pass
class GtkKeys:
    def __init__(s,script="<repo>/tools/platform-probes/gtkkeys.py"):
        env=dict(os.environ,NO_AT_BRIDGE="1",GTK_A11Y="none")
        s.p=subprocess.Popen(["/usr/bin/python3",script],stdout=subprocess.PIPE,env=env,text=True,bufsize=1)
        os.set_blocking(s.p.stdout.fileno(),False); s.lines=[]
    def poll(s):
        try:
            while True:
                l=s.p.stdout.readline()
                if not l: break
                s.lines.append(l.strip())
        except Exception: pass
    def keys(s,since=0):
        s.poll(); return [(int(l.split()[3])) for l in s.lines[since:] if l.startswith("KEY") and l.split()[2]=="press"]

# ---------------- a11y client ----------------
ACC="org.a11y.atspi.Accessible"; COMP="org.a11y.atspi.Component"; TXT="org.a11y.atspi.Text"
_a11y=None
def a11y():
    global _a11y
    if _a11y is None:
        a=call(sess,"org.a11y.Bus","/org/a11y/bus","org.a11y.Bus","GetAddress",None,"(s)").unpack()[0]
        assert a.startswith("unix:path=/run/user/1000/adr6/"), "refusing non-private a11y bus "+a
        _a11y=Gio.DBusConnection.new_for_address_sync(a,Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,None,None)
    return _a11y
def ac(dest,path,iface,m,args=None,sig=None,timeout=5000):
    return call(a11y(),dest,path,iface,m,args,sig,timeout)
def gp(dest,path,iface,name):
    return ac(dest,path,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",(iface,name)),"(v)").unpack()[0]
def app_by_pid(pid,tries=60):
    for _ in range(tries):
        pump(0.2)
        try: kids_=ac("org.a11y.atspi.Registry","/org/a11y/atspi/accessible/root",ACC,"GetChildren",None,"(a(so))").unpack()[0]
        except Exception: kids_=[]
        for n,p in kids_:
            try:
                if call(a11y(),"org.freedesktop.DBus","/org/freedesktop/DBus","org.freedesktop.DBus","GetConnectionUnixProcessID",GLib.Variant("(s)",(n,)),"(u)").unpack()[0]==pid: return n
            except Exception: pass
    return None
def info(b,p):
    return dict(name=gp(b,p,ACC,"Name"),role=ac(b,p,ACC,"GetRoleName",None,"(s)").unpack()[0],n=gp(b,p,ACC,"ChildCount"))
def kids(b,p): return [k for k in ac(b,p,ACC,"GetChildren",None,"(a(so))").unpack()[0] if k[0]==b]
def extents(b,p,coord):
    return tuple(ac(b,p,COMP,"GetExtents",GLib.Variant("(u)",(coord,)),"((iiii))").unpack()[0])
def dump(b,p,d=0,maxd=8,out=None):
    out=out if out is not None else []
    try: i=info(b,p)
    except Exception as e: return out
    ifs=ac(b,p,ACC,"GetInterfaces",None,"(as)").unpack()[0]
    out.append((d,p,i,ifs))
    if d<maxd:
        for _,k in kids(b,p): dump(b,k,d+1,maxd,out)
    return out

def listen(events):
    for ev in events:
        ac("org.a11y.atspi.Registry","/org/a11y/atspi/registry","org.a11y.atspi.Registry","RegisterEvent",GLib.Variant("(sass)",(ev,[],"")))
def collect(store,bus=None):
    t0=time.time()
    def on(conn,snd,path,iface,member,p,*a):
        if bus is None or snd==bus: store.append((time.time()-t0,snd,path,iface,member,p.unpack()))
    a11y().signal_subscribe(None,None,None,None,None,Gio.DBusSignalFlags.NONE,on)
