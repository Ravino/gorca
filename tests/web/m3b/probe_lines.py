#!/usr/bin/python3
"""probe_lines.py - what Firefox answers for the text area of 300 lines (M3b debugging).

    tests/web/m7/run_m7.sh --probe /usr/bin/python3 tests/web/m3b/probe_lines.py
"""
import os
import subprocess
import sys
import time

sys.argv = [sys.argv[0]]
HERE = os.path.dirname(os.path.abspath(__file__))
src = open(os.path.join(HERE, "probe_fields.py")).read()
head = src.split("def main():")[0]
exec(compile(head, "probe_head", "exec"))  # helpers: conn, call, prop, ax, pump, start_firefox ...


def main():
    url = "file://" + os.path.realpath(os.path.join(HERE, "pages", "fields.html"))
    ff = start_firefox(url)
    rd = subprocess.Popen(["/usr/bin/python3", os.path.join(M7, "rd.py")], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, text=True)
    try:
        d = ax({"cmd": "find_doc", "title": "M3b Fields", "timeout_ms": 60000})
        bus, doc = d["bus"], d["path"]
        pump(2.0)
        rd.stdin.write("click\n")
        rd.stdin.flush()
        pump(0.8)
        r = ax({"cmd": "find", "bus": bus, "root": doc, "role": "entry", "name": "Lines", "timeout_ms": 3000})
        path = r["path"]
        print("lines at", path)
        st = call(bus, path, ACC, "GetState")[0]
        print("state words", [hex(x) for x in st], "multi-line bit", (st[0] >> 17) & 1)
        ax({"cmd": "focus", "bus": bus, "path": path})
        pump(0.5)
        n = prop(bus, path, TEXT, "CharacterCount")
        print("count", n, "caret", prop(bus, path, TEXT, "CaretOffset"))
        for off in (0, 31, 62, 93):
            r = call(bus, path, TEXT, "GetStringAtOffset", (off, 3), "(iu)")
            print("line at", off, repr(esc(r[0])), r[1], r[2])
        seen.clear()
        rd.stdin.write("key 108\n")
        rd.stdin.flush()
        dump_events("down")
        print("caret now", prop(bus, path, TEXT, "CaretOffset"))
    finally:
        try:
            rd.stdin.close()
        except Exception:
            pass
        import signal
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
