#!/usr/bin/env python3
"""aria_cover: constrained, weighted, mixed-strength covering arrays for gorca's web presentation.

Research prototype (docs/reports/aria-test-coverage-math.md). Pure Python, no dependencies.

  python tools/research/aria_cover.py stats            # space size, array sizes, bounds
  python tools/research/aria_cover.py gen --out DIR     # covering array -> HTML pages + manifest
  python tools/research/aria_cover.py faults            # interaction-fault detection simulation
  python tools/research/aria_cover.py mutants           # oracle power of the properties (toy presenter)
  python tools/research/aria_cover.py selftest          # test vectors

Model
-----
A test case ("row") assigns one value to every PAGE factor (role, implementation, naming, content,
ARIA states...). RUN factors (presentation context, engine, verbosity, language) are not covered
combinatorially: every node of a generated page is visited in every context, and the properties
compare the contexts with each other, so the context axis is crossed exhaustively for free.

Constraint normal form (keeps tuple validity exact and cheap):
  (a) unary domains conditioned on (role, impl): ALLOWED[(role, impl)][factor] -> set of values;
  (b) a short list of forbidden value pairs between non-role factors;
  (c) every factor has a "safe" value ("-" = absent) that no pair constraint forbids.
Validity of a partial assignment = a DFS completion exists; with (c) the DFS is shallow.
"""
from __future__ import annotations

import argparse
import html
import itertools
import json
import math
import random
import sys
import time
from collections import Counter, defaultdict

# --------------------------------------------------------------------------------------------
# 1. Input space (realistic ARIA 1.2 subset)
# --------------------------------------------------------------------------------------------

A = "-"  # attribute absent

FACTORS: list[tuple[str, list[str]]] = [
    ("role", []),  # filled from ROLES below
    ("impl", ["native", "aria"]),
    ("name_src", ["contents", "aria-label", "labelledby", "title", "label-for", "alt", "none"]),
    ("content", ["text", "icon", "text+icon", "split-text", "empty"]),
    ("desc", [A, "describedby"]),
    ("focus", ["default", "tabindex0", "tabindex-1"]),
    ("expanded", [A, "true", "false"]),
    ("selected", [A, "true", "false"]),
    ("checked", [A, "true", "false", "mixed"]),
    ("pressed", [A, "true", "false", "mixed"]),
    ("level", [A, "1", "3", "6"]),
    ("posinset", [A, "first", "mid", "last"]),
    ("current", [A, "page", "true"]),
    ("invalid", [A, "true", "spelling"]),
    ("required", [A, "true"]),
    ("readonly", [A, "true"]),
    ("disabled", [A, "aria", "native"]),
    ("busy", [A, "true"]),
    ("haspopup", [A, "true", "menu", "dialog"]),
    ("live", [A, "polite", "assertive"]),
    ("hidden", [A, "aria-hidden", "ancestor-hidden", "sr-only"]),
    ("container", ["body", "list", "table", "form", "landmark", "link"]),
    ("value", [A, "min", "mid", "max", "valuetext"]),
]

# role: native element (None = ARIA only), name-from-content, supported attrs, required parent,
#       weight (real-world frequency heuristic: WebAIM Million / APG pattern prevalence).
FORM = {"required", "readonly", "invalid"}
ROLES: dict[str, dict] = {
    "button":       dict(native="button", nfc=True, attrs={"expanded", "pressed", "haspopup", "disabled"}, w=8),
    "link":         dict(native="a", nfc=True, attrs={"expanded", "haspopup"}, w=10),
    "checkbox":     dict(native="input:checkbox", nfc=True, attrs={"checked", "disabled", "expanded"} | FORM, w=3),
    "radio":        dict(native="input:radio", nfc=True, attrs={"checked", "disabled", "posinset", "required"}, parent="radiogroup", w=2),
    "switch":       dict(native=None, nfc=True, attrs={"checked", "disabled", "readonly"}, w=1),
    "textbox":      dict(native="input:text", nfc=False, attrs={"disabled", "haspopup", "value"} | FORM, w=4),
    "searchbox":    dict(native="input:search", nfc=False, attrs={"disabled", "value"} | FORM, w=1),
    "combobox":     dict(native="select", nfc=False, attrs={"disabled", "expanded", "haspopup", "value"} | FORM, w=2),
    "spinbutton":   dict(native="input:number", nfc=False, attrs={"disabled", "value"} | FORM, w=0.5),
    "slider":       dict(native="input:range", nfc=False, attrs={"disabled", "value", "readonly"}, w=0.7),
    "progressbar":  dict(native="progress", nfc=False, attrs={"value", "busy"}, w=0.7),
    "heading":      dict(native="h", nfc=True, attrs={"level"}, w=6),
    "img":          dict(native="img", nfc=False, attrs=set(), w=6),
    "listitem":     dict(native="li", nfc=False, attrs={"level", "posinset"}, parent="list", w=6),
    "tab":          dict(native=None, nfc=True, attrs={"selected", "expanded", "posinset", "haspopup", "disabled"}, parent="tablist", w=2),
    "option":       dict(native=None, nfc=True, attrs={"selected", "checked", "posinset", "disabled"}, parent="listbox", w=2),
    "menuitem":     dict(native=None, nfc=True, attrs={"expanded", "haspopup", "posinset", "disabled"}, parent="menu", w=2),
    "menuitemcheckbox": dict(native=None, nfc=True, attrs={"checked", "posinset", "disabled", "readonly"}, parent="menu", w=0.4),
    "menuitemradio": dict(native=None, nfc=True, attrs={"checked", "posinset", "disabled", "readonly"}, parent="menu", w=0.3),
    "treeitem":     dict(native=None, nfc=True, attrs={"expanded", "selected", "checked", "level", "posinset", "haspopup", "disabled"}, parent="tree", w=1),
    "gridcell":     dict(native=None, nfc=True, attrs={"selected", "expanded", "haspopup", "disabled"} | FORM, parent="row", w=1),
    "row":          dict(native="tr", nfc=True, attrs={"selected", "expanded", "level", "posinset", "disabled"}, parent="table", w=1),
    "columnheader": dict(native="th", nfc=True, attrs={"selected", "expanded", "disabled"} | FORM, parent="row", w=1),
    "dialog":       dict(native="dialog", nfc=False, attrs={"busy", "live"}, w=1.5),
    "alert":        dict(native=None, nfc=False, attrs={"busy", "live"}, w=1),
    "status":       dict(native="output", nfc=False, attrs={"busy", "live"}, w=1),
    "region":       dict(native="section", nfc=False, attrs={"busy", "live"}, w=1.5),
    "navigation":   dict(native="nav", nfc=False, attrs={"busy"}, w=2),
    "generic":      dict(native="div", nfc=False, attrs={"busy", "live"}, w=4),
    "none":         dict(native=None, nfc=False, attrs=set(), w=1),
}
FACTORS[0] = ("role", list(ROLES))
FNAMES = [f for f, _ in FACTORS]
FIDX = {f: i for i, f in enumerate(FNAMES)}
DOM = [vals for _, vals in FACTORS]
K = len(FACTORS)

