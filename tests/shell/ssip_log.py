#!/usr/bin/python3
"""Minimal SSIP server for repro rigs: accepts everything, logs every SPEAK text with a monotonic time.

usage: ssip_log.py SOCKET LOGFILE
"""
import os
import socket
import sys
import threading
import time

path, logf = sys.argv[1], sys.argv[2]
if os.path.exists(path):
    os.unlink(path)
srv = socket.socket(socket.AF_UNIX)
srv.bind(path)
srv.listen(8)
log = open(logf, "a", buffering=1)
msg = [0]


def serve(c):
    f = c.makefile("rwb", buffering=0)
    data = None
    for raw in f:
        line = raw.decode("utf-8", "replace").rstrip("\r\n")
        if data is not None:
            if line == ".":
                text = "\n".join(data)
                msg[0] += 1
                log.write("%.3f SPEAK %r\n" % (time.monotonic(), text))
                f.write(b"225-%d\r\n225 OK MESSAGE QUEUED\r\n" % msg[0])
                data = None
            else:
                data.append(line)
            continue
        cmd = line.split(" ")[0].upper()
        if cmd == "SPEAK":
            data = []
            f.write(b"230 OK RECEIVING DATA\r\n")
        elif cmd in ("STOP", "CANCEL"):
            log.write("%.3f %s\n" % (time.monotonic(), cmd))
            f.write(b"210 OK STOPPED\r\n")
        elif cmd == "QUIT":
            f.write(b"231 HAPPY HACKING\r\n")
            return
        else:
            f.write(b"200 OK\r\n")


while True:
    c, _ = srv.accept()
    threading.Thread(target=serve, args=(c,), daemon=True).start()
