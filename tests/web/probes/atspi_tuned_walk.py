#!/usr/bin/env python3
"""AT-SPI 'tuned to the limit' walk of a web document: P2P socket + pipelined async calls +
multiple client processes (each its own P2P connection) + Collection.GetMatches.

usage: atspi_tuned_walk.py <a11y-bus-address> <app-name-substring> [--procs 1,2,4,8] [--window 1,16,64] [--calls struct,min,full]

Read-only. Works against any app on the given bus; intended for a PRIVATE a11y bus with a
headless Chrome (see scripts in docs/reports). Prints JSON lines.
"""
import sys, time, json, argparse, multiprocessing as mp

A = "org.a11y.atspi.Accessible"; PROP = "org.freedesktop.DBus.Properties"

def gio():
    import gi
    from gi.repository import Gio, GLib
    return Gio, GLib

def connect(addr, bus=False):
    Gio, GLib = gio()
    flags = Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
    if bus: flags |= Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION
    return Gio.DBusConnection.new_for_address_sync(addr, flags, None, None)

def csync(c, dest, path, iface, m, params=None):
    Gio, GLib = gio()
    return c.call_sync(dest, path, iface, m, params, None, Gio.DBusCallFlags.NONE, 60000, None)

def discover(bus_addr, appname):
    Gio, GLib = gio()
    bus = connect(bus_addr, bus=True)
    r = csync(bus, "org.a11y.atspi.Registry", "/org/a11y/atspi/accessible/root", A, "GetChildren").get_child_value(0)
    app = None
    for i in range(r.n_children()):
        d, p = r.get_child_value(i).get_child_value(0).get_string(), r.get_child_value(i).get_child_value(1).get_string()
        nm = csync(bus, d, p, PROP, "Get", GLib.Variant("(ss)", (A, "Name"))).get_child_value(0).get_variant().get_string()
        if appname.lower() in nm.lower(): app = (d, p); break
    if not app: raise SystemExit("app not found")
    d, p = app
    addr = csync(bus, d, p, "org.a11y.atspi.Application", "GetApplicationBusAddress").get_child_value(0).get_string()
    p2p = connect(addr)
    def kids(path):
        ch = csync(p2p, None, path, A, "GetChildren").get_child_value(0)
        return [ch.get_child_value(i).get_child_value(1).get_string() for i in range(ch.n_children())]
    def rolename(path): return csync(p2p, None, path, A, "GetRoleName").get_child_value(0).get_string()
    docs = []; stack = [p]
    while stack:
        x = stack.pop()
        for c in kids(x):
            if rolename(c) == "document web": docs.append(c)
            else: stack.append(c)
    if not docs: raise SystemExit("no document")
    # choose the document with the most children; wait until its child count is stable
    last, stable, t0, doc = -1, 0, time.time(), docs[0]
    while stable < 3 and time.time() - t0 < 60:
        counts = []
        for d in docs:
            try: counts.append((len(kids(d)), d))
            except Exception: pass
        if not counts: time.sleep(0.5); continue
        n, doc = max(counts)
        stable = stable + 1 if (n == last and n > 0) else 0
        last = n; time.sleep(0.5)
    return addr, doc, p2p

def walk_pipelined(addr, roots, window, calls, q=None):
    """Walk subtrees under `roots` with `window` async calls in flight on ONE connection."""
    Gio, GLib = gio()
    c = connect(addr)
    loop = GLib.MainLoop(); ctx = GLib.MainContext.default()
    st = {"inflight": 0, "nodes": 0, "calls": 0, "pending": list(roots), "outstanding_node": {}}
    per = {"struct": [], "min": [("GetRole", A, None), ("GetState", A, None)],
           "full": [("GetRole", A, None), ("GetState", A, None), ("GetInterfaces", A, None), ("GetAttributes", A, None)]}[calls]
    want_name = calls in ("min", "full")
    def issue(path, iface, m, params, cb):
        st["inflight"] += 1; st["calls"] += 1
        c.call(None, path, iface, m, params, None, Gio.DBusCallFlags.NONE, 60000, None, cb, None)
    def pump():
        while st["pending"] and st["inflight"] < window:
            p = st["pending"].pop(); st["nodes"] += 1
            issue(p, A, "GetChildren", None, on_children)
            for (m, i, par) in per: issue(p, i, m, par, on_other)
            if want_name:
                issue(p, PROP, "GetAll", GLib.Variant("(s)", (A,)), on_other)
        if st["inflight"] == 0 and not st["pending"]: loop.quit()
    def on_other(src, res, _):
        st["inflight"] -= 1
        try: src.call_finish(res)
        except Exception as e: st["err"] = str(e)
        pump()
    def on_children(src, res, _):
        st["inflight"] -= 1
        try:
            ch = src.call_finish(res).get_child_value(0)
            for i in range(ch.n_children()):
                st["pending"].append(ch.get_child_value(i).get_child_value(1).get_string())
        except Exception as e: st["err"] = str(e)
        pump()
    t = time.perf_counter()
    pump(); loop.run()
    dt = time.perf_counter() - t
    res = {"nodes": st["nodes"], "calls": st["calls"], "s": dt, "err": st.get("err")}
    if q is not None: q.put(res)
    return res

