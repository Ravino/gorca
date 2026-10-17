#!/usr/bin/env python3
"""Reference model for browse-mode selection mapping (docs/reports/selection-mapping-math.md).

Pure Python, no dependencies. Models:

* a web accessibility tree with hypertext containers, text leaves (Firefox/Chromium text runs as
  gorca's mirror holds them), non-hypertext objects (images) and empty controls;
* gorca's flat projection P (lead '\n' for BREAK_BEFORE, lead ' ' for CELL_SEP, U+FFFC for an
  empty marker-role node, leaf text), see crates/gorca-core/src/doc_actor/translate.rs
  `assign_breaks`, `marker_role` and crates/gorca-doc/src/web/doc.rs `own_chars`;
* the atom space: every DOM-selectable unit (a leaf scalar, an object) in document order;
* engine resolvers for AT-SPI Text.SetSelection offsets:
    - Gecko (Firefox 153 ESR): HyperTextAccessibleBase::ToTextLeafPoint, an end offset that
      names a hypertext child descends to that child's end; offsets are UTF-16 units;
    - Blink (Chromium 154): exclusive ends, offsets are Unicode scalars (code points);
    - Blink Document.SetTextSelections: one range with endpoints in any two containers;
* a faithful port of selmirror.rs `plan` + source.rs `gecko_range`/`select`;
* the proposed lifted minimal-leg planner;
* the mode-B copy function;
* property tests and statistics on random documents.

Run: python tools/research/selection_mapping.py            (tests + stats)
     python tools/research/selection_mapping.py --vectors  (print the test vectors)
"""

from __future__ import annotations

import random
import sys
from dataclasses import dataclass, field

FFFC = "￼"

# ----------------------------------------------------------------------------- tree

HYPER = {"doc", "block", "inline", "link", "list", "item", "table", "row", "cell", "entry",
         "password", "button", "heading"}
BLOCK = {"doc", "block", "list", "item", "table", "row", "heading"}
MARKER = {"image", "entry", "password", "button"}  # marker_role subset used here
FOCUSABLE = {"link", "entry", "password", "button"}


@dataclass(eq=False)
class N:
    kind: str
    text: str = ""          # leaf only
    name: str = ""          # image alt / button name
    children: list = field(default_factory=list)
    parent: "N | None" = None
    nid: int = 0
    # filled by Doc
    lead: str = ""
    body: str = ""
    a0: int = 0             # first atom boundary
    a1: int = 0             # last atom boundary

    def __repr__(self):
        return f"{self.kind}#{self.nid}"


def L(t):
    return N("leaf", text=t)


def mk(kind, *kids, name=""):
    n = N(kind, name=name)
    for k in kids:
        k.parent = n
        n.children.append(k)
    return n


def u16(s):
    return sum(2 if ord(c) > 0xFFFF else 1 for c in s)


class Doc:
    """Flat projection, atoms and hypertext of one tree."""

    def __init__(self, root: N):
        self.root = root
        self.nodes: list[N] = []
        self._pre(root)
        for i, n in enumerate(self.nodes):
            n.nid = i
        self._flags(root)
        self.flat: list[tuple[str, str, N, int]] = []   # (char, class, node, index)
        self.atoms: list[tuple[N, int]] = []             # (node, scalar index or 0)
        self.atom_flat: list[int] = []                   # flat offset of each atom
        self.flat_start: dict[N, int] = {}
        self._project(root)

    def _pre(self, n):
        self.nodes.append(n)
        for c in n.children:
            self._pre(c)

    def _flags(self, n):
        # assign_breaks (FT-4): '\n' before a block and before whatever follows one; ' ' before a
        # cell that follows another node in its row.
        prev_block = False
        seen = False
        for c in n.children:
            block = c.kind in BLOCK
            if block or prev_block:
                c.lead = "\n"
            elif seen and c.kind == "cell":
                c.lead = " "
            else:
                c.lead = ""
            prev_block = block
            seen = True
            self._flags(c)
        if n.kind == "leaf":
            n.body = n.text
        elif n.kind in MARKER and not n.children:
            n.body = FFFC
        else:
            n.body = ""

    def _project(self, n):
        self.flat_start[n] = len(self.flat)
        if n.lead:
            self.flat.append((n.lead, "NL" if n.lead == "\n" else "CS", n, -1))
        n.a0 = len(self.atoms)
        if n.kind == "leaf":
            for i, ch in enumerate(n.text):
                self.atom_flat.append(len(self.flat))
                self.atoms.append((n, i))
                self.flat.append((ch, "T", n, i))
        elif n.body == FFFC and n.kind == "image":
            self.atom_flat.append(len(self.flat))
            self.atoms.append((n, 0))
            self.flat.append((FFFC, "O", n, 0))
        elif n.body == FFFC:
            # an empty control: a marker with no DOM text, so not an atom (class O0)
            self.flat.append((FFFC, "O0", n, 0))
        for c in n.children:
            self._project(c)
        n.a1 = len(self.atoms)

    # --- helpers
    def text(self):
        return "".join(c for c, *_ in self.flat)

    def is_leaf(self, n):
        return n.kind == "leaf"

    def hlen_child(self, c, units):
        if c.kind == "leaf":
            return u16(c.text) if units == "u16" else len(c.text)
        return 1

    def hoffsets(self, a, units):
        out, o = [], 0
        for c in a.children:
            out.append(o)
            o += self.hlen_child(c, units)
        return out, o

    def pos(self, a, child, units):
        offs, _ = self.hoffsets(a, units)
        return offs[a.children.index(child)]

    def hlen(self, a, units):
        return self.hoffsets(a, units)[1]

    def anc(self, n):
        out = []
        while n is not None:
            out.append(n)
            n = n.parent
        return out

    def flat_range_atoms(self, lo, hi):
        """Requested atoms of the flat range [lo, hi): a contiguous atom interval."""
        idx = [k for k, f in enumerate(self.atom_flat) if lo <= f < hi]
        if not idx:
            return None
        return idx[0], idx[-1] + 1

    def hull(self, p, q):
        """P of an atom interval: the smallest flat range holding its atoms."""
        return self.atom_flat[p], self.atom_flat[q - 1] + 1


