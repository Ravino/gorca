"""V-2 extents sanity. usage: v2.py gtk3|gtk4|adw. PRIVATE session only."""
import sys; sys.path.insert(0,"<repo>/tools/platform-probes/adr6")
from rig import *
import json
which=sys.argv[1]
K=Keys(own=False); pump(1); K.tap(125); pump(1)
script={"gtk3":"app3.py","gtk4":"app4.py","adw":"appadw.py"}[which]
p=subprocess.Popen(["/usr/bin/python3","<repo>/tools/platform-probes/adr6/"+script],stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True,bufsize=1,stderr=subprocess.DEVNULL)
def say(c): p.stdin.write(c+"\n"); p.stdin.flush()
def truth():
    say("truth")
    while True:
        l=p.stdout.readline()
        if l.startswith("TRUTH"): return json.loads(l[6:])
print(p.stdout.readline().strip())
b=app_by_pid(p.pid); print("bus",b)
root="/org/a11y/atspi/accessible/root"
def tops(): return [(info(b,f)["name"],info(b,f)["role"],f) for _,f in kids(b,root)]
print("toplevels:",tops())
def walk(maxd=9):
    for d,pth,i,ifs in dump(b,root,0,maxd):
        if d>0 and any("Component" in x for x in ifs):
            try: sc=extents(b,pth,0); wi=extents(b,pth,1); pa=extents(b,pth,2)
            except Exception as e: sc=wi=pa=str(e)[-40:]
            print("  "*d+f"{i['role']}:{i['name'][:24]!r} SCREEN={sc} WINDOW={wi} PARENT={pa}")
print("TRUTH",truth())
walk()
if which=="adw":
    say("dialog"); pump(1.5); print(p.stdout.readline().strip())
    print("toplevels after dialog:",tops()); print("TRUTH",truth()); walk()
say("quit"); pump(0.5)
