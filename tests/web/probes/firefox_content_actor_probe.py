#!/usr/bin/env python3
"""Firefox: register a privileged JSWindowActor (via Marionette chrome context) and walk the
IN-PROCESS content accessible tree from the content process. usage: <page.html>"""
import sys, os, json, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from marionette_client import Marionette
ACTOR_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "ff_actor")
REG = r"""
const [dir] = arguments;
const resProto = Services.io.getProtocolHandler("resource").QueryInterface(Ci.nsIResProtocolHandler);
const f = Cc["@mozilla.org/file/local;1"].createInstance(Ci.nsIFile); f.initWithPath(dir);
resProto.setSubstitution("gorcaprobe", Services.io.newFileURI(f));
ChromeUtils.registerWindowActor("GorcaProbe", {child: {esModuleURI: "resource://gorcaprobe/GorcaProbeChild.sys.mjs"}, allFrames: false, matches: ["file:///*"]});
return true;
"""
CALL = r"""
const [ser, ship] = arguments;
const win = Services.wm.getMostRecentWindow("navigator:browser");
const bc = win.gBrowser.selectedBrowser.browsingContext;
const actor = bc.currentWindowGlobal.getActor("GorcaProbe");
return await actor.sendQuery("walk", {serialize: ser, ship: !!ship});
"""
def main():
    page = os.path.abspath(sys.argv[1])
    m = Marionette()
    try:
        m.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {}}})
        m.call("WebDriver:SetTimeouts", {"script": 900000, "pageLoad": 300000})
        m.call("Marionette:SetContext", {"value": "chrome"})
        m.script(REG, [ACTOR_DIR])
        t0 = time.time()
        m.call("Marionette:SetContext", {"value": "content"})
        m.call("WebDriver:Navigate", {"url": "file://" + page})
        print(json.dumps({"step": "navigate", "s": round(time.time() - t0, 2)}), flush=True)
        m.call("Marionette:SetContext", {"value": "chrome"})
        last, stable, t0 = -1, 0, time.time()
        while stable < 3 and time.time() - t0 < 240:
            r = m.script("return (async()=>{" + CALL + "})()", [False])["value"]
            n = r.get("nodes"); stable = stable + 1 if n == last else 0; last = n; time.sleep(0.5)
        print(json.dumps({"step": "content_tree_stable", "nodes": last, "after_s": round(time.time() - t0, 2)}), flush=True)
        for ser in (False, True, True):
            t0 = time.time()
            r = m.script("return (async()=>{" + CALL + "})()", [ser])
            print(json.dumps({"step": "content_walk", "serialize": ser, "wall_s": round(time.time() - t0, 3), "r": r}), flush=True)
        # shipping cost: ship the compact arrays through Marionette (JSON over TCP) to this process
        t0 = time.time()
        r = m.script("return (async()=>{" + CALL + "})()", [True, True])["value"]
        print(json.dumps({"step": "ship_via_marionette", "wall_s": round(time.time() - t0, 3), "nodes": r["nodes"],
                          "out_len": len(r["out"]), "names": len(r["names"])}), flush=True)
    finally:
        m.close()
main()