# ----------------------------------------------------------------------------- engines

def leaf_rel_to_scalar(leaf, rel, units):
    if units == "cp":
        return min(rel, len(leaf.text))
    acc = 0
    for i, ch in enumerate(leaf.text):
        if acc >= rel:
            return i
        acc += 2 if ord(ch) > 0xFFFF else 1
    return len(leaf.text)


def gecko_point(d: Doc, a: N, off: int, end: bool, top: bool = True):
    """ToTextLeafPoint (refs/firefox/accessible/basetypes/HyperTextAccessibleBase.cpp:317) as
    an atom boundary. `top` applies the ATK-level refusal measured in ctrl-c-passthrough.md: an
    end offset == length of a container whose last child is embedded is invalid."""
    if not a.children:
        return a.a0
    offs, total = d.hoffsets(a, "u16")
    if off > total or off < 0:
        return None
    if off == total:
        i = len(a.children) - 1
        if top and a.children[i].kind != "leaf":
            return None
    else:
        i = max(k for k, o in enumerate(offs) if o <= off and off < o + d.hlen_child(a.children[k], "u16"))
    c = a.children[i]
    rel = off - offs[i]
    if c.kind in HYPER:
        e = end or rel == 1
        return gecko_point(d, c, d.hlen(c, "u16") if e else 0, e, top=False)
    if c.kind == "leaf":
        return c.a0 + leaf_rel_to_scalar(c, rel, "u16")
    return c.a0 + rel  # image: 0 before, 1 after


def gecko_range(d, a, s, e):
    p = gecko_point(d, a, s, False)
    q = gecko_point(d, a, e, True)
    if p is None or q is None:
        return None
    return (p, max(p, q))


def blink_point(d: Doc, a: N, off: int, units="cp"):
    """Blink: hypertext offset -> boundary before the child at `off` (exclusive end)."""
    if not a.children:
        return a.a0
    offs, total = d.hoffsets(a, units)
    if off < 0 or off > total:
        return None
    if off == total:
        return a.a1
    i = max(k for k, o in enumerate(offs) if o <= off)
    c = a.children[i]
    rel = off - offs[i]
    if c.kind == "leaf":
        return c.a0 + leaf_rel_to_scalar(c, rel, units)
    return c.a0 if rel == 0 else c.a1


# ----------------------------------------------------------------------------- copy text

def mode_b(d: Doc, lo: int, hi: int) -> str:
    """Mode-B copy of the flat range [lo, hi) (spec section 3 of the report)."""
    out: list[str] = []
    pend = ""
    for ch, cls, n, _ in d.flat[lo:hi]:
        if cls == "NL":
            pend = "\n"
            continue
        if cls == "CS":
            if pend != "\n":
                pend = "\t"
            continue
        if cls == "O0":
            piece = n.name if n.kind == "button" else ""
            if not piece:
                continue
            if out and pend:
                out.append(pend)
            pend = ""
            out.append(piece)
            continue
        if cls == "T":
            if any(a.kind == "password" for a in d.anc(n)):
                continue
            piece = ch
        else:  # O marker
            piece = n.name if n.kind in ("image", "button") else ""
            if not piece:
                continue
        if out and pend:
            out.append(pend)
        pend = ""
        out.append(piece)
    return "".join(out)


