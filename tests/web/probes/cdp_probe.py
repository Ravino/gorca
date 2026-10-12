#!/usr/bin/env python3
"""CDP (Chrome DevTools Protocol) accessibility-tree probe over --remote-debugging-pipe.

Launches its OWN headless Chromium (own temp profile, no window, no port, no AT-SPI),
loads a generated page file and measures Accessibility.* latency, size, completeness.

usage: cdp_probe.py <chrome-binary> <page.html> [--shell]
Output: JSON lines with measurements; never touches the user's browsers.
"""
import os, sys, json, subprocess, time, tempfile, threading, queue, collections

class CDP:
    def __init__(self, binary, extra=()):
        self.prof = tempfile.mkdtemp(prefix="gorca-cdp-")
        r1, w1 = os.pipe()   # we write -> browser fd3
        r2, w2 = os.pipe()   # browser fd4 -> we read
        def pre():
            os.dup2(r1, 3); os.dup2(w2, 4)
        args = [binary, "--headless", "--remote-debugging-pipe", f"--user-data-dir={self.prof}",
                "--no-sandbox", "--disable-gpu", "--no-first-run", "--disable-extensions", *extra, "about:blank"]
        self.p = subprocess.Popen(args, preexec_fn=pre, pass_fds=(3, 4),
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(r1); os.close(w2)
        self.w = os.fdopen(w1, "wb", buffering=0); self.r = os.fdopen(r2, "rb", buffering=0)
        self.q = queue.Queue(); self.id = 0; self.pending = {}; self.events = collections.deque()
        self.t = threading.Thread(target=self._reader, daemon=True); self.t.start()
        self.bytes_in = 0
    def _reader(self):
        parts = []   # chunk list: avoids quadratic bytes concatenation on 100 MB messages
        while True:
            c = self.r.read(1 << 22)
            if not c: break
            self.bytes_in += len(c)
            while c:
                k = c.find(b"\0")
                if k < 0:
                    parts.append(c); break
                parts.append(c[:k])
                m = b"".join(parts); parts = []
                t = time.perf_counter()
                msg = json.loads(m)
                if "id" in msg: self.pending[msg["id"]] = (t, msg)
                else: self.events.append((t, msg))
                c = c[k + 1:]
    def call(self, method, params=None, session=None, timeout=120):
        self.id += 1; i = self.id
        m = {"id": i, "method": method, "params": params or {}}
        if session: m["sessionId"] = session
        t0 = time.perf_counter()
        self.w.write(json.dumps(m).encode() + b"\0")
        while i not in self.pending:
            time.sleep(0.0005)
            if time.perf_counter() - t0 > timeout: raise TimeoutError(method)
        t1, msg = self.pending.pop(i)
        return msg, t1 - t0
    def close(self):
        self.p.kill()

def out(**kw): print(json.dumps(kw), flush=True)

def main():
    binary, page = sys.argv[1], os.path.abspath(sys.argv[2])
    c = CDP(binary)
    msg, _ = c.call("Target.createTarget", {"url": "about:blank"})
    tid = msg["result"]["targetId"]
    msg, _ = c.call("Target.attachToTarget", {"targetId": tid, "flatten": True})
    s = msg["result"]["sessionId"]
    c.call("Page.enable", session=s)
    t0 = time.perf_counter()
    c.call("Page.navigate", {"url": "file://" + page}, session=s)
    # wait for load
    while not any(e[1].get("method") == "Page.loadEventFired" for e in list(c.events)):
        time.sleep(0.01)
        if time.perf_counter() - t0 > 120: break
    out(step="load", s=round(time.perf_counter() - t0, 3))
    c.events.clear()
    # enable accessibility (turns the AX tree on in the renderer: includes build cost)
    _, dt = c.call("Accessibility.enable", session=s)
    out(step="Accessibility.enable", s=round(dt, 3))
    time.sleep(0.5)
    ev = [e for _, e in c.events]
    out(step="events_after_enable", n=len(ev), methods=dict(collections.Counter(e["method"] for e in ev)))
    c.events.clear()
    # full tree, 3 runs
    for run in range(3):
        b0 = c.bytes_in
        msg, dt = c.call("Accessibility.getFullAXTree", session=s)
        nodes = msg.get("result", {}).get("nodes", [])
        out(step="getFullAXTree", run=run, s=round(dt, 3), nodes=len(nodes), mb=round((c.bytes_in - b0) / 1e6, 2),
            err=msg.get("error"))
    # completeness census
    roles = collections.Counter(); props = collections.Counter(); ign = 0; withname = 0; withdesc = 0; withvalue = 0
    for n in nodes:
        if n.get("ignored"): ign += 1
        roles[n.get("role", {}).get("value")] += 1
        if n.get("name", {}).get("value"): withname += 1
        if n.get("description", {}).get("value"): withdesc += 1
        if "value" in n: withvalue += 1
        for p in n.get("properties", []): props[p["name"]] += 1
    out(step="census", ignored=ign, with_name=withname, with_desc=withdesc, with_value=withvalue,
        top_roles=roles.most_common(12), props=dict(props.most_common(40)),
        node_keys=sorted({k for n in nodes[:2000] for k in n}))
    # sample node to show shape
    for n in nodes:
        if n.get("role", {}).get("value") == "button":
            out(step="sample_button", node=n); break
    # depth-limited: getPartialAXTree / queryAXTree
    msg, _ = c.call("DOM.getDocument", {"depth": 0}, session=s)
    rootid = msg["result"]["root"]["nodeId"]
    msg, dt = c.call("Accessibility.queryAXTree", {"nodeId": rootid, "role": "heading"}, session=s)
    out(step="queryAXTree_heading", s=round(dt, 3), n=len(msg.get("result", {}).get("nodes", [])), err=msg.get("error"))
    msg, dt = c.call("Accessibility.queryAXTree", {"nodeId": rootid, "role": "button", "accessibleName": "Action 3"}, session=s)
    out(step="queryAXTree_button_name", s=round(dt, 3), n=len(msg.get("result", {}).get("nodes", [])))
    msg, dt = c.call("Accessibility.getRootAXNode", session=s)
    out(step="getRootAXNode", s=round(dt, 4), err=msg.get("error"))
    # DOMSnapshot comparison (DOM + computed style subset)
    b0 = c.bytes_in
    msg, dt = c.call("DOMSnapshot.captureSnapshot", {"computedStyles": ["display", "visibility"]}, session=s)
    out(step="DOMSnapshot.captureSnapshot", s=round(dt, 3), mb=round((c.bytes_in - b0) / 1e6, 2), err=msg.get("error"))
    # register nodes: nodesUpdated is only sent for nodes the client already fetched through
    # getRootAXNode/getChildAXNodes/getAXNodeAndAncestors/getPartialAXTree (nodes_requested_ in blink)
    msg, _ = c.call("Runtime.evaluate", {"expression": "document.getElementById('main')"}, session=s)
    oid = msg["result"]["result"]["objectId"]
    msg, dt = c.call("Accessibility.getAXNodeAndAncestors", {"objectId": oid}, session=s)
    out(step="registered_main_and_ancestors", s=round(dt, 4), n=len(msg.get("result", {}).get("nodes", [])), err=msg.get("error"))
    msg, dt = c.call("Accessibility.getPartialAXTree", {"objectId": oid, "fetchRelatives": False}, session=s)
    out(step="getPartialAXTree_main", s=round(dt, 4), n=len(msg.get("result", {}).get("nodes", [])), err=msg.get("error"))
    # update events: mutate and see what arrives
    for name, expr in [("rename100", "mut.rename(100)"), ("addCards50", "mut.addCards(50)"),
                       ("live200", "mut.live(200)"), ("removeCards20", "mut.removeCards(20)"), ("route", "mut.route()")]:
        c.events.clear()
        t0 = time.perf_counter()
        c.call("Runtime.evaluate", {"expression": expr}, session=s)
        time.sleep(1.2)
        ev = list(c.events)
        cnt = collections.Counter(e["method"] for _, e in ev)
        upd_nodes = sum(len(e["params"].get("nodes", [])) for _, e in ev if e.get("method") == "Accessibility.nodesUpdated")
        first = (ev[0][0] - t0) if ev else None
        last = (ev[-1][0] - t0) if ev else None
        out(step="mutation", name=name, events=dict(cnt), nodes_in_updates=upd_nodes,
            first_event_ms=None if first is None else round(first * 1e3, 1), last_event_ms=None if last is None else round(last * 1e3, 1))
    # RSS of renderer is not queried here; report browser total via /proc children
    c.close()

if __name__ == "__main__":
    main()