STATE_FACTORS = ["expanded", "selected", "checked", "pressed", "level", "posinset", "invalid",
                 "required", "readonly", "disabled", "busy", "haspopup", "live", "value"]
GLOBAL = {"current", "desc", "hidden", "focus", "container"}
VOID = {"input:checkbox", "input:radio", "input:text", "input:search", "input:number",
        "input:range", "progress", "img", "select"}
LABELABLE = {"input:checkbox", "input:radio", "input:text", "input:search", "input:number",
             "input:range", "progress", "select", "output", "button"}
MIXED_OK = {"checkbox", "menuitemcheckbox", "treeitem"}  # aria-checked=mixed
NAME_PROHIBITED = {"none", "generic"}  # ARIA 1.2: aria-label/labelledby prohibited
LANDMARKISH = {"dialog", "alert", "status", "region", "navigation", "generic", "none"}


def _allowed(role: str, impl: str) -> list[set[str]] | None:
    """Unary domains for (role, impl); None if the pair itself is invalid."""
    r = ROLES[role]
    native = r["native"]
    if impl == "native" and native is None:
        return None
    if impl == "aria" and role == "generic":
        return None  # role=generic is never authored in practice
    dom = [set(v) for v in DOM]
    dom[FIDX["role"]] = {role}
    dom[FIDX["impl"]] = {impl}
    for f in STATE_FACTORS:
        if f not in r["attrs"]:
            dom[FIDX[f]] = {A}
    # naming
    ns = set(DOM[FIDX["name_src"]])
    if impl != "native" or native not in LABELABLE:
        ns.discard("label-for")
    if not (impl == "native" and role == "img"):
        ns.discard("alt")
    if role in NAME_PROHIBITED:
        ns -= {"aria-label", "labelledby"}
    dom[FIDX["name_src"]] = ns
    # content
    if impl == "native" and native in VOID:
        dom[FIDX["content"]] = {"empty"}
    if role == "img" and impl == "aria":
        dom[FIDX["content"]] = {"empty", "icon"}
    # checked/pressed tri-state
    if role not in MIXED_OK:
        dom[FIDX["checked"]].discard("mixed")
    if role == "heading" and impl == "native":
        dom[FIDX["level"]].discard(A)  # <h1>..<h6> always has a level
    if role not in {"slider", "spinbutton", "progressbar"}:
        dom[FIDX["value"]].discard("valuetext")
        if role in {"textbox", "searchbox", "combobox"}:
            dom[FIDX["value"]] &= {A, "mid"}
    if impl == "aria" or native in {"a", "h", "li", "tr", "th", "nav", "section", "div", "output",
                                    "dialog", "progress", "img"}:
        dom[FIDX["disabled"]].discard("native")
    if role in {"img", "heading", "generic", "none"}:
        pass
    else:
        if r["nfc"] or role in {"textbox", "searchbox", "combobox", "slider", "spinbutton"}:
            dom[FIDX["container"]].discard("link")  # nested interactive content
    if role in LANDMARKISH:
        dom[FIDX["container"]].discard("link")
    return dom


ALLOWED: dict[tuple[str, str], list[set[str]]] = {}
for _r in ROLES:
    for _i in DOM[FIDX["impl"]]:
        _d = _allowed(_r, _i)
        if _d is not None:
            ALLOWED[(_r, _i)] = _d

# (b) forbidden value pairs between non-role factors
FORBIDDEN: set[frozenset] = {
    frozenset({("name_src", "contents"), ("content", "empty")}),  # duplicate of name_src=none
    frozenset({("hidden", "ancestor-hidden"), ("container", "link")}),
    frozenset({("desc", "describedby"), ("hidden", "aria-hidden")}),  # irrelevant, saves rows
}
_FORB_BY = defaultdict(list)
for _p in FORBIDDEN:
    a, b = tuple(_p)
    _FORB_BY[a].append(b)
    _FORB_BY[b].append(a)


def check(assign: dict[str, str]) -> bool:
    """Partial assignment respects (a) where role and impl are fixed, and (b)."""
    for f, v in assign.items():
        for g, w in _FORB_BY.get((f, v), ()):
            if assign.get(g) == w:
                return False
    role, impl = assign.get("role"), assign.get("impl")
    if role is not None and impl is not None:
        dom = ALLOWED.get((role, impl))
        if dom is None:
            return False
        return all(v in dom[FIDX[f]] for f, v in assign.items())
    return True


_memo: dict[frozenset, bool] = {}


def feasible(assign: dict[str, str]) -> bool:
    """Exact: some full valid assignment extends `assign` (DFS over role, impl, then coupled factors)."""
    key = frozenset(assign.items())
    hit = _memo.get(key)
    if hit is not None:
        return hit
    res = _complete(dict(assign), None) is not None
    _memo[key] = res
    return res


ORDER = ["role", "impl", "name_src", "content", "hidden", "container", "desc"]


def _complete(assign, rng):
    if not check(assign):
        return None
    for f in ORDER:
        if f not in assign:
            if f == "impl" or f == "role":
                vals = list(DOM[FIDX[f]])
            else:
                dom = ALLOWED[(assign["role"], assign["impl"])]
                vals = sorted(dom[FIDX[f]])
            if rng:
                rng.shuffle(vals)
            for v in vals:
                assign[f] = v
                r = _complete(assign, rng)
                if r is not None:
                    return r
            del assign[f]
            return None
    # remaining factors are only unary-constrained; their safe value always exists
    dom = ALLOWED[(assign["role"], assign["impl"])]
    for f in FNAMES:
        if f not in assign:
            vals = sorted(dom[FIDX[f]])
            assign[f] = rng.choice(vals) if rng else (A if A in vals else vals[0])
    return assign


