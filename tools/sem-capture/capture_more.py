#!/usr/bin/python3
"""sem-capture, part 2: the corpus families and the state-change corpus the cut-over gate needs.

Runs INSIDE a private world (tests/shell/run_shell.sh), like capture.py, whose recorder it reuses.
Use the system Python (PyGObject + Atspi typelib), not the project venv. Never touches the live
session (refuses to run without GORCA_HARNESS_DIR and a private AT_SPI_BUS_ADDRESS).

  capture_more.py focus  --family gtk3     --out tests/fixtures/sem            (every GTK3 app page)
  capture_more.py focus  --family telegram --out tests/fixtures/sem            (Telegram Web A and K, Chromium)
  capture_more.py focus  --family electron --out tests/fixtures/sem            (Signal Desktop, throwaway profile)
  capture_more.py change --family gtk3|gtk4|firefox|chromium --out tests/fixtures/sem-change

`focus` records one jsonl per page (same schema as capture.py). `change` launches the change
gallery (tools/sem-capture/change_gallery.py for GTK, tests/fixtures/sem-change/pages/change.html
for the browsers), and for each case records the tree, presses the case's trigger through the
AT-SPI Action interface (a real state change made by the toolkit or the page script), waits for the
tree to settle and records it again: <case>.before.jsonl and <case>.after.jsonl.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)
import capture as C  # noqa: E402
from gi.repository import GLib  # noqa: E402

PY = "/usr/bin/python3"

# GTK3 apps already in the repo: page -> (argv, accessible app name fragment)
GTK3_PAGES = {
    "p9-gallery": ([PY, os.path.join(ROOT, "tools/platform-probes/p9apps/gtk3_app.py")], "gtk3_app"),
    "probe-window": ([PY, os.path.join(ROOT, "tests/shell/gtkwin.py")], "gtkwin"),
    "edit-entry": ([PY, os.path.join(ROOT, "tests/edit/gtkedit.py"), "3", "entry", "--text", "hello world"], "gtkedit"),
    "edit-textview": ([PY, os.path.join(ROOT, "tests/edit/gtkedit.py"), "3", "textview", "--text", "one two three"], "gtkedit"),
    "change-gallery": ([PY, os.path.join(HERE, "change_gallery.py"), "3"], "change_gallery"),
}
TELEGRAM_PAGES = {"a": "a.html", "k": "k.html", "echo": "echo.html"}

GTK_VALUE_CASES = ["value-scale", "value-spin", "value-progress", "value-level", "select-combo",
                   "expand-tree", "disable-row", "disable-tab"]
WEB_VALUE_CASES = ["value-range", "value-number", "value-progress", "value-meter", "value-aria-slider",
                   "value-aria-spin", "value-aria-progress", "select-collapsed", "expand-details",
                   "expand-tree", "expand-menu", "disable-tab", "disable-option", "disable-select-option",
                   "disable-treeitem", "select-listbox"]
CASES = ["check", "mixed", "toggle", "press", "expand", "disable", "select", "rename"]


def private_bus():
    addr = os.environ.get("AT_SPI_BUS_ADDRESS") or os.environ.get("GORCA_A11Y_BUS_ADDRESS")
    if not addr or not os.environ.get("GORCA_HARNESS_DIR") or "/run/user/" in addr:
        sys.exit("refusing to run outside a private world (need GORCA_HARNESS_DIR and a private "
                 "AT_SPI_BUS_ADDRESS, got %r)" % addr)
    return C.Bus(addr), os.environ["GORCA_HARNESS_DIR"]


def wait_app(bus, want, tries=120):
    for _ in range(tries):
        found = C.find_app(bus, want)
        if found:
            return found
        time.sleep(0.5)
    sys.exit("app %r never appeared on the private a11y bus" % want)


def write_page(out, page, header, nodes):
    os.makedirs(out, exist_ok=True)
    with open(os.path.join(out, page + ".jsonl"), "w", encoding="utf-8") as f:
        f.write(json.dumps(header, sort_keys=True) + "\n")
        for nd in nodes:
            f.write(json.dumps(nd, sort_keys=True, ensure_ascii=False) + "\n")


def header(engine, version, appname, page, title, nodes, settled):
    return {"kind": "header", "schema": 1, "engine": engine, "engine_version": version,
            "app_name": appname, "page": page, "title": title,
            "date": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "atspi_lib": C.pkg_version("libatspi2.0-0t64"), "at_spi2_core": C.pkg_version("at-spi2-core"),
            "settled_nodes": settled, "nodes": len(nodes),
            "unreadable": bool(nodes and nodes[0]["role"] is None),
            "note": "bus_name/path are per-run and not stable; compare by fixture_id / document order"}


def start(argv, env=None):
    return subprocess.Popen(argv, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            start_new_session=True)


def gtk_version(major):
    code = ("import gi;gi.require_version('Gtk','%s.0');from gi.repository import Gtk;"
            "print('GTK %%d.%%d.%%d'%%(Gtk.get_major_version(),Gtk.get_minor_version(),Gtk.get_micro_version()))"
            % major)
    return subprocess.run([PY, "-c", code], capture_output=True, text=True).stdout.strip()


def focus_gtk3(out):
    bus, _ = private_bus()
    for page, (argv, want) in GTK3_PAGES.items():
        proc = start(argv)
        try:
            d, p, appname = wait_app(bus, want)
            time.sleep(2)
            n = C.quiesce(bus, d, p)
            time.sleep(1)
            nodes = C.walk_subtree(bus, d, p)
            write_page(out, page, header("gtk3", gtk_version(3), appname, page, "", nodes, n), nodes)
            print("gtk3/%s: %d nodes" % (page, len(nodes)), file=sys.stderr)
        finally:
            C.kill_group(proc)
            time.sleep(1)


def focus_telegram(out):
    bus, root = private_bus()
    tg = os.path.join(ROOT, "tests/web/telegram")
    for page, html in TELEGRAM_PAGES.items():
        path = os.path.join(tg, html)
        proc, prof = C.launch("chromium", path, root)
        try:
            title = C.title_of(path)
            d, p, appname = wait_app(bus, "chrom")
            sub = None
            for _ in range(120):
                sub = C.find_doc(bus, d, p, title)
                if sub:
                    break
                time.sleep(0.5)
            if not sub:
                sys.exit("document %r never appeared" % title)
            n = C.quiesce(bus, sub[0], sub[1])
            time.sleep(3)
            nodes = C.walk_subtree(bus, sub[0], sub[1])
            write_page(out, page, header("chromium", C.version_of("chromium"), appname, page, title, nodes, n), nodes)
            print("telegram/%s: %d nodes" % (page, len(nodes)), file=sys.stderr)
        finally:
            C.kill_group(proc)
            shutil.rmtree(prof, ignore_errors=True)
            time.sleep(2)


def focus_electron(out):
    bus, root = private_bus()
    prof = tempfile.mkdtemp(prefix="signalprof.", dir=root)  # throwaway: never the owner's data
    env = dict(os.environ)
    env["HOME"] = prof
    env["XDG_CONFIG_HOME"] = os.path.join(prof, "config")
    env["XDG_CACHE_HOME"] = os.path.join(prof, "cache")
    argv = ["signal-desktop", "--user-data-dir=" + os.path.join(prof, "data"), "--force-renderer-accessibility",
            "--ozone-platform=wayland", "--no-sandbox", "--disable-gpu", "--password-store=basic"]
    proc = start(argv, env)
    try:
        d, p, appname = wait_app(bus, "signal", tries=240)
        time.sleep(8)
        n = C.quiesce(bus, d, p, deadline=90)
        time.sleep(3)
        nodes = C.walk_subtree(bus, d, p)
        ver = subprocess.run(["dpkg-query", "-W", "-f=${Version}", "signal-desktop"],
                             capture_output=True, text=True).stdout.strip()
        write_page(out, "welcome", header("electron", "Signal Desktop " + ver, appname, "welcome", "", nodes, n), nodes)
        print("electron/welcome: %d nodes" % len(nodes), file=sys.stderr)
    finally:
        C.kill_group(proc)
        shutil.rmtree(prof, ignore_errors=True)


SIGNAL_VIEWS = ["conversations", "chat", "compose", "search"]


def focus_electron_fixture(out):
    """The Signal-shaped fixture app (tools/sem-capture/signal-fixture) under Electron, one page per
    view. The real Signal cannot be taken past its link screen without registering a phone number,
    and the owner's account is never used. The Electron binary is not in the repo: SC_ELECTRON_DIR
    (default ~/.cache/electron-probe) holds dist2/signal-desktop, as tests/web/chr0/signal_cmp does."""
    bus, root = private_bus()
    import pwd
    real_home = pwd.getpwuid(os.getuid()).pw_dir  # HOME is private inside the world
    exe = os.path.join(os.environ.get("SC_ELECTRON_DIR", os.path.join(real_home, ".cache", "electron-probe")),
                       "dist2", "signal-desktop")
    if not os.access(exe, os.X_OK):
        sys.exit("no Electron binary at %s (set SC_ELECTRON_DIR)" % exe)
    appdir = os.path.join(HERE, "signal-fixture")
    ver = subprocess.run([exe, "--version"], capture_output=True, text=True).stdout.strip()
    for view in SIGNAL_VIEWS:
        prof = tempfile.mkdtemp(prefix="signalfx.", dir=root)
        argv = [exe, "--no-sandbox", "--disable-gpu", "--ozone-platform=wayland", "--force-renderer-accessibility",
                "--user-data-dir=" + os.path.join(prof, "data"), appdir, "?view=" + view]
        proc = start(argv)
        try:
            d, p, appname = wait_app(bus, "signal-desktop", tries=240)
            sub = None
            for _ in range(120):
                sub = C.find_doc(bus, d, p, "Signal")
                if sub:
                    break
                time.sleep(0.5)
            if not sub:
                sys.exit("fixture document never appeared")
            n = C.quiesce(bus, sub[0], sub[1], deadline=60)
            time.sleep(3)
            nodes = []
            for _ in range(40):  # the app root answers late on a loaded host
                nodes = C.walk_subtree(bus, d, p)  # the whole app: window chrome and menu bar included
                if len(nodes) > 20:
                    break
                time.sleep(1.0)
            if len(nodes) <= 20:
                sys.exit("electron/signal-%s: tree never became readable (%d nodes, process exit %r)"
                         % (view, len(nodes), proc.poll()))
            hdr = header("electron", "Electron " + ver + " (signal-fixture)", appname, "signal-" + view,
                         "Signal", nodes, n)
            write_page(out, "signal-" + view, hdr, nodes)
            print("electron/signal-%s: %d nodes" % (view, len(nodes)), file=sys.stderr)
        finally:
            C.kill_group(proc)
            shutil.rmtree(prof, ignore_errors=True)
            time.sleep(2)


def find_named(nodes, name):
    for nd in nodes:
        if nd.get("name") == name:
            return nd
    return None


def press(bus, nd):
    r = bus.call(nd["bus_name"], nd["path"], "org.a11y.atspi.Action", "DoAction", GLib.Variant("(i)", (0,)))
    return r is True


def change(family, out, page="change", cases=None, settle=1.0):
    bus, root = private_bus()
    prof = None
    if family in ("gtk3", "gtk4"):
        major = family[-1]
        gallery = "change_gallery" if page == "change" else "value_gallery"
        proc = start([PY, os.path.join(HERE, gallery + ".py"), major])
        d, p, appname = wait_app(bus, gallery)
        time.sleep(2)
        sub, title, engine, ver = (d, p), "", family, gtk_version(major)
    else:
        html = os.path.join(ROOT, "tests/fixtures/sem-change/pages/%s.html" % page)
        proc, prof = C.launch(family, html, root)
        title = C.title_of(html)
        d, p, appname = wait_app(bus, "firefox" if family == "firefox" else "chrom")
        sub = None
        for _ in range(120):
            sub = C.find_doc(bus, d, p, title)
            if sub:
                break
            time.sleep(0.5)
        if not sub:
            sys.exit("document %r never appeared" % title)
        engine, ver = family, C.version_of(family)
    try:
        C.quiesce(bus, sub[0], sub[1])
        time.sleep(2)
        default = CASES if page == "change" else (GTK_VALUE_CASES if family.startswith("gtk") else WEB_VALUE_CASES)
        for case in (cases or default):
            before = C.walk_subtree(bus, sub[0], sub[1])
            trig = find_named(before, "do:" + case)
            if not trig or not press(bus, trig):
                print("%s/%s: trigger missing or refused" % (family, case), file=sys.stderr)
                continue
            time.sleep(settle)
            C.quiesce(bus, sub[0], sub[1], need=2, step=0.5, deadline=15)
            after = C.walk_subtree(bus, sub[0], sub[1])
            write_page(out + "/" + family, case + ".before", header(engine, ver, appname, case + ".before", title, before, len(before)), before)
            write_page(out + "/" + family, case + ".after", header(engine, ver, appname, case + ".after", title, after, len(after)), after)
            print("%s/%s: %d -> %d nodes" % (family, case, len(before), len(after)), file=sys.stderr)
    finally:
        C.kill_group(proc)
        if prof:
            shutil.rmtree(prof, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["focus", "change"])
    ap.add_argument("--family", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--page", default="change", help="change: tests/fixtures/sem-change/pages/<page>.html (browsers), "
                    "change_gallery.py (change) or value_gallery.py (values) for GTK")
    ap.add_argument("--settle", type=float, default=1.0, help="seconds to wait after the trigger (change mode)")
    ap.add_argument("--cases", default="", help="comma separated case names (default: the page's own list)")
    a = ap.parse_args()
    if a.mode == "focus":
        fam = {"gtk3": focus_gtk3, "telegram": focus_telegram, "electron": focus_electron,
               "electron-fixture": focus_electron_fixture}[a.family]
        fam(os.path.join(a.out, "electron" if a.family == "electron-fixture" else a.family))
    else:
        change(a.family, a.out, a.page, [c for c in a.cases.split(',') if c] or None, a.settle)


if __name__ == "__main__":
    main()
