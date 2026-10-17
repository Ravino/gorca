"""V-4 Shell/St: any bounds-changed / geometry-ish events while Overview opens and closes. PRIVATE session only."""
import sys; sys.path.insert(0,__import__("os").path.dirname(__import__("os").path.abspath(__file__)))
from rig import *
from collections import Counter
K=Keys(own=False); pump(1)
ev=[]; listen(["object:","window:","object:bounds-changed","object:visible-data-changed"]); collect(ev,":1.0")
for rnd in range(2):
    n0=len(ev); K.tap(125); pump(2.0)
    c=Counter((e[3].split(".")[-1],e[4]) for e in ev[n0:]); print("Super tap",rnd,"events:",dict(c))
n0=len(ev)
# Alt+F2/others not needed. Type-ahead search in overview moves lots of actors
K.tap(125); pump(1.0); 
for kc in (30,48,46): K.tap(kc)   # a b c
pump(1.0); c=Counter((e[3].split(".")[-1],e[4]) for e in ev[n0:]); print("overview + typing:",dict(c))
K.tap(1); pump(1)  # Esc
