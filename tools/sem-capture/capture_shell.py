#!/usr/bin/python3
"""sem-capture, part 3a: GNOME Shell scenes (the `gnome-shell` cut-over family).

Runs INSIDE the private Shell world made by `tests/shell/run_shell.sh` (a real headless GNOME Shell
48 with --unsafe-mode on a private session bus and a private a11y bus). Like capture.py it refuses
to run anywhere else and never touches the live session. Use the system Python.

  tests/shell/run_shell.sh -- /usr/bin/python3 -I tools/sem-capture/capture_shell.py \
      --out tests/fixtures/sem/gnome-shell [--only a,b]

Each scene is put on screen with org.gnome.Shell.Eval (unsafe mode), org.freedesktop.Notifications
.Notify, org.gnome.Shell.ShowOSD or the Shell's own EndSessionDialog D-Bus interface, the Shell
tree is read until it is stable, and one <scene>.jsonl is written (same schema as capture.py).
Scenes that this headless session cannot produce are reported on stderr and skipped, never faked.
"""
import argparse
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import capture as C  # noqa: E402
import capture_more as M  # noqa: E402
from gi.repository import Gio, GLib  # noqa: E402

SESSION = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
if "/gorca-h." not in SESSION:
    sys.exit("capture_shell: refusing to run outside a tests/harness private session")


class Session:
    def __init__(self):
        self.c = Gio.DBusConnection.new_for_address_sync(
            SESSION, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
            None, None)

    def call(self, dest, path, iface, method, args, sig, timeout=8000):
        try:
            v = self.c.call_sync(dest, path, iface, method, GLib.Variant(sig, args) if sig else None, None,
                                 Gio.DBusCallFlags.NONE, timeout, None)
            return v.unpack()
        except GLib.Error as e:
            return ("ERR", e.message[:200])


S = Session()


def js(code):
    r = S.call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "Eval", (code,), "(s)")
    return r


def notify(summary, body, actions=(), urgency=1, replaces=0):
    hints = {"urgency": GLib.Variant("y", urgency)}
    return S.call("org.freedesktop.Notifications", "/org/freedesktop/Notifications",
                  "org.freedesktop.Notifications", "Notify",
                  ("capture-app", replaces, "dialog-information", summary, body, list(actions), hints, -1),
                  "(susssasa{sv}i)")


def osd():
    return S.call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell", "ShowOSD",
                  ({"icon": GLib.Variant("s", "audio-volume-high-symbolic"),
                    "label": GLib.Variant("s", "Volume"), "level": GLib.Variant("d", 0.5)},), "(a{sv})")


def end_session():
    # EndSessionDialog.Open(type 0 = logout, timestamp, seconds_to_stay_open, inhibitor paths)
    return S.call("org.gnome.Shell", "/org/gnome/SessionManager/EndSessionDialog",
                  "org.gnome.SessionManager.EndSessionDialog", "Open", (0, 0, 0, []), "(uuuao)")


def close_menus():
    js("Main.overview.hide(); Main.panel.closeQuickSettings(); "
       "Main.panel.statusArea.dateMenu.menu.close(false); 1")


# scene name -> (setup callable, undo callable). Setup returns nothing; failures show in the log.
SCENES = [
    ("topbar", lambda: js("Main.overview.hide(); 1"), None),
    ("quick-settings", lambda: js("Main.panel.statusArea.quickSettings.menu.open(false); 1"),
     lambda: js("Main.panel.statusArea.quickSettings.menu.close(false); 1")),
    ("date-menu", lambda: js("Main.panel.statusArea.dateMenu.menu.open(false); 1"),
     lambda: js("Main.panel.statusArea.dateMenu.menu.close(false); 1")),
    ("overview", lambda: js("Main.overview.show(); 1"), lambda: js("Main.overview.hide(); 1")),
    ("overview-apps", lambda: (js("Main.overview.show(); 1"),
                               js("Main.overview.dash.showAppsButton.checked = true; 1")),
     lambda: js("Main.overview.dash.showAppsButton.checked = false; Main.overview.hide(); 1")),
    ("overview-search", lambda: (js("Main.overview.show(); 1"),
                                 js("Main.overview.searchEntry.set_text('term'); 1")),
     lambda: js("Main.overview.searchEntry.set_text(''); Main.overview.hide(); 1")),
    ("notification-banner", lambda: notify("Build finished", "All 12 tests passed in 3.4 s",
                                           actions=("default", "Open", "dismiss", "Dismiss")),
     lambda: js("Main.messageTray.close(); 1")),
    ("notification-urgent", lambda: notify("Battery low", "Connect the charger now", urgency=2), None),
    ("notification-list", lambda: (notify("Message from Alex", "Are you coming to lunch today?"),
                                   notify("Update available", "Version 2.1 is ready to install"),
                                   time.sleep(1.5),
                                   js("Main.panel.statusArea.dateMenu.menu.open(false); 1")),
     lambda: js("Main.panel.statusArea.dateMenu.menu.close(false); 1")),
    ("osd", osd, None),
    ("end-session-dialog", end_session, lambda: js("Main.modalCount ? 1 : 0")),
]


