#!/usr/bin/python3
"""probe_typing.py - the offsets of the events when typing after an emoji (M3b debugging).

    tests/web/m7/run_m7.sh --probe /usr/bin/python3 tests/web/m3b/probe_typing.py
"""
import os
import signal
import subprocess
import sys
import time

sys.argv = [sys.argv[0]]
HERE = os.path.dirname(os.path.abspath(__file__))
src = open(os.path.join(HERE, "probe_fields.py")).read()
exec(compile(src.split("def main():")[0], "probe_head", "exec"))


def main():
    url = "file://" + os.path.realpath(os.path.join(HERE, "pages", "fields.html"))
    ff = start_firefox(url)
    rd = subprocess.Popen(["/usr/bin/python3", os.path.join(M7, "rd.py")], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, text=True)

    def keys(*codes):
        rd.stdin.write("key " + " ".join(str(c) for c in codes) + "\n")
        rd.stdin.flush()

    try:
        d = ax({"cmd": "find_doc", "title": "M3b Fields", "timeout_ms": 60000})
        bus, doc = d["bus"], d["path"]
        pump(2.0)
        rd.stdin.write("click\n")
        rd.stdin.flush()
        pump(0.8)
        keys(42)
        pump(0.8)
        r = ax({"cmd": "find", "bus": bus, "root": doc, "role": "entry", "name": "Session", "timeout_ms": 3000})
        path = r["path"]
        ax({"cmd": "focus", "bus": bus, "path": path})
        pump(0.6)
        seen.clear()
        # a, F9 (emoji), b, c, d, Left, e, f
        seq = [("a", [30]), ("Left", [105]), ("F10", [68]), ("b", [48]), ("c", [46]), ("d", [32]),
               ("e", [18]), ("Back", [14]), ("f", [33]), ("g", [34]), ("h", [35]), ("Left", [105]), ("i", [23])]
        for name, codes in seq:
            keys(*codes)
            pump(0.2)
            dump_events(name)
        n, t = text_of(bus, path)
        print("text", n, repr(esc(str(t))), "caret", caret_of(bus, path))
    finally:
        try:
            rd.stdin.close()
        except Exception:
            pass
        try:
            os.killpg(ff.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        time.sleep(1)
        try:
            os.killpg(ff.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


main()
