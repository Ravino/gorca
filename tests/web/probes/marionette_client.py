"""Minimal Marionette (Firefox) client: launches its own headless Firefox with a temp profile.

Marionette framing (protocol 3): '<len>:<json>'; commands are [0, id, name, params],
responses [1, id, error, result]. Used by firefox_a11y_probe.py.
"""
import os, json, socket, subprocess, tempfile, time, shutil

class Marionette:
    def __init__(self, port=28281, headless=True, binary="/usr/bin/firefox", extra_prefs=""):
        self.prof = tempfile.mkdtemp(prefix="gorca-ff-")
        with open(os.path.join(self.prof, "user.js"), "w") as f:
            f.write(f'user_pref("marionette.port", {port});\n'
                    'user_pref("browser.shell.checkDefaultBrowser", false);\n'
                    'user_pref("datareporting.policy.dataSubmissionEnabled", false);\n'
                    'user_pref("app.update.auto", false);\n'
                    'user_pref("browser.startup.homepage_override.mstone", "ignore");\n'
                    'user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);\n' + extra_prefs)
        env = dict(os.environ)
        if headless: env["MOZ_HEADLESS"] = "1"
        self.env_extra = env
        self.p = subprocess.Popen([binary, "--no-remote", "--marionette", "-remote-allow-system-access", "-profile", self.prof, "about:blank"],
                                  env=env, stdout=subprocess.DEVNULL, stderr=open(os.environ.get('GORCA_FF_LOG', os.devnull), 'ab'))
        self.sock = None
        for _ in range(200):
            try:
                self.sock = socket.create_connection(("127.0.0.1", port)); break
            except OSError:
                time.sleep(0.1)
        if not self.sock: raise RuntimeError("no marionette")
        self.buf = b""; self.id = 0
        self._read()  # hello
    def _read(self):
        while b":" not in self.buf:
            self.buf += self.sock.recv(1 << 20)
        ln, rest = self.buf.split(b":", 1)
        ln = int(ln)
        while len(rest) < ln:
            rest += self.sock.recv(1 << 22)
        self.buf = rest[ln:]
        return json.loads(rest[:ln])
    def call(self, name, params=None):
        self.id += 1
        m = json.dumps([0, self.id, name, params or {}]).encode()
        self.sock.sendall(str(len(m)).encode() + b":" + m)
        while True:
            r = self._read()
            if r[0] == 1 and r[1] == self.id:
                if r[2]: raise RuntimeError(r[2])
                return r[3]
    def script(self, js, args=(), timeout=300000):
        return self.call("WebDriver:ExecuteScript", {"script": js, "args": list(args), "scriptTimeout": timeout})["value"] if False else \
               self.call("WebDriver:ExecuteScript", {"script": js, "args": list(args)})
    def close(self):
        self.p.kill(); shutil.rmtree(self.prof, ignore_errors=True)
