#!/usr/bin/env python3
"""semantic_fusion: reference model of gorca's semantic fusion point (ADR-0022 companion).

Research prototype for docs/reports/semantic-fusion-math.md. Pure Python, no dependencies.

  python tools/research/semantic_fusion.py selftest     # test vectors + all properties, small
  python tools/research/semantic_fusion.py order        # order-independence experiment (+ naive baseline)
  python tools/research/semantic_fusion.py ivm          # incremental vs from-scratch, work per event
  python tools/research/semantic_fusion.py override     # override dominance / locality / reversibility
  python tools/research/semantic_fusion.py match        # cross-connector node matching precision/recall
  python tools/research/semantic_fusion.py all          # sections 1-10 (report section 6)
  python tools/research/semantic_fusion.py selftest2    # ADR-0024 r2 sources: TV7-TV20 + small F-runs
  python tools/research/semantic_fusion.py sources      # F1, F11-F19 on random workloads, both layouts
  python tools/research/semantic_fusion.py corpus [--out tests/fixtures/fuse]   # JSON trace corpus
  python tools/research/semantic_fusion.py all2         # selftest2 + sources (report section 10)

Model (see report section 1)
----------------------------
Inputs are *facts*, each owned by one register:
  observed  (src, node, group)  -> max by (seq, value)        src in {atspi, newton}
  lifecycle (src, node)         -> max by (seq, alive)
  invalid   (src, node)         -> max seq                     (stale marker; value kept)
  patch     (patch_id)          -> max by (seq, installed)
  user      (node, group)       -> max by (seq, value|CLEAR)
Every register is a max over a total order, i.e. a join-semilattice: applying any permutation of
any multiset of the same events yields the same register state (ACI). The effective semantics is a
pure function FUSE(registers, static tables) evaluated in strata:
  S0 per-source record -> quirk(src) -> observed winner per group by trust table PI[app][group]
  S1 add-on patches: selectors read S0 only (cascade rule), winner by (specificity, ext_id)
  S2 user overrides
  S3 derived (doc mirror): name-from-content over children's *final* names, lowest precedence
  R  repair: exclusive/not-applicable states canonicalised (SM-2/SM-5), idempotent
Precedence for a group: user > patch > observed > derived > default.
"""
from __future__ import annotations

import argparse
import json
import random
import sys
import time
from collections import Counter, defaultdict

# ------------------------------------------------------------------------------------------------
# 1. Vocabulary
# ------------------------------------------------------------------------------------------------

UNKNOWN = "?"        # never observed (lazy fetch not done) -- not a value
ABSENT = "-"         # observed, property not present -- a value
CLEAR = "<clear>"    # user/patch removal marker

ROLES = ["generic", "button", "checkbox", "link", "img", "heading", "text", "group", "listitem"]
CHECKABLE = {"checkbox"}
DISABLEABLE = {"button", "checkbox", "link"}
EXPANDABLE = {"button", "link", "listitem"}
CONTENT_NAMED = {"button", "link", "heading", "listitem", "checkbox"}
NAMES = ["", "", "Close", "Save", "Menu", "Item", "Logo", "Open"]

DOMAIN = {
    "role": ROLES,
    "name": NAMES,
    "check": ["na", "off", "on", "mixed"],
    "avail": ["na", "enabled", "disabled"],
    "level": [ABSENT, 1, 2, 3],
    "expand": ["na", "collapsed", "expanded"],
}
GROUPS = list(DOMAIN)
DEFAULT = {"role": "generic", "name": "", "check": "na", "avail": "na", "level": ABSENT, "expand": "na"}
OBS = ("atspi", "newton")

# Trust table PI[app_kind][group] -> observed sources in priority order (data, per app profile).
PI = {
    "firefox": {g: ("atspi", "newton") for g in GROUPS},
    "gtk-newton": {g: ("newton", "atspi") for g in GROUPS},
}
# Per-group exception to show the table is per (app, group): Newton's level is the author's value.
PI["firefox"] = dict(PI["firefox"], level=("newton", "atspi"))


def vkey(v):
    """Total order on values for deterministic tie-breaks (type tag first)."""
    return (type(v).__name__, str(v))


# ------------------------------------------------------------------------------------------------
# 2. Quirks (stratum -1: a pure function of one source's record) and repair R
# ------------------------------------------------------------------------------------------------

def quirk(src, rec):
    """Returns (record', hits). Applied per source before cross-source fusion (ADR-0008 tables)."""
    hits = {}
    if src == "atspi":
        r = dict(rec)
        # Q1 DropStaleChecked: engine reports checked on a role that cannot be checked.
        if r.get("check") in ("on", "mixed") and r.get("role") not in CHECKABLE and r.get("role") != UNKNOWN:
            r["check"] = ABSENT
            hits["check"] = "quirk:Q1-drop-stale-checked"
        # Q2 FixXmlRole: generic with a level is a heading (xml-roles lost).
        if r.get("role") == "generic" and r.get("level") not in (UNKNOWN, ABSENT, None):
            r["role"] = "heading"
            hits["role"] = "quirk:Q2-fix-xml-role"
        return r, hits
    return rec, hits


def repair(sem):
    """R: canonicalise role-dependent groups. Idempotent; returns (sem', masked groups)."""
    s = dict(sem)
    masked = {}
    role = s["role"]
    if role not in CHECKABLE and s["check"] != "na":
        masked["check"] = s["check"]
        s["check"] = "na"
    if role in CHECKABLE and s["check"] == "na":
        masked["check"] = s["check"]
        s["check"] = "off"
    if role not in DISABLEABLE and s["avail"] != "na":
        masked["avail"] = s["avail"]
        s["avail"] = "na"
    if role in DISABLEABLE and s["avail"] == "na":
        masked["avail"] = s["avail"]
        s["avail"] = "enabled"
    if role not in EXPANDABLE and s["expand"] != "na":
        masked["expand"] = s["expand"]
        s["expand"] = "na"
    if role != "heading" and s["level"] != ABSENT:
        masked["level"] = s["level"]
        s["level"] = ABSENT
    return s, masked


# ------------------------------------------------------------------------------------------------
# 3. Registers (the only mutable state; each a join-semilattice)
# ------------------------------------------------------------------------------------------------

class Registers:
    def __init__(self):
        self.obs = {}      # (src, x, g) -> (seq, value)
        self.life = {}     # (src, x) -> (seq, alive)
        self.inval = {}    # (src, x) -> seq
        self.patch = {}    # pid -> (seq, installed)
        self.user = {}     # (x, g) -> (seq, value|CLEAR)

    @staticmethod
    def _join(d, k, item):
        """Max by (seq, vkey(value)). Returns True iff the register changed."""
        cur = d.get(k)
        if cur is None or (item[0], vkey(item[1])) > (cur[0], vkey(cur[1])):
            d[k] = item
            return True
        return False

    def apply(self, ev):
        """Apply one event. Returns the set of nodes whose inputs changed ('*' = all)."""
        kind = ev[0]
        if kind == "obs":
            _, src, x, g, seq, v = ev
            return {x} if self._join(self.obs, (src, x, g), (seq, v)) else set()
        if kind == "life":
            _, src, x, seq, alive = ev
            return {x} if self._join(self.life, (src, x), (seq, alive)) else set()
        if kind == "inval":
            _, src, x, seq = ev
            cur = self.inval.get((src, x), -1)
            if seq > cur:
                self.inval[(src, x)] = seq
                return {x}
            return set()
        if kind == "patch":
            _, pid, seq, inst = ev
            return {"*"} if self._join(self.patch, pid, (seq, inst)) else set()
        if kind == "user":
            _, x, g, seq, v = ev
            return {x} if self._join(self.user, (x, g), (seq, v)) else set()
        raise ValueError(kind)

    def snapshot(self):
        return (tuple(sorted(self.obs.items(), key=repr)), tuple(sorted(self.life.items(), key=repr)),
                tuple(sorted(self.inval.items(), key=repr)), tuple(sorted(self.patch.items(), key=repr)),
                tuple(sorted(self.user.items(), key=repr)))


# ------------------------------------------------------------------------------------------------
# 4. Patches (ADR-0019 node patches as typed edits; cascade: selectors read stratum S0)
# ------------------------------------------------------------------------------------------------

class Patch:
    """select: role == sel_role [and base name == ''] [and parent base role == sel_parent]
    action: (group, const) or ('name', 'firstchild') = base name of the first child."""

    def __init__(self, pid, ext_id, spec, sel_role, empty_name, sel_parent, group, value):
        self.pid, self.ext_id, self.spec = pid, ext_id, spec
        self.sel_role, self.empty_name, self.sel_parent = sel_role, empty_name, sel_parent
        self.group, self.value = group, value

    def rank(self):
        # Higher specificity wins; ties broken by ext_id ascending (deterministic, order-free).
        return (-self.spec, self.ext_id, self.pid)

    def matches(self, base_x, base_parent):
        if base_x is None or base_x["role"] != self.sel_role:
            return False
        if self.empty_name and base_x["name"] not in ("", ABSENT):
            return False
        if self.sel_parent is not None and (base_parent is None or base_parent["role"] != self.sel_parent):
            return False
        return True


def random_patches(rng, n):
    ps = []
    for i in range(n):
        g = rng.choice(["name", "name", "role", "level", "avail"])
        if g == "name":
            val = rng.choice(["firstchild", "Patched", "Close"])
        elif g == "role":
            val = rng.choice(["button", "link", "heading"])
        else:
            val = rng.choice(DOMAIN[g][1:])
        ps.append(Patch(f"p{i}", f"org.ext{rng.randrange(3)}", rng.randrange(2),
                        rng.choice(["button", "img", "generic", "link"]), rng.random() < 0.5,
                        rng.choice([None, None, "group", "listitem"]), g, val))
    return ps


# ------------------------------------------------------------------------------------------------
# 5. FUSE: pure function of (registers, tree, tables) -> effective semantics + provenance trail
# ------------------------------------------------------------------------------------------------

