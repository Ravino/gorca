#!/usr/bin/env python3
"""Fusion memory/latency trade-off model (docs/reports/semantic-fusion-math.md §11).

Reproduce:  . ./env.sh && OMP_NUM_THREADS=1 python research/fuse-mem/model.py all
Sections: mem | s0cache | reader | queue | delta | all.  Deterministic (fixed seeds).

Inputs that are measured (cited in the report): type sizes and s0/fuse costs from
research/fuse-mem/src/main.rs, RSS from bench/m22a/results/*-fuse.json. Everything else is a
model parameter printed with the result.
"""
import gzip
import json
import math
import sys
from collections import OrderedDict
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parents[2]
TREES = ROOT / "research/web-graph/real-trees"
RNG = np.random.default_rng(20261019)


# ------------------------------------------------------------------------------------------------
# Real tree shapes (browser dumps: [parent, role, name_len, states, bbox, attrs, nchildren, id])
# ------------------------------------------------------------------------------------------------
def load_tree(name):
    p = TREES / name
    op = gzip.open if name.endswith(".gz") else open
    with op(p, "rt") as f:
        d = json.load(f)
    nodes = d["nodes"]
    parent = np.array([n[0] for n in nodes], dtype=np.int64)
    role = [n[1] for n in nodes]
    name_len = np.array([n[2] for n in nodes])
    y = np.array([n[4][1] if n[4] else 0 for n in nodes], dtype=np.int64)
    return parent, role, name_len, y


def tile(parent, role, name_len, y, n_target):
    """Repeat the page body under one root until n_target nodes (a long document)."""
    n0 = len(parent)
    reps = max(1, math.ceil(n_target / n0))
    P, R, L, Y = [], [], [], []
    page_h = int(y.max() - y.min() + 1)
    for k in range(reps):
        off = k * n0
        pp = parent.copy()
        pp[pp >= 0] += off
        if k > 0:
            pp[0] = 0  # hang each copy's root under the first root
        P.append(pp)
        R += role
        L.append(name_len)
        Y.append(y + k * page_h)
    P = np.concatenate(P)[:n_target]
    return P, R[:n_target], np.concatenate(L)[:n_target], np.concatenate(Y)[:n_target]


def children_lists(parent):
    ch = [[] for _ in range(len(parent))]
    for i, p in enumerate(parent):
        if p >= 0:
            ch[p].append(i)
    return ch


def depth_of(parent):
    d = np.zeros(len(parent), dtype=np.int64)
    for i in range(1, len(parent)):  # dumps are in preorder: parent index < child index
        p = parent[i]
        d[i] = d[p] + 1 if p >= 0 else 0
    return d


CATS = {
    "heading": {"heading"},
    "link": {"link"},
    "landmark": {"landmark"},
    "form": {"entry", "check box", "radio button", "combo box", "push button", "button", "spin button"},
    "list": {"list"},
    "table": {"table"},
}


