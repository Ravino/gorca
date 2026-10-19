#!/usr/bin/python3
"""Chat-app flow with real keys: Tab to a chat, Enter opens it, Escape, Tab to a chat, Enter (see run.sh).

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


# warm up (the first injected key after the click is often lost)
key(SHIFT, wait=0.5)
print("start", page_title(), status_text(), focused(), flush=True)

SEQ = os.environ.get("FFSPA_SEQ", "A")
# round 1
step("tab menu", TAB)
step("tab alice", TAB)
step("enter opens alice", ENTER, wait=1.5)
check("R1 first Enter opens the chat", status_text(), "Opened chat")
print("   status", status_text(), flush=True)

if SEQ == "A":
    # Escape (gorca leaves focus mode of the composer), arrows in the messages, Escape (page closes the chat)
    step("esc 1", ESC, wait=1.0)
    step("down", DOWN)
    step("down", DOWN)
    step("esc 2", ESC, wait=1.5)
elif SEQ == "B":
    step("esc only", ESC, wait=1.5)
elif SEQ == "D":
    # Escape leaves focus mode (the composer keeps the page focus); browse arrows go back to the list
    step("esc", ESC, wait=1.0)
    step("ctrl+home", CTRL, HOME)
    step("down", DOWN)
    step("down", DOWN)
    step("down", DOWN)
    step("enter on the row", ENTER, wait=1.5)
    check("R2 Enter after browsing back to the list", status_text(), "Opened chat")
    sys.exit(1 if fails else 0)
elif SEQ == "E":
    # rows are rewritten by the page every second: Enter must still open a chat (second Enter at the latest)
    step("esc", ESC, wait=1.0)
    step("esc closes", ESC, wait=1.5)
    step("tab", TAB, wait=1.0)
    step("tab", TAB, wait=1.0)
    time.sleep(2.5)
    step("enter 1", ENTER, wait=1.5)
    step("enter 2", ENTER, wait=1.5)
    check("R2 Enter on a churning row opens the chat within two presses", status_text(), "Opened chat")
    sys.exit(1 if fails else 0)
elif SEQ == "C":
    step("esc", ESC, wait=1.0)
    step("esc", ESC, wait=1.5)
step("tab 1", TAB, wait=1.0)
step("tab 2", TAB, wait=1.0)
step("tab 3", TAB, wait=1.0)
step("enter again", ENTER, wait=1.5)
check("R2 second Enter opens a chat again", status_text(), "Opened chat")
print("   title", page_title(), flush=True)
sys.exit(1 if fails else 0)
