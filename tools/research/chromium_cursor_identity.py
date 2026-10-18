#!/usr/bin/env python3
"""Browse-cursor identity across Chromium re-syncs (research prototype, see
docs/reports/chromium-mirror-freshness-math.md section 3).

A message list (Signal-like) is transformed old -> new the way Chromium/React produce it, and a
browse cursor on a random node of the old tree is mapped into the new tree by three matchers:

  M0  object path only (AT-SPI path equality; paths are never reused).
  M1  sibling ladder as in crates/gorca-doc-source-atspi/src/identity.rs (IL-1 path, Rewrite
      detection = >= 2 kept siblings changed own signature -> strong rung only, IL-3 full key
      (role, name, tag) in order within the same gap, then (role, tag) in-gap zip; recursive
      into matched pairs). Approximation: no DOM ids (Signal rows carry none we can rely on).
  M2  proposed: (a) GumTree-style top-down exact matching of isomorphic subtrees by Merkle hash
      (unique hash in both trees, then equal-multiplicity groups paired in document order and
      flagged ambiguous), (b) path identity only when own signature AND item hash are unchanged,
      (c) for an unmatched cursor: candidates with the same own signature, scored by enclosing
      item content hash + k=2 leaf context on both sides; confident only with a unique maximum.

Ground truth: every node carries a logical id (message id, slot in the message template). The
correct target is the new node with the same logical id, or "lost" if that message is gone.

Outcomes: correct, wrong-confident (dangerous: an Enter would act on a different item),
wrong-flagged (mapped but flagged ambiguous, so the displaced-cursor rule makes Enter announce),
lost (no target; also correct when the message is gone).

Usage: python tools/research/chromium_cursor_identity.py [--trials N]
"""
from __future__ import annotations

import argparse
import hashlib
import random
from collections import defaultdict

COMMON = ["ok", "thanks", "lol", "yes", "no", "sure", "see you", "👍", "haha", "good night",
          "on my way", "call me", "nice", "?", "!", "wow", "same", "ok!", "brb", "done"]


class N:
    __slots__ = ("path", "role", "name", "tag", "kids", "parent", "lid")

    def __init__(self, path, role, name, tag, lid):
        self.path, self.role, self.name, self.tag, self.lid = path, role, name, tag, lid
        self.kids, self.parent = [], None


class Gen:
    def __init__(self, rng, authors, times, dup):
        self.rng, self.authors, self.times, self.dup = rng, authors, times, dup
        self.next_path = 1
        self.content = {}  # msg id -> (author, text, time)

    def path(self):
        self.next_path += 1
        return self.next_path

    def msg_content(self, mid):
        if mid not in self.content:
            r = self.rng
            text = r.choice(COMMON) if r.random() < self.dup else f"text {r.randrange(10**9)}"
            self.content[mid] = (f"Author {r.randrange(self.authors)}", text,
                                 f"10:{r.randrange(self.times):02d}")
        return self.content[mid]

    def node(self, role, name, tag, lid, kids=()):
        n = N(self.path(), role, name, tag, lid)
        for k in kids:
            k.parent = n
            n.kids.append(k)
        return n

    def message(self, mid):
        a, txt, tm = self.msg_content(mid)
        g = self.node
        return g("listitem", "", "li", (mid, 0), [g("article", "", "div", (mid, 1), [
            g("section", "", "div", (mid, 2), [g("static", a, None, (mid, 3)),
                                               g("static", txt, None, (mid, 4))]),
            g("static", tm, None, (mid, 5)),
            g("button", "React", "button", (mid, 6)),
            g("button", "Reply", "button", (mid, 7)),
            g("button", "More actions", "button", (mid, 8)),
            g("section", "", "div", (mid, 9), [g("static", "delivered", None, (mid, 10))]),
        ])])

    def doc(self, mids):
        lst = self.node("list", "messages", "ul", ("list",), [self.message(m) for m in mids])
        return self.node("document", "Signal", None, ("doc",), [lst])