def count_valid() -> int:
    """Exact number of valid full assignments."""
    coupled = sorted({f for p in FORBIDDEN for f, _ in p})
    total = 0
    for (role, impl), dom in ALLOWED.items():
        free = 1
        for f in FNAMES:
            if f not in coupled and f not in ("role", "impl"):
                free *= len(dom[FIDX[f]])
        n = 0
        for combo in itertools.product(*(sorted(dom[FIDX[f]]) for f in coupled)):
            a = dict(zip(coupled, combo))
            a["role"], a["impl"] = role, impl
            if check(a):
                n += 1
        total += n * free
    return total


# --------------------------------------------------------------------------------------------
# 2. Weights (real-world frequency heuristics) and APG pattern boosts
# --------------------------------------------------------------------------------------------

VALUE_W: dict[str, dict[str, float]] = {
    "impl": {"native": 3, "aria": 1},
    "name_src": {"contents": 6, "aria-label": 4, "labelledby": 1.5, "title": 0.7, "label-for": 3,
                 "alt": 4, "none": 0.5},
    "content": {"text": 6, "icon": 2, "text+icon": 2, "split-text": 1.5, "empty": 2},
    "focus": {"default": 8, "tabindex0": 1.5, "tabindex-1": 1},
    "hidden": {A: 10, "aria-hidden": 1, "ancestor-hidden": 0.5, "sr-only": 1},
    "container": {"body": 4, "list": 2, "table": 1, "form": 2, "landmark": 3, "link": 1},
    "disabled": {A: 8, "aria": 1, "native": 1},
}
DEFAULT_ABSENT_W = 4.0  # "-" vs any concrete state value (1.0 each)

# APG / common-pattern boosts (partial assignments). A tuple consistent with a pattern on >= 2 of
# its factors gets the boost.
PATTERNS = [
    ({"role": "button", "impl": "aria", "name_src": "aria-label", "content": "icon"}, 6),  # icon button
    ({"role": "button", "impl": "native", "name_src": "aria-label", "content": "icon"}, 6),
    ({"role": "button", "pressed": "true"}, 3),  # toggle button (APG)
    ({"role": "button", "expanded": "false", "haspopup": "menu"}, 4),  # menu button (APG)
    ({"role": "link", "content": "img-alt"}, 3),
    ({"role": "link", "content": "text+icon"}, 3),
    ({"role": "link", "current": "page", "container": "landmark"}, 4),  # nav current page
    ({"role": "checkbox", "checked": "mixed", "impl": "aria"}, 3),  # tri-state checkbox (APG)
    ({"role": "tab", "selected": "true", "posinset": "first"}, 4),
    ({"role": "combobox", "expanded": "false", "haspopup": "true"}, 3),
    ({"role": "treeitem", "expanded": "true", "level": "3"}, 3),
    ({"role": "textbox", "name_src": "label-for", "required": "true", "invalid": "true"}, 4),
    ({"role": "heading", "impl": "native", "container": "link"}, 3),  # card link
    ({"role": "generic", "focus": "tabindex0"}, 3),  # clickable div
    ({"role": "region", "live": "polite"}, 3),
    ({"role": "img", "impl": "aria", "content": "icon", "name_src": "aria-label"}, 3),
]


def value_w(f: str, v: str) -> float:
    if f == "role":
        return ROLES[v]["w"]
    t = VALUE_W.get(f)
    if t:
        return t.get(v, 1.0)
    return DEFAULT_ABSENT_W if v == A else 1.0


def tuple_w(tup) -> float:
    w = 1.0
    for f, v in tup:
        w *= value_w(f, v)
    boost = 1.0
    d = dict(tup)
    for pat, b in PATTERNS:
        hits = sum(1 for f, v in d.items() if pat.get(f) == v)
        clash = any(f in pat and pat[f] != v for f, v in d.items())
        if hits >= 2 and not clash:
            boost = max(boost, b)
    return w * boost


# --------------------------------------------------------------------------------------------
# 3. Mixed-strength constrained weighted AETG
# --------------------------------------------------------------------------------------------

# 3-way groups: where the observed divergence bugs live (naming x role x content x reachability),
# and role x state interplay (e.g. "checked, unavailable", "toggle button expanded").
GROUPS3 = [
    ["role", "impl", "name_src", "content", "focus", "hidden"],
    ["role", "impl", "disabled", "expanded", "checked", "pressed", "selected"],
]


def interactions(strength: str) -> list[tuple[str, ...]]:
    """Factor combinations to cover. strength: '1', '2', 'mixed' (2 + GROUPS3 at 3), '3'."""
    if strength == "1":
        return [(f,) for f in FNAMES]
    pairs = list(itertools.combinations(FNAMES, 2))
    if strength == "2":
        return pairs
    if strength == "3":
        return list(itertools.combinations(FNAMES, 3))
    triples = set()
    for g in GROUPS3:
        for c in itertools.combinations(g, 3):
            triples.add(tuple(sorted(c, key=FIDX.get)))
    return pairs + sorted(triples)


def target_tuples(strength: str) -> dict[tuple, float]:
    out = {}
    for combo in interactions(strength):
        for vals in itertools.product(*(DOM[FIDX[f]] for f in combo)):
            tup = tuple(zip(combo, vals))
            if feasible(dict(tup)):
                out[tup] = tuple_w(tup)
    return out


def _new_tuples(assign: dict, f: str, v: str, by_factor: dict) -> list:
    """Uncovered-tuple keys completed by setting f=v given the other assigned factors."""
    res = []
    for combo in by_factor[f]:
        if all(g in assign or g == f for g in combo):
            res.append(tuple((g, v if g == f else assign[g]) for g in combo))
    return res