def serialize_atoms(d: Doc, p: int, q: int) -> str:
    """What one native range over atoms [p, q) copies (modelled = mode B over its hull)."""
    if p >= q:
        return ""
    lo, hi = d.hull(p, q)
    return mode_b(d, lo, hi)


def native_copy(d, ranges):
    """Firefox joins several ranges with nothing (measured: 'delta.Echo'). Overlaps merge."""
    ivs = sorted((p, q) for p, q in ranges if p < q)
    merged: list[list[int]] = []
    for p, q in ivs:
        # overlaps are truncated (Selection keeps ranges disjoint); adjacent ranges stay apart
        if merged and p < merged[-1][1]:
            p = merged[-1][1]
        if p < q:
            merged.append([p, q])
    return "".join(serialize_atoms(d, p, q) for p, q in merged), merged


# ----------------------------------------------------------------------------- selmirror port

@dataclass
class Span:
    node: N
    s: int
    e: int
    end_on_child: bool
    child_follows: bool
    trim_ws: bool


def sm_child_at(d, p, idx):
    start = 0
    for c in p.children:
        end = start + d.hlen_child(c, "u16")
        if idx < end:
            return c, start
        start = end
    return None


def sm_span(d, node, s, e):
    last = sm_child_at(d, node, e - 1) if e >= 1 else None
    nxt = sm_child_at(d, node, e)
    end_on_child = last is not None and last[0].kind != "leaf"
    child_follows = nxt is not None and nxt[0].kind != "leaf"
    trim_ws = False
    if last is not None and last[0].kind == "leaf":
        c, cs = last
        at = cs
        for ch in c.text:
            here = at
            at += 2 if ord(ch) > 0xFFFF else 1
            if here <= e - 1 < at:
                trim_ws = ch.isspace()
                break
    return Span(node, s, e, end_on_child, child_follows, trim_ws)


def sm_gecko_range(sp: Span):
    e = sp.e
    if (sp.end_on_child or (sp.child_follows and sp.trim_ws)) and e > sp.s:
        e -= 1
    return sp.s, e


def sm_leaves(d: Doc, lo, hi):
    # at_offset(lo): node whose contribution holds lo; then preorder from it.
    if lo >= len(d.flat):
        return None
    n0 = d.flat[lo][2]
    start_i = d.nodes.index(n0)
    first = last = None
    for n in d.nodes[start_i:]:
        start = d.flat_start[n]
        if start >= hi:
            break
        if n.kind == "password":
            break
        if n.kind != "leaf":
            continue
        lead = 1 if n.lead else 0
        m = len(n.text)
        ts = start + lead
        if ts + m <= lo:
            continue
        frm = min(max(lo - ts, 0), m)
        to = min(max(hi - ts, 0), m)
        if to == 0 or frm >= to:
            continue
        hit = dict(id=n, frm=frm, to=to, m=m, end_flat=ts + to)
        if first is None:
            first = hit
        last = hit
    if first is None:
        return None
    return first, last


def u16pre(t, k):
    return u16(t[:k])


def sm_first_leaf(n):
    while n.kind != "leaf":
        if not n.children:
            return None
        n = n.children[0]
    return n


def sm_last_leaf(n):
    while n.kind != "leaf":
        if not n.children:
            return None
        n = n.children[-1]
    return n


