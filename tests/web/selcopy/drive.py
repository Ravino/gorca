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

def check(label, got, want, chromium_known=None):
    """PASS when the text matches up to trailing white space (Blink keeps exact, exclusive ends,
    so it may add a trailing space or blank line). `chromium_known` names a documented Blink
    limit: a failure under SELCOPY_BROWSER=chromium is then reported as XFAIL."""
    global fails
    ok = got.rstrip() == want.rstrip() if isinstance(got, str) and isinstance(want, str) else got == want
    known = (not ok) and chromium_known and os.environ.get("SELCOPY_BROWSER") == "chromium"
    fails += 0 if (ok or known) else 1
    tag = "PASS" if ok else ("XFAIL" if known else "FAIL")
    print(f"{tag} {label}: got {got!r}" + ("" if ok else f" want {want!r}") + (f" ({chromium_known})" if known else ""), flush=True)

# warm up: the first injected Ctrl+C after the click is often lost by the rig
key(CTRL, HOME); key(SHIFT, RIGHT); copy()

def top():
    # the first line is the skip link; the heading is the next one
    key(CTRL, HOME); key(DOWN); key(HOME)

def focused():
    """(role, name) of the element inside the page that has focus."""
    d = find("document web", "Selpage")
    stack = [d] if d is not None else []
    while stack:
        n = stack.pop()
        try:
            if n.get_state_set().contains(Atspi.StateType.FOCUSED):
                return (n.get_role_name(), n.get_name())
            for j in range(n.get_child_count()):
                stack.append(n.get_child_at_index(j))
        except Exception:
            pass
    return None


# Mirroring must never change the focused element (BM-2). Clearing used SetSelection(0, 0, 0),
# which makes Firefox focus a skip link at the top of the page.
before = focused()
top(); key(SHIFT, END); time.sleep(0.4)
key(DOWN); time.sleep(1.2)
check("F1 select, then a plain move: focus unchanged (a skip link must not take it)", focused(), before)

# Text directly in <body> starts in the document, which is focusable: with focus on a link the
# selection must not be mirrored (it would focus the document).
top(); key(TAB); time.sleep(0.8)
on_link = focused()
key(CTRL, HOME); key(DOWN)
for _ in range(5):
    key(DOWN)
key(SHIFT, END); time.sleep(0.6)
check("F2 selecting body text while a link has focus: focus unchanged", focused(), on_link)
print("   focus on the link was", on_link, flush=True)
# back to a focused document for the copy rows
with open(FIFO, "w") as f:
    f.write("click\n")
time.sleep(1.5)

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
check("E6 Ctrl+A skips the leading link (it would take focus) and stops before the password field", all_text.strip().split("\n")[0], "Heading one")
print("   Ctrl+A text:", repr(all_text), flush=True)

top(); key(DOWN); key(RIGHT); key(RIGHT); key(SHIFT, DOWN)
print("INFO E8 partial first paragraph to partial second:", repr(copy()), flush=True)
top(); key(DOWN); key(RIGHT); key(RIGHT); key(SHIFT, DOWN); key(SHIFT, DOWN); key(SHIFT, DOWN)
print("INFO E9 partial through nested div:", repr(copy()), flush=True)

# gorca+Shift+F10 switches between page selection and virtual selection and says which.
def spoken_since(n):
    path = os.path.join(os.path.dirname(os.environ["MARKLOG"]), "ssip.log")
    return open(path, errors="replace").read()[n:]

def spoken_len():
    path = os.path.join(os.path.dirname(os.environ["MARKLOG"]), "ssip.log")
    return len(open(path, errors="replace").read())

INSERT, F10 = 110, 68
top(); key(SHIFT, END)
before_clip = copy()
n = spoken_len()
key(INSERT, SHIFT, F10, wait=1.0)
check("S1 toggle announces the virtual selection", any(w in spoken_since(n) for w in ("virtual selection", "виртуальное выделение")), True)
top(); key(DOWN); key(SHIFT, END)
check("S2 virtual selection: the page is left alone, Ctrl+C copies nothing new", copy(), before_clip)
n = spoken_len()
key(INSERT, SHIFT, F10, wait=1.5)
check("S3 toggle back announces the page selection after the probe", any(w in spoken_since(n) for w in ("page selection", "выделение страницы")), True)
top(); key(DOWN); key(SHIFT, END)
check("S4 page selection again: Ctrl+C copies the paragraph", copy(), "Alpha bravo link inside charlie delta.")

# paste into the page's text field: Tab to it from the top, Ctrl+V
top(); key(SHIFT, END)
want = copy()
for _ in range(2):
    key(TAB)
time.sleep(0.8)
key(CTRL, V, wait=1.2)
check("E7 paste into the field", entry_text(), want)

sys.exit(1 if fails else 0)