def rs_pipelined(addr, paths, window, q=None):
    """Leaf pass: GetRole + GetState for known paths (no GetChildren), pipelined on one connection."""
    Gio, GLib = gio()
    c = connect(addr)
    loop = GLib.MainLoop()
    st = {"inflight": 0, "calls": 0, "todo": list(paths), "err": None}
    def pump():
        while st["todo"] and st["inflight"] < window:
            p = st["todo"].pop()
            for m in ("GetRole", "GetState"):
                st["inflight"] += 1; st["calls"] += 1
                c.call(None, p, A, m, None, None, Gio.DBusCallFlags.NONE, 60000, None, done, None)
        if st["inflight"] == 0 and not st["todo"]: loop.quit()
    def done(src, res, _):
        st["inflight"] -= 1
        try: src.call_finish(res)
        except Exception as e: st["err"] = str(e)
        pump()
    t = time.perf_counter(); pump(); loop.run(); dt = time.perf_counter() - t
    r = {"nodes": len(paths), "calls": st["calls"], "s": dt, "err": st["err"]}
    if q is not None: q.put(r)
    return r

def rs_worker(addr, paths, window, q): rs_pipelined(addr, paths, window, q)

def worker(addr, roots, window, calls, q): walk_pipelined(addr, roots, window, calls, q)

