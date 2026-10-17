#!/usr/bin/python3
"""Browse-mode selection with gorca, copied by the browser's own Ctrl+C (see run.sh)."""
import os, subprocess, sys, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi
FIFO = os.environ["RDFIFO"]
DOWN, RIGHT, LEFT, UP, HOME, END, TAB = 108, 106, 105, 103, 102, 107, 15
SHIFT, CTRL, C, A, V = 42, 29, 46, 30, 47
fails = 0

def key(*codes, wait=0.4):
    open(os.environ.get("MARKLOG","/dev/null"),"a").write(f"{time.monotonic():.3f} KEY {codes}\n")
    with open(FIFO, "w") as f:
        f.write("key " + " ".join(map(str, codes)) + "\n")
    time.sleep(wait)

def paste():
    time.sleep(0.3)
    out = os.environ["MARKLOG"] + ".clip"
    if os.path.exists(out):
        os.unlink(out)
    with open(FIFO, "w") as f:
        f.write("clip " + out + "\n")
    for _ in range(50):
        time.sleep(0.1)
        if os.path.exists(out) and os.path.getsize(out) >= 0:
            time.sleep(0.1)
            break
    return open(out, "rb").read().decode(errors="replace")

def copy():
    key(CTRL, C); key(CTRL, C, wait=0.6)
    return paste()

def find(role_name, name=None):
    desk = Atspi.get_desktop(0)
    for i in range(desk.get_child_count()):
        app = desk.get_child_at_index(i)
        if app and any(b in (app.get_name() or "") for b in ("irefox", "hrom")):
            stack = [app]
            while stack:
                n = stack.pop()
                try:
                    if n.get_role_name() == role_name and (name is None or n.get_name() == name):
                        return n
                    for j in range(n.get_child_count()):
                        stack.append(n.get_child_at_index(j))
                except Exception:
                    pass

def entry_text():
    e = find("entry", "target")
    return None if e is None else Atspi.Text.get_text(e, 0, Atspi.Text.get_character_count(e))

def check(label, got, want):
    global fails
    ok = got == want
    fails += 0 if ok else 1
    print(f"{'PASS' if ok else 'FAIL'} {label}: got {got!r}" + ("" if ok else f" want {want!r}"), flush=True)

# warm up: the first injected Ctrl+C after the click is often lost by the rig
key(CTRL, HOME); key(SHIFT, RIGHT); copy()

def top():
    key(CTRL, HOME); key(HOME)

top(); key(SHIFT, END)
check("E1 Shift+End: the heading, last character included", copy(), "Heading one")

top(); key(DOWN); key(SHIFT, END)
check("E2 Shift+End: a paragraph with a link inside", copy(), "Alpha bravo link inside charlie delta.")

top(); key(DOWN)
for _ in range(2):
    key(CTRL, SHIFT, RIGHT)
check("E3 two words, space before the link left out", copy(), "Alpha bravo")

top(); key(DOWN); key(SHIFT, DOWN)
check("E4 one line down from a paragraph start", copy(), "Alpha bravo link inside charlie delta.")

top(); key(DOWN); key(SHIFT, DOWN); key(SHIFT, DOWN)
check("E5 two paragraphs, blocks whole", copy(), "Alpha bravo link inside charlie delta.\n\nEcho foxtrot golf hotel.")

key(CTRL, A)
all_text = copy()
check("E6 Ctrl+A starts at the heading and stops before the password field", all_text.strip().split("\n")[0], "Heading one")
print("   Ctrl+A text:", repr(all_text), flush=True)

top(); key(DOWN); key(RIGHT); key(RIGHT); key(SHIFT, DOWN)
print("INFO E8 partial first paragraph to partial second:", repr(copy()), flush=True)
top(); key(DOWN); key(RIGHT); key(RIGHT); key(SHIFT, DOWN); key(SHIFT, DOWN); key(SHIFT, DOWN)
print("INFO E9 partial through nested div:", repr(copy()), flush=True)

# paste into the page's text field: Tab to it from the top, Ctrl+V
top(); key(SHIFT, END)
want = copy()
for _ in range(2):
    key(TAB)
time.sleep(0.8)
key(CTRL, V, wait=1.2)
check("E7 paste into the field", entry_text(), want)
sys.exit(1 if fails else 0)