class World:
    def __init__(self, parent, app_kind, patches):
        self.parent = parent                       # parent[x] (root: -1); static structure
        self.n = len(parent)
        self.children = defaultdict(list)
        for x, p in enumerate(parent):
            if p >= 0:
                self.children[p].append(x)
        self.depth = [0] * self.n
        for x in range(self.n):
            p, d = parent[x], 0
            while p >= 0:
                d, p = d + 1, parent[p]
            self.depth[x] = d
        self.app = app_kind
        self.patches = {p.pid: p for p in patches}

    # --- stratum S0: per-source records, quirks, observed winner --------------------------------
    def source_record(self, R, src, x):
        rec = {g: R.obs.get((src, x, g), (None, UNKNOWN))[1] for g in GROUPS}
        return quirk(src, rec)

    def alive(self, R, x):
        """Existence = lifecycle of the highest-priority source that has an opinion."""
        for src in PI[self.app]["role"]:
            lf = R.life.get((src, x))
            if lf is not None:
                return lf[1], src
        return False, None

    def base(self, R, x):
        """S0 result: dict g -> (value, src, seq, flags, candidates). None if node not alive."""
        ok, _ = self.alive(R, x)
        if not ok:
            return None
        recs = {s: self.source_record(R, s, x) for s in OBS}
        out = {}
        for g in GROUPS:
            cands = []
            winner = None
            fallback = False
            for src in PI[self.app][g]:
                rec, hits = recs[src]
                v = rec[g]
                seq = R.obs.get((src, x, g), (None, None))[0]
                stale = seq is not None and R.inval.get((src, x), -1) > seq
                cands.append({"layer": "observed", "src": src, "seq": seq, "value": v,
                              "rule": hits.get(g), "stale": stale})
                if v == UNKNOWN:
                    if winner is None:
                        fallback = True      # a higher-priority source has not answered yet
                    continue
                if winner is None:
                    winner = cands[-1]
            if winner is None:
                out[g] = (UNKNOWN, None, None, {"fallback": False, "stale": False}, cands)
            else:
                out[g] = (winner["value"], winner["src"], winner["seq"],
                          {"fallback": fallback, "stale": winner["stale"]}, cands)
        return out

    @staticmethod
    def plain(b):
        return None if b is None else {g: b[g][0] for g in GROUPS}

    # --- full evaluation ------------------------------------------------------------------------
    def installed(self, R):
        return sorted((self.patches[pid] for pid, (seq, inst) in R.patch.items() if inst),
                      key=Patch.rank)

    def fuse_node(self, R, x, bases, finals, installed):
        """Strata S1..S3 + R for node x, given S0 for all nodes and finals of x's children."""
        b = bases[x]
        if b is None:
            return None, None, []
        p = self.parent[x]
        bp = self.plain(bases[p]) if p >= 0 else None
        bx = self.plain(b)
        trail = {g: list(b[g][4]) for g in GROUPS}
        conflicts = []
        # observed-layer disagreement (both known, different) -> diagnostic, winner by PI
        for g in GROUPS:
            known = [(c["src"], c["value"]) for c in b[g][4] if c["value"] != UNKNOWN]
            if len(known) == 2 and known[0][1] != known[1][1]:
                conflicts.append(("observed-disagree", x, g, known[0], known[1]))
        eff = {}
        layer_of = {}
        for g in GROUPS:
            v = b[g][0]
            eff[g] = v
            layer_of[g] = "observed" if v != UNKNOWN else "default"
        # S1 patches (selectors read S0 only)
        patch_hits = defaultdict(list)
        for pt in installed:
            if pt.matches(bx, bp):
                if pt.value == "firstchild":
                    kids = self.children.get(x, [])
                    val = (self.plain(bases[kids[0]]) or {}).get("name", UNKNOWN) if kids else UNKNOWN
                    if val in (UNKNOWN, ABSENT, ""):
                        trail[pt.group].append({"layer": "patch", "src": pt.ext_id, "rule": pt.pid,
                                                "value": val, "status": "no-value"})
                        continue
                else:
                    val = pt.value
                patch_hits[pt.group].append((pt, val))
        for g, hits in patch_hits.items():
            top_spec = hits[0][0].spec
            ties = [(pt, v) for pt, v in hits if pt.spec == top_spec]
            if len({vkey(v) for _, v in ties}) > 1:
                conflicts.append(("patch-tie", x, g, tuple((pt.pid, v) for pt, v in ties)))
            for i, (pt, v) in enumerate(hits):
                trail[g].append({"layer": "patch", "src": pt.ext_id, "rule": pt.pid, "value": v,
                                 "status": "won" if i == 0 else "shadowed"})
            eff[g] = hits[0][1]
            layer_of[g] = "patch"
        # S2 user overrides
        for g in GROUPS:
            u = R.user.get((x, g))
            if u is not None and u[1] != CLEAR:
                trail[g].append({"layer": "user", "src": "user", "seq": u[0], "value": u[1], "status": "won"})
                eff[g] = u[1]
                layer_of[g] = "user"
        # S3 derived name (doc mirror): name from content, lowest precedence above default
        # Only an observed/default empty name is filled: an explicit patch/user value is final
        # (found by the provenance check: user name "" was overwritten by the derived name).
        if (eff["name"] in (UNKNOWN, ABSENT, "") and eff["role"] in CONTENT_NAMED
                and layer_of["name"] in ("observed", "default")):
            parts = [finals[c]["name"] for c in self.children.get(x, [])
                     if finals.get(c) is not None and finals[c]["name"] not in (UNKNOWN, ABSENT, "")]
            if parts:
                v = " ".join(parts)
                trail["name"].append({"layer": "derived", "src": "doc-mirror", "value": v, "status": "won",
                                      "from": tuple(c for c in self.children.get(x, []))})
                eff["name"] = v
                layer_of["name"] = "derived"
        # defaults for still-unknown groups
        for g in GROUPS:
            if eff[g] in (UNKNOWN,):
                eff[g] = DEFAULT[g]
                layer_of[g] = "default"
            if eff["name"] == ABSENT:
                eff["name"] = ""
        eff, masked = repair(eff)
        prov = {}
        for g in GROUPS:
            prov[g] = {"layer": layer_of[g], "masked": masked.get(g),
                       "fallback": b[g][3]["fallback"] if layer_of[g] == "observed" else False,
                       "stale": b[g][3]["stale"] if layer_of[g] == "observed" else False,
                       "trail": trail[g]}
            # mark the observed winner / shadowed candidates
            for c in trail[g]:
                if c["layer"] == "observed":
                    if c["value"] == UNKNOWN:
                        c["status"] = "unknown"
                    elif layer_of[g] == "observed" and c["src"] == b[g][1]:
                        c["status"] = "won"
                    else:
                        c["status"] = "shadowed"
        return eff, prov, conflicts

    def full(self, R):
        """From-scratch FUSE over all nodes. Children before parents (derived name)."""
        bases = [self.base(R, x) for x in range(self.n)]
        inst = self.installed(R)
        finals, provs, confl = {}, {}, {}
        for x in sorted(range(self.n), key=lambda y: -self.depth[y]):
            finals[x], provs[x], confl[x] = self.fuse_node(R, x, bases, finals, inst)
        return finals, provs, confl, bases


# ------------------------------------------------------------------------------------------------
# 6. Incremental view maintenance (dirty set + early cutoff), checked against full()
# ------------------------------------------------------------------------------------------------

class Incremental:
    def __init__(self, world):
        self.w = world
        self.R = Registers()
        self.finals, self.provs, self.confl, self.bases = world.full(self.R)
        self.work = Counter()

    def apply_batch(self, events):
        dirty = set()
        for ev in events:
            ch = self.R.apply(ev)
            self.work["events"] += 1
            if not ch:
                self.work["noop_events"] += 1
            dirty |= ch
        if not dirty:
            return
        w = self.w
        if "*" in dirty:                         # global epoch change (patch set): re-fuse all
            self.finals, self.provs, self.confl, self.bases = w.full(self.R)
            self.work["full_refuse"] += 1
            self.work["fuse_calls"] += w.n
            return
        inst = w.installed(self.R)
        # S0 for directly dirty nodes; base change propagates to children (parent selectors)
        # and to the parent (firstchild patches).
        todo = set()
        for x in dirty:
            nb = w.base(self.R, x)
            self.work["base_calls"] += 1
            old = self.bases[x]
            self.bases[x] = nb
            todo.add(x)
            if w.plain(nb) != w.plain(old):
                todo.update(w.children.get(x, []))
                if w.parent[x] >= 0:
                    todo.add(w.parent[x])
        # S1..S3 in depth-descending order; final-name change dirties the parent (derived name)
        while todo:
            x = max(todo, key=lambda y: (w.depth[y], y))
            todo.discard(x)
            f, p, c = w.fuse_node(self.R, x, self.bases, self.finals, inst)
            self.work["fuse_calls"] += 1
            changed = f != self.finals.get(x)
            self.finals[x], self.provs[x], self.confl[x] = f, p, c
            if changed:
                self.work["published"] += 1
                if w.parent[x] >= 0:
                    todo.add(w.parent[x])
            else:
                self.work["cutoff"] += 1


# ------------------------------------------------------------------------------------------------
# 7. Workload generation
# ------------------------------------------------------------------------------------------------

def random_tree(rng, n):
    parent = [-1]
    for x in range(1, n):
        parent.append(rng.randrange(max(0, x - 6), x) if rng.random() < 0.7 else rng.randrange(x))
    return parent


def random_events(rng, n_nodes, n_events, patches, p_user=0.04, p_patch=0.02):
    """Events carry source-assigned sequence numbers (per-source timelines). Fetch replies are
    ordinary 'obs' events whose seq is the request issue time, so they may be older than events
    that arrived before them."""
    seq = Counter()
    evs = []
    # initial liveness from both sources
    for x in range(n_nodes):
        for src in OBS:
            if rng.random() < 0.9:
                seq[src] += 1
                evs.append(("life", src, x, seq[src], True))
    for _ in range(n_events):
        r = rng.random()
        x = rng.randrange(n_nodes)
        src = rng.choice(OBS)
        if r < p_user:
            seq["user"] += 1
            g = rng.choice(GROUPS)
            v = CLEAR if rng.random() < 0.3 else rng.choice(DOMAIN[g])
            evs.append(("user", x, g, seq["user"], v))
        elif r < p_user + p_patch and patches:
            seq["patch"] += 1
            evs.append(("patch", rng.choice(patches).pid, seq["patch"], rng.random() < 0.7))
        elif r < 0.12:
            seq[src] += 1
            evs.append(("inval", src, x, seq[src]))
        elif r < 0.15:
            seq[src] += 1
            evs.append(("life", src, x, seq[src], rng.random() < 0.8))
        else:
            g = rng.choice(GROUPS)
            seq[src] += 1
            s = seq[src] - rng.randrange(0, 20) if rng.random() < 0.2 else seq[src]   # late fetch reply
            v = ABSENT if (g != "role" and rng.random() < 0.1) else rng.choice(DOMAIN[g])
            evs.append(("obs", src, x, g, max(1, s), v))
    return evs


def deliver(rng, evs, dup_p=0.1):
    """A permuted delivery with duplicates (retries, resyncs), split into random batches."""
    e = list(evs) + [ev for ev in evs if rng.random() < dup_p]
    rng.shuffle(e)
    out, i = [], 0
    while i < len(e):
        k = rng.randint(1, 32)
        out.append(e[i:i + k])
        i += k
    return out


def strip(provs):
    """Canonical comparable form of provenance."""
    return repr(sorted((x, repr(p)) for x, p in provs.items()))


def conflicts_canon(confl):
    return sorted(repr(c) for cs in confl.values() for c in cs)


# ------------------------------------------------------------------------------------------------
# 8. Experiments / properties
# ------------------------------------------------------------------------------------------------

def naive_lww(world, evs_batches):
    """Baseline: last *arrival* wins across sources and kinds, no seq, no priority."""
    eff = defaultdict(dict)
    for batch in evs_batches:
        for ev in batch:
            if ev[0] == "obs":
                _, src, x, g, seq, v = ev
                eff[x][g] = v
            elif ev[0] == "user":
                _, x, g, seq, v = ev
                if v != CLEAR:
                    eff[x][g] = v
    return {x: tuple(sorted(d.items(), key=repr)) for x, d in eff.items()}


def check_provenance(world, R, finals, provs):
    """F3: completeness (every input fact for (x,g) appears in the trail exactly once) and
    soundness (effective value equals the winning entry, or default, or a repair mask)."""
    errors = 0
    for x in range(world.n):
        if finals[x] is None:
            continue
        for g in GROUPS:
            p = provs[x][g]
            obs_in = {(s, R.obs[(s, x, g)][0]) for s in OBS if (s, x, g) in R.obs}
            obs_tr = {(c["src"], c["seq"]) for c in p["trail"] if c["layer"] == "observed" and c["seq"] is not None}
            if obs_in != obs_tr:
                errors += 1
            u = R.user.get((x, g))
            has_user = u is not None and u[1] != CLEAR
            if has_user != any(c["layer"] == "user" for c in p["trail"]):
                errors += 1
            won = [c for c in p["trail"] if c.get("status") == "won"]
            top = {"user": 3, "patch": 2, "observed": 1, "derived": 0}
            if p["layer"] == "default":
                ok = not won or all(c["layer"] == "observed" and False for c in won)
                ok = ok or (finals[x][g] == DEFAULT[g])
            else:
                best = max(won, key=lambda c: top[c["layer"]]) if won else None
                val = finals[x][g]
                ok = best is not None and best["layer"] == p["layer"] and (
                    best["value"] == val or p["masked"] is not None
                    or (g == "name" and best["value"] == ABSENT and val == ""))
            if not ok:
                errors += 1
    return errors


def exp_order(trials=60, perms=12, n=40, n_events=400, seed=1, verbose=True):
    rng = random.Random(seed)
    stats = Counter()
    t0 = time.time()
    for t in range(trials):
        app = rng.choice(list(PI))
        patches = random_patches(rng, 6)
        world = World(random_tree(rng, n), app, patches)
        evs = random_events(rng, n, n_events, patches)
        ref_R = Registers()
        for ev in evs:
            ref_R.apply(ev)
        ref = world.full(ref_R)
        stats["provenance_errors"] += check_provenance(world, ref_R, ref[0], ref[1])
        naive_ref = None
        for k in range(perms):
            inc = Incremental(world)
            batches = deliver(rng, evs)
            for b in batches:
                inc.apply_batch(b)
            stats["runs"] += 1
            stats["reg_mismatch"] += inc.R.snapshot() != ref_R.snapshot()
            stats["sem_mismatch"] += inc.finals != ref[0]
            stats["prov_mismatch"] += strip(inc.provs) != strip(ref[1])
            stats["conflict_mismatch"] += conflicts_canon(inc.confl) != conflicts_canon(ref[2])
            nv = naive_lww(world, batches)
            if naive_ref is None:
                naive_ref = nv
            else:
                stats["naive_runs"] += 1
                stats["naive_mismatch"] += nv != naive_ref
        stats["conflicts_total"] += len(conflicts_canon(ref[2]))
    stats["seconds"] = round(time.time() - t0, 1)
    if verbose:
        print("order-independence:", dict(stats))
    return stats


def exp_ivm(trials=30, n=400, n_events=4000, seed=2, verbose=True, batch=64):
    """Incremental == from-scratch after every batch; work counts per event."""
    rng = random.Random(seed)
    tot = Counter()
    mism = 0
    for t in range(trials):
        patches = random_patches(rng, 6)
        world = World(random_tree(rng, n), rng.choice(list(PI)), patches)
        # storm profile: mostly obs events, rare patch/user changes
        evs = random_events(rng, n, n_events, patches, p_user=0.002, p_patch=0.0005)
        inc = Incremental(world)
        for i in range(0, len(evs), batch):
            inc.apply_batch(evs[i:i + batch])
            if rng.random() < 0.1:
                ref = world.full(inc.R)
                mism += inc.finals != ref[0]
        ref = world.full(inc.R)
        mism += inc.finals != ref[0]
        tot.update(inc.work)
    per_ev = {k: round(v / tot["events"], 3) for k, v in tot.items() if k != "events"}
    if verbose:
        print(f"ivm: mismatches={mism} events={tot['events']} per-event={per_ev}")
    return mism, per_ev, tot


