#!/usr/bin/python3
"""Telegram A/K fixture driver: real keys through the private compositor, gorca speech from the mock SSIP log.

Run through run.sh (private headless GNOME Shell world, tests/shell/run_shell.sh). Env: TG_SCEN (scenario name),
TG_BROWSER (firefox|chromium, only to pick the AT-SPI app name), RDFIFO, MARKLOG (set by selcopy/inner.sh).
Every step prints one row: label, keys, focused element, what gorca said (list). Scenario verdict lines start
with PASS / FAIL / NOTE so a run can be grepped.
"""
import os, re, sys, time
import gi
gi.require_version("Atspi", "2.0")
from gi.repository import Atspi

FIFO = os.environ["RDFIFO"]
OUT = os.path.dirname(os.environ["MARKLOG"])
SCEN = os.environ.get("TG_SCEN", "list")
BROWSER = os.environ.get("TG_BROWSER", "firefox")
DOWN, UP, RIGHT, LEFT, HOME, END, TAB, ESC, ENTER = 108, 103, 106, 105, 102, 107, 15, 1, 28
SHIFT, CTRL, INSERT, SPACE, PGUP, PGDN = 42, 29, 110, 57, 104, 109
L = {"a": 30, "b": 48, "h": 35, "i": 23, "k": 37, "t": 20, "e": 18, "s": 31, "x": 45, "1": 2}
T0 = time.monotonic()
fails = 0


def key(*codes, wait=0.45):
    open(os.environ["MARKLOG"], "a").write(f"{time.monotonic():.3f} KEY {codes}\n")
    with open(FIFO, "w") as f:
        f.write("key " + " ".join(map(str, codes)) + "\n")
    time.sleep(wait)


def log_text():
    return open(os.path.join(OUT, "ssip.log"), errors="replace").read()


def spoken_len():
    return len(log_text())


def spoken_since(n):
    return [l.split(" SPEAK ", 1)[-1] for l in log_text()[n:].splitlines() if l.strip()]


def app_root():
    desk = Atspi.get_desktop(0)
    want = "irefox" if BROWSER == "firefox" else "hrom"
    for i in range(desk.get_child_count()):
        app = desk.get_child_at_index(i)
        if app and want in (app.get_name() or ""):
            return app


def walk(pred, limit=6000):
    app = app_root()
    stack = [app] if app else []
    seen = 0
    while stack and seen < limit:
        n = stack.pop()
        seen += 1
        try:
            if pred(n):
                return n
            for j in range(n.get_child_count() - 1, -1, -1):
                stack.append(n.get_child_at_index(j))
        except Exception:
            pass


def focused():
    n = walk(lambda n: n.get_state_set().contains(Atspi.StateType.FOCUSED) and n.get_role_name() not in ("frame", "application", "window"))
    return (n.get_role_name(), (n.get_name() or "")[:40]) if n else None


def doc_title():
    d = walk(lambda n: n.get_role_name() == "document web")
    return d.get_name() if d else None


def count_rows():
    """How many chat rows exist in the page's a11y tree right now (the windowed list)."""
    c = [0]

    def p(n):
        try:
            if n.get_role_name() in ("push button", "link") and (n.get_name() or "").strip():
                c[0] += 1
        except Exception:
            pass
        return False
    walk(p)
    return c[0]


def step(label, *codes, wait=0.45, settle=0.35, show=True):
    n = spoken_len()
    if codes:
        key(*codes, wait=wait)
    time.sleep(settle)
    said = spoken_since(n)
    if show:
        print(f"[{time.monotonic()-T0:6.1f}] {label:22s} focus={focused()} said={said}", flush=True)
    return said


def verdict(ok, label, detail=""):
    global fails
    fails += 0 if ok else 1
    print(f"{'PASS' if ok else 'FAIL'} {label} {detail}", flush=True)


def note(label, detail=""):
    print(f"NOTE {label} {detail}", flush=True)


def nums(said, pat=r"Message (\d+)"):
    out = []
    for s in said:
        m = re.search(pat, s)
        if m:
            out.append(int(m.group(1)))
    return out