def aetg(strength: str, seed: int = 1, cands: int = 12, max_rows: int = 100000, log=False):
    """Greedy weighted AETG. Returns (rows, per-row weight gained, total weight)."""
    rng = random.Random(seed)
    unc = target_tuples(strength)
    total = sum(unc.values())
    by_factor = defaultdict(list)
    for combo in interactions(strength):
        for f in combo:
            by_factor[f].append(combo)
    rows, gains = [], []
    t0 = time.time()
    while unc and len(rows) < max_rows:
        # seed with the heaviest uncovered tuple (ties broken randomly)
        top = max(unc.values())
        seeds = [t for t, w in unc.items() if w >= top * 0.999]
        best, best_gain = None, -1.0
        for _ in range(cands):
            s = rng.choice(seeds)
            assign = dict(s)
            rest = [f for f in FNAMES if f not in assign]
            rng.shuffle(rest)
            rest.sort(key=lambda f: 0 if f in ("role", "impl") else 1)
            for f in rest:
                opts = []
                for v in DOM[FIDX[f]]:
                    assign[f] = v
                    if not feasible(assign):
                        continue
                    g = sum(unc.get(t, 0.0) for t in _new_tuples(assign, f, v, by_factor))
                    opts.append((g, rng.random(), v))
                assign.pop(f, None)
                if not opts:
                    break
                assign[f] = max(opts)[2]
            if len(assign) != K:
                continue
            gain = sum(unc.get(tuple((g, assign[g]) for g in combo), 0.0)
                       for combo in interactions_cache(strength))
            if gain > best_gain:
                best, best_gain = dict(assign), gain
        if best is None:
            raise RuntimeError("no feasible candidate row")
        for combo in interactions_cache(strength):
            unc.pop(tuple((g, best[g]) for g in combo), None)
        rows.append(best)
        gains.append(best_gain)
        if log and len(rows) % 25 == 0:
            print(f"  rows={len(rows)} uncovered={len(unc)} t={time.time()-t0:.1f}s", file=sys.stderr)
    return rows, gains, total


_icache: dict[str, list] = {}


def interactions_cache(strength):
    if strength not in _icache:
        _icache[strength] = interactions(strength)
    return _icache[strength]


def random_valid_row(rng: random.Random) -> dict:
    """Uniform over (role, impl) pairs, then uniform per factor (not uniform over the space)."""
    while True:
        role, impl = rng.choice(list(ALLOWED))
        r = _complete({"role": role, "impl": impl}, rng)
        if r is not None:
            return r


def slj_bound(strength_t: int, k: int, v: int) -> float:
    """Stein-Lovasz-Johnson upper bound on CAN(t, k, v): (ln C(k,t) + t ln v) / ln(v^t/(v^t-1))."""
    vt = v ** strength_t
    return (math.log(math.comb(k, strength_t)) + strength_t * math.log(v)) / math.log(vt / (vt - 1))


# --------------------------------------------------------------------------------------------
# 4. Expected-property oracle (abstract tokens, no golden text) and page rendering
# --------------------------------------------------------------------------------------------

def expected(row: dict) -> dict:
    """Abstract expectation for a case: accessible name, role token, state tokens, reachability.

    Name per accname 1.2 for the constructs this generator emits. Tokens are catalog-level ids
    (role:<x>, state:<x>), never words, so the same oracle serves en and ru."""
    role, ns, content = row["role"], row["name_src"], row["content"]
    visible_text = {"text": "Alpha", "icon": "", "text+icon": "Alpha", "split-text": "Alpha beta",
                    "empty": ""}[content]
    name = {"aria-label": "Label one", "labelledby": "Ref label", "title": "Tip text",
            "label-for": "For label", "alt": "Alt text"}.get(ns)
    if ns == "contents":
        name = visible_text if ROLES[role]["nfc"] else None
    if ns == "none":
        name = None
    if role in ("none", "generic"):
        name = None
    if ns == "title" and ROLES[role]["nfc"] and visible_text:
        name = visible_text  # contents win over title for name-from-content roles
    hidden = row["hidden"] in ("aria-hidden", "ancestor-hidden")
    role_tok = None if role in ("none", "generic") else f"role:{role}"
    if role == "button" and row["pressed"] != A:
        role_tok = "role:togglebutton"
    states = []
    for f in ("expanded", "selected", "checked", "pressed"):
        v = row[f]
        if v != A:
            states.append(f"state:{f}={v}")
    for f in ("required", "readonly", "busy"):
        if row[f] == "true":
            states.append(f"state:{f}")
    if row["invalid"] != A:
        states.append(f"state:invalid={row['invalid']}")
    if row["disabled"] != A:
        states.append("state:unavailable")
    if row["current"] != A:
        states.append(f"state:current={row['current']}")
    if row["haspopup"] not in (A, "false"):
        states.append(f"state:haspopup={row['haspopup']}")
    focusable = (row["focus"] == "tabindex0" or
                 (row["focus"] == "default" and row["impl"] == "native" and role in
                  {"button", "link", "checkbox", "radio", "textbox", "searchbox", "combobox",
                   "spinbutton", "slider"}))
    if row["disabled"] == "native":
        focusable = False
    perceivable = bool(name) or bool(visible_text) or role in ("img", "progressbar", "slider",
                                                               "textbox", "searchbox", "combobox",
                                                               "spinbutton", "checkbox", "radio")
    return dict(name=name, role=role_tok, states=sorted(states), hidden=hidden,
                tab_reachable=focusable and not hidden, browse_reachable=perceivable and not hidden,
                quicknav=QUICKNAV.get(role) if not hidden else None)


QUICKNAV = {"button": "b", "link": "k", "checkbox": "x", "radio": "r", "textbox": "e",
            "searchbox": "e", "combobox": "c", "heading": "h", "img": "g", "listitem": "i",
            "navigation": "d", "region": "d", "spinbutton": "f", "slider": "f", "switch": "f"}

PARENT_HTML = {"radiogroup": ('<div role="radiogroup" aria-label="Group">', "</div>"),
               "list": ("<ul>", "</ul>"), "tablist": ('<div role="tablist">', "</div>"),
               "listbox": ('<div role="listbox" aria-label="Box">', "</div>"),
               "menu": ('<div role="menu" aria-label="Menu">', "</div>"),
               "tree": ('<div role="tree" aria-label="Tree">', "</div>"),
               "row": ('<table><tr>', "</tr></table>"), "table": ("<table>", "</table>")}
CONTAINER_HTML = {"body": ("", ""), "list": ("<ul><li>", "</li></ul>"),
                  "table": ("<table><tr><td>", "</td></tr></table>"), "form": ("<form>", "</form>"),
                  "landmark": ('<nav aria-label="Area">', "</nav>"), "link": ('<a href="#x">', "</a>")}
ICON = '<svg aria-hidden="true" width="8" height="8"><rect width="8" height="8"/></svg>'


