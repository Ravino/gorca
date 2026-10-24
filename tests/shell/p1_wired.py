#!/usr/bin/python3
"""P1: keypress-to-speech check of the wired focus and change path (M22b1 cut-over).

Run inside the private Shell world (never the live session):

    tests/shell/run_p1.sh OUTDIR [GORCA_BINARY]

What happens: a minimal SSIP server records every SPEAK; the gorca daemon under test runs against
the private a11y bus and owns the Orca keyboard-monitor name; real keys are sent through Mutter's
RemoteDesktop virtual keyboard; the transcript of what was spoken after each key is checked against
the words the cut-over goldens and the gate promise. Targets: a real GTK4 window, real Firefox on a
local page, and the real GNOME Shell (quick settings). Every target covers a focus and a change.
Output: OUTDIR/p1.json (all steps), OUTDIR/p1.md (the table), OUTDIR/daemon.log. Exit 1 on a
failed expectation.
"""
import json
import os
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import probe_shell as P  # noqa: E402  (refuses to run outside a private session)

TAB, ENTER, SPACE, RIGHT, LEFT, ALT, CTRL, META, ESC, DOWN, S = 15, 28, 57, 106, 105, 56, 29, 125, 1, 108, 31
HOME = 102
KEY_T, KEY_H, KEY_K = 20, 35, 37  # browse-mode quick navigation: table, heading, link


class Ssip:
    """Minimal SSIP server: accepts every verb, logs SPEAK texts and cancels with a monotonic time."""

    def __init__(self, path):
        self.path = path
        self.events = []  # (t, kind, text)
        self.lock = threading.Lock()
        self.n = 0
        srv = socket.socket(socket.AF_UNIX)
        if os.path.exists(path):
            os.unlink(path)
        srv.bind(path)
        srv.listen(8)
        self.srv = srv
        threading.Thread(target=self.accept, daemon=True).start()

    def accept(self):
        while True:
            try:
                c, _ = self.srv.accept()
            except OSError:
                return
            threading.Thread(target=self.serve, args=(c,), daemon=True).start()

    def log(self, kind, text=""):
        with self.lock:
            self.events.append((time.monotonic(), kind, text))

    def serve(self, c):
        f = c.makefile("rwb", buffering=0)
        wl = threading.Lock()
        notify = [False]
        data = None

        def send(b):
            with wl:
                f.write(b)

        def event(code, mid, word):
            send(("%d-%d\r\n%d-1\r\n%d %s\r\n" % (code, mid, code, code, word)).encode())

        def speak_done(mid):
            # A short utterance: BEGIN at once, END 120 ms later (the daemon waits for them).
            if notify[0]:
                event(701, mid, "BEGIN")
                time.sleep(0.12)
                event(702, mid, "END")

        for raw in f:
            line = raw.decode("utf-8", "replace").rstrip("\r\n")
            if data is not None:
                if line == ".":
                    self.n += 1
                    mid = self.n
                    self.log("SPEAK", "\n".join(data))
                    send(b"225-%d\r\n225 OK MESSAGE QUEUED\r\n" % mid)
                    threading.Thread(target=speak_done, args=(mid,), daemon=True).start()
                    data = None
                else:
                    data.append(line)
                continue
            cmd = line.split(" ")[0].upper()
            if cmd == "SPEAK":
                data = []
                send(b"230 OK RECEIVING DATA\r\n")
            elif cmd in ("STOP", "CANCEL"):
                self.log(cmd)
                send(b"210 OK STOPPED\r\n")
            elif cmd in ("CHAR", "KEY"):
                self.n += 1
                self.log("SPEAK", line.split(" ", 1)[1] if " " in line else "")
                send(b"225-%d\r\n225 OK MESSAGE QUEUED\r\n" % self.n)
                threading.Thread(target=speak_done, args=(self.n,), daemon=True).start()
            elif cmd == "QUIT":
                send(b"231 HAPPY HACKING\r\n")
                return
            elif cmd == "SET" and "NOTIFICATION" in line.upper():
                notify[0] = line.upper().rstrip().endswith(" ON")
                send(b"220 OK NOTIFICATION SET\r\n")
            else:
                send(b"200 OK\r\n")

    def mark(self):
        with self.lock:
            return len(self.events)

    def speech_since(self, mark):
        with self.lock:
            return [t for (_, k, t) in self.events[mark:] if k == "SPEAK"]

    def timed_since(self, mark):
        with self.lock:
            return [(t, x) for (t, k, x) in self.events[mark:] if k == "SPEAK"]

    def last_time(self):
        with self.lock:
            return self.events[-1][0] if self.events else 0.0


