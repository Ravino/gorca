"""V-1b: keysym/state delivered for every keypad key, NumLock off/on, with/without Shift. PRIVATE session only."""
import sys; sys.path.insert(0,"<repo>/tools/platform-probes/adr6")
from rig import *
K=Keys(); G=GtkKeys(); pump(1.5); K.tap(125); pump(1)
names={71:"KP7",72:"KP8",73:"KP9",74:"KP-",75:"KP4",76:"KP5",77:"KP6",78:"KP+",79:"KP1",80:"KP2",81:"KP3",82:"KP0",83:"KP.",96:"KPEnter",98:"KP/",55:"KP*"}
import gi; 
def row(nl):
    for kc,n in names.items():
        out=[]
        for mods in ((),(42,)):
            n0=len(K.ev); K.chord(mods,kc); pump(0.1)
            e=[x for x in K.ev[n0:] if not x[1] and x[4] in (kc,kc+8)]
            out.append("%s:%d"%(hex(e[0][3])[2:],e[0][2]) if e else "none")
        print(f"NL={nl} {n:8s} plain={out[0]:12s} shift={out[1]}")
row("off"); K.tap(69); pump(0.2); row("on"); K.tap(69); K.stop(); G.p.terminate()
