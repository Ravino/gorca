"""V-2 Shell: extents of St/Clutter a11y objects. PRIVATE session only."""
import sys; sys.path.insert(0,"<repo>/tools/platform-probes/adr6")
from rig import *
shell_pid=int(sys.argv[1])
b=":1.0"; print("shell bus",b); root="/org/a11y/atspi/accessible/root"
for _,f in kids(b,root)[:40]: print("child",f,info(b,f))
n=0
for d,pth,i,ifs in dump(b,root,0,5):
    if d>0 and any("Component" in x for x in ifs):
        try: r=[extents(b,pth,c) for c in (0,1,2)]
        except Exception as e: r=str(e)[-40:]
        if r and (r[0][2]>1) : print("  "*d+f"{i['role']}:{i['name'][:26]!r} {r}"); n+=1
    if n>45: break
