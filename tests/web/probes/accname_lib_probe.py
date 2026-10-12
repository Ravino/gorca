#!/usr/bin/env python3
"""How well does a JS library (dom-accessibility-api) reproduce the browser's own accessible names,
and how long does it take in-page?  Oracle = Chromium AX tree (CDP) and/or Gecko accessibles (Marionette actor).

usage: accname_lib_probe.py chrome <chrome-binary> <page.html> <daa.bundle.js>
       accname_lib_probe.py firefox <page.html> <daa.bundle.js>
"""
import sys, os, json, time, collections
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

PAGE_JS = r"""
(() => {
  const els = Array.from(document.querySelectorAll('*')).filter(e => !['SCRIPT','STYLE','HEAD','META','TITLE','LINK'].includes(e.tagName));
  els.forEach((e, i) => e.setAttribute('data-gx', i));
  const t0 = performance.now();
  const names = [], roles = [];
  for (const e of els) {
    let n = ''; try { n = window.__daa.computeAccessibleName(e); } catch (x) { n = '!ERR'; }
    let r = null; try { r = window.__daa.getRole(e); } catch (x) {}
    names.push(n); roles.push(r);
  }
  const ms = performance.now() - t0;
  window.__daa_out = {names, roles, ms, tags: els.map(e => e.tagName.toLowerCase())};
  return {count: els.length, ms: Math.round(ms)};
})()
"""

def chrome(binary, page, bundle):
    from cdp_probe import CDP
    c = CDP(binary)
    msg, _ = c.call("Target.createTarget", {"url": "about:blank"}); tid = msg["result"]["targetId"]
    msg, _ = c.call("Target.attachToTarget", {"targetId": tid, "flatten": True}); s = msg["result"]["sessionId"]
    c.call("Page.enable", session=s)
    c.call("Page.navigate", {"url": "file://" + page}, session=s)
    t0 = time.time()
    while not any(e[1].get("method") == "Page.loadEventFired" for e in list(c.events)):
        time.sleep(0.02)
    c.call("Runtime.evaluate", {"expression": open(bundle).read()}, session=s)
    msg, _ = c.call("Runtime.evaluate", {"expression": PAGE_JS, "returnByValue": True}, session=s)
    print(json.dumps({"step": "lib_in_page", "r": msg["result"]["result"]["value"]}), flush=True)
    msg, _ = c.call("Runtime.evaluate", {"expression": "JSON.stringify(window.__daa_out)", "returnByValue": True}, session=s)
    out = json.loads(msg["result"]["result"]["value"])
    # oracle
    c.call("Accessibility.enable", session=s)
    c.call("DOM.enable", session=s)
    c.call("DOM.getDocument", {"depth": 0}, session=s)
    msg, _ = c.call("DOM.getFlattenedDocument", {"depth": -1, "pierce": False}, session=s)
    if "result" not in msg: print(json.dumps(msg)[:300]); return
    skip = ("SCRIPT", "STYLE", "HEAD", "META", "TITLE", "LINK")
    flat = [n for n in msg["result"]["nodes"] if n["nodeType"] == 1 and n["nodeName"] not in skip]
    msg, _ = c.call("Accessibility.getFullAXTree", session=s)
    byb = {n["backendDOMNodeId"]: n for n in msg["result"]["nodes"] if "backendDOMNodeId" in n and n.get("role", {}).get("value") not in ("StaticText", "InlineTextBox", "LineBreak")}
    cmp = collections.Counter(); mism = []
    for n in flat:
        at = n.get("attributes", [])
        if "data-gx" not in at: continue
        i = int(at[at.index("data-gx") + 1])
        ax = byb.get(n["backendNodeId"])
        if not ax or ax.get("ignored"): continue
        oracle = ax.get("name", {}).get("value", "") or ""
        lib = out["names"][i] or ""
        if not oracle and not lib: cmp["both_empty"] += 1; continue
        if oracle == lib: cmp["equal"] += 1
        elif oracle.strip() == lib.strip(): cmp["equal_trim"] += 1
        else:
            cmp["differ"] += 1
            if len(mism) < 8: mism.append({"tag": out["tags"][i], "chrome": oracle[:60], "lib": lib[:60]})
    print(json.dumps({"step": "name_agreement_vs_chromium", "counts": dict(cmp), "samples": mism}), flush=True)
    c.close()

def firefox(page, bundle):
    from marionette_client import Marionette
    ACTOR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ff_actor")
    m = Marionette()
    try:
        m.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {}}})
        m.call("WebDriver:SetTimeouts", {"script": 900000, "pageLoad": 300000})
        m.call("Marionette:SetContext", {"value": "chrome"})
        m.script(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "firefox_content_actor_probe.py")).read().split('REG = r"""')[1].split('"""')[0], [ACTOR])
        m.call("Marionette:SetContext", {"value": "content"})
        m.call("WebDriver:Navigate", {"url": "file://" + page})
        time.sleep(3)
        m.script(open(bundle).read())
        r = m.script("return " + PAGE_JS)["value"]
        print(json.dumps({"step": "lib_in_page", "r": r}), flush=True)
        # the sandbox does not share window.__daa_out; recompute and return values
        out = m.script("return " + PAGE_JS.replace("window.__daa_out = ", "const O = ").replace("return {count", "return {count") + ";")
    finally:
        m.close()

if __name__ == "__main__":
    if sys.argv[1] == "chrome":
        chrome(sys.argv[2], os.path.abspath(sys.argv[3]), sys.argv[4])
    else:
        firefox(os.path.abspath(sys.argv[2]), sys.argv[3])