def declared_dependents(world, x, g):
    """D(x, g): what a user override on (x, g) may change (property F5)."""
    deps = {(x, h) for h in GROUPS} if g == "role" else {(x, g)}
    if g in ("name", "role"):
        p = world.parent[x]
        while p >= 0:
            deps.add((p, "name"))
            p = world.parent[p]
    return deps


def exp_override(trials=200, n=40, seed=3, verbose=True):
    rng = random.Random(seed)
    st = Counter()
    for t in range(trials):
        patches = random_patches(rng, 6)
        world = World(random_tree(rng, n), rng.choice(list(PI)), patches)
        evs = random_events(rng, n, 300, patches)
        R = Registers()
        for ev in evs:
            R.apply(ev)
        base = world.full(R)
        alive = [x for x in range(n) if base[0][x] is not None]
        if not alive:
            continue
        x = rng.choice(alive)
        g = rng.choice(GROUPS)
        v = rng.choice(DOMAIN[g])
        useq = max([s for (s, _) in R.user.values()] + [0]) + 1
        R2 = Registers()
        R2.__dict__ = {k: dict(vv) for k, vv in R.__dict__.items()}
        R2.apply(("user", x, g, useq, v))
        after = world.full(R2)
        st["runs"] += 1
        eff = after[0][x][g]
        expect = v if not (g == "name" and v == ABSENT) else ""
        if eff == expect:
            st["dominance_ok"] += 1
        elif after[1][x][g]["masked"] is not None:
            st["dominance_masked_by_repair"] += 1
        else:
            st["dominance_violation"] += 1
        changed = {(y, h) for y in range(n) for h in GROUPS
                   if (base[0][y] or {}).get(h) != (after[0][y] or {}).get(h)}
        if not changed <= declared_dependents(world, x, g):
            st["locality_violation"] += 1
        # reversibility: CLEAR with a newer seq restores the original effective state
        R2.apply(("user", x, g, useq + 1, CLEAR))
        back = world.full(R2)
        if R.user.get((x, g)) is None or R.user[(x, g)][1] == CLEAR:
            st["reversible_ok"] += back[0] == base[0]
            st["reversible_fail"] += back[0] != base[0]
    if verbose:
        print("override:", dict(st))
    return st


# ------------------------------------------------------------------------------------------------
# 9. Cross-connector identity matching (AT-SPI view vs AccessKit/Newton view of one window)
# ------------------------------------------------------------------------------------------------

def make_views(rng, n, p_drop, p_rename, p_key):
    """Truth tree A (Newton). View B (AT-SPI) drops some generic nodes (children spliced up),
    renames some nodes, and carries the AccessKit id in the object path with probability p_key
    (1.0 when B is AccessKit's own AT-SPI adapter; 0 for an unrelated AT-SPI implementation)."""
    parent = random_tree(rng, n)
    role = [rng.choice(ROLES) for _ in range(n)]
    role[0] = "group"
    name = [rng.choice(NAMES + ["n%d" % rng.randrange(n)]) for _ in range(n)]
    dropped = {x for x in range(1, n) if role[x] == "generic" and rng.random() < p_drop}
    b_parent = {}
    for x in range(n):
        if x in dropped:
            continue
        p = parent[x]
        while p in dropped:
            p = parent[p]
        b_parent[x] = p
    b_name = {x: (name[x] + "*" if rng.random() < p_rename else name[x]) for x in b_parent}
    has_key = {x: rng.random() < p_key for x in b_parent}
    return parent, role, name, b_parent, b_name, has_key


def match_views(parent, role, name, b_parent, b_name, has_key):
    """Tier 0: key (AccessKit id from path) -> exact. Tier 2: top-down alignment of the
    'effective children' lists (transparent generic nodes in A expanded) by LCS on (role, name),
    accept exact-label pairs, and role-only pairs if unique between anchors. Returns {b: a}."""
    n = len(parent)
    a_children = defaultdict(list)
    for x in range(n):
        if parent[x] >= 0:
            a_children[parent[x]].append(x)
    b_children = defaultdict(list)
    for x, p in b_parent.items():
        if p >= 0:
            b_children[p].append(x)
    m = {}
    for x, k in has_key.items():
        if k:
            m[x] = x              # key equality is identity: B node x carries A id x
    cost = 0

    def eff_children_a(a, depth=0):
        out = []
        for c in a_children.get(a, []):
            out.append(c)
            if role[c] == "generic" and depth < 3:
                out.extend(eff_children_a(c, depth + 1))     # candidate splice-through
        return out

    stack = [(0, 0)]
    m[0] = 0
    while stack:
        a, b = stack.pop()
        ca = eff_children_a(a)
        cb = b_children.get(b, [])
        used_a = set(m[y] for y in cb if y in m)
        la = [x for x in ca if x not in used_a]
        lb = [y for y in cb if y not in m]
        cost += len(la) * len(lb)
        # LCS on labels
        L = [[0] * (len(lb) + 1) for _ in range(len(la) + 1)]
        for i in range(len(la) - 1, -1, -1):
            for j in range(len(lb) - 1, -1, -1):
                if role[la[i]] == role[lb[j]] and name[la[i]] == b_name[lb[j]]:
                    L[i][j] = 1 + L[i + 1][j + 1]
                else:
                    L[i][j] = max(L[i + 1][j], L[i][j + 1])
        i = j = 0
        pairs = []
        while i < len(la) and j < len(lb):
            if role[la[i]] == role[lb[j]] and name[la[i]] == b_name[lb[j]] and L[i][j] == 1 + L[i + 1][j + 1]:
                pairs.append((la[i], lb[j]))
                i += 1
                j += 1
            elif L[i + 1][j] >= L[i][j + 1]:
                i += 1
            else:
                j += 1
        for xa, yb in pairs:
            m[yb] = xa
        # role-only pairs, unique in the gap between consecutive anchors
        anchors = [(-1, -1)] + [(la.index(xa), lb.index(yb)) for xa, yb in pairs] + [(len(la), len(lb))]
        for (i0, j0), (i1, j1) in zip(anchors, anchors[1:]):
            ga, gb = la[i0 + 1:i1], lb[j0 + 1:j1]
            for r in set(role[y] for y in gb):
                xs = [x for x in ga if role[x] == r]
                ys = [y for y in gb if role[y] == r]
                if len(xs) == 1 and len(ys) == 1:
                    m[ys[0]] = xs[0]
        for y in cb:
            if y in m:
                stack.append((m[y], y))
    return m, cost


def exp_match(trials=40, n=2000, seed=4, verbose=True):
    rng = random.Random(seed)
    rows = []
    for p_key in (1.0, 0.5, 0.0):
        for p_rename in (0.0, 0.05, 0.2):
            tp = fp = tot = 0
            cost = 0
            for t in range(trials):
                views = make_views(rng, n, 0.5, p_rename, p_key)
                m, c = match_views(*views)
                cost += c
                b_parent = views[3]
                tot += len(b_parent)
                for y, x in m.items():
                    if y == x:
                        tp += 1
                    else:
                        fp += 1
            rows.append((p_key, p_rename, tp / tot, fp / max(1, tp + fp), cost / trials / n))
    if verbose:
        print("match: p_key p_rename recall false_match_rate pair_cmp_per_node")
        for r in rows:
            print("   %.1f   %.2f    %.4f   %.4f   %.1f" % r)
    return rows


# ------------------------------------------------------------------------------------------------
# 10. Test vectors
# ------------------------------------------------------------------------------------------------

def selftest():
    # TV1: per-source LWW by seq, not arrival; trust table picks AT-SPI for Firefox.
    w = World([-1, 0, 1], "firefox", [])
    evs = [("life", "atspi", x, 1, True) for x in range(3)] + [("life", "newton", x, 1, True) for x in range(3)]
    evs += [("obs", "atspi", 1, "role", 5, "checkbox"), ("obs", "atspi", 1, "check", 7, "on"),
            ("obs", "atspi", 1, "check", 6, "off"),            # late fetch reply (issued earlier)
            ("obs", "newton", 1, "check", 9, "mixed")]         # lower trust for firefox
    R = Registers()
    for ev in evs:
        R.apply(ev)
    f, p, c, _ = w.full(R)
    assert f[1]["role"] == "checkbox" and f[1]["check"] == "on", f[1]
    assert any(k[0] == "observed-disagree" and k[2] == "check" for k in c[1]), c[1]
    # TV2: same events reversed -> same result (order independence on a vector)
    R2 = Registers()
    for ev in reversed(evs):
        R2.apply(ev)
    assert w.full(R2)[0] == f
    # TV3: quirk Q1 drops a stale 'checked' on a button, and provenance records it
    R.apply(("obs", "atspi", 2, "role", 10, "button"))
    R.apply(("obs", "atspi", 2, "check", 11, "on"))
    R.apply(("obs", "newton", 2, "check", 3, UNKNOWN))
    f, p, c, _ = w.full(R)
    assert f[2]["check"] == "na"
    assert any(e.get("rule") == "quirk:Q1-drop-stale-checked" for e in p[2]["check"]["trail"])
    # TV4: derived name from children, patch beats observed, user beats patch
    w2 = World([-1, 0, 1], "gtk-newton", [Patch("p0", "org.a", 1, "button", True, None, "name", "firstchild")])
    R = Registers()
    for x in range(3):
        R.apply(("life", "newton", x, 1, True))
    R.apply(("obs", "newton", 1, "role", 2, "button"))
    R.apply(("obs", "newton", 1, "name", 3, ""))
    R.apply(("obs", "newton", 2, "role", 4, "img"))
    R.apply(("obs", "newton", 2, "name", 5, "Close"))
    f, p, _, _ = w2.full(R)
    assert f[1]["name"] == "Close" and p[1]["name"]["layer"] == "derived", p[1]["name"]
    R.apply(("patch", "p0", 1, True))
    f, p, _, _ = w2.full(R)
    assert f[1]["name"] == "Close" and p[1]["name"]["layer"] == "patch"
    R.apply(("user", 1, "name", 1, "Dismiss"))
    f, p, _, _ = w2.full(R)
    assert f[1]["name"] == "Dismiss" and p[1]["name"]["layer"] == "user"
    R.apply(("user", 1, "name", 2, CLEAR))
    assert w2.full(R)[0][1]["name"] == "Close"
    # TV5: fallback flag: preferred source unknown, secondary answers
    w3 = World([-1, 0], "gtk-newton", [])
    R = Registers()
    R.apply(("life", "newton", 1, 1, True))
    R.apply(("obs", "atspi", 1, "role", 1, "link"))
    f, p, _, _ = w3.full(R)
    assert f[1]["role"] == "link" and p[1]["role"]["fallback"] is True
    # TV6: repair idempotent
    for _ in range(500):
        rr = random.Random(_)
        s = {g: rr.choice(DOMAIN[g]) for g in GROUPS}
        a, _m = repair(s)
        assert repair(a)[0] == a
    # small runs of every property
    s = exp_order(trials=8, perms=6, verbose=False)
    assert s["sem_mismatch"] == s["prov_mismatch"] == s["reg_mismatch"] == s["conflict_mismatch"] == 0, s
    assert s["provenance_errors"] == 0, s
    mism, _, _ = exp_ivm(trials=3, n=120, n_events=1500, verbose=False)
    assert mism == 0
    o = exp_override(trials=60, verbose=False)
    assert o["dominance_violation"] == 0 and o["locality_violation"] == 0 and o["reversible_fail"] == 0, o
    print("selftest ok")

# ------------------------------------------------------------------------------------------------
# 11. ADR-0024 r2: fact sources (M22a-A3 reference oracle; report section 10)
# ------------------------------------------------------------------------------------------------
# Everything above is unchanged (sections 1-10 reproduce report section 6). This section adds the
# non-primary source classes on top of the same register discipline:
#   fact      (src, anchor, group)  -> max by (seq, record)   record = (value, p_milli, basis_vis,
#                                                              bind_gen, admitted_t, ttl_ms, scope)
#   attr      (src, x)              -> max by (seq, dom_id)    primary-reported HTML id (binding)
#   vis       x                     -> max rev                 writer's vis_rev, an explicit input
#   clock                           -> max t                   TTL is a lifecycle input, not a timer
#   epoch     src                   -> max by (n, state)       mask/revoke/detach are inputs
# Admission (scope pinning, grant, anchor kind, value check, truncation) is a pure function of
# one fact and static tables, so it commutes with delivery order. bind_gen and admitted_t are
# stamped by the writer at admission and are part of the delivered fact (the Rust writer stamps
# them; the differential test feeds stamped facts). BindGen is defined as the primary seq at
# which the anchor's current target acquired the anchor (life seq for Node/Exact/Region, attr
# seq for DomId): it changes exactly when the anchor resolves to another node or the node is
# re-created, and is monotone because primary seqs are.

