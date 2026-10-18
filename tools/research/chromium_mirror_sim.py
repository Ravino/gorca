#!/usr/bin/env python3
"""Chromium mirror-freshness simulator (research prototype, see
docs/reports/chromium-mirror-freshness-math.md).

Ground truth: a Signal-like Electron document that starts as a 4-node loading shell, is replaced and
grows to ~900 nodes, then lives on (focus moves, virtualized rows recycled in place, messages
appended, conversation switches, silent reorders, an optional 10k ev/s name storm).

Observation channel (Chromium ATK bridge, refs/chromium/ui/accessibility/platform/
ax_platform_node_auralinux.cc OnSubtreeCreated/OnSubtreeWillBeDeleted):
  * subtree insert  -> one children-changed:add on the subtree root's parent, prob P_ADD
  * subtree delete  -> one children-changed:remove on the parent, prob P_RM ("sometimes none")
  * in-place name   -> name-changed on the node, prob P_NAME
  * reorder/move    -> half recreate (remove+add as above), half silent same-path reorder
  * focus           -> always (path of the focused object)
Object paths are never reused (at-spi2-core atk-adaptor/accessible-register.c assign_reference:
monotonic u32 counter), so "path unknown to the mirror" is an exact newness test.

The mirror is maintained by one serial source actor (one op in flight, FIFO + priority), as in
crates/gorca-doc-source-atspi/src/source.rs. Op costs follow measured numbers
(docs/reports/2026-10-12-web-source-layer.md, m9-baseline.md):
  call RTT C0 ~ lognormal, median 0.25 ms, clipped to [0.1, 2] ms
  GetTree server time BETA_TREE = 10 us/node (106k nodes in 1.1 s)
  GetMatches server time BETA_MATCH = 3 us/node (ASSUMED, unmeasured)
  role+state passes for NEW nodes only: 2 x 25 us/node (pipelined, window 64)

Strategies:
  S0   event-driven only; events whose source is unknown to the mirror are dropped (today)
  S1   S0 + orphan trigger: focus/event on an unknown path -> walk Parent to the lowest known
       ancestor (LKA) -> GetTree resync of the LKA subtree (single flight, 100 ms hysteresis)
  S2   S1 + budgeted background sweep: GetTree chunks of <= B nodes, R chunks/s, round robin
  S2m  S1 + sweep with GetMatches (paths only, misses in-place name changes)
  S3   S0 + periodic full rebuild every 2 s (baseline)

Usage: python tools/research/chromium_mirror_sim.py [--seeds N] [--T seconds] [--quick]
"""
from __future__ import annotations

import argparse
import heapq
import math
import random
from collections import defaultdict

C0_MED = 0.25e-3
BETA_TREE = 10e-6
BETA_MATCH = 3e-6
GAMMA2 = 50e-6
SERVER_CALL = 30e-6  # UI-thread time of a trivial call
SWEEP_NODES_PER_S = 2000.0  # sweep budget: 2000 nodes/s = 20 ms/s GetTree UI time
ROWS = 50  # conversation rows (scale knob: 50 -> ~800 nodes, 5000 -> ~45k)


class Tree:
    """Ground truth. Paths are monotonic and never reused."""

    def __init__(self, rng):
        self.rng = rng
        self.next_path = 1
        self.par = {}
        self.kids = {}
        self.name = {}
        self.role = {}
        self.root = None

    def new(self, role, name=""):
        p = self.next_path
        self.next_path += 1
        self.par[p] = None
        self.kids[p] = []
        self.name[p] = name
        self.role[p] = role
        return p

    def attach(self, parent, child, idx=None):
        self.par[child] = parent
        if idx is None:
            self.kids[parent].append(child)
        else:
            self.kids[parent].insert(idx, child)

    def build(self, spec):
        """spec = (role, name, [children specs]) -> root path."""
        role, name, ch = spec
        p = self.new(role, name)
        for c in ch:
            self.attach(p, self.build(c))
        return p

    def subtree(self, p):
        out, st = [], [p]
        while st:
            x = st.pop()
            out.append(x)
            st.extend(reversed(self.kids[x]))
        return out

    def remove(self, p):
        par = self.par[p]
        if par is not None:
            self.kids[par].remove(p)
        for x in self.subtree(p):
            del self.par[x], self.kids[x], self.name[x], self.role[x]

    def alive(self, p):
        return p in self.par

    def in_doc(self, p):
        while p is not None:
            if p == self.root:
                return True
            p = self.par.get(p)
        return False