def render_case(cid: str, row: dict) -> str:
    role, impl = row["role"], row["impl"]
    native = ROLES[role]["native"]
    attrs: dict[str, str] = {"id": cid, "data-case": cid}
    content_html = {"text": "Alpha", "icon": ICON, "text+icon": "Alpha " + ICON,
                    "split-text": "<span>Alpha</span> <b>beta</b>", "empty": ""}[row["content"]]
    pre = ""
    ns = row["name_src"]
    if ns == "aria-label":
        attrs["aria-label"] = "Label one"
    elif ns == "labelledby":
        attrs["aria-labelledby"] = cid + "-lbl"
        pre += f'<span id="{cid}-lbl" hidden>Ref label</span>'
    elif ns == "title":
        attrs["title"] = "Tip text"
    elif ns == "label-for":
        pre += f'<label for="{cid}">For label</label>'
    elif ns == "alt":
        attrs["alt"] = "Alt text"
    if row["desc"] == "describedby":
        attrs["aria-describedby"] = cid + "-desc"
        pre += f'<span id="{cid}-desc" hidden>Extra info</span>'
    if row["focus"] == "tabindex0":
        attrs["tabindex"] = "0"
    elif row["focus"] == "tabindex-1":
        attrs["tabindex"] = "-1"
    for f in ("expanded", "selected", "busy", "required", "readonly", "invalid", "current",
              "haspopup", "live"):
        v = row[f]
        if v != A:
            attrs[f"aria-{f}"] = v
    if row["checked"] != A:
        if impl == "native" and native in ("input:checkbox", "input:radio"):
            if row["checked"] == "true":
                attrs["checked"] = ""
            elif row["checked"] == "mixed":
                attrs["data-indeterminate"] = ""
        else:
            attrs["aria-checked"] = row["checked"]
    if row["pressed"] != A:
        attrs["aria-pressed"] = row["pressed"]
    if row["level"] != A and not (impl == "native" and role == "heading"):
        attrs["aria-level"] = row["level"]
    if row["posinset"] != A:
        attrs["aria-posinset"] = {"first": "1", "mid": "3", "last": "5"}[row["posinset"]]
        attrs["aria-setsize"] = "5"
    if row["disabled"] == "aria":
        attrs["aria-disabled"] = "true"
    elif row["disabled"] == "native":
        attrs["disabled"] = ""
    if row["hidden"] == "aria-hidden":
        attrs["aria-hidden"] = "true"
    elif row["hidden"] == "sr-only":
        attrs["class"] = "sr-only"
    val = row["value"]
    if val != A:
        num = {"min": "0", "mid": "50", "max": "100", "valuetext": "50"}[val]
        if impl == "native" and role in ("slider", "spinbutton", "progressbar"):
            attrs["value"] = num
            if role != "progressbar":
                attrs["min"], attrs["max"] = "0", "100"
            else:
                attrs["max"] = "100"
        elif role in ("slider", "spinbutton", "progressbar"):
            attrs.update({"aria-valuenow": num, "aria-valuemin": "0", "aria-valuemax": "100"})
        elif impl == "native":
            attrs["value"] = "Typed"
        if val == "valuetext":
            attrs["aria-valuetext"] = "Half"
    # element
    if impl == "native":
        if native.startswith("input:"):
            attrs["type"] = native.split(":")[1]
            tag, void = "input", True
        elif native == "h":
            tag, void = f"h{row['level']}", False
        elif native in ("img", "progress"):
            tag, void = native, native == "img"
            if native == "img":
                attrs["src"] = "data:image/gif;base64,R0lGODlhAQABAAAAACw="
        elif native == "a":
            tag, void = "a", False
            attrs["href"] = f"#{cid}-t"
        elif native == "dialog":
            tag, void = "dialog", False
            attrs["open"] = ""
        else:
            tag, void = native, False
    else:
        tag, void = ("span" if row["content"] == "split-text" else "div"), False
        attrs["role"] = role
    a = "".join(f' {k}="{html.escape(v, quote=True)}"' if v != "" else f" {k}"
                for k, v in attrs.items())
    if tag == "select":
        el = f"<select{a}><option>One</option><option>Two</option></select>"
    elif void:
        el = f"<{tag}{a}>"
    else:
        el = f"<{tag}{a}>{content_html}</{tag}>"
    parent = ROLES[role].get("parent")
    if parent and not (impl == "native" and parent in ("row", "table", "list")):
        po, pc = PARENT_HTML[parent]
        el = po + el + pc
    elif impl == "native" and role in ("row",):
        el = "<table>" + el + "</table>"
    elif impl == "native" and role == "columnheader":
        el = "<table><tr>" + el + "</tr></table>"
    elif impl == "native" and role == "listitem":
        el = "<ul>" + el + "</ul>"
    co, cc = CONTAINER_HTML[row["container"]]
    el = co + pre + el + cc
    if row["hidden"] == "ancestor-hidden":
        el = f'<div aria-hidden="true">{el}</div>'
    return f'<p class="m">Case {cid}.</p>\n<section class="case">{el}</section>\n'


PAGE_HEAD = """<!doctype html><html lang="en"><head><meta charset="utf-8"><title>{title}</title>
<style>.sr-only{{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0)}}</style>
</head><body><main>
"""
PAGE_TAIL = """</main><script>
for (const e of document.querySelectorAll('[data-indeterminate]')) e.indeterminate = true;
// State-flip hook for P6: gorcaFlip('c12', 'aria-expanded', 'true')
window.gorcaFlip = (id, attr, v) => {{ const e = document.getElementById(id);
  if (attr === 'checked') e.checked = v === 'true'; else e.setAttribute(attr, v); }};
</script></body></html>
"""


def flips(row: dict) -> list[tuple[str, str, str]]:
    out = []
    for f in ("expanded", "selected", "checked", "pressed"):
        v = row[f]
        if v in ("true", "false"):
            out.append((f, v, "false" if v == "true" else "true"))
    if row["disabled"] == "aria":
        out.append(("disabled", "aria", A))
    return out