def sm_plan(d: Doc, lo, hi):
    """Port of selmirror.rs `plan` (worktree agent-aa24d6c83ee085224, head 435d8a13 + diff)."""
    if lo >= hi:
        return None
    r = sm_leaves(d, lo, hi)
    if r is None:
        return None
    f, l = r
    fp, lp = f["id"].parent, l["id"].parent
    s_in = lambda p: d.pos(p, f["id"], "u16") + u16pre(f["id"].text, f["frm"])
    e_in = lambda p: d.pos(p, l["id"], "u16") + u16pre(l["id"].text, l["to"])
    if fp is lp:
        sp = sm_span(d, fp, s_in(fp), e_in(fp))
        return dict(spans=[sp], snap=sp, start_container=fp, first_end=f["end_flat"])
    chain_f, chain_l = d.anc(fp), d.anc(lp)
    c = next(a for a in chain_f if a in chain_l)

    def toward(chain, leaf):
        if leaf.parent is c:
            return leaf
        prev = leaf
        for a in chain:
            if a is c:
                break
            prev = a
        return prev

    xs, xe = toward(chain_f, f["id"]), toward(chain_l, l["id"])
    snap_s = s_in(c) if fp is c else d.pos(c, xs, "u16")
    snap_e = e_in(c) if lp is c else d.pos(c, xe, "u16") + 1
    snap = sm_span(d, c, snap_s, snap_e)
    whole_start = fp is c or (f["frm"] == 0 and sm_first_leaf(xs) is f["id"])
    whole_end = lp is c or (l["to"] == l["m"] and sm_last_leaf(xe) is l["id"])
    if whole_start and whole_end:
        spans = [snap]
    else:
        spans = []

        def push(node, s, e, out):
            if s < e:
                out.append(sm_span(d, node, s, e))

        if fp is not c and not whole_start:
            push(fp, d.pos(fp, f["id"], "u16") + u16pre(f["id"].text, f["frm"]), d.hlen(fp, "u16"), spans)
            child = fp
            for a in chain_f[1:]:
                if a is c:
                    break
                push(a, d.pos(a, child, "u16") + 1, d.hlen(a, "u16"), spans)
                child = a
        if fp is c:
            mid_s = s_in(c)
        elif whole_start:
            mid_s = d.pos(c, xs, "u16")
        else:
            mid_s = d.pos(c, xs, "u16") + 1
        if lp is c:
            mid_e = e_in(c)
        elif whole_end:
            mid_e = d.pos(c, xe, "u16") + 1
        else:
            mid_e = d.pos(c, xe, "u16")
        push(c, mid_s, mid_e, spans)
        tails: list[Span] = []
        if lp is not c and not whole_end:
            push(lp, 0, e_in(lp), tails)
            child = lp
            for a in chain_l[1:]:
                if a is c:
                    break
                push(a, 0, d.pos(a, child, "u16"), tails)
                child = a
        tails.reverse()
        spans.extend(tails)
    return dict(spans=spans, snap=snap, start_container=fp, first_end=f["end_flat"])


def sm_apply_gecko(d, plan):
    ranges = []
    for sp in plan["spans"]:
        r = gecko_range(d, sp.node, *sm_gecko_range(sp))
        if r is None:
            ranges = None
            break
        ranges.append(r)
    if ranges is None:  # source.rs falls back to the snap
        r = gecko_range(d, plan["snap"].node, *sm_gecko_range(plan["snap"]))
        return [r] if r else []
    return ranges


def sm_apply_blink(d, plan):
    sp = plan["snap"]
    p = blink_point(d, sp.node, sp.s, "cp")   # selmirror sends UTF-16, Blink reads code points
    q = blink_point(d, sp.node, sp.e, "cp")
    if p is None or q is None:
        return []
    return [(p, max(p, q))]


# ----------------------------------------------------------------------------- proposed planner

def hyper_off_of_atom(d, k, units):
    """Container and hypertext offset just before atom k."""
    n, i = d.atoms[k]
    a = n.parent
    base = d.pos(a, n, units)
    if n.kind == "leaf":
        return a, base + (u16(n.text[:i]) if units == "u16" else i)
    return a, base


def hyper_end_of_atom(d, k, units):
    """Container and hypertext offset just after atom k."""
    n, i = d.atoms[k]
    a = n.parent
    base = d.pos(a, n, units)
    if n.kind == "leaf":
        return a, base + (u16(n.text[: i + 1]) if units == "u16" else i + 1)
    return a, base + 1


def lca(d, a, b):
    ca = d.anc(a)
    return next(x for x in d.anc(b) if x in ca)


def lift_start(d, a, s, c, units, p):
    """Lift while the start is the first atom of `a` (atom-equivalent to the start of a)."""
    while a is not c and p == a.a0:
        s, a = d.pos(a.parent, a, units), a.parent
    return a, s


def lift_end(d, b, e, c, units, q):
    """Lift while the end is after the last atom of `b`."""
    while b is not c and q == b.a1:
        e, b = d.pos(b.parent, b, units) + 1, b.parent
    return b, e


def gecko_end(d, a, e):
    """Gecko API end offset for the boundary at hypertext offset e of a: (offset, cause).
    cause None: exact. 'before-hyper': the boundary sits right after a text leaf or an image and
    right before a hypertext child x, so offset e would take all of x along (Proposition G2).
    'trailing-object': e == length and the last child is an image (measured refusal)."""
    offs, total = d.hoffsets(a, "u16")
    if e == 0:
        return 0, None

    def child_at(o):
        for k, st in enumerate(offs):
            if st <= o < st + d.hlen_child(a.children[k], "u16"):
                return a.children[k]
        return None

    before = child_at(e - 1)
    at = child_at(e) if e < total else None
    if before is not None and before.kind in HYPER:
        return e - 1, None    # names the child before: Gecko descends to its end
    if at is None:
        return (e, None) if before is not None and before.kind == "leaf" else (None, "trailing-object")
    if at.kind in HYPER and at.a0 < at.a1:
        return None, "before-hyper"
    return e, None          # a text leaf, an image, or an empty hypertext follows