def set_content(n, gen, mid):
    """Recycle a row object in place: same paths, new content (virtualized list)."""
    a, txt, tm = gen.msg_content(mid)
    names = {3: a, 4: txt, 5: tm}
    for x in walk(n):
        slot = x.lid[1]
        x.lid = (mid, slot)
        if slot in names:
            x.name = names[slot]


def walk(n):
    st = [n]
    while st:
        x = st.pop()
        yield x
        st.extend(reversed(x.kids))


def clone(n):
    c = N(n.path, n.role, n.name, n.tag, n.lid)
    for k in n.kids:
        kk = clone(k)
        kk.parent = c
        c.kids.append(kk)
    return c


# ------------------------------------------------------------------ transforms
def t_keyed_rerender(rng, gen, mids):
    """React re-render that recreates every object (new paths), plus one insert/delete/edit."""
    new = list(mids)
    i = rng.randrange(len(new))
    new.insert(i, max(gen.content) + 1 if gen.content else 10**6)
    del new[rng.randrange(len(new))]
    old_doc = gen.doc(mids)
    e = rng.choice(new)
    new_doc = gen.doc(new)
    if e in gen.content:  # edit one message's text in the new tree only
        a, txt, tm = gen.content[e]
        for x in walk(new_doc):
            if x.lid == (e, 4):
                x.name = txt + " (edited)"
    return old_doc, new_doc


def t_virtual_scroll(rng, gen, mids):
    """Window of rows recycled in place: row i now shows message i+s (same paths)."""
    old_doc = gen.doc(mids)
    new_doc = clone(old_doc)
    s = rng.randint(1, 5)
    stream = list(mids) + [max(mids) + k + 1 for k in range(s)]
    rows = new_doc.kids[0].kids
    for i, r in enumerate(rows):
        set_content(r, gen, stream[i + s])
    return old_doc, new_doc


def t_mixed(rng, gen, mids):
    """Scroll by s; half of the rows recycled in place, half recreated with new paths."""
    old_doc = gen.doc(mids)
    new_doc = clone(old_doc)
    s = rng.randint(1, 5)
    stream = list(mids) + [max(mids) + k + 1 for k in range(s)]
    lst = new_doc.kids[0]
    for i, r in enumerate(list(lst.kids)):
        mid = stream[i + s]
        if rng.random() < 0.5:
            set_content(r, gen, mid)
        else:
            m = gen.message(mid)
            m.parent = lst
            lst.kids[i] = m
    return old_doc, new_doc


def t_append(rng, gen, mids):
    old_doc = gen.doc(mids)
    new_doc = clone(old_doc)
    m = gen.message(max(mids) + 1)
    m.parent = new_doc.kids[0]
    new_doc.kids[0].kids.append(m)
    return old_doc, new_doc


TRANSFORMS = {"keyed-rerender": t_keyed_rerender, "virtual-scroll": t_virtual_scroll,
              "mixed": t_mixed, "append": t_append}


# ------------------------------------------------------------------ matchers
def sig(n):
    return (n.role, n.name, n.tag)


def m0_path(old, new, cur):
    by = {x.path: x for x in walk(new)}
    return by.get(cur.path), False