def gen_pages(rows: list[dict], out: str, per_page: int = 30):
    import os
    os.makedirs(out, exist_ok=True)
    manifest = {"version": 1, "factors": FNAMES, "cases": []}
    for p in range(0, len(rows), per_page):
        chunk = rows[p:p + per_page]
        name = f"aria-cover-{p // per_page:03d}.html"
        body = []
        for i, row in enumerate(chunk):
            cid = f"c{p + i}"
            body.append(render_case(cid, row))
            twin = None
            if row["hidden"] == A:
                twin = {"metamorphic": "aria-hidden", "expect": "absent-in-all-contexts"}
            manifest["cases"].append({"id": cid, "page": name, "row": row,
                                      "expected": expected(row), "flips": flips(row),
                                      "twin": twin})
        with open(os.path.join(out, name), "w") as fh:
            fh.write(PAGE_HEAD.format(title=name) + "".join(body) + PAGE_TAIL.replace("{{", "{").replace("}}", "}"))
    with open(os.path.join(out, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=1)
    return manifest


# --------------------------------------------------------------------------------------------
# 5. Toy presenter + properties + mutants (estimate of oracle power)
# --------------------------------------------------------------------------------------------

CONTEXTS = ["focus", "browse", "quicknav", "review"]
ENGINES = ["firefox", "chromium"]
LANGS = ["en", "ru"]
VERBOSITY = ["low", "normal", "high"]
CATALOG_RU_MISSING: set[str] = set()


def present(row, ctx, engine="firefox", lang="en", verb="normal", mut=None):
    """Toy reference presenter -> list of (kind, token) or None if the node is not reachable.

    `mut` names an injected bug (see MUTANTS)."""
    e = expected(row)
    m = mut or ""
    reach = {"focus": e["tab_reachable"] or row["focus"] == "tabindex-1",
             "browse": e["browse_reachable"], "review": e["browse_reachable"],
             "quicknav": e["quicknav"] is not None and e["browse_reachable"]}[ctx]
    if m == "M2" and ctx == "browse" and row["content"] == "icon" and row["name_src"] in ("aria-label", "labelledby"):
        reach = False
    if m == "M6" and ctx == "quicknav" and row["hidden"] == "ancestor-hidden" and QUICKNAV.get(row["role"]):
        reach = True
    if m == "M14" and ctx in ("browse", "review") and row["hidden"] == "sr-only":
        reach = False
    if not reach:
        return None
    segs = []
    name = e["name"]
    if m == "M12" and ctx == "quicknav" and row["name_src"] == "aria-label" and row["content"] == "text":
        name = "Alpha"
    if name and not (m == "M10" and verb == "low" and row["desc"] == "describedby"):
        segs.append(("name", name))
    if e["role"]:
        rt = e["role"]
        if m == "M9" and engine == "chromium" and row["role"] == "switch":
            rt = "role:togglebutton"
        segs.append(("role", rt))
        if m == "M1" and ctx == "focus" and row["role"] == "button" and row["impl"] == "aria" \
                and row["name_src"] == "aria-label" and row["content"] in ("icon", "text+icon"):
            segs.append(("role", rt))
    for s in e["states"]:
        if m == "M3" and ctx == "quicknav" and s.startswith("state:pressed"):
            continue
        if m == "M8" and ctx == "browse" and s == "state:unavailable" and row["impl"] == "aria":
            continue
        if m == "M13" and s == "state:readonly":
            continue
        if m == "M15" and ctx == "browse" and s.startswith("state:expanded") and row["container"] == "table":
            s = "state:expanded=" + ("false" if s.endswith("true") else "true")
        if verb == "low" and s.startswith(("state:haspopup", "state:current", "state:busy")):
            continue
        segs.append(("state", s))
        if m == "M11" and ctx == "focus" and s.startswith("state:current") and row["container"] == "landmark":
            segs.append(("state", s))
    if row["level"] != A and not (m == "M7" and ctx == "browse" and row["role"] == "treeitem"):
        segs.append(("level", row["level"]))
    if row["desc"] == "describedby" and verb != "low":
        segs.append(("desc", "Extra info"))
    # render to language: catalog tokens get a language tag; missing ru entries fall back to en
    out = []
    for kind, tok in segs:
        if kind in ("role", "state"):
            missing = lang == "ru" and (tok in CATALOG_RU_MISSING or
                                        (m == "M4" and tok == "state:checked=mixed"))
            out.append((kind, tok, "en" if missing else lang))
        else:
            out.append((kind, tok, None))
    return out


def announce_flip(row, f, old, new, lang="en", mut=None):
    """Event context: announcements caused by one state change."""
    m = mut or ""
    if f == "disabled":
        n = [("state", "state:available")]
    else:
        n = [("state", f"state:{f}={new}")]
    if m == "M5" and f == "checked" and row["impl"] == "aria":
        n = n * 2
    if m == "M16" and f == "expanded" and row["role"] == "treeitem":
        n = []
    return n


def props(row, mut=None, skip_spec=False) -> list[str]:
    """Return the names of violated properties for one case (empty = pass)."""
    bad = []
    e = expected(row)
    outs = {}
    for ctx in CONTEXTS:
        for eng in ENGINES:
            for lang in LANGS:
                for verb in VERBOSITY:
                    outs[(ctx, eng, lang, verb)] = present(row, ctx, eng, lang, verb, mut)

    def proj(o, kinds=("name", "role", "state", "level")):
        return Counter((k, t) for k, t, _ in o if k in kinds) if o is not None else None

    # P0 spec: focus/browse output carries exactly the model-derived tokens (abstract oracle)
    if not skip_spec:
        o = outs[("browse", "firefox", "en", "high")]
        if e["browse_reachable"]:
            want = Counter(("state", s) for s in e["states"])
            if o is None or Counter((k, t) for k, t, _ in o if k == "state") != want:
                bad.append("P0-spec-states")
        if e["browse_reachable"] != (o is not None):
            bad.append("P0-spec-reach")
    for key, o in outs.items():
        if o is None:
            continue
        c = proj(o)
        # P1 name once, P2 role once
        if e["name"] and c[("name", e["name"])] != 1:
            bad.append("P1-name-once")
        if sum(v for (k, _), v in c.items() if k == "role") > 1:
            bad.append("P2-role-once")
        if sum(v for (k, _), v in c.items() if k == "state") != len(set(t for k, t, _ in o if k == "state")):
            bad.append("P2b-state-once")
    for eng in ENGINES:
        for lang in LANGS:
            for verb in VERBOSITY:
                ps = {ctx: proj(outs[(ctx, eng, lang, verb)]) for ctx in CONTEXTS}
                live = [p for p in ps.values() if p is not None]
                # P3 cross-context consistency (here: no declared differences)
                if any(p != live[0] for p in live):
                    bad.append("P3-context-consistency")
                # P4 reachability: quicknav-visible implies browse-visible, focus implies browse
                if ps["quicknav"] is not None and ps["browse"] is None:
                    bad.append("P4-reach-quicknav-subset")
                if ps["focus"] is not None and ps["browse"] is None and e["browse_reachable"]:
                    bad.append("P4-reach-focus-subset")
                if e["quicknav"] and not e["hidden"] and ps["browse"] is not None and ps["quicknav"] is None:
                    bad.append("P4-reach-quicknav-missing")
    # P5 hidden: hidden nodes are absent everywhere
    # Declared exception: a focus event on an (erroneously) focusable hidden node is still spoken.
    if e["hidden"] and any(o is not None for k, o in outs.items() if k[0] != "focus"):
        bad.append("P5-hidden")
    # P7 language: same token structure; every catalog token of ru output is ru
    for ctx in CONTEXTS:
        for eng in ENGINES:
            for verb in VERBOSITY:
                en, ru = outs[(ctx, eng, "en", verb)], outs[(ctx, eng, "ru", verb)]
                if (en is None) != (ru is None) or (en and [(k, t) for k, t, _ in en] != [(k, t) for k, t, _ in ru]):
                    bad.append("P7-lang-structure")
                if ru and any(l not in (None, "ru") for _, _, l in ru):
                    bad.append("P7b-lang-script")
    # P8 verbosity monotone; name never dropped
    for ctx in CONTEXTS:
        for eng in ENGINES:
            seq = [proj(outs[(ctx, eng, "en", v)], ("name", "role", "state", "level", "desc")) for v in VERBOSITY]
            if seq[0] is None:
                continue
            if not (seq[0] <= seq[1] <= seq[2]):
                bad.append("P8-verbosity-monotone")
            if e["name"] and seq[0][("name", e["name"])] != 1:
                bad.append("P8-name-kept")
    # P9 engine invariance
    for ctx in CONTEXTS:
        if proj(outs[(ctx, "firefox", "en", "normal")]) != proj(outs[(ctx, "chromium", "en", "normal")]):
            bad.append("P9-engine")
    # P6 one announcement per flip, carrying the new state
    for f, old, new in flips(row):
        n = announce_flip(row, f, old, new, mut=mut)
        if len(n) != 1:
            bad.append("P6-flip-once")
    return sorted(set(bad))


MUTANTS = {
    "M1": "focus: role twice for ARIA button with aria-label and icon content (observed bug)",
    "M2": "browse arrows skip icon-only node named by aria-label/labelledby (observed bug)",
    "M3": "quick-nav drops the pressed state",
    "M4": "ru catalog lacks 'half checked'; English fallback",
    "M5": "checked flip on ARIA widget announced twice",
    "M6": "quick-nav ignores aria-hidden on an ancestor",
    "M7": "browse drops level for treeitem",
    "M8": "browse drops 'unavailable' for aria-disabled on ARIA widgets",
    "M9": "Chromium: switch spoken as toggle button (xml-roles ignored)",
    "M10": "low verbosity drops the name when a description exists",
    "M11": "focus: current spoken twice inside a landmark",
    "M12": "quick-nav uses content text instead of aria-label",
    "M13": "aria-readonly never spoken anywhere (uniform omission)",
    "M14": "sr-only (visually hidden) node missing from browse",
    "M15": "browse inverts expanded inside a table",
    "M16": "treeitem expand/collapse not announced",
}


def kill_matrix(rows, skip_spec=False):
    killed = {}
    for m in MUTANTS:
        killed[m] = None
        for i, r in enumerate(rows):
            if props(r, m, skip_spec):
                killed[m] = i
                break
    return killed


# --------------------------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------------------------

def coverage_curve(rows, strength):
    tt = target_tuples(strength)
    total = sum(tt.values())
    covered, cw, curve = set(), 0.0, []
    combos = interactions_cache(strength)
    for r in rows:
        for c in combos:
            t = tuple((g, r[g]) for g in c)
            if t in tt and t not in covered:
                covered.add(t)
                cw += tt[t]
        curve.append((len(covered) / len(tt), cw / total))
    return curve


def cmd_stats(a):
    print(f"factors k={K}; domain sizes: " + ", ".join(f"{f}={len(v)}" for f, v in FACTORS))
    raw = math.prod(len(v) for v in DOM)
    print(f"raw cartesian page space: {raw:.3e}")
    print(f"valid (role,impl) pairs: {len(ALLOWED)}")
    t0 = time.time()
    print(f"valid page configurations (exact): {count_valid():.3e}  ({time.time()-t0:.1f}s)")
    run = len(CONTEXTS) * len(ENGINES) * len(VERBOSITY) * len(LANGS)
    print(f"run factors crossed per case: {len(CONTEXTS)} ctx x {len(ENGINES)} eng x {len(VERBOSITY)} verb x {len(LANGS)} lang = {run}")
    for t, v in ((2, 31), (3, 31), (2, 7), (3, 7)):
        print(f"SLJ bound CAN({t},{K},{v}) <= {slj_bound(t, K, v):.0f}")
    for s in a.strengths.split(","):
        t0 = time.time()
        tt = target_tuples(s)
        rows, gains, total = aetg(s, seed=a.seed, cands=a.cands)
        curve = coverage_curve(rows, s)
        def first(x, idx):
            return next((i + 1 for i, c in enumerate(curve) if c[idx] >= x), None)
        print(f"strength={s}: valid tuples={len(tt)} rows={len(rows)} "
              f"(lower bound max-pair {lower_bound(s)}) time={time.time()-t0:.1f}s; "
              f"rows to 50/90/99% weighted: {first(.5,1)}/{first(.9,1)}/{first(.99,1)}; "
              f"to 90% unweighted: {first(.9,0)}")


def lower_bound(strength):
    """Trivial lower bound: the largest number of valid tuples over a single interaction."""
    tt = target_tuples(strength)
    per = Counter(tuple(f for f, _ in t) for t in tt)
    return max(per.values())


def cmd_gen(a):
    rows, _, _ = aetg(a.strength, seed=a.seed, cands=a.cands)
    m = gen_pages(rows, a.out, a.per_page)
    print(f"{len(rows)} cases -> {a.out} ({len(set(c['page'] for c in m['cases']))} pages + manifest.json)")


def cmd_faults(a):
    rng = random.Random(a.seed)
    suites = {}
    for s in ("2", "mixed"):
        rows, _, _ = aetg(s, seed=a.seed, cands=a.cands)
        suites[f"CA-{s}"] = rows
        suites[f"random-{s}(N={len(rows)})"] = [random_valid_row(rng) for _ in rows]
    base = []
    for (role, impl) in ALLOWED:
        base.append(_complete({"role": role, "impl": impl}, None))
    suites[f"one-per-role-impl(N={len(base)})"] = base
    # faults: valid t-tuples sampled (u) uniformly and (w) by real-world weight
    for t in (1, 2, 3):
        pool = list(target_tuples(str(t) if t < 3 else "3core"))
        weights = [tuple_w(x) for x in pool]
        for mode in ("u", "w"):
            fs = rng.choices(pool, weights=None if mode == "u" else weights, k=a.nfaults)
            line = []
            for name, rows in suites.items():
                rs = [set(r.items()) for r in rows]
                det = sum(1 for f in fs if any(set(f) <= r for r in rs))
                line.append(f"{name}={det / len(fs):.3f}")
            print(f"t={t} faults({mode}) detected: " + "  ".join(line))
    # 3-way faults over ANY factor triple (outside the 3-way groups too), by rejection sampling
    fs = []
    while len(fs) < a.nfaults:
        combo = rng.sample(FNAMES, 3)
        tup = tuple((f, rng.choice(DOM[FIDX[f]])) for f in combo)
        if feasible(dict(tup)) and any(v != A for _, v in tup):
            fs.append(tup)
    line = []
    for name, rows in suites.items():
        rs = [set(r.items()) for r in rows]
        line.append(f"{name}={sum(1 for f in fs if any(set(f) <= r for r in rs)) / len(fs):.3f}")
    print("t=3 faults(any triple, uniform, >=1 non-absent) detected: " + "  ".join(line))


_orig_interactions = interactions


def interactions(strength):  # noqa: F811  (adds the '3core' fault pool)
    if strength == "3core":
        return [tuple(sorted(c, key=FIDX.get)) for c in itertools.combinations(GROUPS3[0], 3)]
    return _orig_interactions(strength)


def cmd_mutants(a):
    rng = random.Random(a.seed)
    rows2, _, _ = aetg("2", seed=a.seed, cands=a.cands)
    rowsm, _, _ = aetg("mixed", seed=a.seed, cands=a.cands)
    suites = {f"CA-2(N={len(rows2)})": rows2, f"CA-mixed(N={len(rowsm)})": rowsm}
    for n in (len(rows2), len(rowsm)):
        kills = Counter()
        reps = 20
        for rep in range(reps):
            rr_rng = random.Random(a.seed * 1000 + rep * 7 + n)
            rr = [random_valid_row(rr_rng) for _ in range(n)]
            for m, k in kill_matrix(rr).items():
                kills[m] += k is not None
        print(f"random N={n} (mean over {reps} suites): mutation score "
              f"{sum(kills.values()) / (reps * len(MUTANTS)):.3f}; per mutant "
              + " ".join(f"{m}:{kills[m] / reps:.2f}" for m in MUTANTS))
    base = [_complete({"role": r, "impl": i}, None) for (r, i) in ALLOWED]
    suites[f"one-per-role-impl(N={len(base)})"] = base
    for name, rows in suites.items():
        for skip in (False, True):
            km = kill_matrix(rows, skip_spec=skip)
            score = sum(v is not None for v in km.values()) / len(km)
            print(f"{name} {'metamorphic-only' if skip else 'with-spec-oracle'}: mutation score {score:.3f}; "
                  "first killing row: " + " ".join(f"{m}:{v}" for m, v in km.items()))


def cmd_selftest(a):
    # constraint engine
    assert feasible({"role": "button", "impl": "aria", "name_src": "aria-label", "content": "icon"})
    assert not feasible({"role": "switch", "impl": "native"})
    assert not feasible({"name_src": "alt", "impl": "aria"})
    assert not feasible({"role": "textbox", "impl": "native", "content": "text"})
    assert feasible({"role": "textbox", "content": "text"})  # contenteditable ARIA textbox
    assert not feasible({"role": "link", "pressed": "true"})
    assert not feasible({"role": "radio", "checked": "mixed"})
    assert feasible({"role": "checkbox", "checked": "mixed", "disabled": "native"})
    assert not feasible({"name_src": "contents", "content": "empty"})
    assert not feasible({"role": "heading", "impl": "native", "level": A})
    # expected-name vectors (accname 1.2)
    base = _complete({"role": "button", "impl": "aria", "name_src": "aria-label", "content": "icon",
                      "focus": "tabindex0"}, None)
    assert expected(base)["name"] == "Label one" and expected(base)["role"] == "role:button"
    b2 = dict(base, name_src="contents", content="split-text")
    assert expected(b2)["name"] == "Alpha beta"
    b3 = dict(base, name_src="title", content="text")
    assert expected(b3)["name"] == "Alpha"
    b4 = dict(base, pressed="mixed")
    assert expected(b4)["role"] == "role:togglebutton" and "state:pressed=mixed" in expected(b4)["states"]
    # properties: correct presenter passes, observed bugs are caught on their trigger row
    assert props(base) == [], props(base)
    assert "P2-role-once" in props(base, "M1")
    assert "P4-reach-quicknav-subset" in props(base, "M2") or "P3-context-consistency" in props(base, "M2")
    # covering array: every valid pair is covered
    rows, _, _ = aetg("2", seed=3, cands=4)
    tt = target_tuples("2")
    cov = {tuple((g, r[g]) for g in c) for r in rows for c in interactions("2")}
    assert set(tt) <= cov, "pairwise coverage incomplete"
    assert all(check(r) and len(r) == K for r in rows)
    print(f"selftest ok: pairwise rows={len(rows)}, valid pairs={len(tt)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("stats", "gen", "faults", "mutants", "selftest"):
        p = sub.add_parser(name)
        p.add_argument("--seed", type=int, default=1)
        p.add_argument("--cands", type=int, default=12)
        if name == "stats":
            p.add_argument("--strengths", default="1,2,mixed")
        if name == "gen":
            p.add_argument("--strength", default="mixed")
            p.add_argument("--out", required=True)
            p.add_argument("--per-page", type=int, default=30)
        if name == "faults":
            p.add_argument("--nfaults", type=int, default=2000)
    a = ap.parse_args()
    {"stats": cmd_stats, "gen": cmd_gen, "faults": cmd_faults, "mutants": cmd_mutants,
     "selftest": cmd_selftest}[a.cmd](a)


if __name__ == "__main__":
    main()