R2_GROUPS = ["role", "name", "description", "check", "press", "expand", "select", "current",
             "invalid", "popup", "avail", "flags", "level", "setpos", "grid", "live"]
LAYOUTS = {
    # ADR-0024 r2 section 2.12: 16 frozen groups, u16 masks, ProvTag u64, hedge u32, no taint field
    "r2-16": dict(groups=R2_GROUPS, cap=16, mask_bits=16, prov_bits=64, hedge_bits=32, taint_bits=0),
    # Size recommendation (report section 10.1): capacity 32, u32 masks, ProvTag 2 x u64,
    # hedge u64, taint u64; orientation and sort split out of flags as codes 16 and 17
    "x32": dict(groups=R2_GROUPS + ["orientation", "sort"], cap=32, mask_bits=32, prov_bits=128,
                hedge_bits=64, taint_bits=64),
}
EVIDENCE = {"hint.icon_class": 0x8000, "hint.src_name": 0x8001, "hint.title_attr": 0x8002,
            "visual.ocr_text": 0x8003, "visual.label_guess": 0x8004, "layout.box": 0x8005,
            "layout.css_hidden": 0x8006}
PROV = {"Default": 0, "Derived": 1, "DerivedTainted": 2, "Inferred": 3, "ObservedPrimary": 4,
        "ObservedSecondary": 5, "Patch": 6, "User": 7}
PROV_NAME = {v: k for k, v in PROV.items()}
TAINTED = {PROV["DerivedTainted"], PROV["Inferred"], PROV["ObservedSecondary"]}
PROTECTED = {PROV["User"], PROV["Patch"], PROV["ObservedPrimary"], PROV["Derived"]}
CLASS_CODE = {"Primary": 0, "Secondary": 1, "Inferred": 2, "Annotation": 3}
# ConfClass as stored in the hedge mask: 0 = Asserted (plain), so a zeroed mask means "all plain".
CONF = {"Asserted": 0, "High": 1, "Probable": 2, "Possible": 3}
CONF_NAME = {v: k for k, v in CONF.items()}
BQ = {"Unbound": 0, "Geometric": 1, "Heuristic": 2, "Unique": 3, "Exact": 4}
BQ_NAME = {v: k for k, v in BQ.items()}
MASKED_STATES = {"Paused", "Quarantined", "Revoked"}
EPOCH_STATES = ["Active", "Throttled", "Detached", "Paused", "Quarantined", "Revoked"]
RETRACT = "<retract>"
HIGH_MILLI, PROBABLE_MILLI = 970, 900
MAX_STR = 64            # stands in for the 1 KiB page-string cap (truncate and count, M-6)
DEFAULT_TTL = 300_000   # Inferred facts without a TTL get 5 min (ADR-0024 section 2.4)

DOMAIN2 = dict(DOMAIN, description=["", "Opens a dialog", "Required", ABSENT],
               live=["off", "polite", "assertive"], orientation=["na", "horizontal", "vertical"])
DEFAULT2 = dict(DEFAULT, description="", live="off", orientation="na")
MODELLED = ["role", "name", "description", "check", "expand", "avail", "level", "live", "orientation"]
STATE_GROUPS = {"check", "avail", "expand", "live", "orientation"}   # groups with change utterances
TITLE_ROLES = {"button", "link", "img", "checkbox"}
EVID_TEXT = ["Close", "Delete", "Search", "Menu", "", "x" * 80]
# Static inexpressible rows per source profile (toy data, D-4: each real row needs a decision row).
INEXPRESSIBLE = {"firefox": {"live", "orientation"}, "gtk-newton": set()}

SOURCES = {
    # name: (slot, gen, class, tier, kind order inside S4)
    "atspi": (0, 0, "Primary", 1, None),
    "newton": (1, 0, "Primary", 1, None),
    "bus": (2, 0, "Secondary", 3, None),
    "ocr": (3, 0, "Inferred", 2, 0),
    "icon": (4, 0, "Inferred", 2, 1),
    "ext.a": (5, 0, "Inferred", 3, 2),
    "ext.b": (6, 0, "Inferred", 3, 2),
}
NONPRIMARY = [s for s, v in SOURCES.items() if v[2] != "Primary"]
GRANTS = {
    "bus": dict(semantic={"role", "name", "description", "check", "expand", "avail", "level", "live",
                          "orientation"},
                evidence={"hint.title_attr", "layout.css_hidden"}, anchors={"node", "dom"},
                floor={"name": 800, "sem": 850, "evidence": 700, "visual": 700}),
    "ocr": dict(semantic={"name"}, evidence={"visual.ocr_text"}, anchors={"node", "region"},
                floor={"name": 800, "sem": 900, "evidence": 700, "visual": 750}),
    "icon": dict(semantic={"name", "description"}, evidence=set(), anchors={"node", "dom"},
                 floor={"name": 800, "sem": 900, "evidence": 700, "visual": 750}),
    "ext.a": dict(semantic={"name", "description"}, evidence={"hint.title_attr"}, anchors={"node", "dom"},
                  floor={"name": 850, "sem": 900, "evidence": 700, "visual": 750}),
    "ext.b": dict(semantic={"name", "description"}, evidence={"hint.title_attr"}, anchors={"node", "dom"},
                  floor={"name": 850, "sem": 900, "evidence": 700, "visual": 750}),
}
MIN_BIND = {"name": BQ["Unique"], "sem": BQ["Unique"], "evidence": BQ["Heuristic"],
            "visual": BQ["Geometric"]}


def gclass(g):
    """Grant group class of a semantic group or evidence kind."""
    if g in EVIDENCE:
        return "visual" if g.startswith("visual.") else "evidence"
    return "name" if g in ("name", "description") else "sem"


def conf_class(p, floor):
    """ConfClass computed by the writer from p_milli and the grant (never taken from the source)."""
    if p >= HIGH_MILLI:
        return CONF["High"]
    if p >= PROBABLE_MILLI:
        return CONF["Probable"]
    if p >= floor:
        return CONF["Possible"]
    return None


def hedge_cap(h, tier):
    """Any T3 input caps the class at Probable (H-5)."""
    return max(h, CONF["Probable"]) if tier >= 3 else h


def admit(ev, scope, layout):
    """Pure admission of one non-primary fact. Returns (status, key, item); key None = rejected
    and never stored. Order of checks as ADR-0024 section 2.6 (binding is re-checked in FUSE)."""
    _, src, fscope, anchor, g, seq, v, p, basis, gen, t, ttl = ev
    if src not in SOURCES or SOURCES[src][2] in ("Primary", "Annotation"):
        return "reject:class", None, None
    if fscope != scope:
        return "reject:scope", None, None                     # F19 / FS-11
    gr = GRANTS[src]
    if g in EVIDENCE:
        if g not in gr["evidence"]:
            return "reject:group", None, None
    elif g not in gr["semantic"] or g not in LAYOUTS[layout]["groups"]:
        return "reject:group", None, None
    if anchor[0] not in gr["anchors"]:
        return "reject:anchor", None, None
    if not (0 <= p <= 1000):
        return "reject:value", None, None
    status = "accept"
    if v != RETRACT:
        if g in EVIDENCE:
            if not isinstance(v, str):
                return "reject:value", None, None
            if len(v) > MAX_STR:
                v, status = v[:MAX_STR], "accept:truncated"
        elif v not in DOMAIN2[g]:
            return "reject:value", None, None
    if ttl is None and SOURCES[src][2] == "Inferred":
        ttl = DEFAULT_TTL
    return status, (src, tuple(anchor), g), (seq, (v, p, basis, gen, t, ttl, fscope))


class Registers2(Registers):
    def __init__(self):
        super().__init__()
        self.attr = {}       # (src, x) -> (seq, dom_id|None)
        self.vis = {}        # x -> rev
        self.clock = 0
        self.epoch = {}      # src -> (n, state)
        self.fact = {}       # (src, anchor, group) -> (seq, record)

    def apply(self, ev):
        kind = ev[0]
        if kind == "attr":
            _, src, x, seq, dom = ev
            return {x, "#"} if self._join(self.attr, (src, x), (seq, dom)) else set()
        if kind == "vis":
            _, x, rev = ev
            if rev > self.vis.get(x, 0):
                self.vis[x] = rev
                return {x}
            return set()
        if kind == "tick":
            if ev[1] > self.clock:
                self.clock = ev[1]
                return {"*"}
            return set()
        if kind == "epoch":
            _, src, n, state = ev
            return {"*"} if self._join(self.epoch, src, (n, state)) else set()
        if kind == "fact_admitted":
            _, key, item = ev
            return {"*"} if self._join(self.fact, key, item) else set()
        if kind == "life":
            ch = super().apply(ev)
            return ch | {"#"} if ch else ch
        return super().apply(ev)

    def snapshot(self):
        return super().snapshot() + (tuple(sorted(self.attr.items(), key=repr)),
                                     tuple(sorted(self.vis.items())), self.clock,
                                     tuple(sorted(self.epoch.items(), key=repr)),
                                     tuple(sorted(self.fact.items(), key=repr)))


def ingest(world, R, events, stats=None):
    """Deliver events in the given order: facts go through admit() first."""
    for ev in events:
        if ev[0] == "fact":
            st, key, item = admit(ev, world.scope, world.layout)
            if stats is not None:
                stats[st] += 1
            if key is not None:
                R.apply(("fact_admitted", key, item))
        else:
            R.apply(ev)
    return R