# ---------------------------------------------------------------- Signal-like content specs
def row_spec(rng, i):
    return ("listitem", "", [("article", "", [
        ("static", f"Contact {rng.randrange(40)}", []),
        ("static", f"msg preview {rng.randrange(10**6)}", []),
        ("static", f"10:{rng.randrange(60):02d}", []),
        ("button", "More actions", []),
        ("button", f"Open conversation {i}", []),
        ("section", "", [("static", "unread", [])]),
    ])])


def msg_spec(rng):
    return ("listitem", "", [("article", "", [
        ("section", "", [("static", f"Author {rng.randrange(5)}", []),
                         ("static", f"text {rng.randrange(10**6)}", [])]),
        ("static", f"10:{rng.randrange(60):02d}", []),
        ("button", "React", []), ("button", "Reply", []), ("button", "More actions", []),
        ("section", "", [("static", "delivered", [])]),
    ])])


def panel_spec(rng, n_msgs=23):
    return ("section", "conversation", [
        ("heading", f"Chat {rng.randrange(100)}", []),
        ("list", "messages", [msg_spec(rng) for _ in range(n_msgs)]),
        ("section", "composer", [("entry", "Message", []), ("button", "Send", [])]),
    ])


class World:
    """Truth + mutation script; emits observation events."""

    def __init__(self, rng, P, storm):
        self.rng, self.P, self.storm = rng, P, storm
        self.t = Tree(rng)
        t = self.t
        # outside-doc browser UI (Electron menu bar)
        self.app = t.new("application", "signal")
        self.frame = t.new("frame", "Signal")
        t.attach(self.app, self.frame)
        self.menu = [t.new("button", m) for m in ("File", "Edit", "View", "Window", "Help")]
        for m in self.menu:
            t.attach(self.frame, m)
        # loading shell: doc > body > div > static  (4 nodes)
        self.doc = t.build(("document", "Signal", [("section", "body", [
            ("section", "loading", [("static", "Loading...", [])])])]))
        t.root = self.doc
        t.attach(self.frame, self.doc)
        self.body = t.kids[self.doc][0]
        self.convlist = self.panel_parent = self.msglist = None
        self.events = []  # (time, kind, path)
        self.created = {}  # path -> time created
        self.deleted = {}  # path -> time deleted
        self.renamed = {}  # path -> last rename time
        self.silent_new, self.silent_del = set(), set()

    def note_new(self, root, now):
        for x in self.t.subtree(root):
            self.created[x] = now

    def note_del(self, root, now):
        for x in self.t.subtree(root):
            self.deleted[x] = now

    def emit(self, now, kind, path, prob):
        if self.rng.random() < prob:
            self.events.append((now + 0.001, kind, path))
            return True
        return False

    def insert(self, now, parent, spec, idx=None):
        r = self.t.build(spec)
        self.t.attach(parent, r, idx)
        self.note_new(r, now)
        if not self.emit(now, "add", parent, self.P["add"]):
            for x in self.t.subtree(r):
                self.silent_new.add(x)
        return r

    def delete(self, now, p):
        par = self.t.par[p]
        self.note_del(p, now)
        gone = self.t.subtree(p)
        self.t.remove(p)
        if not self.emit(now, "rm", par, self.P["rm"]):
            self.silent_del.update(gone)

    def rename(self, now, p, name):
        self.t.name[p] = name
        self.renamed[p] = now
        self.emit(now, "name", p, self.P["name"])

    # -------------------------------------------------------------- script
    def script(self, T):
        """Yields (time, callable) mutations."""
        rng = self.rng
        ev = []
        ev.append((1.5, self.m_replace_shell))
        for k in range(16):
            ev.append((2.0 + 0.25 * k, self.m_grow))
        ev.append((6.0, self.m_shrink))
        t = 6.0
        rates = [("focus", 2.0), ("recycle", 0.3), ("newmsg", 0.2), ("reorder", 0.2),
                 ("switch", 0.05)]
        for kind, lam in rates:
            tt = 6.0
            while True:
                tt += rng.expovariate(lam)
                if tt > T:
                    break
                ev.append((tt, getattr(self, "m_" + kind)))
        if self.storm:
            for k in range(20000):  # 10k ev/s for 2 s on 50 live nodes
                ev.append((60.0 + k / 10000.0, self.m_storm))
        ev.sort(key=lambda x: x[0])
        return ev

    def m_replace_shell(self, now):
        old = self.t.kids[self.body][0]
        self.insert(now, self.body, ("section", "app-container", [
            ("section", "nav", [("tab", n, []) for n in ("Chats", "Calls", "Stories", "Settings")]),
            ("list", "conversations", []),
            ("section", "panel-host", []),
            ("section", "status", [("static", "Connecting", [])] * 1),
        ]))
        self.delete(now, old)
        ac = self.t.kids[self.body][-1]
        self.convlist = self.t.kids[ac][1]
        self.panel_parent = self.t.kids[ac][2]
        self.grown = 0

    def m_grow(self, now):
        if self.grown < 10:  # 50 conversation rows in 10 batches
            for _ in range(ROWS // 10):
                self.insert(now, self.convlist, row_spec(self.rng, self.grown))
        elif self.grown == 10:
            p = self.insert(now, self.panel_parent, panel_spec(self.rng))
            self.msglist = self.t.kids[p][1]
        else:
            for _ in range(5):
                self.insert(now, self.msglist, msg_spec(self.rng))
        self.grown += 1

    def m_shrink(self, now):
        st = self.t.kids[self.t.kids[self.body][-1]][3]
        self.delete(now, st)  # status banner disappears (usually silently)

    def focusables(self):
        out = []
        for root in (self.convlist, self.msglist):
            if root is not None and self.t.alive(root):
                out += [x for x in self.t.subtree(root) if self.t.role[x] == "button"]
        return out

    def m_focus(self, now):
        if self.rng.random() < 0.05:
            p = self.rng.choice(self.menu)
        else:
            f = self.focusables()
            if not f:
                return
            p = self.rng.choice(f)
        self.events.append((now + 0.001, "focus", p))

    def m_recycle(self, now):
        rows = self.t.kids[self.convlist]
        for r in self.rng.sample(rows, min(5, len(rows))):
            for x in self.t.subtree(r):
                if self.t.role[x] == "static" and self.rng.random() < 0.7:
                    self.rename(now, x, f"msg preview {self.rng.randrange(10**6)}")

    def m_newmsg(self, now):
        if self.msglist is not None and self.t.alive(self.msglist):
            self.insert(now, self.msglist, msg_spec(self.rng))

    def m_reorder(self, now):
        rows = self.t.kids[self.convlist]
        if len(rows) < 2:
            return
        r = self.rng.choice(rows[1:])
        if self.rng.random() < 0.5:  # recreate at top
            self.delete(now, r)
            self.insert(now, self.convlist, row_spec(self.rng, 0), 0)
        else:  # silent same-path move
            rows.remove(r)
            rows.insert(0, r)
            self.renamed[r] = now  # structural change without new paths

    def m_switch(self, now):
        old = self.t.kids[self.panel_parent]
        p = self.insert(now, self.panel_parent, panel_spec(self.rng))
        self.msglist = self.t.kids[p][1]
        for o in old:
            self.delete(now, o)

    def m_storm(self, now):
        if not hasattr(self, "live"):
            self.live = [x for x in self.t.subtree(self.doc) if self.t.role[x] == "static"][:50]
        p = self.rng.choice(self.live)
        if self.t.alive(p):
            self.rename(now, p, f"progress {self.rng.randrange(100)}")


class Mirror:
    def __init__(self):
        self.par, self.kids, self.name = {}, {}, {}
        self.seen = {}  # path -> time first present
        self.purged = {}  # path -> time removed

    def has(self, p):
        return p in self.par

    def subtree(self, p):
        out, st = [], [p]
        while st:
            x = st.pop()
            out.append(x)
            st.extend(reversed(self.kids.get(x, [])))
        return out

    def drop(self, p, now):
        for x in self.subtree(p):
            self.par.pop(x, None)
            self.kids.pop(x, None)
            self.name.pop(x, None)
            self.purged.setdefault(x, now)

    def put_snapshot(self, root, snap, now):
        """snap: dict path -> (parent, kids, name) for the subtree at root (truth at op start)."""
        if root in self.par:
            for c in list(self.kids.get(root, [])):
                if c not in snap:
                    self.drop(c, now)
            # descendants that left the subtree
            for x in self.subtree(root):
                if x not in snap and x in self.par:
                    self.drop(x, now)
        for x, (pa, ks, nm) in snap.items():
            if x != root or root not in self.par:
                self.par[x] = pa
            self.kids[x] = list(ks)
            self.name[x] = nm
            self.seen.setdefault(x, now)
        # nodes moved out of subtree into elsewhere are handled by their new parent's relist


def snapshot(t: Tree, root):
    return {x: (t.par[x], list(t.kids[x]), t.name[x]) for x in t.subtree(root)}


class Source:
    """Serial source actor with one op in flight."""

    def __init__(self, strat, world, rng, sweep_B=256, sweep_R=20.0):
        self.s, self.w, self.rng = strat, world, rng
        self.m = Mirror()
        self.q_hi, self.q_lo = [], []  # ops
        self.busy_until = 0.0
        self.calls = 0
        self.ui_time = 0.0
        self.nodes_fetched = 0
        self.dirty = {}  # path -> kind, coalesced per batch
        self.last_resync = {}
        self.pending_orphans = set()
        self.neg_cache = set()
        self.sweep_B, self.sweep_R = sweep_B, sweep_R
        self.sweep_idx = 0
        self.last_chunk = 1
        self.focus_log = []  # (t_event, path, hit, in_doc)
        self.max_queue = 0
        self.detect = defaultdict(int)  # sweep detector stats
        self.resyncs = 0

    def c0(self):
        return min(2e-3, max(0.1e-3, C0_MED * math.exp(0.6 * self.rng.gauss(0, 1))))

    def call(self, server=0.0):
        self.calls += 1
        self.ui_time += SERVER_CALL + server
        return self.c0() + server

    # ------------------------------------------------------------- ops
    def op_fetch_subtree(self, root, now):
        """GetTree(root) + role/state for new nodes. Returns (duration, apply-fn)."""
        t = self.w.t
        if not t.alive(root):
            return self.call(), None
        snap = snapshot(t, root)
        new = sum(1 for x in snap if not self.m.has(x))
        self.nodes_fetched += len(snap)
        d = self.call(BETA_TREE * len(snap)) + GAMMA2 * new
        self.ui_time += 0.4 * GAMMA2 * new  # server share of role/state passes
        self.calls += 2 * new
        return d, (lambda tn: self.m.put_snapshot(root, snap, tn))

    def op_relist(self, p, now):
        """GetChildren(p); GetTree for each new child; drop vanished children."""
        t = self.w.t
        if not t.alive(p):
            return self.call(), None
        d = self.call()
        ks = list(t.kids[p])
        snaps = {}
        for c in ks:
            if not self.m.has(c):
                s = snapshot(t, c)
                snaps.update(s)
                new = len(s)
                self.nodes_fetched += new
                d += self.call(BETA_TREE * new) * 0.3 + GAMMA2 * new  # pipelined over lanes
                self.calls += 2 * new
                self.ui_time += 0.4 * GAMMA2 * new
        nm = t.name[p]

        def apply(tn):
            if not self.m.has(p):
                return
            for c in list(self.m.kids.get(p, [])):
                if c not in ks:
                    self.m.drop(c, tn)
            for x, (pa, kk, n) in snaps.items():
                self.m.par[x], self.m.kids[x], self.m.name[x] = pa, list(kk), n
                self.m.seen.setdefault(x, tn)
            self.m.kids[p] = list(ks)
            self.m.name[p] = nm
        return d, apply

    def op_name(self, p, now):
        t = self.w.t
        if not t.alive(p):
            return self.call(), None
        n = t.name[p]
        return self.call(), (lambda tn: self.m.has(p) and self.m.name.__setitem__(p, n))

    def op_orphan(self, p, now):
        """Walk Parent from an unknown path to the lowest known ancestor, then resync it."""
        t = self.w.t
        if p in self.neg_cache or self.m.has(p) or not t.alive(p):
            return 0.0, None
        d, x = 0.0, p
        while x is not None and not self.m.has(x):
            d += self.call()
            x = t.par.get(x)
        if x is None:  # outside the document: exact negative cache (paths never reused)
            self.neg_cache.add(p)
            return d, None
        lka = x
        if now - self.last_resync.get(lka, -1.0) < 0.1:  # hysteresis: retry later
            self.pending_orphans.add(p)
            return d, None
        self.last_resync[lka] = now
        self.resyncs += 1
        d2, ap = self.op_fetch_subtree(lka, now)
        return d + d2, ap

    def op_sweep(self, now):
        """Validate one chunk (<= B nodes) of the mirror against truth."""
        m, t = self.m, self.w.t
        order = m.subtree(self.w.doc) if m.has(self.w.doc) else []
        if not order:
            return 0.0, None
        self.sweep_idx %= len(order)
        v = order[self.sweep_idx]
        size = len(m.subtree(v))
        if size > self.sweep_B:
            # frontier node: validate only its own child list and name (one GetChildren call)
            self.sweep_idx += 1
            d = self.call()
            if not t.alive(v):
                return d, None
            if list(t.kids[v]) == m.kids.get(v, []) and t.name[v] == m.name.get(v):
                return d, None
            self.detect["mismatch"] += 1
            self.detect["paths"] += 1
            return d, (lambda tn, v=v: self.q_hi.append(("relist", v)))
        while m.par.get(v) is not None and m.has(m.par[v]) and \
                len(m.subtree(m.par[v])) <= self.sweep_B and m.par[v] != self.w.frame:
            v = m.par[v]
            size = len(m.subtree(v))
        self.sweep_idx += max(1, size)
        self.last_chunk = size
        if not t.alive(v):
            par = m.par.get(v)
            return self.call(), (lambda tn: par is not None and self.m.has(par) and
                                 self._enqueue_hi(("resync", par)))
        snap = snapshot(t, v)
        old = {x: (m.par.get(x), list(m.kids.get(x, [])), m.name.get(x)) for x in m.subtree(v)}
        if self.s == "S2m":
            d = self.call(BETA_MATCH * len(snap))
            same_paths = list(snap) == list(old)
            if same_paths:
                return d, None
        else:
            d = self.call(BETA_TREE * len(snap))
        if snap == {x: (a, b, c) for x, (a, b, c) in old.items()}:
            return d, None
        # which cheaper detector would have caught it
        self.detect["mismatch"] += 1
        if len(t.kids[v]) != len(m.kids.get(v, [])):
            self.detect["root_childcount"] += 1
        if any(len(snap[x][1]) != len(old[x][1]) for x in snap if x in old):
            self.detect["any_childcount"] += 1
        if set(snap) != set(old) or any(snap[x][1] != old[x][1] for x in snap if x in old):
            self.detect["paths"] += 1
        new = sum(1 for x in snap if not m.has(x))
        self.calls += 2 * new
        self.nodes_fetched += len(snap)
        return d + GAMMA2 * new, (lambda tn: self.m.put_snapshot(v, snap, tn))

    def _enqueue_hi(self, op):
        self.q_hi.append(op)

    # ------------------------------------------------------------- event intake
    def on_event(self, now, kind, p):
        if kind == "focus":
            hit = self.m.has(p)
            self.focus_log.append((now, p, hit, self.w.t.in_doc(p)))
            if not hit and self.s != "S0" and self.s != "S3":
                self.q_hi.append(("orphan", p))
            return
        if not self.m.has(p):
            if self.s in ("S1", "S2", "S2m"):
                self.q_hi.append(("orphan", p))
            return  # S0/S3: dropped (to_atspi returns None for unknown locals)
        k = {"add": "relist", "rm": "relist", "name": "name"}[kind]
        prev = self.dirty.get(p)
        self.dirty[p] = "relist" if "relist" in (prev, k) else k

    def flush_dirty(self):
        for p, k in self.dirty.items():
            self.q_lo.insert(0, (k, p)) if False else self.q_hi.append((k, p))
        self.dirty.clear()

    def next_op(self, now):
        q = self.q_hi
        # dedupe the high queue (coalescing): one op per (kind, path)
        seen, dq = set(), []
        for op in q:
            if op not in seen:
                seen.add(op)
                dq.append(op)
        self.q_hi = dq
        self.max_queue = max(self.max_queue, len(dq))
        if dq:
            return dq.pop(0)
        if self.pending_orphans:
            return ("orphan", self.pending_orphans.pop())
        return None

    def run_op(self, op, now):
        kind, arg = op
        if kind == "relist":
            return self.op_relist(arg, now)
        if kind == "name":
            return self.op_name(arg, now)
        if kind == "orphan":
            return self.op_orphan(arg, now)
        if kind == "resync":
            self.resyncs += 1
            return self.op_fetch_subtree(arg, now)
        if kind == "full":
            self.resyncs += 1
            return self.op_fetch_subtree(self.w.doc, now)
        if kind == "sweep":
            return self.op_sweep(now)
        raise ValueError(kind)


def simulate(strat, seed, T=120.0, P=None, storm=False, sweep_B=256, sweep_R=20.0):
    P = P or {"add": 0.97, "rm": 0.4, "name": 0.95}
    rng = random.Random(seed)
    w = World(rng, P, storm)
    src = Source(strat, w, random.Random(seed + 1), sweep_B, sweep_R)
    # initial build at 50 ms (the 4-node loading shell)
    h = []
    seq = 0

    def push(t, kind, payload=None):
        nonlocal seq
        seq += 1
        heapq.heappush(h, (t, seq, kind, payload))

    for (t, fn) in w.script(T):
        push(t, "mut", fn)
    push(0.05, "op", ("full", None))
    push(0.005, "tick", None)
    if strat in ("S2", "S2m"):
        push(6.0, "sweep", None)
    if strat == "S3":
        push(2.0, "periodic", None)
    push(0.1, "sample", None)
    busy = False
    ev_ptr = 0
    div_samples = []
    while h:
        t, _, kind, payload = heapq.heappop(h)
        if t > T:
            break
        if kind == "mut":
            payload(t)
            # deliver new events
            while ev_ptr < len(w.events):
                et, ek, ep = w.events[ev_ptr]
                push(et, "ev", (ek, ep))
                ev_ptr += 1
        elif kind == "ev":
            src.on_event(t, *payload)
        elif kind == "tick":
            src.flush_dirty()
            push(t + 0.005, "tick", None)
        elif kind == "periodic":
            src.q_hi.append(("full", None))
            push(t + 2.0, "periodic", None)
        elif kind == "sweep":
            if not src.q_hi and not busy:
                src.q_hi.append(("sweep", None))
            # budget in nodes/s (browser UI-thread share), at most sweep_R calls/s
            push(t + max(1.0 / sweep_R, src.last_chunk / SWEEP_NODES_PER_S), "sweep", None)
            src.last_chunk = 1
        elif kind == "op":
            src.q_hi.insert(0, payload)
        elif kind == "done":
            ap = payload
            if ap is not None:
                ap(t)
            busy = False
        elif kind == "sample":
            truth = set(w.t.subtree(w.doc))
            mir = set(src.m.subtree(w.doc)) if src.m.has(w.doc) else set()
            wrong = sum(1 for x in truth & mir if src.m.name.get(x) != w.t.name[x])
            div_samples.append((t, len(truth), len(truth - mir), len(mir - truth), wrong))
            push(t + 0.1, "sample", None)
        if not busy:
            op = src.next_op(t)
            if op is not None:
                d, ap = src.run_op(op, t)
                busy = True
                push(t + max(d, 1e-6), "done", ap)
    return w, src, div_samples


def pct(xs, q):
    if not xs:
        return float("nan")
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))]


