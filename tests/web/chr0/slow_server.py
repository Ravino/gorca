#!/usr/bin/python3
"""slow_server.py PORT - a loopback HTTP server whose /slow?ms=N answers after N milliseconds.

CHR-0 test helper. A page that references /slow keeps its load open for that long, which is
what makes Chromium suppress children-changed:add (the engine sends no structural event for a
document that is still loading). Binds 127.0.0.1 only. Started and stopped by
tests/web/chr0/with_slow_server.sh, which records the PID.
"""
import base64
import http.server
import socketserver
import sys
import time
import urllib.parse

GIF = base64.b64decode("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7")


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        q = urllib.parse.urlparse(self.path)
        ms = 0
        if q.path == "/slow":
            try:
                ms = int(urllib.parse.parse_qs(q.query).get("ms", ["0"])[0])
            except ValueError:
                ms = 0
        sys.stderr.write("%.3f request %s\n" % (time.time(), self.path))
        sys.stderr.flush()
        time.sleep(min(ms, 120000) / 1000.0)
        sys.stderr.write("%.3f answered %s\n" % (time.time(), self.path))
        sys.stderr.flush()
        try:
            self.send_response(200)
            self.send_header("Content-Type", "image/gif")
            self.send_header("Content-Length", str(len(GIF)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(GIF)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *a):
        pass


class S(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


if __name__ == "__main__":
    port = int(sys.argv[1])
    S(("127.0.0.1", port), H).serve_forever()
