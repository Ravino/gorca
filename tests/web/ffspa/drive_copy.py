#!/usr/bin/python3
"""Chat-app Ctrl+C (bug 2): select a message in browse mode while the composer holds the focus, copy (see run.sh).

Bug 1 (owner, Telegram Web): after one open/Escape round the second Enter does nothing.
Each row prints what the page did (title, status line) and what gorca said.
"""
import os, sys, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

FIFO = os.environ["RDFIFO"]
OUT = os.path.dirname(os.environ["MARKLOG"])
DOWN, UP, RIGHT, LEFT, HOME, END, TAB, ESC, ENTER = 108, 103, 106, 105, 102, 107, 15, 1, 28
SHIFT, CTRL, C, A, V = 42, 29, 46, 30, 47
INSERT, SPACE = 110, 57
fails = 0
T0 = time.monotonic()


def key(*codes, wait=0.5):
    open(os.environ["MARKLOG"], "a").write(f"{time.monotonic():.3f} KEY {codes}\n")
    with open(FIFO, "w") as f:
        f.write("key " + " ".join(map(str, codes)) + "\n")
    time.sleep(wait)


def spoken_len():
    return len(open(os.path.join(OUT, "ssip.log"), errors="replace").read())


def spoken_since(n):
    s = open(os.path.join(OUT, "ssip.log"), errors="replace").read()[n:]
    return [l.split(" SPEAK ", 1)[-1] for l in s.splitlines()]


def app_root():
    desk = Atspi.get_desktop(0)
    for i in range(desk.get_child_count()):
        app = desk.get_child_at_index(i)
        if app and "irefox" in (app.get_name() or ""):
            return app


def walk(pred):
    app = app_root()
    stack = [app] if app else []
    while stack:
        n = stack.pop()
        try:
            if pred(n):
                return n
            for j in range(n.get_child_count()):
                stack.append(n.get_child_at_index(j))
        except Exception:
            pass


def page_title():
    d = walk(lambda n: n.get_role_name() == "document web" and (n.get_name() or "").startswith(("Tgapp", "Alice", "Bob", "Carol", "Dave")))
    return d.get_name() if d else None


def status_text():
    n = walk(lambda n: n.get_role_name() in ("static", "section", "paragraph", "label") and (n.get_name() or "").startswith(("Opened", "Closed", "Menu")))
    if n is not None:
        return n.get_name()
    # fall back: first text leaf under the status node
    def p(n):
        try:
            return Atspi.Text.get_text(n, 0, Atspi.Text.get_character_count(n)).startswith(("Opened", "Closed", "Menu"))
        except Exception:
            return False
    n = walk(p)
    return Atspi.Text.get_text(n, 0, Atspi.Text.get_character_count(n)) if n else None


def focused():
    n = walk(lambda n: n.get_state_set().contains(Atspi.StateType.FOCUSED) and n.get_role_name() != "frame")
    return (n.get_role_name(), n.get_name()) if n else None


def step(label, *codes, wait=0.8):
    n = spoken_len()
    key(*codes, wait=wait)
    time.sleep(0.4)
    print(f"[{time.monotonic()-T0:6.1f}] {label}: title={page_title()!r} status={status_text()!r} focus={focused()} said={spoken_since(n)}", flush=True)


def check(label, got, want_sub):
    global fails
    ok = want_sub in (got or "")
    fails += 0 if ok else 1
    print(f"{'PASS' if ok else 'FAIL'} {label}: got {got!r} want {want_sub!r}", flush=True)



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


def say(label, n):
    print(f"   {label}: said={spoken_since(n)}", flush=True)


def check(label, got, want):
    global fails
    ok = (got or "").strip() == want
    fails += 0 if ok else 1
    print(f"{'PASS' if ok else 'FAIL'} {label}: got {got!r} want {want!r}", flush=True)


key(SHIFT, wait=0.5)
key(CTRL, C); key(CTRL, C, wait=0.6)   # warm up: the first injected Ctrl+C is often lost
# Open a chat: the page focuses the composer (a contenteditable), gorca goes to focus mode.
for _ in range(3):
    key(TAB, wait=0.9)
    f = focused()
    if f and f[1] and f[1].startswith("Alice"):
        break
step("open", ENTER, wait=1.5)
print("   focus after open:", focused(), status_text(), flush=True)
mode = os.environ.get("FFSPA_COPY", "composer")
if mode == "composer":
    step("esc: leave focus mode (the composer keeps the page focus)", ESC, wait=1.0)
else:
    # focus the document instead: click into the message area through the rig pointer
    with open(FIFO, "w") as f:
        f.write("click\n")
    time.sleep(1.5)
    print("   focus after click:", focused(), flush=True)
    step("esc", ESC, wait=1.0)
print("   focus before selecting:", focused(), flush=True)
_e = walk(lambda n: n.get_state_set().contains(Atspi.StateType.FOCUSED) and n.get_role_name() != "frame")
print("   focused attrs:", dict(_e.get_attributes()) if _e else None, flush=True)
step("up (to the messages)", UP)
step("up", UP)
key(HOME)
n = spoken_len()
key(SHIFT, END, wait=1.2)
say("select line", n)
print("   focus after select:", focused(), flush=True)
got = copy()
check("C1 Ctrl+C copies the selected message line", got, "Hello from Alice")
sys.exit(1 if fails else 0)
