#!/usr/bin/env python3
"""Firefox: which accessible events does the PARENT process see via Services.obs 'accessible-event'
(privileged, Marionette chrome context), and how fast do they arrive after a DOM mutation?
usage: firefox_events_probe.py <page.html>   (page from gen_large.py, has window.mut helpers)
"""
import sys, os, json, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from marionette_client import Marionette

INSTALL = r"""
const win = Services.wm.getMostRecentWindow("navigator:browser");
const svc = Cc["@mozilla.org/accessibilityService;1"].getService(Ci.nsIAccessibilityService);
const names = {};
for (const k of Object.keys(Ci.nsIAccessibleEvent)) if (k.startsWith("EVENT_")) names[Ci.nsIAccessibleEvent[k]] = k;
win.__g = {counts: {}, first: null, last: null, n: 0, names, samples: []};
win.__g.obs = {observe(subject) {
  const e = subject.QueryInterface(Ci.nsIAccessibleEvent);
  const g = win.__g; const t = performance.now();
  const nm = g.names[e.eventType] || e.eventType;
  g.counts[nm] = (g.counts[nm] || 0) + 1; g.n++;
  if (g.first === null) g.first = t; g.last = t;
}};
Services.obs.addObserver(win.__g.obs, "accessible-event");
// make sure the engine is live and the doc mirrored
const br = win.gBrowser.selectedBrowser; const a0 = svc.getAccessibleFor(br); if (a0) a0.firstChild;
return true;
"""
READ = r"""
const win = Services.wm.getMostRecentWindow("navigator:browser"); const g = win.__g;
const r = {counts: g.counts, n: g.n, spanMs: g.first === null ? null : Math.round(g.last - g.first), tNow: performance.now(), tFirst: g.first};
g.counts = {}; g.n = 0; g.first = null; g.last = null;
return r;
"""

def main():
    page = os.path.abspath(sys.argv[1])
    m = Marionette()
    try:
        m.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {}}})
        m.call("WebDriver:Navigate", {"url": "file://" + page})
        time.sleep(3)
        m.call("Marionette:SetContext", {"value": "chrome"})
        m.script(INSTALL); time.sleep(1.5); m.script(READ)
        for name, expr in [("rename100", "mut.rename(100)"), ("addCards50", "mut.addCards(50)"),
                           ("live200", "mut.live(200)"), ("removeCards20", "mut.removeCards(20)"), ("route", "mut.route()")]:
            m.call("Marionette:SetContext", {"value": "content"})
            t0 = time.time()
            m.script(expr)
            m.call("Marionette:SetContext", {"value": "chrome"})
            time.sleep(1.5)
            r = m.script(READ)["value"]
            print(json.dumps({"mutation": name, "events": r["counts"], "total": r["n"], "span_ms": r["spanMs"]}), flush=True)
    finally:
        m.close()
main()
