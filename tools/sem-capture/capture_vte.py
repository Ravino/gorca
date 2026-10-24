#!/usr/bin/python3
"""sem-capture, part 3b: a real VTE (gnome-terminal) in the private Shell world (family `vte`).

Runs INSIDE `tests/shell/run_shell.sh` (private session bus, private a11y bus, headless Shell). It
starts gnome-terminal-server on the private session bus, one window with a bash prompt, types into
it through Mutter's RemoteDesktop virtual keyboard and records the terminal's AT-SPI tree after
each step.

  tests/shell/run_shell.sh -- /usr/bin/python3 -I tools/sem-capture/capture_vte.py \
      --out tests/fixtures/sem/vte

Teardown signals only the PIDs started here. Never touches the live session.
"""
import argparse
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "..", "tests", "shell"))
import capture as C  # noqa: E402
import capture_more as M  # noqa: E402
import probe_shell as P  # noqa: E402

# US layout evdev codes
KEYS = {c: k for c, k in zip("1234567890", range(2, 12))}
KEYS.update({c: k for c, k in zip("qwertyuiop", range(16, 26))})
KEYS.update({c: k for c, k in zip("asdfghjkl", range(30, 39))})
KEYS.update({c: k for c, k in zip("zxcvbnm", range(44, 51))})
KEYS.update({" ": 57, "\n": 28, "-": 12, ".": 52, "/": 53})
CTRL, SHIFT, ALT = 29, 42, 56


class Drv(P.Probe):
    def __init__(self):
        self.out = os.environ["GORCA_HARNESS_DIR"]
        self.sess = P.Peer(P.SESSION)
        self.clients = []
        self.rd = None

    def type_text(self, text):
        for ch in text:
            self.tap(KEYS[ch], gap=0.03)
            time.sleep(0.06)

    def chord(self, *codes):
        for c in codes:
            self.key(c, True)
            time.sleep(0.03)
        for c in reversed(codes):
            self.key(c, False)
            time.sleep(0.03)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    bus, _ = M.private_bus()
    drv = Drv()
    print("RD", drv.rd_start(), file=sys.stderr)
    time.sleep(2)
    env = {k: os.environ[k] for k in ("PATH", "HOME", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS",
                                      "AT_SPI_BUS_ADDRESS", "GSETTINGS_BACKEND", "XDG_CONFIG_HOME",
                                      "XDG_DATA_HOME", "XDG_CACHE_HOME", "WAYLAND_DISPLAY",
                                      "GORCA_HARNESS_DIR") if k in os.environ}
    env.update(GDK_BACKEND="wayland", GTK_A11Y="atspi", LANG="C.UTF-8", PS1="$ ")
    log = open(os.path.join(os.environ["GORCA_HARNESS_DIR"], "vte.log"), "ab")
    # Every shell in the private HOME gets the neutral prompt (no user name or path in a fixture).
    with open(os.path.join(os.environ["HOME"], ".bashrc"), "w") as f:
        f.write("PS1='$ '\n")
    procs = []
    try:
        procs.append(subprocess.Popen(["/usr/libexec/gnome-terminal-server", "--app-id", "org.gnome.Terminal"],
                                      env=env, stdout=log, stderr=log, start_new_session=True))
        time.sleep(2)
        procs.append(subprocess.Popen(["gnome-terminal", "--app-id", "org.gnome.Terminal", "--window",
                                       "--geometry=80x24", "--", "env", "PS1=$ ", "bash", "--norc",
                                       "--noprofile", "-i"], env=env, stdout=log, stderr=log,
                                      start_new_session=True))
        found = None
        for _ in range(60):
            found = C.find_app(bus, "org.gnome.Terminal")
            if found:
                break
            time.sleep(0.5)
        if not found:
            print(open(log.name).read()[-1500:], file=sys.stderr)
            kids = bus.call("org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root", C.ACC, "GetChildren")
            print("registry children:", [(k, bus.prop(k[0], k[1], C.ACC, "Name")) for k in (kids or [])],
                  file=sys.stderr)
            print("procs:", [pr.poll() for pr in procs], file=sys.stderr)
            sys.exit("gnome-terminal never appeared on the private a11y bus")
        d, p, appname = found
        time.sleep(3)
        n = C.quiesce(bus, d, p, need=2, step=0.7, deadline=30)
        version = C.pkg_version("gnome-terminal") + " / " + C.pkg_version("libvte-2.91-0")
        # Give the terminal window the keyboard focus (needs the Shell's --unsafe-mode Eval).
        r = drv.eval("let w = global.get_window_actors().map(a => a.meta_window).find(w => /terminal/i.test("
                     "w.get_wm_class() || '')); if (w) { w.activate(global.get_current_time()); 1 } else { 0 }")
        print("activate", r, file=sys.stderr)
        time.sleep(1.5)
        # A click into the window clears the Shell's key focus; the first key after it is lost,
        # so a Shift tap goes first.
        S = "org.gnome.Mutter.RemoteDesktop.Session"
        rd = "org.gnome.Mutter.RemoteDesktop"
        drv.sess.call(rd, drv.rd, S, "NotifyPointerMotionRelative", (640.0, 400.0), "(dd)")
        drv.sess.call(rd, drv.rd, S, "NotifyPointerButton", (272, True), "(ib)")
        drv.sess.call(rd, drv.rd, S, "NotifyPointerButton", (272, False), "(ib)")
        time.sleep(1.0)
        drv.tap(SHIFT)
        time.sleep(0.5)

        def snap(name):
            time.sleep(1.0)
            C.quiesce(bus, d, p, need=2, step=0.5, deadline=15)
            nodes = C.walk_subtree(bus, d, p)
            M.write_page(a.out, name, M.header("vte", version, appname, name, "", nodes, len(nodes)), nodes)
            print("vte/%s: %d nodes" % (name, len(nodes)), file=sys.stderr)

        snap("prompt")
        drv.type_text("echo hello world")
        snap("typed")
        drv.tap(28)
        time.sleep(0.5)
        drv.type_text("seq 1 40\n")
        snap("output")
        drv.chord(CTRL, SHIFT, KEYS["t"])
        time.sleep(1.5)
        snap("two-tabs")
        drv.chord(CTRL, SHIFT, KEYS["f"])
        time.sleep(1.0)
        snap("find-bar")
        drv.tap(1)  # Escape
        time.sleep(0.5)
        drv.chord(CTRL, 104)  # Ctrl+PageUp: back to the first tab
        snap("tab-switch")
    finally:
        for pr in reversed(procs):
            try:
                pr.terminate()  # only the PIDs this run started
            except OSError:
                pass
        for c in drv.clients:
            c.terminate()


if __name__ == "__main__":
    main()