class World2(World):
    """FUSE with sources: S-1 quirk, S0 primaries (+ secondaries on inexpressible groups),
    S1 patch, S2 user, S3 derived (content, title evidence), S4 inferred, R repair."""

    def __init__(self, parent, app_kind, patches, layout="x32", scope="app1",
                 content_rule="untainted-first"):
        super().__init__(parent, app_kind, patches)
        self.layout = layout
        self.scope = scope
        self.groups = [g for g in MODELLED if g in LAYOUTS[layout]["groups"]]
        self.content_rule = content_rule

    def pi(self, g):
        return PI[self.app].get(g, PI[self.app]["role"])

    # --- binding --------------------------------------------------------------------------------
    def life_seq(self, R, x):
        ok, src = self.alive(R, x)
        return R.life[(src, x)][0] if ok else None

    def dom_index(self, R):
        idx = defaultdict(list)
        for (src, x), (seq, dom) in R.attr.items():
            if src == "atspi" and dom is not None and self.alive(R, x)[0]:
                idx[dom].append((x, seq))
        return idx

    def resolve(self, R, anchor, idx=None):
        """-> (node|None, quality, gen). Pure in the registers."""
        kind = anchor[0]
        if kind in ("node", "exact", "region"):
            x = anchor[1]
            if not (0 <= x < self.n) or not self.alive(R, x)[0]:
                return None, BQ["Unbound"], 0
            q = BQ["Geometric"] if kind == "region" else BQ["Exact"]
            return x, q, self.life_seq(R, x)
        if kind == "dom":
            _, dom, count = anchor
            idx = idx if idx is not None else self.dom_index(R)
            cands = sorted(idx.get(dom, []))
            if not cands:
                return None, BQ["Unbound"], 0
            x, seq = cands[0]
            q = BQ["Unique"] if (count == 1 and len(cands) == 1) else BQ["Heuristic"]
            return x, q, seq
        raise ValueError(kind)

    def bind_all(self, R):
        idx = self.dom_index(R)
        by_node = defaultdict(list)
        unbound = []
        for key in sorted(R.fact, key=repr):
            seq, rec = R.fact[key]
            x, q, gen = self.resolve(R, key[1], idx)
            item = (key, seq, rec, (x, q, gen))
            (by_node[x] if x is not None else unbound).append(item)
        return by_node, unbound

    # --- validity of one non-primary register for winning ----------------------------------------
    def status(self, R, x, item):
        (src, anchor, g), seq, rec, (bx, q, gen) = item
        v, p, basis, fgen, t, ttl, _scope = rec
        cls = SOURCES[src][2]
        if v == RETRACT:
            return "retracted", None
        ep = R.epoch.get(src, (0, "Active"))[1]
        if ep in MASKED_STATES:
            return "masked", None
        if ep == "Detached":
            return "detached", None
        gc = gclass(g)
        if q < MIN_BIND[gc]:
            return ("unbound" if q == BQ["Unbound"] else "weak-binding"), None
        if fgen != gen:
            return "rebound", None
        if (cls == "Inferred" or g.startswith("visual.")) and basis is not None and basis < R.vis.get(x, 0):
            return "stale", None
        if ttl is not None and t + ttl <= R.clock:
            return "expired", None
        c = conf_class(p, GRANTS[src]["floor"][gc])
        if c is None:
            return "low-confidence", None
        return "ok", hedge_cap(c, SOURCES[src][3])

    @staticmethod
    def fact_trail(layer, item, status, h=None):
        (src, anchor, g), seq, rec, (bx, q, gen) = item
        return {"layer": layer, "src": src, "seq": seq, "anchor": list(anchor), "group": g,
                "value": rec[0], "p_milli": rec[1], "class": None if h is None else CONF_NAME[h],
                "bind": BQ_NAME[q], "gen": gen, "fact_gen": rec[3], "status": status}

    # --- S0 --------------------------------------------------------------------------------------
    def base2(self, R, x, facts):
        ok, _ = self.alive(R, x)
        if not ok:
            return None
        recs = {}
        for s in OBS:
            rec = {g: R.obs.get((s, x, g), (None, UNKNOWN))[1] for g in self.groups}
            recs[s] = quirk(s, rec)
        inexp = INEXPRESSIBLE[self.app]
        out = {}
        for g in self.groups:
            cands, winner, fallback = [], None, False
            for src in self.pi(g):
                rec, hits = recs[src]
                v = rec[g]
                seq = R.obs.get((src, x, g), (None, None))[0]
                stale = seq is not None and R.inval.get((src, x), -1) > seq
                c = {"layer": "observed", "src": src, "seq": seq, "value": v, "rule": hits.get(g),
                     "stale": stale}
                cands.append(c)
                if v == UNKNOWN:
                    if winner is None:
                        fallback = True
                    continue
                if winner is None:
                    winner = c
            sec = []
            for item in facts:
                (src, anchor, fg) = item[0]
                if fg != g or SOURCES[src][2] != "Secondary":
                    continue
                st, h = self.status(R, x, item)
                if st == "ok" and g not in inexp:
                    st = "not-candidate"          # H-3: secondaries never win expressible groups
                sec.append((st, h, item))
            sec.sort(key=lambda e: (e[0] != "ok", SOURCES[e[2][0][0]][3], e[1] if e[1] is not None else 9,
                                    SOURCES[e[2][0][0]][0], e[2][1], vkey(e[2][2][0])))
            sec_win = None
            for st, h, item in sec:
                if st == "ok" and winner is None and sec_win is None:
                    sec_win = (item, h)
                    cands.append(self.fact_trail("secondary", item, "won", h))
                else:
                    cands.append(self.fact_trail("secondary", item,
                                                 "shadowed" if st == "ok" else st, h))
            if winner is not None:
                info = (PROV["ObservedPrimary"], CONF["Asserted"], 0)
                out[g] = (winner["value"], winner["src"], winner["seq"],
                          {"fallback": fallback, "stale": winner["stale"]}, cands, info)
            elif sec_win is not None:
                item, h = sec_win
                info = (PROV["ObservedSecondary"], h, SOURCES[item[0][0]][3])
                out[g] = (item[2][0], item[0][0], item[1], {"fallback": False, "stale": False}, cands, info)
            else:
                out[g] = (UNKNOWN, None, None, {"fallback": False, "stale": False}, cands,
                          (PROV["Default"], 0, 0))
        return out

    def plain2(self, b):
        return None if b is None else {g: b[g][0] for g in self.groups}

    # --- S1..S4, R --------------------------------------------------------------------------------
    def fuse_node2(self, R, x, bases, finals, installed, facts):
        b = bases[x]
        if b is None:
            return None
        p = self.parent[x]
        bp = self.plain2(bases[p]) if p >= 0 else None
        bx = self.plain2(b)
        G = self.groups
        trail = {g: [dict(c) for c in b[g][4]] for g in G}
        eff, layer, info = {}, {}, {}
        for g in G:
            eff[g] = b[g][0]
            layer[g] = "default" if eff[g] == UNKNOWN else (
                "observed" if b[g][5][0] == PROV["ObservedPrimary"] else "secondary")
            info[g] = b[g][5]
        # S1 patches (selectors read S0)
        patch_hits = defaultdict(list)
        for pt in installed:
            if pt.group not in G:
                continue
            if pt.matches(bx, bp):
                if pt.value == "firstchild":
                    kids = self.children.get(x, [])
                    val = (self.plain2(bases[kids[0]]) or {}).get("name", UNKNOWN) if kids else UNKNOWN
                    if val in (UNKNOWN, ABSENT, ""):
                        trail[pt.group].append({"layer": "patch", "src": pt.ext_id, "rule": pt.pid,
                                                "value": val, "status": "no-value"})
                        continue
                else:
                    val = pt.value
                patch_hits[pt.group].append((pt, val))
        for g, hits in sorted(patch_hits.items()):
            for i, (pt, v) in enumerate(hits):
                trail[g].append({"layer": "patch", "src": pt.ext_id, "rule": pt.pid, "value": v,
                                 "status": "won" if i == 0 else "shadowed"})
            eff[g], layer[g], info[g] = hits[0][1], "patch", (PROV["Patch"], CONF["Asserted"], 0)
        # S2 user
        for g in G:
            u = R.user.get((x, g))
            if u is not None and u[1] != CLEAR:
                trail[g].append({"layer": "user", "src": "user", "seq": u[0], "value": u[1], "status": "won"})
                eff[g], layer[g], info[g] = u[1], "user", (PROV["User"], CONF["Asserted"], 0)

        def fillable(g):
            return eff[g] in (UNKNOWN, ABSENT, "") and layer[g] in ("observed", "default")

        # S3 derived: (a) name from content over children's final names, (b) title evidence
        if fillable("name") and eff["role"] in CONTENT_NAMED:
            parts = []
            for c in self.children.get(x, []):
                f = finals.get(c)
                if f is not None and f["values"]["name"] not in (UNKNOWN, ABSENT, ""):
                    parts.append((c, f["values"]["name"], f["info"]["name"]))
            clean = [e for e in parts if e[2][0] not in TAINTED]
            dirty = [e for e in parts if e[2][0] in TAINTED]
            use = parts if self.content_rule == "all" else (clean or dirty)
            if use:
                tainted = [e for e in use if e[2][0] in TAINTED]
                v = " ".join(e[1] for e in use)
                if tainted:
                    tier = max(e[2][2] for e in tainted)
                    h = hedge_cap(max(e[2][1] for e in tainted), tier)
                    inf = (PROV["DerivedTainted"], h, tier)
                else:
                    inf = (PROV["Derived"], CONF["Asserted"], 0)
                trail["name"].append({"layer": "derived", "src": "content", "value": v, "status": "won",
                                      "from": [e[0] for e in use], "tainted_from": [e[0] for e in tainted]})
                eff["name"], layer["name"], info["name"] = v, "derived", inf
        ev_title = []
        for item in facts:
            if item[0][2] == "hint.title_attr":
                st, h = self.status(R, x, item)
                ev_title.append((st, h, item))
        ev_title.sort(key=lambda e: (e[0] != "ok", SOURCES[e[2][0][0]][3], e[1] if e[1] is not None else 9,
                                     SOURCES[e[2][0][0]][0], e[2][1], vkey(e[2][2][0])))
        title_ok = fillable("name") and eff["role"] in TITLE_ROLES
        won = False
        for st, h, item in ev_title:
            v = item[2][0]
            if st == "ok" and v in ("", ABSENT):
                st = "no-value"
            if st == "ok" and title_ok and not won:
                won = True
                tier = SOURCES[item[0][0]][3]
                eff["name"], layer["name"] = v, "derived"
                info["name"] = (PROV["DerivedTainted"], hedge_cap(h, tier), tier)
                trail["name"].append(self.fact_trail("derived:title", item, "won", h))
            else:
                trail["name"].append(self.fact_trail("derived:title", item,
                                                     ("not-applicable" if st == "ok" and not won else
                                                      "shadowed" if st == "ok" else st), h))
        # S4 inferred: semantic facts of Inferred sources + rule visual.ocr_text -> name
        for g in ("name", "description"):
            if g not in G:
                continue
            cands = []
            for item in facts:
                (src, anchor, fg) = item[0]
                if SOURCES[src][2] != "Inferred":
                    continue
                if fg == g or (g == "name" and fg == "visual.ocr_text"):
                    st, h = self.status(R, x, item)
                    if st == "ok" and item[2][0] in ("", ABSENT):
                        st = "no-value"
                    rule = 0 if fg == g else 1
                    cands.append((st, h, rule, item))
            cands.sort(key=lambda e: (e[0] != "ok", e[1] if e[1] is not None else 9,
                                      SOURCES[e[3][0][0]][4], e[3][0][0], e[2], vkey(e[3][2][0]), e[3][0][1], e[3][1]))
            can_fill = fillable(g)
            won = False
            for st, h, rule, item in cands:
                if st == "ok" and can_fill and not won:
                    won = True
                    tier = SOURCES[item[0][0]][3]
                    eff[g], layer[g] = item[2][0], "inferred"
                    info[g] = (PROV["Inferred"], hedge_cap(h, tier), tier)
                    trail[g].append(self.fact_trail("inferred", item, "won", h))
                else:
                    trail[g].append(self.fact_trail("inferred", item,
                                                    ("shadowed" if st == "ok" else st), h))
        # defaults
        known = set()
        for g in G:
            if eff[g] == UNKNOWN:
                eff[g], layer[g], info[g] = DEFAULT2[g], "default", (PROV["Default"], 0, 0)
            else:
                known.add(g)
        if eff["name"] == ABSENT:
            eff["name"] = ""
        if eff["description"] == ABSENT:
            eff["description"] = ""
        eff, masked = repair(eff)
        for g in masked:
            info[g] = (PROV["Default"], 0, 0)
        # observed statuses
        stale, fallback = set(), set()
        for g in G:
            for c in trail[g]:
                if c["layer"] == "observed":
                    if c["value"] == UNKNOWN:
                        c["status"] = "unknown"
                    elif layer[g] == "observed" and c["src"] == b[g][1]:
                        c["status"] = "won"
                    else:
                        c["status"] = "shadowed"
            if layer[g] == "observed" and g not in masked:
                if b[g][3]["stale"]:
                    stale.add(g)
                if b[g][3]["fallback"]:
                    fallback.add(g)
            if g in masked:
                trail[g].append({"layer": "repair", "value": eff[g], "masked": masked[g], "status": "won"})
        evid = [self.fact_trail("evidence", it, self.status(R, x, it)[0]) for it in facts
                if it[0][2] in EVIDENCE and it[0][2] not in ("hint.title_attr", "visual.ocr_text")]
        return {"values": eff, "info": info, "known": known, "stale": stale, "fallback": fallback,
                "trail": trail, "evidence": evid}

    def full2(self, R):
        by_node, unbound = self.bind_all(R)
        bases = [self.base2(R, x, by_node.get(x, [])) for x in range(self.n)]
        inst = self.installed(R)
        finals = {}
        for x in sorted(range(self.n), key=lambda y: -self.depth[y]):
            finals[x] = self.fuse_node2(R, x, bases, finals, inst, by_node.get(x, []))
        parked = [self.fact_trail("parked", it, "unbound") for it in unbound]
        return finals, parked


# --- projections, encodings, utterances -----------------------------------------------------------

def proj(world, finals):
    """Effective state without trails (what F12/F14 compare)."""
    out = {}
    for x, f in finals.items():
        if f is None:
            out[x] = None
        else:
            out[x] = (tuple((g, f["values"][g], f["info"][g]) for g in world.groups),
                      tuple(sorted(f["known"])), tuple(sorted(f["stale"])), tuple(sorted(f["fallback"])))
    return out