NEVER = 1e3


def raw(w, src, div, T):
    """Raw per-run samples; pooled over seeds by run_matrix."""
    r = defaultdict(list)
    for (t, p, h, ind) in src.focus_log:
        if not ind or t <= 6.0:
            continue
        r["focus_hit"].append(1.0 if h else 0.0)
        if not h:
            s = src.m.seen.get(p)
            r["miss_tth"].append(s - t if s is not None and s >= t else NEVER)
    for d in div:
        if d[0] > 6.0:
            r["missing"].append(d[2] / d[1])
            r["extra"].append(d[3] / d[1])
            r["wrong"].append(d[4] / d[1])
    for p, tc in w.created.items():
        if tc < 6.0 or tc > T - 10:
            continue
        s = src.m.seen.get(p)
        if s is not None and s >= tc:
            v = s - tc
        elif p in w.deleted:
            continue
        else:
            v = NEVER
        r["create_lat"].append(v)
        if p in w.silent_new:
            r["silent_create_lat"].append(v)
    for p, td in w.deleted.items():
        if td < 6.0 or td > T - 10 or p not in w.silent_del:
            continue
        if p not in src.m.seen:
            continue
        pu = src.m.purged.get(p)
        r["silent_del_lat"].append(pu - td if pu is not None and pu >= td else NEVER)
    r["calls_per_s"].append(src.calls / T)
    r["ui_ms_per_s"].append(src.ui_time / T * 1e3)
    r["nodes_per_s"].append(src.nodes_fetched / T)
    r["resyncs"].append(src.resyncs)
    r["max_queue"].append(src.max_queue)
    r["final_nodes"].append(div[-1][1])
    r["stuck"].append(1.0 if div[-1][2] / div[-1][1] > 0.5 else 0.0)
    return r


