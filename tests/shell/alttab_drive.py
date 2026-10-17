#!/usr/bin/python3
"""Alt+Tab repro driver (run inside tests/shell/run_shell.sh, with gorca already running).

Spawns two GTK windows (a gtk3 probe window and gedit), gives the Shell some activity, then performs
real Alt+Tab switches through Mutter's RemoteDesktop virtual keyboard: held (switcher shown for a
while) and quick (Alt+Tab released at once), forward and backward. Prints the marks to stderr.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import probe_shell as P  # noqa: E402

ALT, TAB, SHIFT = 56, 15, 42


class D(P.Probe):
    def __init__(self, out):
        self.out = out
        os.makedirs(out, exist_ok=True)
        self.sess = P.Peer(P.SESSION)
        self.clients = []
        self.rd = None


def main():
    out = sys.argv[1]
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 2
    d = D(out)
    print("RD", d.rd_start(), file=sys.stderr, flush=True)
    time.sleep(3)
    d.eval("Main.overview.hide(); 1")
    time.sleep(1)
    d.activity()
    d.spawn_client(["/usr/bin/python3", os.path.join(os.path.dirname(os.path.abspath(__file__)), "gtkwin.py"), "Probe Window One"])
    d.spawn_client(["/usr/bin/gedit", "--standalone"])
    for _ in range(180):
        w = d.windows()
        if isinstance(w, list) and len(w) >= 2:
            break
        time.sleep(0.5)
    print("WINDOWS", d.windows(), file=sys.stderr, flush=True)
    d.click()
    time.sleep(3)

    def mark(s):
        print("MARK", time.monotonic(), s, d.windows(), file=sys.stderr, flush=True)

    for i in range(rounds):
        mark("round %d: held Alt+Tab" % i)
        d.key(ALT, True)
        time.sleep(0.15)
        d.tap(TAB)
        time.sleep(1.5)
        d.key(ALT, False)
        time.sleep(3)
        mark("round %d: quick Alt+Tab" % i)
        d.key(ALT, True)
        d.tap(TAB, gap=0.01)
        d.key(ALT, False)
        time.sleep(3)
        mark("round %d: Alt+Shift+Tab" % i)
        d.key(ALT, True)
        d.key(SHIFT, True)
        d.tap(TAB, gap=0.02)
        d.key(SHIFT, False)
        time.sleep(1.0)
        d.key(ALT, False)
        time.sleep(3)
    # Ctrl+Alt+Tab: the Shell's accessibility switcher over the top bar, the dash, the windows.
    for i in range(rounds):
        mark("round %d: Ctrl+Alt+Tab" % i)
        d.key(29, True)
        d.key(ALT, True)
        time.sleep(0.1)
        for _ in range(4):
            d.tap(TAB, gap=0.03)
            time.sleep(1.2)
        d.key(ALT, False)
        d.key(29, False)
        time.sleep(1.5)
        d.tap(1)  # Escape, in case the popup stayed
        time.sleep(1)
    mark("done")
    for c in d.clients:
        c.terminate()
    time.sleep(1)


main()
