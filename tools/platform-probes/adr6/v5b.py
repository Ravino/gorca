"""V-5b: does VTE a11y Text expose scrollback? Scroll up with Shift+PageUp and compare. PRIVATE session only."""
import sys; sys.path.insert(0,"<repo>/tools/platform-probes/adr6")
from rig import *
b=app_by_pid(int(sys.argv[1])); t="/org/a11y/atspi/accessible/60"
K=Keys(own=False)
def snap(l):
    cc=gp(b,t,TXT,"CharacterCount"); s=ac(b,t,TXT,"GetText",GLib.Variant("(ii)",(0,-1)),"(s)").unpack()[0]
    lines=s.split("\n"); print(f"{l:22s} chars={cc} first={lines[0][:34]!r} last_nonempty={[x for x in lines if x][-1][:34]!r} caret={gp(b,t,TXT,'CaretOffset')}")
snap("bottom")
for n in (1,20,200,600):
    for _ in range(n if n<=20 else 20): 
        K.key(42,True); K.tap(104,0.005); K.key(42,False)
    pump(0.5); snap(f"after {n if n<=20 else 20}xShift+PgUp")