def m1_ladder(old, new, cur):
    """Approximation of identity.rs IL-1..IL-3 (no DOM ids)."""
    mapping = {}

    def match_lists(o_list, n_list):
        n_by_path = {x.path: j for j, x in enumerate(n_list)}
        kept = {}
        changed = 0
        for i, o in enumerate(o_list):
            j = n_by_path.get(o.path)
            if j is not None:
                kept[i] = j
                if sig(o) != sig(n_list[j]):
                    changed += 1
        rewrite = changed >= 2
        out = {}
        if rewrite:
            # Burst::Rewrite: strong rung only (full key), objects are the same, content moved
            q = defaultdict(list)
            for j, x in enumerate(n_list):
                q[sig(x)].append(j)
            used = set()
            for i, o in enumerate(o_list):
                for j in q.get(sig(o), []):
                    if j not in used:
                        used.add(j)
                        out[i] = j
                        break
            return out
        out.update(kept)
        # gaps: anchors = kept pairs; leftovers paired within the same gap
        def gap_index(lst_len, anchors):
            g, res = 0, []
            aset = set(anchors)
            for k in range(lst_len):
                if k in aset:
                    g += 1
                    res.append(None)
                else:
                    res.append(g)
            return res
        og = gap_index(len(o_list), out.keys())
        ng = gap_index(len(n_list), out.values())
        used = set(out.values())
        for key in (lambda x: sig(x), lambda x: (x.role, x.tag)):
            q = defaultdict(list)
            for j, x in enumerate(n_list):
                if j not in used:
                    q[(ng[j], key(x))].append(j)
            for i, o in enumerate(o_list):
                if i in out:
                    continue
                cand = q.get((og[i], key(o)), [])
                while cand and cand[0] in used:
                    cand.pop(0)
                if cand:
                    j = cand.pop(0)
                    used.add(j)
                    out[i] = j
        return out

    def rec(o, n):
        mapping[id(o)] = n
        pairs = match_lists(o.kids, n.kids)
        for i, j in pairs.items():
            rec(o.kids[i], n.kids[j])

    rec(old, new)
    return mapping.get(id(cur)), False


def merkle(root):
    h = {}

    def rec(n):
        m = hashlib.blake2b(digest_size=8)
        m.update(repr(sig(n)).encode())
        for k in n.kids:
            m.update(rec(k))
        h[id(n)] = m.digest()
        return h[id(n)]

    rec(root)
    return h


ITEM_ROLES = {"listitem", "article", "row"}


def item_of(n):
    x = n
    while x is not None and x.role not in ITEM_ROLES:
        x = x.parent
    if x is None:
        return None
    while x.parent is not None and x.parent.role in ITEM_ROLES:
        x = x.parent
    return x


def leaves(root):
    return [x for x in walk(root) if not x.kids]


def m2_proposed(old, new, cur, k=2):
    ho, hn = merkle(old), merkle(new)
    # (a) top-down isomorphic subtree matching, largest first
    n_by_hash = defaultdict(list)
    for x in walk(new):
        n_by_hash[hn[id(x)]].append(x)
    o_by_hash = defaultdict(list)
    for x in walk(old):
        o_by_hash[ho[id(x)]].append(x)
    mapping, ambiguous, used_n = {}, set(), set()

    def size(x):
        return sum(1 for _ in walk(x))

    olist = sorted(walk(old), key=lambda x: -size(x))
    for o in olist:
        if id(o) in mapping:
            continue
        h = ho[id(o)]
        ol, nl = o_by_hash[h], n_by_hash.get(h, [])
        if not nl:
            continue
        free_o = [x for x in ol if id(x) not in mapping]
        free_n = [x for x in nl if id(x) not in used_n]
        if not free_n:
            continue
        amb = not (len(ol) == 1 and len(nl) == 1)
        # pair by document order rank inside the group
        for a, b in zip(free_o, free_n):
            for xa, xb in zip(walk(a), walk(b)):
                if id(xa) not in mapping:
                    mapping[id(xa)] = xb
                    used_n.add(id(xb))
                    if amb:
                        ambiguous.add(id(xa))
    # content-bearing ambiguity: a cursor inside an ambiguous group whose item is unique is fine
    if id(cur) in mapping:
        tgt = mapping[id(cur)]
        if id(cur) not in ambiguous:
            return tgt, False
    # (b) path identity if own signature and item content unchanged
    by_path = {x.path: x for x in walk(new)}

    def item_hash(n, h):
        it = item_of(n)
        return h[id(it)] if it is not None else None

    p = by_path.get(cur.path)
    if p is not None and sig(p) == sig(cur) and item_hash(p, hn) == item_hash(cur, ho):
        # same object, same content; if the item content has a twin, the object may now show
        # the twin (a recycled row), so the answer is flagged
        ih = item_hash(p, hn)
        twins = ih is not None and len(n_by_hash.get(ih, [])) > 1
        return p, twins
    # (c) content-anchored relocation
    lo, ln = leaves(old), leaves(new)
    pos_o = {id(x): i for i, x in enumerate(lo)}
    pos_n = {id(x): i for i, x in enumerate(ln)}

    def anchor_leaf(n, pos, ls):
        if id(n) in pos:
            return pos[id(n)]
        l = leaves(n)
        return pos[id(l[0])] if l else None

    def ctx(n, pos, ls):
        i = anchor_leaf(n, pos, ls)
        if i is None:
            return ()
        return tuple(ls[j].name if 0 <= j < len(ls) else None
                     for j in range(i - k, i + k + 1) if j != i)

    c_item = item_hash(cur, ho)
    c_ctx = ctx(cur, pos_o, lo)
    cands = [x for x in walk(new) if sig(x) == sig(cur)]
    scored = []
    for x in cands:
        s = 0
        if c_item is not None and item_hash(x, hn) == c_item:
            s += 10
        cx = ctx(x, pos_n, ln)
        s += sum(1 for a, b in zip(c_ctx, cx) if a == b and a is not None)
        scored.append((s, x))
    if not scored:
        return None, False
    scored.sort(key=lambda t: -t[0])
    best = scored[0]
    if best[0] < 10:  # no item-level evidence: refuse
        return None, False
    tie = len(scored) > 1 and scored[1][0] == best[0]
    # a twin item (same content hash) in either tree makes even a unique maximum unsafe:
    # the original may be gone and the survivor is its twin
    twins = c_item is not None and (len(o_by_hash.get(c_item, [])) > 1 or
                                    len(n_by_hash.get(c_item, [])) > 1)
    if twins and not tie:
        return best[1], True
    if tie:
        # rank tie-break by relative document position, flagged ambiguous
        rel = anchor_leaf(cur, pos_o, lo) / max(1, len(lo))
        top = [x for s, x in scored if s == best[0]]
        x = min(top, key=lambda y: abs(anchor_leaf(y, pos_n, ln) / max(1, len(ln)) - rel))
        return x, True
    return best[1], False