# ------------------------------------------------------------------------------------------------
# 1. Memory model
# ------------------------------------------------------------------------------------------------
GLIBC = lambda n: max(32, ((n + 8 + 15) // 16) * 16)  # chunk size of a malloc(n) on glibc x86_64


def vec_cap(k):
    """Vec capacity after k single inserts from empty (Rust RawVec: 4 for 32-byte T, then x2)."""
    if k == 0:
        return 0
    c = 4
    while c < k:
        c *= 2
    return c


def per_node_bytes(design, regs_k, absent_k, read_groups=1, delta_frac=0.03, idx_b=60):
    """Return dict of bytes per node for one design.

    regs_k: observed registers per node (one primary); absent_k of them are `Absent`.
    idx_b: hash indices (exact anchors, dom ids, children vec heap) per node, fitted to perfA.
    """
    entry = GLIBC(128 + 16)  # Arc<SemEntry>: 160 B (measured 16.02 MB / 100k)
    regs_heap = GLIBC(32 * vec_cap(regs_k)) if regs_k else 0
    regs_heap_exact = GLIBC(32 * regs_k) if regs_k else 0
    out = {"entry": entry}
    if design == "A_b6":  # baseline B6: full S0 inline (1008 B), Node rest ~300 B
        out.update(node=300 + 1008, regs=regs_heap, s0_heap=0, idx=idx_b)
    elif design == "B_proj":  # engineer branch: S0 projected to selector-read groups, Vec heap
        out.update(node=300, regs=regs_heap, s0_heap=GLIBC(48 * read_groups), idx=idx_b)
    elif design == "C_delta":  # S0 = rebuild(entry) + sparse delta; Node diet; exact-cap regs
        out.update(node=176, regs=regs_heap_exact, s0_heap=delta_frac * (GLIBC(32 + 8) + 24),
                   idx=idx_b)
    elif design == "D_compact":  # C + absent runs + thin Arc names (Reg 24 B) + SoA topology
        vals = regs_k - absent_k
        regs_c = (GLIBC(24 * vals) if vals else 0) + 16  # one (seq, mask) absent run inline
        out.update(node=128, regs=regs_c, s0_heap=delta_frac * (GLIBC(32 + 8) + 24), idx=idx_b)
    else:
        raise ValueError(design)
    out["total"] = sum(out.values())
    return out


def sec_mem():
    print("== 1. memory model (bytes per node; MB at N nodes; one primary source) ==")
    # Calibrate: perfA (B_proj, bench world 4 regs, role-only selector reads) measured 687 B/node.
    b = per_node_bytes("B_proj", 4, 0)
    print(f"calibration B_proj bench-world: model {b['total']} B/node vs measured 687 (perfA) ;"
          f" A_b6 model {per_node_bytes('A_b6', 4, 0)['total']} vs measured 1539 (B6 e)")
    for label, k, a in [("bench world (4 regs)", 4, 0), ("AT-SPI full state read (14 regs, 9 absent)", 14, 9),
                        ("rich ARIA widget (18 regs, 10 absent)", 18, 10)]:
        print(f"-- {label}")
        for d in ["A_b6", "B_proj", "C_delta", "D_compact"]:
            r = per_node_bytes(d, k, a)
            mb = [r["total"] * n / 1e6 for n in (10_000, 100_000, 500_000)]
            parts = " ".join(f"{kk}={vv:.0f}" for kk, vv in r.items() if kk != "total")
            print(f"  {d:10s} {r['total']:6.0f} B/node  10k {mb[0]:6.1f}  100k {mb[1]:6.1f}  500k {mb[2]:6.1f} MB   [{parts}]")


# ------------------------------------------------------------------------------------------------
# 2. S0 neighbour-read cache (alternative to the delta design)
# ------------------------------------------------------------------------------------------------
class LRU:
    def __init__(self, cap):
        self.cap, self.d, self.hit, self.miss = cap, OrderedDict(), 0, 0

    def get(self, k):
        if k in self.d:
            self.d.move_to_end(k)
            self.hit += 1
            return True
        self.miss += 1
        self.d[k] = 1
        if len(self.d) > self.cap:
            self.d.popitem(last=False)
        return False


def sec_s0cache():
    print("== 2. S0 cache for neighbour reads (LRU), storm trace on a real-shape 100k tree ==")
    parent, role, nl, y = tile(*load_tree("chrome__static-large10000__initial.json.gz"), 100_000)
    ch = children_lists(parent)
    n = len(parent)
    fan = np.array([len(c) for c in ch])
    print(f"tree: n={n} internal={np.mean(fan>0):.2f} mean fanout(internal)={fan[fan>0].mean():.1f} "
          f"p99 fanout={np.percentile(fan[fan>0],99):.0f}")
    hot = RNG.choice(n, 2000, replace=False)
    ev = 200_000
    is_hot = RNG.random(ev) < 0.7
    tgt = np.where(is_hot, hot[RNG.integers(0, 2000, ev)], RNG.integers(0, n, ev))
    s0_cost_us = (0.25, 0.54)  # measured s0_with cold p50, rich/bench (worktree branch)
    for reads in ("parent", "parent+children"):
        nread = 0
        caches = {c: LRU(c) for c in (1_000, 4_000, 16_000, 64_000)}
        for x in tgt:
            keys = []
            p = parent[x]
            if p >= 0:
                keys.append(p)
            if reads == "parent+children":
                keys += ch[x][:256]
            nread += len(keys)
            for c in caches.values():
                for k in keys:
                    c.get(k)
        rpf = nread / ev
        row = []
        for cap, c in caches.items():
            h = c.hit / max(1, c.hit + c.miss)
            add = rpf * (1 - h)
            row.append(f"C={cap//1000}k hit {h:.2f} misses/fusion {add:.2f} (+{add*s0_cost_us[0]:.2f}..{add*s0_cost_us[1]:.2f} us)")
        print(f"  reads={reads:16s} reads/fusion {rpf:.2f}: " + "; ".join(row))
    print("  delta design: 0 recomputes; rebuild of |R| slots from the entry ~5-20 ns per read (no misses)")


# ------------------------------------------------------------------------------------------------
# 3. Reader access patterns: lazily materialised entries (evictable) vs resident
# ------------------------------------------------------------------------------------------------
def sec_reader():
    print("== 3. reader access patterns vs an evictable entry cache (LRU), real-shape trees ==")
    for N in (10_000, 100_000, 500_000):
        parent, role, nl, y = tile(*load_tree("chrome__static-large10000__initial.json.gz"), N)
        dep = depth_of(parent)
        n = len(parent)
        cat_idx = {c: np.array([i for i, r in enumerate(role) if r in s]) for c, s in CATS.items()}
        focusable = np.concatenate([cat_idx["link"], cat_idx["form"]])
        focusable.sort()
        # reading order = preorder index; a "line" = node with text (name_len>0) or a leaf
        keys = 20_000
        cur = 0
        trace = []  # list of node-sets read per keypress

        def chain(x, cap=12):
            out = []
            while x >= 0 and len(out) < cap:
                out.append(x)
                x = parent[x]
            return out

        for _ in range(keys):
            u = RNG.random()
            if u < 0.55:  # sequential browse: next/prev line
                cur = min(n - 1, max(0, cur + (1 if RNG.random() < 0.9 else -1)))
            elif u < 0.80:  # quick nav next(Cat)
                c = RNG.choice(["heading", "link", "landmark", "form", "list"],
                               p=[0.4, 0.25, 0.1, 0.2, 0.05])
                arr = cat_idx[c]
                if len(arr):
                    j = np.searchsorted(arr, cur, side="right")
                    cur = int(arr[j]) if j < len(arr) else int(arr[0])
            elif u < 0.95:  # Tab: next focusable, Zipf over the visible window's focusables
                j = np.searchsorted(focusable, cur, side="right")
                k = min(len(focusable) - 1, j + int(RNG.zipf(2.0)) - 1)
                cur = int(focusable[k])
            else:  # find / elements-list jump: uniform
                cur = int(RNG.integers(0, n))
            trace.append(chain(cur))
        reads_per_key = np.mean([len(t) for t in trace])
        res = []
        for frac in (0.01, 0.05, 0.2):
            cap = max(256, int(frac * n))
            c = LRU(cap)
            miss_per_key = []
            for t in trace:
                m0 = c.miss
                for x in t:
                    c.get(int(x))
                miss_per_key.append(c.miss - m0)
            mk = np.array(miss_per_key[2000:])  # skip warm-up
            res.append(f"cache {frac:.0%} ({cap}): hit {c.hit/(c.hit+c.miss):.3f}, misses/key mean {mk.mean():.2f} "
                       f"p99 {np.percentile(mk,99):.0f} -> +{np.percentile(mk,99)*3.0:.0f} us p99")
        print(f" N={N}: reads/keypress {reads_per_key:.1f}")
        for r in res:
            print("   " + r)
    print(" (miss cost 3 us = one cold fusion incl. bookkeeping; elements list of k items costs k misses)")


# ------------------------------------------------------------------------------------------------
# 4. Queueing: per-event cost distribution -> latency of a priority event
# ------------------------------------------------------------------------------------------------
def lognorm_from(p50, p99, size):
    s = math.log(p99 / p50) / 2.3263
    return np.exp(np.log(p50) + s * RNG.standard_normal(size))


def sim_queue(rate, p50, p99, horizon_s=2.0, prio_frac=0.01, policy="prio", slice_us=100.0,
              publish="incremental", burst=0):
    """Single writer thread. Events Poisson(rate) + optional burst at t=0.5 s. Non-preemptive.

    policy: 'fifo' or 'prio' (priority events first, by arrival). The writer runs in slices of
    `slice_us`; with publish='end' a fused priority node is published at the slice end,
    'incremental' publishes right after its own fusion. Returns priority sojourn times (us).
    """
    n = RNG.poisson(rate * horizon_s)
    t = np.sort(RNG.random(n) * horizon_s * 1e6)
    prio = RNG.random(n) < prio_frac
    if burst:  # a page-load storm: `burst` non-priority events at once (e.g. children-changed)
        t = np.concatenate([t, np.full(burst, 0.5e6)])
        prio = np.concatenate([prio, np.zeros(burst, dtype=bool)])
        o = np.argsort(t, kind="stable")
        t, prio = t[o], prio[o]
    n = len(t)
    svc = lognorm_from(p50, p99, n)
    import heapq
    from collections import deque
    hp, lp = [], deque()
    i, clock, out = 0, 0.0, []
    slice_end, pending_pub = None, []
    while i < n or hp or lp:
        if not hp and not lp:
            clock = max(clock, t[i])
        while i < n and t[i] <= clock:
            (heapq.heappush(hp, (t[i], i)) if (prio[i] and policy == "prio") else lp.append(i))
            i += 1
        if slice_end is None:
            slice_end = clock + slice_us
        if hp:
            _, j = heapq.heappop(hp)
        elif lp:
            j = lp.popleft()
        else:
            continue
        clock += svc[j]
        if prio[j]:
            if publish == "incremental":
                out.append(clock - t[j])
            else:
                pending_pub.append(j)
        if clock >= slice_end or (not hp and not lp and (i >= n or t[i] > clock)):
            for k in pending_pub:
                out.append(clock - t[k])
            pending_pub, slice_end = [], None
    return np.array(out)


def sec_queue():
    print("== 4. queueing: priority event -> published (us), single writer ==")
    print("   service per event lognormal fitted to (p50, p99); 1% priority events")
    for (p50, p99, tag) in [(1.4, 5.1, "perfA clean"), (2.65, 7.8, "perfA +5k facts/s"),
                            (3.2, 13.0, "B6 worst")]:
        for rate in (10_000, 50_000):
            row = []
            for pol, pub, burst in [("fifo", "incremental", 0), ("prio", "incremental", 0),
                                    ("fifo", "incremental", 20_000), ("prio", "incremental", 20_000),
                                    ("prio", "end", 20_000)]:
                s = sim_queue(rate, p50, p99, horizon_s=1.0, policy=pol, publish=pub, burst=burst,
                              slice_us=2000.0 if pub == "end" else 100.0)
                row.append(f"{pol}/{pub[:3]}/{'burst' if burst else 'poiss'} p99 {np.percentile(s,99):8.1f}")
            rho = rate * (lognorm_from(p50, p99, 200_000).mean()) * 1e-6
            print(f"  {tag:18s} {rate//1000:2d}k ev/s rho={rho:.3f}: " + " | ".join(row))


# ------------------------------------------------------------------------------------------------
# 5. Expected S0-delta fraction from tree statistics (name fill from observed-empty)
# ------------------------------------------------------------------------------------------------
def sec_delta():
    print("== 5. S0 delta fraction estimates ==")
    for f in ["chrome__static-large10000__initial.json.gz", "firefox__static-large2000__initial.json",
              "chrome__react-grid__initial.json", "chrome__react-form__initial.json"]:
        try:
            parent, role, nl, y = load_tree(f)
        except FileNotFoundError:
            continue
        n = len(parent)
        nameable = {"link", "push button", "button", "heading", "check box", "radio button",
                    "menu item", "tab", "list item", "cell", "table cell", "label"}
        fill = sum(1 for r, l in zip(role, nl) if r in nameable and l == 0)
        print(f"  {f:48s} n={n:6d}: content-nameable with empty observed name {fill/n:.3%}"
              f" (delta only if `name` is a selector-read group)")
    print("  oracle (§10.3) non-observed winners over all cells: (User 713 + Derived 320 + Inferred 312 +"
          " Patch 127 + DerivedTainted 62) / 38,844 = 3.9 % of cells (stress mix, an upper bound)")


def main():
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    for name, fn in [("mem", sec_mem), ("s0cache", sec_s0cache), ("reader", sec_reader),
                     ("queue", sec_queue), ("delta", sec_delta)]:
        if which in (name, "all"):
            fn()
            print()


if __name__ == "__main__":
    main()
