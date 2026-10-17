#!/usr/bin/env python3
"""semantic_fusion: reference model of gorca's semantic fusion point (ADR-0022 companion).

Research prototype for docs/reports/semantic-fusion-math.md. Pure Python, no dependencies.

  python tools/research/semantic_fusion.py selftest     # test vectors + all properties, small
  python tools/research/semantic_fusion.py order        # order-independence experiment (+ naive baseline)
  python tools/research/semantic_fusion.py ivm          # incremental vs from-scratch, work per event
  python tools/research/semantic_fusion.py override     # override dominance / locality / reversibility
  python tools/research/semantic_fusion.py match        # cross-connector node matching precision/recall
  python tools/research/semantic_fusion.py all

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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["selftest", "order", "ivm", "override", "match", "all"])
    a = ap.parse_args()
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