def plan_lifted(d: Doc, p: int, q: int, rounding="up"):
    """Minimal exact Gecko legs for atoms [p, q). Returns (ranges, info)."""
    a0, s0 = hyper_off_of_atom(d, p, "u16")
    b0, e0 = hyper_end_of_atom(d, q - 1, "u16")
    c = lca(d, a0, b0)
    a, s = lift_start(d, a0, s0, c, "u16", p)
    b, e = lift_end(d, b0, e0, c, "u16", q)
    # a whole container: name it in its parent (its end may be a trailing object, G3)
    while a is c and b is c and p == c.a0 and q == c.a1 and c.parent is not None:
        s = d.pos(c.parent, c, "u16")
        e = s + 1
        c = a = b = c.parent
    legs: list[tuple[N, int, int]] = []
    if a is c and b is c:
        legs.append((c, s, e))
    else:
        if a is not c:
            legs.append((a, s, d.hlen(a, "u16")))
            child = a
            for x in d.anc(a)[1:]:
                if x is c:
                    break
                legs.append((x, d.pos(x, child, "u16") + 1, d.hlen(x, "u16")))
                child = x
            ms = d.pos(c, child, "u16") + 1
        else:
            ms = s
        tails = []
        if b is not c:
            tails.append((b, 0, e))
            child = b
            for x in d.anc(b)[1:]:
                if x is c:
                    break
                tails.append((x, 0, d.pos(x, child, "u16")))
                child = x
            me = d.pos(c, child, "u16")
        else:
            me = e
        legs.append((c, ms, me))
        legs.extend(reversed(tails))
    legs = [(n, s_, e_) for n, s_, e_ in legs if s_ < e_]
    ranges = []
    causes = []
    for n, s_, e_ in legs:
        ge, cause = gecko_end(d, n, e_)
        if cause is not None:
            causes.append(cause)
            ge = e_ if (rounding == "up" and cause == "before-hyper") else e_ - 1
        r = gecko_range(d, n, s_, ge)
        if r is not None and r[0] < r[1]:
            ranges.append(r)
    return ranges, dict(legs=len(ranges), inexact=len(causes), causes=causes)


def certify(d: Doc, ranges, info) -> bool:
    """Exactness certificate for a Gecko multi-range plan, computed from the plan alone:
    no inexpressible end (G2/G3) and no separator (NL/CS flat char) at any junction between
    consecutive ranges. Sound (tested): certify => the browser's copy equals mode B. Not
    complete: a junction whose neighbour copies as nothing (empty alt) is rejected."""
    if info["inexact"]:
        return False
    rs = sorted(ranges)
    for (a, b), (c, _) in zip(rs, rs[1:]):
        if b != c:
            return False
        lo, hi = d.atom_flat[b - 1] + 1, d.atom_flat[c]
        if any(cls in ("NL", "CS") for _, cls, _, _ in d.flat[lo:hi]):
            return False
    return True


def plan_docsel(d: Doc, p: int, q: int):
    """Chromium Document.SetTextSelections: one range, endpoints in their own containers,
    offsets in code points (ax_platform_node_auralinux.cc:4758)."""
    a, s = hyper_off_of_atom(d, p, "cp")
    b, e = hyper_end_of_atom(d, q - 1, "cp")
    pp = blink_point(d, a, s, "cp")
    qq = blink_point(d, b, e, "cp")
    return [(pp, qq)]


# ----------------------------------------------------------------------------- generator

WORDS = ["alpha", "bravo", "charlie", "delta", "echo", "fox", "golf", "hotel", "x\U0001F600y"]


def words(rng, k, astral):
    ws = [rng.choice(WORDS if astral else WORDS[:-1]) for _ in range(k)]
    return " ".join(ws)


def inline_run(rng, astral, depth=0):
    kids = []
    for _ in range(rng.randint(1, 4)):
        r = rng.random()
        if r < 0.5 or depth > 1:
            t = words(rng, rng.randint(1, 4), astral)
            if kids and kids[-1].kind == "leaf":
                kids[-1].text += " " + t
            else:
                kids.append(L(t + rng.choice(["", " ", " "])))
        elif r < 0.75:
            kids.append(mk("link", L(words(rng, rng.randint(1, 3), astral))))
        elif r < 0.85:
            kids.append(mk("inline", *inline_run(rng, astral, depth + 1)))
        elif r < 0.95:
            kids.append(N("image", name=rng.choice(["", "logo", "chart"])))
        else:
            kids.append(mk("entry", L(words(rng, 1, astral))) if rng.random() < .5 else N("entry"))
    return kids