def open_first_chat():
    """Tab to the first chat row, Enter. The page focuses the composer 150 ms later."""
    step("tab 1", TAB, wait=0.8)
    step("tab 2", TAB, wait=0.8)
    step("tab 3", TAB, wait=0.8)
    return step("enter", ENTER, wait=2.0)


# ---------------------------------------------------------------------------- warm-up
key(SHIFT, wait=0.6)
print("start focus=", focused(), "rows in tree:", count_rows(), flush=True)

if SCEN == "login":
    for i in range(6):
        step(f"down {i+1}", DOWN)
    step("h", L["h"])
    step("k", L["k"])

elif SCEN == "echo":
    # control page echo.html (APP=E, ?k=input|textarea|ce|cerole): type h i, Enter (clears), h i, Backspace
    step("click-focus", wait=0.2, settle=1.0)
    step("type h", L["h"], wait=0.7)
    step("type i", L["i"], wait=0.7)
    step("enter", ENTER, wait=1.0)
    step("type h", L["h"], wait=0.7)
    step("type i", L["i"], wait=0.7)
    step("backspace", 14, wait=0.8)

elif SCEN == "list":
    # browse the chat list without opening a chat; then Enter on a row in browse mode
    for i in range(8):
        step(f"down {i+1}", DOWN)
    for i in range(3):
        step(f"up {i+1}", UP)
    step("enter (browse)", ENTER, wait=2.0)
    step("esc", ESC, wait=1.0)
    step("down", DOWN)

elif SCEN == "chat":
    # the owner flow: Tab to a chat, Enter, read the messages, Escape, Tab back
    open_first_chat()
    step("type hi", L["h"], L["i"], wait=0.6)
    step("enter (send)", ENTER, wait=1.2)
    step("esc 1", ESC, wait=1.0)
    for i in range(6):
        step(f"up {i+1}", UP)
    step("home", HOME)
    step("end", END)
    step("esc 2", ESC, wait=1.5)
    step("tab", TAB, wait=0.8)
    step("tab", TAB, wait=0.8)
    step("enter again", ENTER, wait=2.0)

elif SCEN == "scrollup":
    # a long run of Up through the history: prepend of older messages must not jump the cursor
    open_first_chat()
    step("esc 1", ESC, wait=1.0)
    allsaid = []
    for i in range(150):
        s = step(f"up {i+1}", UP, wait=0.3, settle=0.2, show=(i < 2 or i % 25 == 0))
        if i % 25 == 0:
            print("   page:", doc_title(), flush=True)
        allsaid += s
    seq = nums(allsaid)
    print("sequence:", seq, flush=True)
    steps = [b - a for a, b in zip(seq, seq[1:])]
    bad = [(i, a, b) for i, (a, b) in enumerate(zip(seq, seq[1:])) if b - a != -1]
    verdict(len(seq) >= 45, "scrollup spoke message numbers across the prepend (about 3 stops per message)", f"{len(seq)}/150")
    verdict(not bad, "scrollup never skipped, repeated or jumped", f"anomalies={bad[:6]}")

elif SCEN == "scrollprobe":
    # What does Component.ScrollTo do on this browser? (a2 root-cause probe, bypasses gorca)
    open_first_chat()
    step("esc 1", ESC, wait=1.0)
    print("page before:", doc_title(), flush=True)

    def cand(n):
        try:
            if (n.get_name() or "").startswith("Message 3"):
                return True
            if n.get_child_count() == 0:
                t = n.get_text_iface()
                if t:
                    return (Atspi.Text.get_text(n, 0, 20) or "").startswith("Message 3")
            return False
        except Exception:
            return False
    seen = []

    def dump(n):
        try:
            t = n.get_text_iface()
            tx = Atspi.Text.get_text(n, 0, 30) if t else None
            if "Message 39" in (n.get_name() or "") or "Message 39" in (tx or ""):
                seen.append((n.get_role_name(), n.get_name(), tx, n.get_child_count()))
        except Exception as e:
            seen.append(("ERR", str(e)))
        return False
    walk(dump, limit=60000)
    print("NOTE probe seen", seen[:8], flush=True)
    tgt = walk(cand, limit=60000)
    if tgt is None:
        print("NOTE probe no node named Message 3*", flush=True)
    else:
        c = tgt.get_component_iface()
        for ty in (Atspi.ScrollType.TOP_LEFT, Atspi.ScrollType.ANYWHERE, Atspi.ScrollType.TOP_EDGE):
            ok = c.scroll_to(ty)
            time.sleep(0.6)
            print("NOTE probe", tgt.get_role_name(), repr(tgt.get_name())[:40], ty, "->", ok, doc_title(), flush=True)
        kid = tgt.get_child_at_index(0) if tgt.get_child_count() else None
        if kid is not None:
            ok = kid.get_component_iface().scroll_to(Atspi.ScrollType.TOP_LEFT)
            time.sleep(0.6)
            print("NOTE probe child", kid.get_role_name(), "->", ok, doc_title(), flush=True)