def split_roots(p2p, doc, want=256):
    """BFS (sync) until the frontier has >= want nodes; return (frontier, nodes_consumed)."""
    frontier = [doc]; consumed = 0
    while len(frontier) < want:
        nxt = []
        for x in frontier:
            ch = csync(p2p, None, x, A, "GetChildren").get_child_value(0); consumed += 1
            nxt += [ch.get_child_value(i).get_child_value(1).get_string() for i in range(ch.n_children())]
        if not nxt: break
        frontier = nxt
    return frontier, consumed

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("bus"); ap.add_argument("app")
    ap.add_argument("--procs", default="1,2,4,8"); ap.add_argument("--window", default="1,16,64")
    ap.add_argument("--calls", default="struct,min,full")
    ap.add_argument("--matches", action="store_true")
    ap.add_argument("--stable-polls", type=int, default=8)
    ap.add_argument("--gettree", action="store_true", help="time Collection.GetTree (undocumented, at-spi2-core >= 2.5x) with property subsets")
    a = ap.parse_args()
    addr, doc, p2p = discover(a.bus, a.app)
    Gio, GLib = gio()
    print(json.dumps({"p2p": addr[:48], "doc": doc}), flush=True)
    # wait until the browser-side tree stops growing (big pages are serialized to the browser process in chunks)
    rule0 = ([], 1, {}, 1, [], 1, [], 1, False); last = -1; stable = 0; t0 = time.time()
    while stable < a.stable_polls and time.time() - t0 < 300:
        r = csync(p2p, None, doc, "org.a11y.atspi.Collection", "GetTree", GLib.Variant("((aiia{ss}iaiiasib)as)", (rule0, ["ChildCount"])))
        n = r.get_child_value(0).n_children()
        if n != last: print(json.dumps({"step": "tree_growth", "t": round(time.time() - t0, 1), "nodes": n}), flush=True)
        stable = stable + 1 if n == last else 0
        last = n; time.sleep(2.0)
    print(json.dumps({"step": "tree_stable", "nodes": last, "after_s": round(time.time() - t0, 1)}), flush=True)
    top = []
    arr0 = csync(p2p, None, doc, A, "GetChildren").get_child_value(0)
    for i0 in range(arr0.n_children()):
        cp = arr0.get_child_value(i0).get_child_value(1).get_string()
        top.append([csync(p2p, None, cp, A, "GetRoleName").get_child_value(0).get_string(),
                    csync(p2p, None, cp, A, "GetChildren").get_child_value(0).n_children()])
    print(json.dumps({"step": "doc_children", "role_childcount": top}), flush=True)
    if a.matches:
        # one-call enumeration of every node: Collection.GetMatches with a match-all rule
        # rule struct: (a{i} states, i stateMatch, a{ss} attrs, i attrMatch, ai roles, i roleMatch, as ifaces, i ifaceMatch, b invert)
        rule = GLib.Variant("((aiia{ss}iaiiasib))", (([], 1, {}, 1, [], 1, [], 1, False),))
        for sort, lim in ((0, 0), ):
            t = time.perf_counter()
            try:
                r = csync(p2p, None, doc, "org.a11y.atspi.Collection", "GetMatches",
                          GLib.Variant("((aiia{ss}iaiiasib)uib)", (([], 1, {}, 1, [], 1, [], 1, False), 0, lim, False)))
                n = r.get_child_value(0).n_children()
                print(json.dumps({"step": "GetMatches_all", "returned": n, "s": round(time.perf_counter() - t, 3)}), flush=True)
            except Exception as e:
                print(json.dumps({"step": "GetMatches_all", "error": str(e)[:200]}), flush=True)
    if a.gettree:
        rule = ([], 1, {}, 1, [], 1, [], 1, False)
        for props in (["ChildCount"], ["Name", "ChildCount"], ["Name", "ChildCount", "Attributes"], []):
            t = time.perf_counter()
            try:
                r = csync(p2p, None, doc, "org.a11y.atspi.Collection", "GetTree", GLib.Variant("((aiia{ss}iaiiasib)as)", (rule, props)))
                dt = time.perf_counter() - t
                arr = r.get_child_value(0)
                print(json.dumps({"step": "GetTree", "props": props or "ALL", "nodes": arr.n_children(), "s": round(dt, 3)}), flush=True)
                if props == ["Name", "ChildCount", "Attributes"]:
                    paths = [arr.get_child_value(i).get_child_value(0).get_child_value(1).get_string() for i in range(arr.n_children())]
                    ctx = mp.get_context("spawn")
                    for k in (1, 4, 8):
                        q = ctx.Queue(); t1 = time.perf_counter()
                        ps = [ctx.Process(target=rs_worker, args=(addr, paths[i::k], 64, q)) for i in range(k)]
                        for pr in ps: pr.start()
                        res = [q.get() for _ in ps]
                        for pr in ps: pr.join()
                        busy = max(x["s"] for x in res)
                        print(json.dumps({"step": "GetTree+role/state", "procs": k, "nodes": len(paths), "role_state_s": round(busy, 3),
                                          "total_s_with_tree": round(busy + dt, 3), "err": next((x["err"] for x in res if x["err"]), None)}), flush=True)
            except Exception as e:
                print(json.dumps({"step": "GetTree", "props": props, "error": str(e)[:160]}), flush=True)
    frontier, consumed = split_roots(p2p, doc)
    print(json.dumps({"step": "frontier", "size": len(frontier), "consumed": consumed}), flush=True)
    ctx = mp.get_context("spawn")
    for calls in a.calls.split(","):
        for w in map(int, a.window.split(",")):
            for k in map(int, a.procs.split(",")):
                parts = [frontier[i::k] for i in range(k)]
                q = ctx.Queue(); t = time.perf_counter()
                ps = [ctx.Process(target=worker, args=(addr, parts[i], w, calls, q)) for i in range(k)]
                for p in ps: p.start()
                res = [q.get() for _ in ps]
                for p in ps: p.join()
                dt = time.perf_counter() - t
                n = sum(r["nodes"] for r in res); cl = sum(r["calls"] for r in res)
                busy = max(r["s"] for r in res)
                print(json.dumps({"calls": calls, "window": w, "procs": k, "nodes": n, "rpc": cl,
                                  "walk_s": round(busy, 3), "wall_s_incl_spawn": round(dt, 3),
                                  "kcalls_per_s": round(cl / busy / 1000, 1), "us_per_node": round(busy / max(n, 1) * 1e6, 1),
                                  "err": next((r["err"] for r in res if r["err"]), None)}), flush=True)

if __name__ == "__main__":
    main()