def block(rng, astral, depth=0):
    r = rng.random()
    if depth > 2 or r < 0.45:
        return mk("block", *inline_run(rng, astral))
    if r < 0.55:
        return mk("heading", *inline_run(rng, astral))
    if r < 0.75:
        items = []
        for _ in range(rng.randint(1, 3)):
            kids = [L("• ")] + inline_run(rng, astral)
            if rng.random() < 0.3:
                kids.append(block(rng, astral, depth + 1) if rng.random() < .3 else
                            mk("list", mk("item", L("• "), *inline_run(rng, astral))))
            items.append(mk("item", *kids))
        return mk("list", *items)
    if r < 0.88:
        rows = [mk("row", *[mk("cell", *([L(words(rng, 1, astral))] if rng.random() < .85 else []))
                            for _ in range(rng.randint(1, 3))]) for _ in range(rng.randint(1, 3))]
        return mk("table", *rows)
    # mixed content: text then blocks (div with inline text followed by a block child)
    return mk("block", *inline_run(rng, astral), *[block(rng, astral, depth + 1)
                                                    for _ in range(rng.randint(1, 2))])


def random_doc(seed, astral=False):
    rng = random.Random(seed)
    return Doc(mk("doc", *[block(rng, astral) for _ in range(rng.randint(2, 6))]))


# ----------------------------------------------------------------------------- experiments

def random_range(rng, d, unit):
    T = d.text()
    n = len(T)
    if unit == "char":
        lo = rng.randrange(n)
        hi = rng.randrange(lo + 1, min(n, lo + 60) + 1)
        return lo, hi
    if unit == "word":
        bounds = [0] + [i + 1 for i, ch in enumerate(T) if ch in " \n"] + [n]
        lo = rng.choice(bounds[:-1])
        his = [b for b in bounds if lo < b <= lo + 80] or [n]
        return lo, rng.choice(his)
    # line: start of a line to start of another line
    starts = [0] + [i + 1 for i, ch in enumerate(T) if ch == "\n"]
    i = rng.randrange(len(starts))
    j = rng.randrange(i + 1, len(starts) + 1)
    return starts[i], (starts[j] if j < len(starts) else n)


def classify(d, req, ranges):
    if req is None:
        return "nothing-requested"
    p, q = req
    want = serialize_atoms(d, p, q)
    got, merged = native_copy(d, ranges)
    cover = set()
    for a, b in merged:
        cover.update(range(a, b))
    wantset = set(range(p, q))
    if not merged:
        return "not-mirrored"
    if cover == wantset:
        return "exact" if got == want else "exact-atoms-glued"
    if cover > wantset:
        return "over"
    if cover < wantset:
        return "under"
    return "wrong"


def stats(n_docs=300, per_doc=40, astral=False):
    rng = random.Random(7)
    from collections import Counter
    keys = ("selmirror-gecko", "selmirror-gecko-snap", "lifted-gecko-up", "lifted-gecko-down",
            "selmirror-blink", "docsel-blink", "mode-b")
    res = {k: Counter() for k in keys}
    res.update({f"{k}/{u}": Counter() for k in ("selmirror-gecko", "lifted-gecko-up")
                for u in ("char", "word", "line")})
    for seed in range(n_docs):
        d = random_doc(seed, astral)
        for _ in range(per_doc):
            unit = rng.choice(["char", "word", "line"])
            lo, hi = random_range(rng, d, unit)
            req = d.flat_range_atoms(lo, hi)
            if req is None:
                continue
            # password ranges are not compared here
            pl = sm_plan(d, lo, hi)
            sm_g = sm_apply_gecko(d, pl) if pl else []
            sm_b = sm_apply_blink(d, pl) if pl else []
            res["selmirror-gecko"][classify(d, req, sm_g)] += 1
            res[f"selmirror-gecko/{unit}"][classify(d, req, sm_g)] += 1
            snap = [gecko_range(d, pl["snap"].node, *sm_gecko_range(pl["snap"]))] if pl else []
            res["selmirror-gecko-snap"][classify(d, req, [r for r in snap if r])] += 1
            res["selmirror-blink"][classify(d, req, sm_b)] += 1
            up, _ = plan_lifted(d, *req, rounding="up")
            dn, _ = plan_lifted(d, *req, rounding="down")
            res["lifted-gecko-up"][classify(d, req, up)] += 1
            res[f"lifted-gecko-up/{unit}"][classify(d, req, up)] += 1
            res["lifted-gecko-down"][classify(d, req, dn)] += 1
            res["docsel-blink"][classify(d, req, plan_docsel(d, *req))] += 1
            # mode B is exact by definition against the same serializer; check it equals the
            # atom serialization of the requested interval
            res["mode-b"]["exact" if mode_b(d, lo, hi) == serialize_atoms(d, *req) else "diff"] += 1
    return res


# ----------------------------------------------------------------------------- properties