MATCHERS = {"M0-path": m0_path, "M1-ladder": m1_ladder, "M2-proposed": m2_proposed}


def trial(rng, transform, matcher, authors, times, dup, n_msgs=40):
    gen = Gen(rng, authors, times, dup)
    mids = list(range(n_msgs))
    old, new = TRANSFORMS[transform](rng, gen, mids)
    cands = [x for x in walk(old) if x.role in ("static", "button")]
    cur = rng.choice(cands)
    truth = next((x for x in walk(new) if x.lid == cur.lid), None)
    got, amb = MATCHERS[matcher](old, new, cur)
    if got is None:
        return "lost-ok" if truth is None else "lost"
    if truth is not None and got is truth:
        return "correct-flagged" if amb else "correct"
    return "wrong-flagged" if amb else "WRONG-confident"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trials", type=int, default=400)
    a = ap.parse_args()
    scen = [("distinct", 5, 60, 0.1), ("duplicates", 2, 2, 0.6)]
    cols = ["correct", "correct-flagged", "wrong-flagged", "WRONG-confident", "lost", "lost-ok"]
    print("| content | transform | matcher | " + " | ".join(cols) + " |")
    print("|" + "---|" * (3 + len(cols)))
    for sname, au, tm, dup in scen:
        for tr in TRANSFORMS:
            for mname in MATCHERS:
                rng = random.Random(hash((sname, tr)) & 0xffff)
                c = defaultdict(int)
                for _ in range(a.trials):
                    c[trial(rng, tr, mname, au, tm, dup)] += 1
                cells = [f"{c[k] / a.trials:.3f}" for k in cols]
                print(f"| {sname} | {tr} | {mname} | " + " | ".join(cells) + " |")
    # analytic ambiguity: probability that a message has a content twin among M-1 others
    for sname, au, tm, dup in scen:
        q_text = dup * dup / len(COMMON)
        q = (1 / au) * (1 / tm) * q_text
        for M in (40, 200, 1000):
            print(f"twin probability {sname} M={M}: q={q:.2e}, P(twin)={1 - (1 - q) ** (M - 1):.3f}")


if __name__ == "__main__":
    main()