class Rig:
    def __init__(self, out, gorca):
        self.out = out
        self.sess = P.Peer(P.SESSION)
        self.rd = None
        self.ssip = Ssip(os.path.join(out, "ssip.sock"))
        self.gorca = gorca
        self.daemon = None
        self.apps = []
        self.steps = []

    # ---- the reader under test -----------------------------------------------------------
    def start_daemon(self):
        log = open(os.path.join(self.out, "daemon.log"), "wb")
        env = dict(os.environ, GORCA_LOG=os.environ.get("P1_LOG", "warn,gorca_core::present=info"), NO_COLOR="1")
        self.daemon = subprocess.Popen(
            [self.gorca, "--speech=ssip", "--ssip-socket=" + self.ssip.path, "--impersonate-orca",
             "--no-pointer", "--no-probe", "--config=" + os.path.join(self.out, "gorca.toml")],
            env=env, stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)

    # ---- Mutter RemoteDesktop keyboard ----------------------------------------------------
    def rd_start(self):
        r = self.sess.call("org.gnome.Mutter.RemoteDesktop", "/org/gnome/Mutter/RemoteDesktop",
                           "org.gnome.Mutter.RemoteDesktop", "CreateSession")
        if P.iserr(r):
            sys.exit("RemoteDesktop CreateSession failed: %r" % (r,))
        self.rd = r[0]
        s = self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, "org.gnome.Mutter.RemoteDesktop.Session", "Start")
        if P.iserr(s):
            sys.exit("RemoteDesktop Start failed: %r" % (s,))

    def key(self, code, down):
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, "org.gnome.Mutter.RemoteDesktop.Session",
                       "NotifyKeyboardKeycode", (code, down), "(ub)")

    def tap(self, *codes, gap=0.04):
        for c in codes:
            self.key(c, True)
            time.sleep(gap)
        for c in reversed(codes):
            self.key(c, False)
            time.sleep(gap)

    def click(self, x=640.0, y=420.0):
        S_ = "org.gnome.Mutter.RemoteDesktop.Session"
        # Relative motion only: clamp to the top-left corner first, then move to (x, y).
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S_, "NotifyPointerMotionRelative",
                       (-5000.0, -5000.0), "(dd)")
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S_, "NotifyPointerMotionRelative", (x, y), "(dd)")
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S_, "NotifyPointerButton", (272, True), "(ib)")
        self.sess.call("org.gnome.Mutter.RemoteDesktop", self.rd, S_, "NotifyPointerButton", (272, False), "(ib)")

    def eval(self, js):
        # org.gnome.Shell.Eval of the PRIVATE Shell (run_shell.sh starts it with --unsafe-mode); the
        # strings are this file's own constants, never external input.
        return self.sess.call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "Eval", (js,), "(s)", 5000)

    def windows(self):
        r = self.eval("JSON.stringify(global.get_window_actors().map(a=>({t:a.meta_window.get_title(),"
                      "app:a.meta_window.get_wm_class(),f:a.meta_window.has_focus()})))")
        return r[1] if r and r[0] is True else r

    def click_window(self, title):
        """Click the middle of the window whose title contains `title` (frame rect from the Shell)."""
        r = self.eval("JSON.stringify(global.get_window_actors().map(a=>{let r=a.meta_window.get_frame_rect();"
                      "return {t:a.meta_window.get_title(),x:r.x,y:r.y,w:r.width,h:r.height}}))")
        try:
            for w in json.loads(r[1]):
                if title.lower() in (w["t"] or "").lower():
                    self.click(w["x"] + w["w"] / 2.0, w["y"] + w["h"] / 2.0)
                    return True
        except (TypeError, ValueError, KeyError, IndexError):
            pass
        return False

    def focus_title(self):
        r = self.eval("global.display.focus_window ? global.display.focus_window.get_title() : ''")
        return r[1] if r and r[0] is True else ""

    # ---- apps -----------------------------------------------------------------------------
    def spawn(self, argv, name, extra_env=None):
        env = dict(os.environ)
        env["GDK_BACKEND"] = "wayland"
        env["MOZ_ENABLE_WAYLAND"] = "1"
        env.update(extra_env or {})
        log = open(os.path.join(self.out, "app-%s.log" % name), "ab")
        p = subprocess.Popen(argv, env=env, stdout=log, stderr=log, stdin=subprocess.DEVNULL,
                             start_new_session=True)
        self.apps.append(p)
        return p

    def stop(self, p):
        """Only a process group this rig started (setsid => pgid == pid)."""
        if p is None or p.poll() is not None:
            return
        try:
            os.killpg(p.pid, signal.SIGTERM)
        except ProcessLookupError:
            return
        for _ in range(40):
            if p.poll() is not None:
                return
            time.sleep(0.2)
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    # ---- steps ----------------------------------------------------------------------------
    def settle(self, quiet=1.2, longest=8.0, shortest=0.8):
        t0 = time.monotonic()
        while True:
            time.sleep(0.15)
            now = time.monotonic()
            last = self.ssip.last_time()
            if now - t0 >= longest:
                return
            if now - t0 >= shortest and now - max(last, t0) >= quiet:
                return

    def step(self, target, name, act, expect=(), forbid=(), longest=8.0, _since=None):
        m = self.ssip.mark() if _since is None else _since
        t0 = time.monotonic()
        act()
        if _since is None:
            self.settle(longest=longest)
        said = self.ssip.speech_since(m)
        text = " | ".join(said).lower()
        missing = [w for w in expect if w.lower() not in text]
        bad = [w for w in forbid if w.lower() in text]
        ok = not missing and not bad and bool(said)
        rec = {"target": target, "step": name, "spoken": said, "ok": ok, "missing": missing,
               "forbidden": bad, "s": round(time.monotonic() - t0, 2),
               "at_ms": [round((t - t0) * 1000) for (t, _) in self.ssip.timed_since(m)]}
        self.steps.append(rec)
        print("%-8s %-22s %s %r @%r%s" % (target, name, "PASS" if ok else "FAIL", said, rec["at_ms"],
                                      "" if ok else "  missing=%r forbidden=%r" % (missing, bad)),
              file=sys.stderr, flush=True)
        return rec

    def tab_to(self, target, name, word, expect, tries=4):
        """Tab until `word` is spoken (the very first Tab after a click can be swallowed by the
        toolkit); the step is the Tab that got there, earlier misses are listed in `skipped`."""
        skipped = 0
        for _ in range(tries):
            m = self.ssip.mark()
            self.tap(TAB)
            self.settle()
            if word in " ".join(self.ssip.speech_since(m)).lower():
                break
            skipped += 1
        rec = self.step(target, name, lambda: None, expect=expect, _since=m)
        rec["skipped_tabs"] = skipped
        return rec

    def write(self):
        with open(os.path.join(self.out, "p1.json"), "w") as f:
            json.dump({"steps": self.steps, "load": open("/proc/loadavg").read().strip(),
                       "governor": _gov(), "reader": self.gorca,
                       "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}, f, indent=1)
        with open(os.path.join(self.out, "p1.md"), "w") as f:
            f.write("| target | step | result | spoken |\n|---|---|---|---|\n")
            for s in self.steps:
                f.write("| %s | %s | %s | %s |\n" % (s["target"], s["step"], "PASS" if s["ok"] else "FAIL",
                                                     " / ".join(s["spoken"]).replace("|", "\\|")))


def _gov():
    try:
        return open("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").read().strip()
    except OSError:
        return "n/a"


def wait_window(rig, title, timeout=60.0):
    """Wait for the window to be mapped, click it once (a headless Shell has no keyboard focus
    until something is clicked), then wait for it to hold the focus."""
    end = time.time() + timeout
    clicked = False
    while time.time() < end:
        w = rig.windows()
        if isinstance(w, str) and title.lower() in w.lower():
            if title.lower() in rig.focus_title().lower():
                return True
            if not clicked:
                time.sleep(2.0)
                rig.click_window(title)
                clicked = True
        time.sleep(0.5)
    return False


def gtk4(rig):
    p = rig.spawn(["/usr/bin/python3", os.path.join(HERE, "p1_gtk4.py")], "gtk4")
    got = wait_window(rig, "P1 Wired")
    print("gtk4 window focused:", got, rig.windows(), file=sys.stderr, flush=True)
    time.sleep(3.0)
    rig.settle()
    # The click that activated the window moved the focus to the check button (spoken above).
    rig.tab_to("gtk4", "focus: expander", "details", ("details", "button"))
    rig.step("gtk4", "change: Space expands", lambda: rig.tap(SPACE), expect=("expanded",))
    rig.tab_to("gtk4", "focus: scale", "volume", ("volume", "slider"))
    rig.step("gtk4", "change: Right sets the value", lambda: rig.tap(RIGHT), expect=("60",))
    rig.tab_to("gtk4", "focus: toggle button", "mute", ("mute", "toggle button", "not pressed"))
    rig.step("gtk4", "change: Space presses", lambda: rig.tap(SPACE), expect=("pressed",), forbid=("not pressed",))
    rig.tab_to("gtk4", "focus: check button", "subscribe", ("subscribe", "check box", "not checked"))
    rig.step("gtk4", "change: Space checks", lambda: rig.tap(SPACE), expect=("checked",), forbid=("not checked",))
    rig.stop(p)
    return got


def firefox(rig):
    page = os.path.join(HERE, "p1_page.html")
    prof = tempfile.mkdtemp(prefix="p1prof.")
    with open(os.path.join(prof, "user.js"), "w") as f:
        f.write('user_pref("browser.shell.checkDefaultBrowser", false);\n'
                'user_pref("browser.startup.homepage_override.mstone","ignore");\n'
                'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
                'user_pref("browser.aboutwelcome.enabled", false);\n'
                'user_pref("browser.tabs.warnOnClose", false);\n')
    p = rig.spawn(["firefox-esr", "--no-remote", "--new-instance", "-profile", prof, "--width", "900",
                   "--height", "700", "file://" + page], "firefox")
    got = wait_window(rig, "P1 Page", 90)
    time.sleep(8.0)
    rig.click_window("P1 Page")
    rig.settle(longest=10)
    # Browse mode (M22b2): the browse cursor and quick navigation through the wired presenter.
    rig.step("firefox", "browse: Ctrl+Home reads the first line", lambda: rig.tap(CTRL, HOME), expect=("heading",))
    rig.step("firefox", "browse: Down reads a line", lambda: rig.tap(DOWN), expect=("intro",))
    rig.step("firefox", "quicknav: k lands on a link", lambda: rig.tap(KEY_K), expect=("alpha", "link"))
    rig.step("firefox", "quicknav: t lands on the table", lambda: rig.tap(KEY_T), expect=("table",))
    rig.step("firefox", "browse: Ctrl+Home returns to the top", lambda: rig.tap(CTRL, HOME), expect=("heading",))
    rig.step("firefox", "quicknav: h lands on a heading", lambda: rig.tap(KEY_H), expect=("heading",))
    rig.tab_to("firefox", "focus: check box", "subscribe", ("subscribe", "check box"))
    rig.step("firefox", "change: Space checks", lambda: rig.tap(SPACE), expect=("checked",), forbid=("not checked",))
    rig.tab_to("firefox", "focus: disclosure button", "details", ("details", "button", "collapsed"))
    rig.step("firefox", "change: Space expands", lambda: rig.tap(SPACE), expect=("expanded",))
    rig.tab_to("firefox", "focus: slider", "volume", ("volume", "slider"))
    rig.step("firefox", "change: Right sets the value", lambda: rig.tap(RIGHT), expect=("60",))
    rig.stop(p)
    shutil.rmtree(prof, ignore_errors=True)
    return got


def shell(rig):
    """Real GNOME Shell: two windows, a held Alt+Tab (the switcher is Shell UI), then the quick
    settings menu opened with Super+S and walked with the keyboard (toggle buttons)."""
    rig.eval("Main.overview.hide(); 1")
    time.sleep(1)
    rig.tap(42)  # user activity
    p1 = rig.spawn(["/usr/bin/python3", os.path.join(HERE, "gtkwin.py"), "Probe Window One"], "shell-win1")
    p2 = rig.spawn(["/usr/bin/python3", os.path.join(HERE, "p1_gtk4.py")], "shell-win2")
    end = time.time() + 60
    while time.time() < end:
        w = rig.windows()
        if isinstance(w, str) and "Probe Window One" in w and "P1 Wired" in w:
            break
        time.sleep(0.5)
    rig.click_window("Probe Window One")
    time.sleep(3.0)
    rig.settle(longest=10)
    # Alt+Tab held: the switcher shows, its selected entry is the other window.
    rig.step("shell", "focus: Alt+Tab switcher", lambda: (rig.key(ALT, True), time.sleep(0.05), rig.tap(TAB)),
             expect=("button",), longest=6)
    rig.step("shell", "focus: release Alt", lambda: rig.key(ALT, False), longest=6)
    # Quick settings: Super+S opens it, Tab walks its controls.
    rig.step("shell", "open quick settings (Super+S)", lambda: rig.tap(META, S), longest=6)
    rig.tab_to("shell", "focus: first button", "button", ("button",))
    rig.tab_to("shell", "focus: a toggle button", "toggle button", ("toggle button",), tries=14)
    rig.step("shell", "change: Space flips the toggle", lambda: rig.tap(SPACE), expect=("pressed",))
    rig.step("shell", "change: Space flips it back", lambda: rig.tap(SPACE), expect=("not pressed",))
    rig.tap(ESC)
    rig.stop(p1)
    rig.stop(p2)


def main():
    out = os.path.abspath(sys.argv[1])
    gorca = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else "target/release/gorca"
    os.makedirs(out, exist_ok=True)
    rig = Rig(out, gorca)
    rig.start_daemon()
    time.sleep(2.5)
    rig.rd_start()
    only = set(sys.argv[3:]) or {"shell", "gtk4", "firefox"}
    try:
        if "shell" in only:
            shell(rig)
        if "gtk4" in only:
            gtk4(rig)
        if "firefox" in only:
            firefox(rig)
    finally:
        rig.write()
        try:
            os.unlink(rig.ssip.path)
        except OSError:
            pass
        for a in rig.apps:
            rig.stop(a)
        if rig.daemon is not None:
            try:
                os.killpg(rig.daemon.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
    print("daemon exit status: %r" % (rig.daemon.poll(),), file=sys.stderr, flush=True)
    bad = [s for s in rig.steps if not s["ok"]]
    print("P1: %d steps, %d failed" % (len(rig.steps), len(bad)), file=sys.stderr, flush=True)
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