def prop_tests(n_docs=200):
    rng = random.Random(11)
    fails = []
    for seed in range(n_docs):
        d = random_doc(seed, astral=(seed % 3 == 0))
        T = d.text()
        assert len(T) == len(d.flat)
        # P1: atom order equals flat order (P is monotone and injective on atoms)
        assert d.atom_flat == sorted(d.atom_flat)
        for _ in range(30):
            lo, hi = random_range(rng, d, rng.choice(["char", "word", "line"]))
            req = d.flat_range_atoms(lo, hi)
            if req is None:
                continue
            p, q = req
            # P2: hull(inverse(r)) is inside r, and adds only separators back
            h = d.hull(p, q)
            assert lo <= h[0] and h[1] <= hi
            assert all(c[1] in ("NL", "CS", "O0") for c in d.flat[lo:h[0]] + d.flat[h[1]:hi])
            # P3: Chromium document selection is exact
            (a, b), = plan_docsel(d, p, q)
            assert (a, b) == (p, q), (seed, lo, hi, a, b, p, q)
            # P4: lifted 'up' is a cover; 'down' is a subset; exact when no inexact leg
            up, info = plan_lifted(d, p, q, "up")
            cov = set()
            for x, y in up:
                cov.update(range(x, y))
            if "trailing-object" not in info["causes"] and not cov >= set(range(p, q)):
                fails.append(("lifted-up-not-cover", seed, lo, hi))
            if info["inexact"] == 0 and cov != set(range(p, q)):
                fails.append(("lifted-exact-claim-wrong", seed, lo, hi, up, (p, q)))
            # P7: the certificate predicts exact copies
            want = serialize_atoms(d, p, q)
            # (sound, not complete: a junction next to an empty-alt image loses nothing)
            if certify(d, up, info) and native_copy(d, up)[0] != want:
                fails.append(("certificate-unsound", seed, lo, hi))
            # P5: round trip fixpoint M(P(M(r))) == M(r) for the 'up' planner
            if up and min(x for x, _ in up) < max(y for _, y in up):
                lo2 = min(x for x, _ in up)
                hi2 = max(y for _, y in up)
                up2, _ = plan_lifted(d, lo2, hi2, "up")
                if sorted(up2) != sorted(up) and set().union(*[set(range(x, y)) for x, y in up2]) != cov:
                    fails.append(("fixpoint", seed, lo, hi))
            # P6: mode B never emits U+FFFC, never starts/ends with a separator
            b = mode_b(d, lo, hi)
            assert FFFC not in b
            assert not b.startswith(("\n", "\t")) and not b.endswith(("\n", "\t"))
    return fails


def all_gecko_intervals(d: Doc):
    """Every atom interval a single Gecko Text.SetSelection/AddSelection call can produce."""
    out = set()
    for a in d.nodes:
        if a.kind not in HYPER:
            continue
        total = d.hlen(a, "u16")
        for s_ in range(total + 1):
            for e_ in range(s_, total + 1):
                r = gecko_range(d, a, s_, e_)
                if r and r[0] < r[1]:
                    out.add(r)
    return out


def min_exact_cover(ivs, p, q):
    """Fewest intervals from `ivs` (all inside [p, q)) whose union is exactly [p, q); greedy is
    optimal for covering a line segment. None when no exact cover exists."""
    inside = [iv for iv in ivs if p <= iv[0] and iv[1] <= q]
    cur, n = p, 0
    while cur < q:
        best = max((b for a, b in inside if a <= cur < b), default=None)
        if best is None:
            return None
        cur, n = best, n + 1
    return n


def brute_check(n_docs=150):
    """Minimality and impossibility (G1..G3) against exhaustive search on small documents."""
    rng = random.Random(3)
    agree = worse = impossible_ok = impossible_bad = 0
    for seed in range(n_docs):
        d = random_doc(1000 + seed)
        if len(d.atoms) > 160:
            continue
        ivs = all_gecko_intervals(d)
        for _ in range(25):
            lo, hi = random_range(rng, d, rng.choice(["char", "word", "line"]))
            req = d.flat_range_atoms(lo, hi)
            if req is None:
                continue
            best = min_exact_cover(ivs, *req)
            ranges, info = plan_lifted(d, *req, "up")
            if info["inexact"]:
                if best is None:
                    impossible_ok += 1
                else:
                    impossible_bad += 1
                continue
            got = len([r for r in ranges if r[0] < r[1]])
            if best is not None and got == best:
                agree += 1
            else:
                worse += 1
    return dict(lifted_minimal=agree, lifted_not_minimal=worse,
                inexpressible_confirmed=impossible_ok, inexpressible_but_cover_exists=impossible_bad)


# ----------------------------------------------------------------------------- vectors