def mean(xs):
    return sum(xs) / len(xs) if xs else float("nan")


def run_matrix(seeds, T, Ps, storm=False, strats=("S0", "S1", "S2", "S2m", "S3"), **kw):
    rows = []
    for pname, P in Ps:
        for strat in strats:
            acc = defaultdict(list)
            det = defaultdict(int)
            for sd in range(seeds):
                w, src, div = simulate(strat, 1000 + sd, T, P, storm, **kw)
                for k, v in raw(w, src, div, T).items():
                    acc[k] += v
                for k, v in src.detect.items():
                    det[k] += v
            row = {"P": pname, "strat": strat, "detect": dict(det)}
            row["focus_hit"] = mean(acc["focus_hit"])
            row["stuck_runs"] = mean(acc["stuck"])
            row["miss_tth_p50_ms"] = pct(acc["miss_tth"], 0.5) * 1e3
            row["miss_tth_p95_ms"] = pct(acc["miss_tth"], 0.95) * 1e3
            for k in ("missing", "extra", "wrong"):
                row[k + "_frac"] = mean(acc[k])
            for k in ("create_lat", "silent_create_lat", "silent_del_lat"):
                row[k + "_p50_ms"] = pct(acc[k], 0.5) * 1e3
                row[k + "_p95_ms"] = pct(acc[k], 0.95) * 1e3
                row[k + "_never"] = mean([1.0 if x >= NEVER else 0.0 for x in acc[k]])
            for k in ("calls_per_s", "ui_ms_per_s", "nodes_per_s", "resyncs", "max_queue",
                      "final_nodes"):
                row[k] = mean(acc[k])
            rows.append(row)
    return rows


