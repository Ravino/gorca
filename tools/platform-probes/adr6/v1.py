"""V-1: Shift+KP_* grab compilation. PRIVATE session only."""
import sys; sys.path.insert(0,"<repo>/tools/platform-probes/adr6")
from rig import *
K=Keys(); G=GtkKeys()
pump(1.5); K.tap(125); pump(1.0)   # leave Overview
G.poll(); print("gtk:",G.lines)
SH=42; KP7=71; NUM=69
def trial(label,grabs,pre=None,mods=(SH,),kc=KP7):
    K.grabs([],grabs); pump(0.05)
    G.poll(); n0=len(K.ev); g0=len(G.lines)
    if pre: pre()
    K.chord(mods,kc); pump(0.2)
    seen=[(hex(e[3]),e[2],"rel" if e[1] else "press") for e in K.ev[n0:] if e[4] in (kc,kc+8)]
    gk=[hex(v) for v in G.keys(g0)]
    print(f"{label:46s} monitor={seen} gtk_press={gk}")
KP_Home=0xff95; KP_7=0xffb7
print("--- baseline: no grabs, NumLock off")
trial("no grab",[])
for ks,name in ((KP_Home,"KP_Home"),(KP_7,"KP_7")):
    for m,mn in ((0,"mask0"),(1,"Shift"),(1|16,"Shift|Mod2")):
        trial(f"grab ({name},{mn}) Shift+KP7",[(ks,m)])
print("--- unshifted KP7, grab KP_Home mask0")
trial("grab (KP_Home,0) KP7 alone",[(KP_Home,0)],mods=())
print("--- NumLock ON (tap 69)")
K.grabs([],[]); K.tap(NUM); pump(0.2)
trial("numlock on, no grab, KP7 alone",[],mods=())
trial("numlock on, no grab, Shift+KP7",[])
for ks,name in ((KP_Home,"KP_Home"),(KP_7,"KP_7")):
    for m,mn in ((0,"mask0"),(1,"Shift"),(16,"Mod2"),(1|16,"Shift|Mod2")):
        trial(f"NL on grab ({name},{mn}) Shift+KP7",[(ks,m)])
        trial(f"NL on grab ({name},{mn}) KP7 alone",[(ks,m)],mods=())
K.grabs([],[]); K.tap(NUM); pump(0.2)
trial("numlock off again, Shift+KP7 no grab",[])
K.grabs([],[]); K.stop(); G.p.terminate()