elif SCEN == "scrollshort":
    # a2 quick check: 40 Up presses must move the page (scrollTop) and load older messages (lo)
    open_first_chat()
    step("esc 1", ESC, wait=1.0)
    p0 = doc_title()
    for i in range(40):
        step(f"up {i+1}", UP, wait=0.3, settle=0.1, show=False)
    time.sleep(0.5)
    p1 = doc_title()
    print("page before:", p0, "after:", p1, flush=True)
    verdict(p0 != p1, "scrollshort: the page followed the browse cursor", f"{p0} -> {p1}")

elif SCEN == "listscroll":
    # Down through more rows than the windowed chat list renders (22 of 300)
    allsaid = []
    for i in range(120):
        allsaid += step(f"down {i+1}", DOWN, wait=0.3, settle=0.2, show=(i % 12 == 0))
    names = [s for s in allsaid]
    print("rows spoken:", names[-8:], flush=True)
    last = names[-1] if names else ""
    verdict(any(k in " ".join(names[-5:]) for k in ("Mallory 2", "Niaj 2", "Olivia 2", "Peggy 2", "Rupert 2", "Alice 3", "Bob 3", "Carol 3", "Dave 3")),
            "listscroll reached chats beyond the first window", f"last={last!r} rows_in_tree={count_rows()}")

elif SCEN == "incoming":
    # flags: f=incoming,typing: messages arrive while the cursor rests on the last message
    open_first_chat()
    step("esc 1", ESC, wait=1.0)
    step("end", END)
    step("up", UP)
    n = spoken_len()
    time.sleep(14)
    said = spoken_since(n)
    print("silent window 14 s, gorca said:", said, flush=True)
    note("incoming-count", f"{len(said)} utterances in 14 s")
    step("down", DOWN)
    step("down", DOWN)
    step("down", DOWN)

elif SCEN == "flood":
    open_first_chat()
    step("esc 1", ESC, wait=1.0)
    step("end", END)
    n = spoken_len()
    time.sleep(12)
    said = spoken_since(n)
    print("flood window 12 s, gorca said", len(said), "utterances:", said[:8], flush=True)
    note("flood-count", f"{len(said)}")
    for i in range(4):
        step(f"up {i+1}", UP)

elif SCEN == "churn":
    # f=churn: the list re-sorts every 2 s while the cursor rests on a row
    for i in range(28):
        step(f"down {i+1}", DOWN, wait=0.3, settle=0.15, show=(i % 9 == 0))
    n = spoken_len()
    time.sleep(7)
    said = spoken_since(n)
    print("idle 7 s with churn, gorca said:", said, flush=True)
    note("churn-idle-utterances", f"{len(said)}")
    step("down", DOWN)
    step("up", UP)
    step("enter", ENTER, wait=2.0)

elif SCEN == "typing":
    open_first_chat()
    n = spoken_len()
    time.sleep(8)
    said = spoken_since(n)
    print("typing flag 8 s in focus mode, said:", said, flush=True)
    note("typing-utterances", f"{len(said)}")

else:
    print("unknown scenario", SCEN)
    sys.exit(2)

print("DONE fails=%d" % fails, flush=True)
sys.exit(1 if fails else 0)