def vectors():
    """Hand-built cases referenced in the report (V1..V9)."""
    out = []

    def case(name, root, lo_s, hi_s, note):
        d = Doc(root)
        T = d.text()
        lo = T.index(lo_s[0]) + lo_s[1] if isinstance(lo_s, tuple) else lo_s
        hi = T.index(hi_s[0]) + hi_s[1] if isinstance(hi_s, tuple) else hi_s
        req = d.flat_range_atoms(lo, hi)
        pl = sm_plan(d, lo, hi)
        smg = sm_apply_gecko(d, pl) if pl else []
        smb = sm_apply_blink(d, pl) if pl else []
        up, info = plan_lifted(d, *req, "up") if req else ([], {})
        ds = plan_docsel(d, *req) if req else []
        out.append(dict(
            name=name, note=note, flat=T, range=(lo, hi), requested=repr(T[lo:hi]),
            want=repr(serialize_atoms(d, *req)) if req else "''",
            mode_b=repr(mode_b(d, lo, hi)),
            selmirror_spans=[(sp.node, sp.s, sp.e, sm_gecko_range(sp)) for sp in pl["spans"]] if pl else None,
            selmirror_gecko=repr(native_copy(d, smg)[0]), cls_sm_g=classify(d, req, smg),
            selmirror_blink=repr(native_copy(d, smb)[0]), cls_sm_b=classify(d, req, smb),
            lifted=repr(native_copy(d, up)[0]), lifted_info=info, cls_lifted=classify(d, req, up),
            docsel=repr(native_copy(d, ds)[0]), cls_docsel=classify(d, req, ds),
        ))

    # V1 one paragraph, inside a run
    case("V1 single container", mk("doc", mk("block", L("Alpha bravo charlie"))),
         ("bravo", 0), ("bravo", 5), "exact everywhere")
    # V2 ends inside a link preceded by a space
    case("V2 into a link", mk("doc", mk("block", L("See "), mk("link", L("link text")), L(" end"))),
         ("See", 0), ("link", 2), "Gecko cannot end inside the link and keep the space")
    # V3 nested list: item text then nested list, select into the nested item
    case("V3 nested list", mk("doc", mk("list", mk("item", L("• Item one"),
                                                    mk("list", mk("item", L("• Sub one")),
                                                       mk("item", L("• Sub two")))))),
         ("Item", 0), ("Sub one", 3), "mid leg ends before a hypertext child after text")
    # V4 two paragraphs, partial at both ends
    case("V4 partial blocks", mk("doc", mk("block", L("Alpha bravo")), mk("block", L("Charlie delta")),
                                 mk("block", L("Echo fox"))),
         ("bravo", 0), ("Echo", 4), "multi-range glue loses the line break")
    # V5 whole list items 2..3 then partial paragraph (lifting saves a glue point)
    case("V5 lifting", mk("doc", mk("block", mk("list", mk("item", L("• one")), mk("item", L("• two")),
                                                mk("item", L("• three"))),
                                    mk("block", L("Para text")))),
         ("• two", 0), ("Para", 4), "start at an item start lifts to the list")
    # V6 ends after an image at the end of a block
    case("V6 image end", mk("doc", mk("block", L("Logo "), N("image", name="ACME")), mk("block", L("Next"))),
         ("Logo", 0), (FFFC, 1), "end_on_child on a non-hypertext child")
    # V7 only an image
    case("V7 image only", mk("doc", mk("block", L("a "), N("image", name="chart"), L(" b"))),
         (FFFC, 0), (FFFC, 1), "no text leaf: selmirror plans nothing")
    # V8 astral scalar in Chromium
    case("V8 astral", mk("doc", mk("block", L("x\U0001F600y alpha"))), ("alpha", 0), ("alpha", 5),
         "UTF-16 offsets read as code points")
    # V9 table cells
    case("V9 table", mk("doc", mk("table", mk("row", mk("cell", L("a1")), mk("cell", L("b1"))),
                                  mk("row", mk("cell", L("a2")), mk("cell", L("b2"))))),
         0, 10**6, "cells: tab, rows: newline")
    return out


def main():
    if "--vectors" in sys.argv:
        for v in vectors():
            print(f"--- {v['name']}: {v['note']}")
            for k, val in v.items():
                if k not in ("name", "note"):
                    print(f"  {k}: {val}")
        return
    if "--brute" in sys.argv:
        print(brute_check())
        return
    fails = prop_tests()
    print(f"property failures: {len(fails)}")
    for f in fails[:10]:
        print("  ", f)
    for astral in (False, True):
        print(f"\nstats astral={astral}")
        for k, c in stats(astral=astral).items():
            tot = sum(c.values())
            print(f"  {k:18s} " + "  ".join(f"{a}={b / tot:.1%}" for a, b in sorted(c.items())))


if __name__ == "__main__":
    main()
