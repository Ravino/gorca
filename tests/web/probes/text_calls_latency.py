import sys, time, gi
gi.require_version('Atspi','2.0')
from gi.repository import Atspi, Gio, GLib
Atspi.init()
which=sys.argv[1]; idx=int(sys.argv[2]); dest=sys.argv[3]
desk=Atspi.get_desktop(0)
app=[desk.get_child_at_index(i) for i in range(desk.get_child_count()) if desk.get_child_at_index(i).get_name()==which][idx]
def find(o,d=0):
    if d>7: return None
    for i in range(o.get_child_count()):
        ch=o.get_child_at_index(i)
        if ch is None: continue
        if ch.get_role_name()=='document web' and ch.get_state_set().contains(Atspi.StateType.SHOWING): return ch
        r=find(ch,d+1)
        if r: return r
doc=find(app)
# Collection timings
for roles,label in (([Atspi.Role.HEADING],'heading'),([Atspi.Role.LINK],'link'),([Atspi.Role.LANDMARK],'landmark'),([Atspi.Role.PUSH_BUTTON],'button')):
    rule=Atspi.MatchRule.new(None,Atspi.CollectionMatchType.NONE,None,Atspi.CollectionMatchType.NONE,roles,Atspi.CollectionMatchType.ANY,None,Atspi.CollectionMatchType.NONE,False)
    t=time.perf_counter(); res=doc.get_collection_iface().get_matches(rule,Atspi.CollectionSortOrder.CANONICAL,0,False) if hasattr(doc,'get_collection_iface') else Atspi.Collection.get_matches(doc,rule,Atspi.CollectionSortOrder.CANONICAL,0,False)
    dt=(time.perf_counter()-t)*1000
    print(f"Collection.GetMatches({label}) -> {len(res)} matches in {dt:.1f} ms")
# find a text node
c=Gio.DBusConnection.new_for_address_sync("unix:path=/run/user/1000/at-spi/bus", Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT|Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
def call(path, iface, m, params=None):
    return c.call_sync(dest,path,iface,m,params,None,Gio.DBusCallFlags.NONE,20000,None)
best=None
def walk(o,d=0):
    global best
    t=o.get_text_iface() if hasattr(o,'get_text_iface') else None
    try:
        n=Atspi.Text.get_character_count(o)
    except Exception: n=0
    if n and (best is None or n>best[1]) and o.get_role_name() in ('section','paragraph','static','heading','link'):
        best=(o,n)
    for i in range(o.get_child_count()):
        ch=o.get_child_at_index(i)
        if ch: walk(ch,d+1)
walk(doc)
o,n=best; p=o.path
print("text node role",o.get_role_name(),"chars",n)
def lat(label,f,k=300):
    ts=[]
    for _ in range(k):
        t=time.perf_counter(); f(); ts.append((time.perf_counter()-t)*1e6)
    ts.sort(); print(f"{label}: p50 {ts[len(ts)//2]:.0f}us p99 {ts[int(len(ts)*.99)]:.0f}us")
T="org.a11y.atspi.Text"
lat("Text.GetText(0,-1)", lambda: call(p,T,"GetText",GLib.Variant("(ii)",(0,-1))))
lat("Text.GetStringAtOffset(line)", lambda: call(p,T,"GetStringAtOffset",GLib.Variant("(iu)",(0,3))))
lat("Text.GetTextAtOffset(line old)", lambda: call(p,T,"GetTextAtOffset",GLib.Variant("(iu)",(0,2))))
lat("Text.GetRangeExtents(0,1)", lambda: call(p,T,"GetRangeExtents",GLib.Variant("(iiu)",(0,1,0))))
lat("Text.GetCharacterExtents(0)", lambda: call(p,T,"GetCharacterExtents",GLib.Variant("(iu)",(0,0))))
lat("Prop CaretOffset", lambda: call(p,"org.freedesktop.DBus.Properties","Get",GLib.Variant("(ss)",(T,"CaretOffset"))))
