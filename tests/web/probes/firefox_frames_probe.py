#!/usr/bin/env python3
"""Does the PARENT-process privileged walk see cross-site iframe content, closed shadow roots and canvas fallback?
Serves tests/web/pages/frames on two origins (localhost:8101, 127.0.0.1:8102 = different sites => OOP iframe under Fission)."""
import sys, os, json, time, threading, http.server, functools
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from marionette_client import Marionette
D = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "pages", "frames")
def serve(port):
    h = functools.partial(http.server.SimpleHTTPRequestHandler, directory=D)
    class Q(h.func):
        def log_message(self, *a): pass
    s = http.server.ThreadingHTTPServer(("127.0.0.1", port), functools.partial(Q, directory=D))
    threading.Thread(target=s.serve_forever, daemon=True).start()
serve(8101); serve(8102)
JS = r"""
const svc = Cc["@mozilla.org/accessibilityService;1"].getService(Ci.nsIAccessibilityService);
const win = Services.wm.getMostRecentWindow("navigator:browser");
const br = win.gBrowser.selectedBrowser;
let doc = null;
for (let i = 0; i < 100 && !doc; i++) { const a = svc.getAccessibleFor(br); doc = a && a.firstChild; if (!doc) await new Promise(r => win.setTimeout(r, 50)); }
const rows = []; const stack = [[doc, 0]];
const roleName = r => Object.keys(Ci.nsIAccessibleRole).find(k => Ci.nsIAccessibleRole[k] === r) || r;
while (stack.length) {
  const [a, d] = stack.pop();
  rows.push([d, roleName(a.role), a.name, a.childCount]);
  const kids = []; for (let c = a.firstChild; c; c = c.nextSibling) kids.push(c);
  for (let i = kids.length - 1; i >= 0; i--) stack.push([kids[i], d + 1]);
}
return rows;
"""
def main():
    m = Marionette()
    try:
        m.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {}}})
        m.call("WebDriver:Navigate", {"url": "http://localhost:8101/outer.html"})
        time.sleep(4)
        m.call("Marionette:SetContext", {"value": "chrome"})
        rows = m.script("return (async()=>{" + JS + "})()")["value"]
        rows = m.script("return (async()=>{" + JS + "})()")["value"]
        for d, role, name, cc in rows:
            print(json.dumps({"depth": d, "role": role, "name": (name or "")[:50], "children": cc}))
    finally:
        m.close()
main()