def switcher_scenes(bus, d, p, out, appname, ver, only):
    """Alt+Tab (the window switcher) and Ctrl+Alt+Tab (the accessibility switcher), with two real
    GTK windows, through Mutter's RemoteDesktop virtual keyboard of this private session."""
    want = [n for n in ("window-switcher", "a11y-switcher") if not only or n in only]
    if not want:
        return
    sys.path.insert(0, os.path.join(HERE, "..", "..", "tests", "shell"))
    import probe_shell as P  # noqa: E402

    class D(P.Probe):
        def __init__(self):
            self.out = os.environ["GORCA_HARNESS_DIR"]  # client logs stay in the private world
            self.sess = P.Peer(P.SESSION)
            self.clients = []
            self.rd = None

    drv = D()
    print("RD", drv.rd_start(), file=sys.stderr)
    time.sleep(2)
    gtkwin = os.path.join(HERE, "..", "..", "tests", "shell", "gtkwin.py")
    drv.spawn_client(["/usr/bin/python3", gtkwin, "Probe Window One"])
    drv.spawn_client(["/usr/bin/python3", gtkwin, "Probe Window Two"])
    try:
        for _ in range(120):
            w = drv.windows()
            if isinstance(w, list) and len(w) >= 2:
                break
            time.sleep(0.5)
        drv.click()
        time.sleep(2)
        ALT, TAB, CTRL = 56, 15, 29
        for name in want:
            if name == "window-switcher":
                drv.key(ALT, True)
                time.sleep(0.15)
                drv.tap(TAB)
            else:
                drv.key(CTRL, True)
                drv.key(ALT, True)
                time.sleep(0.15)
                drv.tap(TAB)
                time.sleep(0.5)
                drv.tap(TAB)
            n = settle(bus, d, p, 1.0)
            nodes = prune(C.walk_subtree(bus, d, p))
            M.write_page(out, name, hdr(ver, appname, name, nodes, n), nodes)
            print("gnome-shell/%s: %d nodes (%d showing)" % (
                name, len(nodes), sum(1 for x in nodes if "showing" in x["states"])), file=sys.stderr)
            drv.key(ALT, False)
            if name == "a11y-switcher":
                drv.key(CTRL, False)
            time.sleep(1.5)
    finally:
        for c in drv.clients:
            try:
                c.terminate()  # only the PIDs this run started
            except OSError:
                pass


def hdr(ver, appname, name, nodes, n):
    h = M.header("gnome-shell", ver, appname, name, "", nodes, n)
    h["pruned"] = "nodes that are not showing, and have no showing descendant, were dropped"
    return h


def prune(nodes):
    """Keep the nodes that are showing and their ancestors (a hidden Shell actor is never read),
    renumbering `i`/`parent`/relation targets so the file is a valid page again."""
    by_i = {n["i"]: n for n in nodes}
    keep = set()
    for n in nodes:
        if "showing" in n["states"]:
            j = n["i"]
            while j is not None and j >= 0 and j not in keep:
                keep.add(j)
                j = by_i[j]["parent"]
    new = {}
    out = []
    for n in nodes:
        if n["i"] in keep:
            new[n["i"]] = len(out)
            out.append(n)
    import pwd
    me = pwd.getpwuid(os.getuid()).pw_name
    for n in out:
        for f in ("name", "desc", "text"):  # the account name is not fixture data
            if isinstance(n.get(f), str) and me:
                n[f] = n[f].replace(me, "user")
        n["i"] = new[n["i"]]
        n["parent"] = new.get(n["parent"], -1)
        for r in n.get("relations", []):
            r["targets"] = [new.get(t) if t is not None else None for t in r["targets"]]
    return out


def settle(bus, d, p, secs=1.0):
    time.sleep(secs)
    return C.quiesce(bus, d, p, need=2, step=0.5, deadline=20)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--only", default="")
    a = ap.parse_args()
    bus, _ = M.private_bus()
    d, p, appname = M.wait_app(bus, "gnome-shell")
    only = set(filter(None, a.only.split(",")))
    for _ in range(40):  # wait until the JS UI answers
        r = js("Main.panel ? 1 : 0")
        if r and r[0] is True:
            break
        time.sleep(0.5)
    ver = "GNOME Shell " + str(js("imports.misc.config.PACKAGE_VERSION")[1:])
    for name, setup, undo in SCENES:
        if only and name not in only:
            continue
        close_menus()
        time.sleep(0.8)
        setup()
        n = settle(bus, d, p, 1.2)
        nodes = prune(C.walk_subtree(bus, d, p))
        showing = sum(1 for x in nodes if "showing" in x["states"])
        M.write_page(a.out, name, hdr(ver, appname, name, nodes, n), nodes)
        print("gnome-shell/%s: %d nodes (%d showing)" % (name, len(nodes), showing), file=sys.stderr)
        if undo:
            undo()
        close_menus()
        time.sleep(0.5)
    switcher_scenes(bus, d, p, a.out, appname, ver, only)


if __name__ == "__main__":
    main()
