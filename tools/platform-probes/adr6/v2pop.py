"""V-2 popover: GTK4 popover surface coordinates. PRIVATE session only."""
import sys; sys.path.insert(0,"<repo>/tools/platform-probes/adr6")
from rig import *
import json
K=Keys(own=False); pump(1); K.tap(125); pump(1)
p=subprocess.Popen(["/usr/bin/python3","<repo>/tools/platform-probes/adr6/app4.py"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,bufsize=1,stderr=subprocess.DEVNULL)
def say(c): p.stdin.write(c+"\n"); p.stdin.flush()
def truth():
    say("truth")
    while True:
        l=p.stdout.readline()
        if l.startswith("TRUTH"): return json.loads(l[6:])
print(p.stdout.readline().strip()); b=app_by_pid(p.pid); root="/org/a11y/atspi/accessible/root"
print("TRUTH closed",truth())
say("popup"); pump(1.0)
print("TRUTH open",truth())
for _,f in kids(b,root): print("toplevel",info(b,f))
for d,pth,i,ifs in dump(b,root,0,10):
    if i["role"] in ("label","menu button","toggle button","popover","window","dialog") or "Popover" in i["name"] or d<=1:
        try: print("  "*d+f"{i['role']}:{i['name'][:30]!r} SCREEN={extents(b,pth,0)} WINDOW={extents(b,pth,1)} PARENT={extents(b,pth,2)}")
        except Exception as e: print("  "*d+f"{i['role']} ERR {str(e)[-50:]}")
say("quit"); pump(0.5)
