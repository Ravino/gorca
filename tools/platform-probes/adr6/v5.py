"""V-5 VTE (GNOME Terminal) Text costs with a 10 000-line scrollback. PRIVATE session only. usage: v5.py SERVERPID"""
import sys; sys.path.insert(0,__import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from rig import *
import statistics
b=app_by_pid(int(sys.argv[1])); print("bus",b); root="/org/a11y/atspi/accessible/root"
nodes=dump(b,root,0,10)
term=[(pth,i) for d,pth,i,ifs in nodes if i["role"]=="terminal"]
print("terminal nodes",term)
t=term[0][0]
def T(m,args,sig,n=1):
    ts=[]; r=None
    for _ in range(n):
        t0=time.perf_counter()
        try: r=ac(b,t,TXT,m,args,sig,15000).unpack()
        except Exception as e: r="ERR "+str(e).split(":")[-1][:60]
        ts.append((time.perf_counter()-t0)*1e3)
    ts.sort(); return r,ts
def st(ts): return f"n={len(ts)} p50={ts[len(ts)//2]:.2f} p99={ts[min(len(ts)-1,int(len(ts)*0.99))]:.2f} max={ts[-1]:.2f} ms"
cc=gp(b,t,TXT,"CharacterCount"); print("CharacterCount",cc, "CaretOffset", gp(b,t,TXT,"CaretOffset"))
x,y,w,h=extents(b,t,0); print("terminal extents SCREEN",(x,y,w,h),"WINDOW",extents(b,t,1))
r,ts=T("GetText",GLib.Variant("(ii)",(0,-1)),"(s)",3); print("GetText(0,-1) len",len(r[0]),"lines",r[0].count("\n"),st(ts))
r,ts=T("GetOffsetAtPoint",GLib.Variant("(iiu)",(x+3,y+3,0)),"(i)",50); print("GetOffsetAtPoint(top-left, SCREEN)",r,st(ts))
r2,ts=T("GetOffsetAtPoint",GLib.Variant("(iiu)",(x+3,y+h-3,0)),"(i)",50); print("GetOffsetAtPoint(bottom-left, SCREEN)",r2,st(ts))
r3,ts=T("GetOffsetAtPoint",GLib.Variant("(iiu)",(x+3,y+3,1)),"(i)",5); print("GetOffsetAtPoint WINDOW coord",r3)
o0=r[0] if isinstance(r,tuple) else None
a=r[0]; z=r2[0]
print("visible char range",a,z,"->",z-a,"chars; scrollback total",cc)
r,ts=T("GetRangeExtents",GLib.Variant("(iiu)",(a,z,0)),"(iiii)",20); print("GetRangeExtents(visible range)",r,st(ts))
r,ts=T("GetRangeExtents",GLib.Variant("(iiu)",(0,cc,0)),"(iiii)",3); print("GetRangeExtents(whole scrollback)",r,st(ts))
r,ts=T("GetRangeExtents",GLib.Variant("(iiu)",(a,a+60,0)),"(iiii)",50); print("GetRangeExtents(one line)",r,st(ts))
r,ts=T("GetCharacterExtents",GLib.Variant("(iu)",(a,0)),"(iiii)",50); print("GetCharacterExtents",r,st(ts))
r,ts=T("GetTextAtOffset",GLib.Variant("(iu)",(a,2)),"(sii)",30); print("GetTextAtOffset(line)",(r[0][:30],r[1],r[2]) if isinstance(r,tuple) else r,st(ts))
r,ts=T("GetText",GLib.Variant("(ii)",(a,z)),"(s)",30); print("GetText(visible range)",len(r[0]) if isinstance(r,tuple) else r,st(ts))
r,ts=T("GetBoundedRanges",GLib.Variant("(iiiiuuu)",(x,y,w,h,0,0,0)),"(a(iisv))",5); print("GetBoundedRanges(whole widget)",(len(r[0]),r[0][0][:2]) if isinstance(r,tuple) and r[0] else r,st(ts))
print("--- per-row clustering emulation: 1 OffsetAtPoint + 1 RangeExtents per visible row (rows of ~"+str(int(h/ (h/ 24)))+")")
rows=24; t0=time.perf_counter()
for i in range(rows):
    yy=y+3+int(i*(h-6)/rows); o,_=T("GetOffsetAtPoint",GLib.Variant("(iiu)",(x+3,yy,0)),"(i)",1); 
total=(time.perf_counter()-t0)*1e3; print(f"{rows} OffsetAtPoint calls total {total:.1f} ms")