def encode(world, f):
    """Layout encoding of one entry: ProvTag, hedge, taint and group masks as integers."""
    L = LAYOUTS[world.layout]
    code = {g: i for i, g in enumerate(L["groups"])}
    prov = hedge = taint = known = stale = fb = 0
    for g in world.groups:
        c, h, t = f["info"][g]
        i = code[g]
        prov |= c << (4 * i)
        hedge |= h << (2 * i)
        taint |= t << (2 * i)
        known |= (g in f["known"]) << i
        stale |= (g in f["stale"]) << i
        fb |= (g in f["fallback"]) << i
    assert prov < (1 << L["prov_bits"]) and hedge < (1 << L["hedge_bits"])
    assert known < (1 << L["mask_bits"])
    return {"prov_tag": "0x%0*x" % (L["prov_bits"] // 4, prov), "hedge": "0x%0*x" % (L["hedge_bits"] // 4, hedge),
            "taint": ("0x%0*x" % (L["taint_bits"] // 4, taint)) if L["taint_bits"] else None,
            "known": known, "stale": stale, "fallback": fb}


def utterances(world, before, after):
    """State-change utterances between two published states (LB-5, H-3): only value changes of a
    state group whose new winner is Observed-primary, Patch or User speak."""
    n = 0
    speak = {PROV["ObservedPrimary"], PROV["Patch"], PROV["User"]}
    for x in range(world.n):
        a, b = before.get(x), after.get(x)
        if a is None or b is None:
            continue
        for g in world.groups:
            if g in STATE_GROUPS and a["values"][g] != b["values"][g] and b["info"][g][0] in speak:
                n += 1
    return n


def utterance_sources(world, before, after):
    """Like utterances() but returns the offending (x, g, code) for non-speaking transitions that a
    naive presenter would announce (value change of a state group with a non-speaking winner)."""
    out = []
    speak = {PROV["ObservedPrimary"], PROV["Patch"], PROV["User"]}
    for x in range(world.n):
        a, b = before.get(x), after.get(x)
        if a is None or b is None:
            continue
        for g in world.groups:
            if g in STATE_GROUPS and a["values"][g] != b["values"][g] and b["info"][g][0] not in speak:
                out.append((x, g, b["info"][g][0]))
    return out


# --- independent predicate checker (F11, F12 parts, F13, F15, F18, F19, FS-5) -----------------------

def check_sources(world, R, finals):
    """Re-derives every non-primary winner's admissibility from the registers without using
    World2.status(). Returns a Counter of violations (all must be 0)."""
    bad = Counter()
    idx = world.dom_index(R)
    for key, (seq, rec) in R.fact.items():
        src, anchor, g = key
        if rec[6] != world.scope:
            bad["F19-stored-out-of-scope"] += 1
        if anchor[0] == "dom":
            cands = idx.get(anchor[1], [])
            x, q, gen = world.resolve(R, anchor, idx)
            if (anchor[2] != 1 or len(cands) != 1) and q == BQ["Unique"]:
                bad["F15-ambiguous-unique"] += 1
    for x, f in finals.items():
        if f is None:
            continue
        for g in world.groups:
            code, h, t = f["info"][g]
            won = [c for c in f["trail"][g] if c.get("status") == "won"]
            if code == PROV["ObservedSecondary"] and g not in INEXPRESSIBLE[world.app]:
                bad["F12-secondary-on-expressible"] += 1
            if code in (PROV["ObservedSecondary"], PROV["Inferred"]) or (
                    code == PROV["DerivedTainted"] and any(c["layer"] == "derived:title" for c in won)):
                facts = [c for c in won if "anchor" in c]
                if len(facts) != 1:
                    bad["trail-missing-winner"] += 1
                    continue
                c = facts[0]
                k = (c["src"], tuple(c["anchor"]), c["group"])
                if k not in R.fact or R.fact[k][0] != c["seq"]:
                    bad["F3-trail-not-register"] += 1
                    continue
                v, p, basis, fgen, tt, ttl, sc = R.fact[k][1]
                bx, q, gen = world.resolve(R, k[1])
                gc = gclass(k[2])
                cls = SOURCES[k[0]][2]
                if bx != x:
                    bad["bound-elsewhere"] += 1
                if q < MIN_BIND[gc]:
                    bad["F15-weak-binding-won"] += 1
                if fgen != gen:
                    bad["F18-rebound-won"] += 1
                if (cls == "Inferred" or k[2].startswith("visual.")) and basis is not None and basis < R.vis.get(x, 0):
                    bad["F13-stale-won"] += 1
                if ttl is not None and tt + ttl <= R.clock:
                    bad["FS5-expired-won"] += 1
                if p < GRANTS[k[0]]["floor"][gc]:
                    bad["F11-below-floor-won"] += 1
                if R.epoch.get(k[0], (0, "Active"))[1] in MASKED_STATES | {"Detached"}:
                    bad["F14-masked-won"] += 1
                if SOURCES[k[0]][3] >= 3 and h < CONF["Probable"]:
                    bad["F12-T3-not-hedged"] += 1
                if h == CONF["Asserted"]:
                    bad["F12-nonprimary-plain"] += 1
                if t != SOURCES[k[0]][3]:
                    bad["F12-taint-tier"] += 1
            if code == PROV["DerivedTainted"] and (h == CONF["Asserted"] or t == 0):
                bad["F12-tainted-not-hedged"] += 1
            if code == PROV["Derived"]:
                for c in won:
                    for y in c.get("tainted_from", []):
                        bad["F12-derived-from-tainted"] += 1
            if code in PROTECTED | {PROV["Default"]} and (h != 0 or t != 0):
                bad["hedge-on-plain"] += 1
    return bad


# --- workload generation --------------------------------------------------------------------------

def random_events2(rng, world, n_events, patches, p_fact=0.34):
    """Canonical event log with writer-stamped facts. A running register set gives the stamp
    (bind_gen, admitted_t, basis); some facts are deliberately stamped stale."""
    n = world.n
    R = Registers2()
    seq = Counter()
    evs = []
    ids = ["id%d" % i for i in range(max(2, n // 2))]
    vis = Counter()
    clock = 0

    def emit(ev):
        evs.append(ev)
        ingest(world, R, [ev])

    for x in range(n):
        for src in OBS:
            if rng.random() < 0.9:
                seq[src] += 1
                emit(("life", src, x, seq[src], True))
        if rng.random() < 0.6:
            seq["atspi"] += 1
            emit(("attr", "atspi", x, seq["atspi"], rng.choice(ids)))
    inexp = INEXPRESSIBLE[world.app]
    epoch_n = Counter()
    for _ in range(n_events):
        r = rng.random()
        x = rng.randrange(n)
        src = rng.choice(OBS)
        if r < p_fact:
            s = rng.choice(NONPRIMARY)
            gr = GRANTS[s]
            ak = rng.choice(sorted(gr["anchors"]))
            if ak == "dom":
                dom = R.attr.get(("atspi", x), (None, None))[1] or rng.choice(ids)
                anchor = ("dom", dom, 1 if rng.random() < 0.8 else 2)
            else:
                anchor = (ak, x)
            if gr["evidence"] and rng.random() < 0.35:
                g = rng.choice(sorted(gr["evidence"]))
                v = rng.choice(EVID_TEXT) if g != "layout.css_hidden" else rng.choice(["true", "false"])
            else:
                g = rng.choice(sorted(gr["semantic"]))
                v = rng.choice(DOMAIN2[g])
                if rng.random() < 0.02:
                    v = "bogus"
            if rng.random() < 0.05:
                v = RETRACT
            if rng.random() < 0.02:
                g = "check" if s != "bus" else g       # outside the grant for inferred sources
            seq[s] += 1
            bx, q, gen = world.resolve(R, anchor)
            if rng.random() < 0.1:
                gen = max(0, gen - 1)
            tx = bx if bx is not None else x
            basis = vis[tx] - (1 if rng.random() < 0.1 else 0)
            p = rng.choice([rng.randint(600, 1000), rng.randint(880, 1000), 1000])
            ttl = None if SOURCES[s][2] == "Secondary" and rng.random() < 0.7 else rng.choice([None, 5000, 60000, 300000])
            scope = world.scope if rng.random() > 0.03 else "app2"
            emit(("fact", s, scope, anchor, g, seq[s], v, p, basis, gen, clock, ttl))
        elif r < p_fact + 0.04:
            vis[x] += 1
            emit(("vis", x, vis[x]))
        elif r < p_fact + 0.07:
            clock += rng.choice([1000, 5000, 30000, 120000])
            emit(("tick", clock))
        elif r < p_fact + 0.11:
            seq["atspi"] += 1
            emit(("attr", "atspi", x, seq["atspi"], rng.choice(ids + [None])))
        elif r < p_fact + 0.12:
            s = rng.choice(NONPRIMARY)
            epoch_n[s] += 1
            st = rng.choice(["Active", "Active", "Throttled", "Detached", "Paused", "Quarantined", "Revoked"])
            emit(("epoch", s, epoch_n[s], st))
        elif r < p_fact + 0.14:
            seq["user"] += 1
            g = rng.choice(world.groups)
            emit(("user", x, g, seq["user"], CLEAR if rng.random() < 0.3 else rng.choice(DOMAIN2[g])))
        elif r < p_fact + 0.15 and patches:
            seq["patch"] += 1
            emit(("patch", rng.choice(patches).pid, seq["patch"], rng.random() < 0.7))
        elif r < p_fact + 0.19:
            seq[src] += 1
            emit(("inval", src, x, seq[src]))
        elif r < p_fact + 0.21:
            seq[src] += 1
            emit(("life", src, x, seq[src], rng.random() < 0.8))
        else:
            g = rng.choice([h for h in world.groups if h not in inexp])
            seq[src] += 1
            s_ = seq[src] - rng.randrange(0, 20) if rng.random() < 0.2 else seq[src]
            v = ABSENT if (g != "role" and rng.random() < 0.1) else rng.choice(DOMAIN2[g])
            emit(("obs", src, x, g, max(1, s_), v))
    return evs


def run2(world, events, stats=None):
    R = ingest(world, Registers2(), events, stats)
    return R, world.full2(R)[0]


def is_protected(g, v, code):
    """F12 scope: User, Patch, untainted Derived, and Observed-primary unless the observed value is
    empty on a fillable group (the A6 fill rule lets S3/S4 fill observed-empty name/description)."""
    if code not in PROTECTED:
        return False
    return not (code == PROV["ObservedPrimary"] and g in ("name", "description") and v in ("", ABSENT))


def deps_of(world, x, g):
    """D(x, g) for a non-primary fact on (x, g): itself; for name, every ancestor's name."""
    d = {(x, g)}
    if g in ("name", "visual.ocr_text", "hint.title_attr"):
        d.add((x, "name"))
        p = world.parent[x]
        while p >= 0:
            d.add((p, "name"))
            p = world.parent[p]
    return d


def diff_cells(world, a, b):
    out = set()
    for x in range(world.n):
        fa, fb = a.get(x), b.get(x)
        if (fa is None) != (fb is None):
            out.add((x, "*"))
            continue
        if fa is None:
            continue
        for g in world.groups:
            if fa["values"][g] != fb["values"][g] or fa["info"][g] != fb["info"][g]:
                out.add((x, g))
    return out


def exp_sources(trials=150, n=30, n_events=500, seed=11, layout="x32", content_rule="untainted-first",
                perms=4, verbose=True):
    """F1 (extended), F11-F19 on random workloads. Returns a Counter."""
    rng = random.Random(seed)
    st = Counter()
    t0 = time.time()
    for t in range(trials):
        app = rng.choice(sorted(PI))
        patches = random_patches(rng, 4)
        world = World2(random_tree(rng, n), app, patches, layout=layout, content_rule=content_rule)
        evs = random_events2(rng, world, n_events, patches)
        adm = Counter()
        R, fin = run2(world, evs, adm)
        st.update({"adm:" + k: v for k, v in adm.items()})
        bad = check_sources(world, R, fin)
        st.update(bad)
        st["predicate-violations"] += sum(bad.values())
        codes = Counter(PROV_NAME[f["info"][g][0]] for f in fin.values() if f for g in world.groups)
        st.update({"code:" + k: v for k, v in codes.items()})
        # F1: delivery order (reversed + shuffled with duplicates)
        ref = (R.snapshot(), repr(fin))
        for k in range(perms):
            order = list(reversed(evs)) if k == 0 else [e for b in deliver(rng, evs) for e in b]
            R2, f2 = run2(world, order)
            st["F1-runs"] += 1
            st["F1-mismatch"] += (R2.snapshot(), repr(f2)) != ref
        # F12: adding all non-primary facts never changes a protected (x, g)
        evs0 = [e for e in evs if e[0] != "fact"]
        R0, f0 = run2(world, evs0)
        p0, p1 = proj(world, f0), proj(world, fin)
        for x in range(n):
            if p0[x] is None:
                st["F12-existence-changed"] += p1[x] is not None
                continue
            for (g, v, inf), (g1, v1, inf1) in zip(p0[x][0], p1[x][0]):
                if is_protected(g, v, inf[0]):
                    st["F12-checked"] += 1
                    if (v, inf[0]) != (v1, inf1[0]):
                        st["F12-protected-changed"] += 1
                        st["F12-protected-changed:" + g] += 1
        # F14: revoke / pause / quarantine / detach s == run without s
        for s in NONPRIMARY:
            evs_wo = [e for e in evs if not (e[0] in ("fact", "epoch") and e[1] == s)]
            _, fwo = run2(world, evs_wo)
            for state in ("Revoked", "Paused", "Quarantined", "Detached"):
                _, fm = run2(world, evs + [("epoch", s, 10 ** 6, state)])
                st["F14-runs"] += 1
                st["F14-mismatch"] += proj(world, fm) != proj(world, fwo)
            # F16 (pure): a flood from s leaves other sources' registers and protected cells alone
        flood_src = rng.choice(NONPRIMARY)
        flood = []
        fseq = 10 ** 6
        for i in range(1500):
            fseq += 1
            x = rng.randrange(n)
            g = rng.choice(sorted(GRANTS[flood_src]["semantic"] | GRANTS[flood_src]["evidence"]) + ["check"])
            flood.append(("fact", flood_src, world.scope if rng.random() < 0.9 else "app2",
                          ("node", x), g, fseq, rng.choice(DOMAIN2.get(g, EVID_TEXT)), rng.randint(0, 1000),
                          0, 0, 0, None))
        mixed = list(evs)
        for e in flood:
            mixed.insert(rng.randrange(len(mixed) + 1), e)
        Rf = Registers2()
        cap = 64
        keys = set()
        drops = 0
        for e in mixed:                       # admission with a per-(source, scope) register cap
            if e[0] == "fact":
                stt, key, item = admit(e, world.scope, world.layout)
                if key is None:
                    continue
                is_flood = key[0] == flood_src and e[5] > 10 ** 6
                if is_flood and key not in keys and len(keys) >= cap:
                    drops += 1
                    continue
                if is_flood:
                    keys.add(key)
                Rf.apply(("fact_admitted", key, item))
            else:
                Rf.apply(e)
        ff = world.full2(Rf)[0]
        st["F16-drops"] += drops
        other = {k: v for k, v in Rf.fact.items() if k[0] != flood_src}
        other_ref = {k: v for k, v in R.fact.items() if k[0] != flood_src}
        st["F16-other-registers-changed"] += other != other_ref
        st["F16-cap-exceeded"] += len(keys) > cap
        pf = proj(world, ff)
        for x in range(n):
            if p0[x] is None or pf[x] is None:
                continue
            for (g, v, inf), (g1, v1, inf1) in zip(p0[x][0], pf[x][0]):
                if is_protected(g, v, inf[0]) and (v, inf[0]) != (v1, inf1[0]):
                    st["F16-protected-changed"] += 1
        # F11: move one winning non-primary fact below its floor
        winners = [(x, g, c) for x, f in fin.items() if f for g in world.groups
                   for c in f["trail"][g] if c.get("status") == "won" and "anchor" in c]
        rng.shuffle(winners)
        if winners:
            x, g, c = rng.choice(winners)
            key = (c["src"], tuple(c["anchor"]), c["group"])
            floor = GRANTS[c["src"]]["floor"][gclass(c["group"])]
            evs_lo = [e[:7] + (floor - 1,) + e[8:] if (e[0] == "fact" and (e[1], tuple(e[3]), e[4]) == key
                                                      and e[5] == c["seq"]) else e for e in evs]
            Rl, fl = run2(world, evs_lo)
            st["F11-runs"] += 1
            st.update({"F11-" + k: v for k, v in check_sources(world, Rl, fl).items()})
            ch = diff_cells(world, fin, fl)
            if not ch <= deps_of(world, x, c["group"]) | deps_of(world, x, g):
                st["F11-nonlocal-change"] += 1
            # F13: bump vis_rev of x; inferred winners on x fall, and nothing speaks
            Rv = ingest(world, Registers2(), evs + [("vis", x, R.vis.get(x, 0) + 1)])
            fv = world.full2(Rv)[0]
            st["F13-runs"] += 1
            st["F13-utterances"] += utterances(world, fin, fv)
            fx = fv.get(x)
            if fx is not None:
                for h in world.groups:
                    for cc in fx["trail"][h]:
                        if cc.get("status") == "won" and SOURCES.get(cc.get("src"), (0, 0, ""))[2] == "Inferred":
                            st["F13-inferred-survived"] += 1
        # F18: move a winning DomId to another node (or drop it); old-gen facts never win
        dom_w = [w_ for w_ in winners if w_[2]["anchor"][0] == "dom"]
        for x, g, c in dom_w[:4]:
            holders = [y for (s, y), (sq, d) in R.attr.items() if s == "atspi" and d == c["anchor"][1]]
            Rr = ingest(world, Registers2(), evs + [("attr", "atspi", y, 10 ** 7 + i, None)
                                                    for i, y in enumerate(holders)]
                        + [("attr", "atspi", rng.randrange(n), 10 ** 7 + 99, c["anchor"][1])])
            fr = world.full2(Rr)[0]
            st["F18-runs"] += 1
            st.update({"F18-" + k: v for k, v in check_sources(world, Rr, fr).items()})
            for y, f in fr.items():
                if f is None:
                    continue
                for h in world.groups:
                    for cc in f["trail"][h]:
                        if (cc.get("status") == "won" and cc.get("src") == c["src"]
                                and cc.get("anchor") == c["anchor"] and cc.get("seq") == c["seq"]):
                            st["F18-old-gen-won"] += 1
        # F15: count ambiguous DomId registers the predicate checker saw
        idx = world.dom_index(R)
        for (s_, a_, g_), _v in R.fact.items():
            if a_[0] == "dom" and (a_[2] != 1 or len(idx.get(a_[1], [])) > 1):
                st["F15-ambiguous-checked"] += 1
        # LB-5: transitions caused purely by non-primary facts never speak
        st["LB5-silent-transitions"] += len(utterance_sources(world, f0, fin))
        st["LB5-utterances-from-nonprimary"] += utterances(world, f0, fin)
        # F19: out-of-scope facts never stored, and the result equals the run without them
        evs_in = [e for e in evs if not (e[0] == "fact" and e[2] != world.scope)]
        Ri, fi = run2(world, evs_in)
        st["F19-runs"] += 1
        st["F19-mismatch"] += (Ri.snapshot(), repr(fi)) != ref
    st["seconds"] = round(time.time() - t0, 1)
    for k in ("F1-mismatch", "F11-nonlocal-change", "F12-protected-changed", "F13-utterances",
              "F13-inferred-survived", "F14-mismatch", "F16-other-registers-changed", "F16-protected-changed",
              "F16-cap-exceeded", "F18-old-gen-won", "F19-mismatch", "LB5-utterances-from-nonprimary"):
        st[k] += 0
    if verbose:
        keys = sorted(k for k in st if not k.startswith(("adm:", "code:")))
        print("sources[%s, %s]:" % (layout, content_rule), {k: st[k] for k in keys})
        print("  admission:", {k[4:]: st[k] for k in sorted(st) if k.startswith("adm:")})
        print("  winners by ProvCode:", {k[5:]: st[k] for k in sorted(st) if k.startswith("code:")})
    return st


# --- test vectors TV7-TV20 (also emitted into the corpus) ------------------------------------------

def F(src, anchor, g, seq, v, p=1000, basis=0, gen=None, t=0, ttl=None, scope="app1"):
    """Fact event; gen=None means 'stamp with the writer's current binding generation'."""
    return ("fact", src, scope, tuple(anchor), g, seq, v, p, basis, gen, t, ttl)


def stamp(world, evs):
    """Fill gen=None facts with the binding generation current at their canonical position."""
    R = Registers2()
    out = []
    for e in evs:
        if e[0] == "fact" and e[9] is None:
            e = e[:9] + (world.resolve(R, e[3])[2],) + e[10:]
        out.append(e)
        ingest(world, R, [e])
    return out


def _live(src, xs, seq0=1):
    return [("life", src, x, seq0 + i, True) for i, x in enumerate(xs)]


def tv_cases(layout):
    """Each case: (id, description, world, events, check(fin, R) -> None)."""
    cases = []

    def add(cid, desc, parent, app, evs, check, patches=()):
        w = World2(parent, app, list(patches), layout=layout)
        cases.append((cid, desc, w, stamp(w, evs), check))

    # TV7: secondary wins only a statically inexpressible group, hedged and tainted by tier
    def c7(f, R):
        x = f[1]
        assert x["values"]["live"] == "assertive" and x["info"]["live"] == (PROV["ObservedSecondary"], CONF["Probable"], 3), x["info"]["live"]
        assert x["values"]["check"] == "off" and x["info"]["check"][0] == PROV["Default"], x["info"]["check"]
        assert any(c.get("status") == "not-candidate" for c in x["trail"]["check"])
    add("TV7", "secondary wins inexpressible 'live' (hedged Probable, taint T3); 'check' is not a candidate",
        [-1, 0], "firefox",
        _live("atspi", [0, 1]) + [("obs", "atspi", 1, "role", 10, "checkbox"),
                                  F("bus", ("node", 1), "live", 1, "assertive", 990),
                                  F("bus", ("node", 1), "check", 2, "on", 1000)], c7)

    # TV8: dynamic Unknown never admits a secondary (gtk-newton has no inexpressible rows)
    def c8(f, R):
        assert f[1]["values"]["live"] == "off" and f[1]["info"]["live"][0] == PROV["Default"]
    add("TV8", "no inexpressible row: an Unknown primary group stays default", [-1, 0], "gtk-newton",
        _live("newton", [0, 1]) + [F("bus", ("node", 1), "live", 1, "polite")], c8)

    # TV9: taint through derivation: title evidence names a button, parent link derives from it
    def c9(f, R):
        b, l = f[2], f[1]
        assert b["values"]["name"] == "Delete" and b["info"]["name"] == (PROV["DerivedTainted"], CONF["Probable"], 3), b["info"]["name"]
        assert l["values"]["name"] == "Delete" and l["info"]["name"][0] == PROV["DerivedTainted"], l["info"]["name"]
    add("TV9", "T3 title evidence -> DerivedTainted 'probably Delete'; taint propagates to the parent's content name",
        [-1, 0, 1], "firefox",
        _live("atspi", [0, 1, 2]) + [("obs", "atspi", 1, "role", 10, "link"), ("obs", "atspi", 2, "role", 11, "button"),
                                     ("obs", "atspi", 2, "name", 12, ""),
                                     F("bus", ("node", 2), "hint.title_attr", 1, "Delete", 1000)], c9)

    # TV10: S4 ranking (ConfClass, kind, id); T3 extension capped at Probable
    def c10(f, R):
        assert f[1]["values"]["name"] == "Search" and f[1]["info"]["name"] == (PROV["Inferred"], CONF["High"], 2), f[1]["info"]["name"]
        assert f[2]["values"]["name"] == "Close" and f[2]["info"]["name"] == (PROV["Inferred"], CONF["Probable"], 2), f[2]["info"]["name"]
    add("TV10", "S4: OCR High beats T3 extension (High capped to Probable); equal class -> kind order (ocr < icon)",
        [-1, 0, 0], "firefox",
        _live("atspi", [0, 1, 2]) + [("obs", "atspi", 1, "role", 10, "img"), ("obs", "atspi", 2, "role", 11, "img"),
                                     F("ext.a", ("node", 1), "name", 1, "Menu", 990),
                                     F("ocr", ("region", 1), "visual.ocr_text", 1, "Search", 980),
                                     F("icon", ("node", 2), "name", 1, "Open", 950),
                                     F("ocr", ("region", 2), "visual.ocr_text", 2, "Close", 920)], c10)

    # TV11: S4 sits below S3 (content name wins over an inferred name)
    def c11(f, R):
        assert f[1]["values"]["name"] == "Save" and f[1]["info"]["name"][0] == PROV["Derived"]
        assert any(c["layer"] == "inferred" and c["status"] == "shadowed" for c in f[1]["trail"]["name"])
    add("TV11", "S3 content name beats S4 inferred name", [-1, 0, 1], "firefox",
        _live("atspi", [0, 1, 2]) + [("obs", "atspi", 1, "role", 10, "button"), ("obs", "atspi", 2, "role", 11, "text"),
                                     ("obs", "atspi", 2, "name", 12, "Save"),
                                     F("icon", ("node", 1), "name", 1, "Open", 990)], c11)

    # TV12: below the floor never wins
    def c12(f, R):
        assert f[1]["values"]["name"] == "" and any(c["status"] == "low-confidence" for c in f[1]["trail"]["name"])
    add("TV12", "inferred fact below its grant floor is stored, never wins", [-1, 0], "firefox",
        _live("atspi", [0, 1]) + [("obs", "atspi", 1, "role", 10, "img"),
                                  F("icon", ("node", 1), "name", 1, "Open", 799)], c12)

    # TV13: vis_rev staleness
    def c13(f, R):
        assert f[1]["values"]["name"] == "Logo" and f[1]["info"]["name"][0] == PROV["Inferred"]
        assert f[2]["values"]["name"] == "" and any(c["status"] == "stale" for c in f[2]["trail"]["name"])
    add("TV13", "vis_rev bump past an inferred basis makes it stale (falls to default)", [-1, 0, 0], "firefox",
        _live("atspi", [0, 1, 2]) + [("obs", "atspi", 1, "role", 10, "img"), ("obs", "atspi", 2, "role", 11, "img"),
                                     F("icon", ("node", 1), "name", 1, "Logo", 990, basis=0),
                                     F("icon", ("node", 2), "name", 2, "Logo", 990, basis=0), ("vis", 2, 1)], c13)

    # TV14: BindGen: DomId moves to another node; the old fact never wins there until re-asserted
    def c14(f, R):
        assert f[1]["values"]["name"] == "" and f[2]["values"]["name"] == "Menu", (f[1]["values"], f[2]["values"])
        assert f[2]["info"]["name"][0] == PROV["Inferred"]
        assert any(c["status"] == "rebound" and c["value"] == "Open" for c in f[2]["trail"]["name"])
    add("TV14", "DomId rebinding: old-generation fact is 'rebound' on the new node; re-assertion wins",
        [-1, 0, 0], "firefox",
        _live("atspi", [0, 1, 2]) + [("obs", "atspi", 1, "role", 10, "img"), ("obs", "atspi", 2, "role", 11, "img"),
                                     ("attr", "atspi", 1, 12, "id1"),
                                     F("icon", ("dom", "id1", 1), "name", 1, "Open", 990),
                                     ("attr", "atspi", 1, 13, None), ("attr", "atspi", 2, 14, "id1"),
                                     F("ext.a", ("dom", "id1", 1), "name", 1, "Menu", 990)], c14)

    # TV15: ambiguous DomId never binds Unique: semantic fact weak, evidence fine
    def c15(f, R):
        assert f[1]["values"]["name"] == "Close" and f[1]["info"]["name"][0] == PROV["DerivedTainted"]
        assert any(c["status"] == "weak-binding" for c in f[1]["trail"]["name"])
    add("TV15", "dom_count 2: Heuristic binding; name fact weak-binding, title evidence (min Heuristic) admitted",
        [-1, 0], "firefox",
        _live("atspi", [0, 1]) + [("obs", "atspi", 1, "role", 10, "button"), ("attr", "atspi", 1, 11, "x"),
                                  F("icon", ("dom", "x", 2), "name", 1, "Open", 990),
                                  F("ext.a", ("dom", "x", 2), "hint.title_attr", 1, "Close", 990)], c15)

    # TV16: TTL expiry is a lifecycle input
    def c16(f, R):
        assert f[1]["values"]["name"] == "" and any(c["status"] == "expired" for c in f[1]["trail"]["name"])
    add("TV16", "TTL: an OCR fact expires when the clock input passes admitted_t + ttl", [-1, 0], "firefox",
        _live("atspi", [0, 1]) + [("obs", "atspi", 1, "role", 10, "img"),
                                  F("ocr", ("region", 1), "visual.ocr_text", 1, "Search", 990, ttl=5000),
                                  ("tick", 5000)], c16)

    # TV17: revocation masks a source
    def c17(f, R):
        assert f[1]["values"]["name"] == "Open" and any(c["status"] == "masked" for c in f[1]["trail"]["name"])
    add("TV17", "revoked source is masked; next S4 candidate wins", [-1, 0], "firefox",
        _live("atspi", [0, 1]) + [("obs", "atspi", 1, "role", 10, "img"),
                                  F("ext.a", ("node", 1), "name", 1, "Menu", 999),
                                  F("icon", ("node", 1), "name", 1, "Open", 950),
                                  ("epoch", "ext.a", 1, "Revoked")], c17)

    # TV18: scope pinning
    def c18(f, R):
        assert f[1]["values"]["name"] == "" and not R.fact
    add("TV18", "fact for another scope is rejected at admission and never stored", [-1, 0], "firefox",
        _live("atspi", [0, 1]) + [("obs", "atspi", 1, "role", 10, "img"),
                                  F("icon", ("node", 1), "name", 1, "Open", 990, scope="app2")], c18)

    # TV19: user and patch beat inferred
    def c19(f, R):
        assert f[1]["values"]["name"] == "Dismiss" and f[1]["info"]["name"][0] == PROV["User"]
        assert f[2]["values"]["name"] == "Close" and f[2]["info"]["name"][0] == PROV["Patch"]
    add("TV19", "user and patch outrank S4", [-1, 0, 0], "firefox",
        _live("atspi", [0, 1, 2]) + [("obs", "atspi", 1, "role", 10, "img"), ("obs", "atspi", 2, "role", 11, "img"),
                                     ("obs", "atspi", 1, "name", 12, ""), ("obs", "atspi", 2, "name", 13, ""),
                                     F("icon", ("node", 1), "name", 1, "Open", 990),
                                     F("icon", ("node", 2), "name", 2, "Open", 990),
                                     ("user", 1, "name", 1, "Dismiss"), ("patch", "p0", 1, True)], c19,
        patches=[Patch("p0", "org.a", 1, "img", True, None, "name", "Close")])

    # TV20: oversize page string truncated, counted, admitted
    def c20(f, R):
        assert f[1]["values"]["name"] == "x" * MAX_STR, f[1]["values"]["name"]
    add("TV20", "page string over the cap is truncated (not a violation)", [-1, 0], "firefox",
        _live("atspi", [0, 1]) + [("obs", "atspi", 1, "role", 10, "button"),
                                  F("bus", ("node", 1), "hint.title_attr", 1, "x" * 80, 1000)], c20)
    return cases


def selftest2():
    for layout in LAYOUTS:
        for cid, desc, w, evs, check in tv_cases(layout):
            R, fin = run2(w, evs)
            check(fin, R)
            assert not check_sources(w, R, fin), (cid, check_sources(w, R, fin))
            assert (run2(w, list(reversed(evs)))[1]) == fin, cid
    st = exp_sources(trials=4, n=20, n_events=250, perms=2, verbose=False)
    bad = {k: v for k, v in st.items() if v and k.split("-")[0] in ("F1", "F11", "F12", "F13", "F14",
                                                                     "F15", "F16", "F18", "F19", "FS5")
           and not k.endswith(("runs", "checked", "drops"))}
    assert not bad, bad
    print("selftest2 ok")


# --- deterministic JSON corpus (tests/fixtures/fuse/) ------------------------------------------------

CORPUS_SCHEMA = """gorca FUSE reference corpus (M22a-A3), produced by tools/research/semantic_fusion.py corpus.
Deterministic: same script + seed -> byte-identical file. One file per entry layout.
Top level: {schema, version, layout, tables, cases[]}.
tables: group_codes (fused group -> code; evidence kinds have bit 15), prov_codes, conf_codes
  (hedge values: 0 Asserted/plain, 1 High, 2 Probable, 3 Possible), bind_quality, source table
  {name: [slot, gen, class, tier, s4_kind_order]}, grants {name: {semantic, evidence, anchors, floor}},
  min_bind per group class, inexpressible rows per source profile, PI per source profile, thresholds,
  defaults, max_str (TEST-SCALE value 64 so truncation is exercised; the product limit is 1 KiB), default_ttl_ms.
case: {id, desc, profile, scope, content_rule, parent[] (static tree, root -1), patches[],
  events[] (canonical order), deliveries[] ({name, batches: [[event index...]...]}),
  admission[] (per event index: accept | accept:truncated | reject:<reason> | - for non-facts),
  expected: {nodes[]: null (not alive) | {x, values{group: value}, prov{group: ProvCode name},
  hedge{group: conf code}, taint{group: tier code, 0 = untainted}, known[], stale[], fallback[],
  enc{prov_tag, hedge, taint (null in r2-16), known, stale, fallback} (layout bit encodings),
  trail{group: [entries]}, evidence[]}, parked[] (facts with no binding)}}.
Events: ["obs", src, x, group, seq, value] | ["life", src, x, seq, alive] | ["inval", src, x, seq]
  | ["patch", pid, seq, installed] | ["user", x, group, seq, value|"<clear>"]
  | ["attr", src, x, seq, dom_id|null] | ["vis", x, rev] | ["tick", t_ms] | ["epoch", src, n, state]
  | ["fact", src, transport_scope, anchor, group, seq, value|"<retract>", p_milli, basis_vis_rev,
     bind_gen (writer-stamped), admitted_t_ms, ttl_ms|null].
  anchor: ["node", x] | ["dom", id, dom_count] | ["region", x] (toy Geometric).
Values: "?" Unknown (never in expected values), "-" Absent, "<clear>" user clear.
Every delivery (canonical, reversed, shuffled with duplicates and random batch cuts) must give the
expected state exactly (F1). Trails are part of the contract (explain, FS-9)."""


def jsonable(o):
    if isinstance(o, (list, tuple)):
        return [jsonable(e) for e in o]
    if isinstance(o, dict):
        return {str(k): jsonable(v) for k, v in o.items()}
    if isinstance(o, set):
        return sorted(o)
    return o


def case_json(cid, desc, w, evs, rng, n_shuffles=2):
    stats = []
    for e in evs:
        stats.append(admit(e, w.scope, w.layout)[0] if e[0] == "fact" else "-")
    R, fin = run2(w, evs)
    parked = w.full2(R)[1]
    idx = list(range(len(evs)))
    deliveries = [{"name": "canonical", "batches": [idx]},
                  {"name": "reversed", "batches": [idx[::-1]]}]
    for k in range(n_shuffles):
        e = idx + [i for i in idx if rng.random() < 0.1]
        rng.shuffle(e)
        batches, i = [], 0
        while i < len(e):
            m = rng.randint(1, 16)
            batches.append(e[i:i + m])
            i += m
        deliveries.append({"name": "shuffled-dup-%d" % k, "batches": batches})
    for d in deliveries:                  # the corpus is self-checking: every delivery agrees
        order = [evs[i] for b in d["batches"] for i in b]
        assert run2(w, order)[1] == fin, (cid, d["name"])
    nodes = []
    for x in range(w.n):
        f = fin[x]
        if f is None:
            nodes.append(None)
            continue
        nodes.append({"x": x, "values": f["values"],
                      "prov": {g: PROV_NAME[f["info"][g][0]] for g in w.groups},
                      "hedge": {g: f["info"][g][1] for g in w.groups},
                      "taint": {g: f["info"][g][2] for g in w.groups},
                      "known": sorted(f["known"]), "stale": sorted(f["stale"]),
                      "fallback": sorted(f["fallback"]), "enc": encode(w, f),
                      "trail": f["trail"], "evidence": f["evidence"]})
    return {"id": cid, "desc": desc, "profile": w.app, "scope": w.scope, "content_rule": w.content_rule,
            "parent": w.parent, "patches": [vars(p) for p in sorted(w.patches.values(), key=lambda p: p.pid)],
            "events": evs, "deliveries": deliveries, "admission": stats,
            "expected": {"nodes": nodes, "parked": parked}}


def build_corpus(layout, seed=20261019, n_random=16):
    rng = random.Random(seed)
    L = LAYOUTS[layout]
    tables = {
        "group_codes": dict({g: i for i, g in enumerate(L["groups"])}, **EVIDENCE),
        "modelled_groups": [g for g in MODELLED if g in L["groups"]],
        "layout": {k: v for k, v in L.items() if k != "groups"},
        "prov_codes": PROV, "conf_codes": CONF, "bind_quality": BQ, "class_codes": CLASS_CODE,
        "sources": {k: list(v) for k, v in SOURCES.items()},
        "grants": {k: {"semantic": sorted(v["semantic"]), "evidence": sorted(v["evidence"]),
                       "anchors": sorted(v["anchors"]), "floor": v["floor"]} for k, v in GRANTS.items()},
        "min_bind": {k: BQ_NAME[v] for k, v in MIN_BIND.items()},
        "inexpressible": {k: sorted(v) for k, v in INEXPRESSIBLE.items()},
        "pi": {app: {g: list(PI[app].get(g, PI[app]["role"])) for g in MODELLED} for app in sorted(PI)},
        "thresholds": {"high": HIGH_MILLI, "probable": PROBABLE_MILLI},
        "defaults": DEFAULT2, "max_str": MAX_STR, "default_ttl_ms": DEFAULT_TTL,
        "masked_states": sorted(MASKED_STATES), "stale_states": ["Detached"],
    }
    cases = [case_json(cid, desc, w, evs, rng) for cid, desc, w, evs, _ in tv_cases(layout)]
    for i in range(n_random):
        app = sorted(PI)[i % 2]
        patches = random_patches(rng, 3)
        w = World2(random_tree(rng, 10), app, patches, layout=layout)
        evs = random_events2(rng, w, 70, patches)
        cases.append(case_json("R%02d" % i, "random workload (seeded)", w, evs, rng))
    doc = {"schema": CORPUS_SCHEMA, "version": 1, "layout": layout, "tables": tables, "cases": cases}
    return json.dumps(jsonable(doc), sort_keys=True, indent=1, ensure_ascii=True) + "\n"


def write_corpus(outdir):
    import hashlib
    import os
    os.makedirs(outdir, exist_ok=True)
    for layout in LAYOUTS:
        text = build_corpus(layout)
        assert text == build_corpus(layout), "corpus not deterministic"
        path = os.path.join(outdir, "fuse-%s.json" % layout)
        with open(path, "w") as fh:
            fh.write(text)
        print("corpus:", path, len(text), "bytes sha256", hashlib.sha256(text.encode()).hexdigest()[:16])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["selftest", "order", "ivm", "override", "match", "all",
                                    "selftest2", "sources", "corpus", "all2"])
    ap.add_argument("--out", default="tests/fixtures/fuse")
    a = ap.parse_args()
    if a.cmd in ("selftest2", "all2"):
        selftest2()
    if a.cmd in ("sources", "all2"):
        for layout in LAYOUTS:
            exp_sources(layout=layout)
        exp_sources(layout="x32", content_rule="all", seed=11)
    if a.cmd == "corpus":
        write_corpus(a.out)
    if a.cmd in ("selftest", "all"):
        selftest()
    if a.cmd in ("order", "all"):
        exp_order()
    if a.cmd in ("ivm", "all"):
        exp_ivm()
    if a.cmd in ("override", "all"):
        exp_override()
    if a.cmd in ("match", "all"):
        exp_match()


if __name__ == "__main__":
    sys.exit(main())
