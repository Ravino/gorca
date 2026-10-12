#!/usr/bin/env python3
"""Firefox privileged a11y probe: Marionette (chrome context, needs -remote-allow-system-access)
driving nsIAccessibilityService in a PRIVATE headless Firefox with a temp profile.

Measures (a) a full walk of the web document from the parent process (RemoteAccessible, cache based)
and (b) optionally from the content process through a JSWindowActor (LocalAccessible, in-process).

usage: firefox_a11y_probe.py <page.html> [parent|content|both]
"""
import sys, os, json, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from marionette_client import Marionette

WALK_JS = r"""
const [mode] = arguments;
const svc = Cc["@mozilla.org/accessibilityService;1"].getService(Ci.nsIAccessibilityService);
const win = Services.wm.getMostRecentWindow("navigator:browser");
const br = win.gBrowser.selectedBrowser;
let t0 = Date.now();
let doc = null;
for (let i = 0; i < 400 && !doc; i++) {
  const a = svc.getAccessibleFor(br);
  doc = a && a.firstChild;
  if (!doc) await new Promise(r => win.setTimeout(r, 50));
}
const tFind = Date.now() - t0;
if (!doc) return {error: "no doc accessible"};
// iterative DFS using firstChild/nextSibling
const roles = {};
let n = 0, named = 0, withState = 0, maxDepth = 0, textleaf = 0, live = 0;
const t1 = performance.now();
const stack = [[doc, 0]];
while (stack.length) {
  const [a, d] = stack.pop();
  n++;
  if (d > maxDepth) maxDepth = d;
  const r = a.role; roles[r] = (roles[r] || 0) + 1;
  if (mode !== "bare") {
    if (a.name) named++;
    const st = {}, ex = {};
    a.getState(st, ex);
    if (st.value) withState++;
  }
  // push children in reverse order so DFS is in document order
  const kids = [];
  for (let c = a.firstChild; c; c = c.nextSibling) kids.push(c);
  for (let i = kids.length - 1; i >= 0; i--) stack.push([kids[i], d + 1]);
}
const t2 = performance.now();
return {tFindDocMs: tFind, nodes: n, named, withState, maxDepth, walkMs: Math.round(t2 - t1), distinctRoles: Object.keys(roles).length};
"""

def main():
    page = os.path.abspath(sys.argv[1]); which = sys.argv[2] if len(sys.argv) > 2 else "parent"
    m = Marionette()
    try:
        m.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {}}})
        m.call("WebDriver:SetTimeouts", {"script": 900000, "pageLoad": 300000})
        t0 = time.time()
        m.call("WebDriver:Navigate", {"url": "file://" + page})
        print(json.dumps({"step": "navigate", "s": round(time.time() - t0, 2)}), flush=True)
        m.call("Marionette:SetContext", {"value": "chrome"})
        if which in ("parent", "both"):
            # wait until the mirrored tree stops growing (parent mirror is built incrementally via IPC)
            t0 = time.time(); last = -1; stable = 0
            while stable < 3 and time.time() - t0 < 240:
                r = m.script("return (async()=>{" + WALK_JS + "})()", ["bare"])["value"]
                stable = stable + 1 if r.get("nodes") == last else 0
                last = r.get("nodes"); time.sleep(0.4)
            print(json.dumps({"step": "tree_stable", "nodes": last, "after_s": round(time.time() - t0, 2)}), flush=True)
            for mode in ("bare", "full", "full"):
                t0 = time.time()
                r = m.script("return (async()=>{" + WALK_JS + "})()", [mode])
                print(json.dumps({"step": "parent_walk", "mode": mode, "wall_s": round(time.time() - t0, 3), "r": r}), flush=True)
    finally:
        m.close()
main()