COLS = ["P", "strat", "focus_hit", "stuck_runs", "miss_tth_p50_ms", "miss_tth_p95_ms",
        "missing_frac", "wrong_frac", "silent_create_lat_p50_ms", "silent_create_lat_p95_ms",
        "silent_create_lat_never", "silent_del_lat_p95_ms", "silent_del_lat_never",
        "calls_per_s", "ui_ms_per_s", "resyncs", "max_queue", "final_nodes"]


def fmt(rows, cols=COLS):
    out = ["| " + " | ".join(cols) + " |", "|" + "---|" * len(cols)]
    for r in rows:
        cells = []
        for c in cols:
            v = r[c]
            cells.append(f"{v:.3g}" if isinstance(v, float) else str(v))
        out.append("| " + " | ".join(cells) + " |")
    return "\n".join(out)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", type=int, default=8)
    ap.add_argument("--T", type=float, default=120.0)
    ap.add_argument("--storm", action="store_true")
    ap.add_argument("--sweep-B", type=int, default=512)
    ap.add_argument("--sweep-R", type=float, default=50.0)
    ap.add_argument("--sweep-rate", type=float, default=2000.0)
    ap.add_argument("--rows", type=int, default=50)
    ap.add_argument("--strats", default="S0,S1,S2,S2m,S3")
    ap.add_argument("--P", default="ideal,chromium,lossy")
    a = ap.parse_args()
    ROWS = a.rows
    SWEEP_NODES_PER_S = a.sweep_rate
    Ps = [("ideal", {"add": 1.0, "rm": 1.0, "name": 1.0}),
          ("chromium", {"add": 0.97, "rm": 0.4, "name": 0.95}),
          ("lossy", {"add": 0.8, "rm": 0.2, "name": 0.8})]
    Ps = [x for x in Ps if x[0] in a.P.split(",")]
    rows = run_matrix(a.seeds, a.T, Ps, a.storm, strats=tuple(a.strats.split(",")),
                      sweep_B=a.sweep_B, sweep_R=a.sweep_R)
    print(fmt(rows))
    for r in rows:
        if r["detect"]:
            print(r["P"], r["strat"], "sweep detectors:", r["detect"])
