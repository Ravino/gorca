"""Operation counts per keypress and per patch for three designs:

  A  signed-weight set rank on the document treap (setrank.py): O(log n + depth)
  B  per-list member array cached on the list node, rebuilt lazily when dirty
     (dirty = any mutation under the list; rebuild walks the list's subtree, skipping nested lists)
  C  current code: direct children walk (count = scan children, rank = scan previous siblings)

Counts are abstract steps (treap node visits, ancestor steps, child/subtree node visits); convert to
time with ~5-20 ns/step in Rust (pointer chasing over 100k nodes, mostly L2/L3 hits).

    python tools/research/list_position/bench.py synth     # 100k nodes, one 10k-item list
    python tools/research/list_position/bench.py fixtures  # recorded Telegram A trees + churn
"""

from __future__ import annotations

import json
import random
import statistics
import sys
from pathlib import Path

from setrank import ITEM, LIST, OTHER, Doc, Spec, check

ROOT = Path(__file__).resolve().parents[3]


# ------------------------------------------------------------------ baselines B and C (cost only)
class LazyCache:
    """Design B: members[list] rebuilt on demand; mutations mark list ancestors dirty."""

    def __init__(self, doc: Doc):
        self.doc = doc
        self.cache: dict[int, dict[int, int]] = {}
        self.steps = 0

    def touch(self, x: int) -> None:
        d = self.doc
        p = x
        while p != -1 and d.alive[p]:
            self.steps += 1
            if d.kind(p) == LIST:
                self.cache.pop(p, None)
            p = d.parent[p]

    def _rebuild(self, lst: int) -> dict[int, int]:
        d = self.doc
        idx: dict[int, int] = {}
        st = list(reversed(d.kids[lst]))
        while st:
            y = st.pop()
            self.steps += 1
            k = d.kind(y)
            if k == ITEM:
                idx[y] = len(idx)
            if k == LIST:
                continue  # nested list: its subtree is skipped (needs its range: O(log n) in Rust)
            st.extend(reversed(d.kids[y]))
        self.cache[lst] = idx
        return idx

    def query(self, x: int) -> tuple[int, int] | None:
        d = self.doc
        if d.kind(x) != ITEM:
            return None
        p = d.parent[x]
        while d.kind(p) != LIST:
            self.steps += 1
            p = d.parent[p]
        if p == d.root:
            return None
        idx = self.cache.get(p)
        if idx is None:
            idx = self._rebuild(p)
        self.steps += 1
        return idx[x] + 1, len(idx)


def direct_walk(doc: Doc, x: int) -> tuple[int, int] | None:
    """Design C (crates/gorca-core/src/browse/present.rs facts_of): direct children only."""
    steps = 0
    p = doc.parent[x]
    if doc.kind(x) != ITEM or doc.kind(p) != LIST or p == doc.root:
        return None, 1
    k = n = 0
    for c in doc.kids[p]:
        steps += 1
        if doc.kind(c) == ITEM:
            n += 1
            if c == x:
                k = n
    return (k, n), steps


# ------------------------------------------------------------------ synthetic page
def item_spec(rng: random.Random, nested: bool) -> Spec:
    kids = [Spec(), Spec(children=[Spec()]), Spec()]  # text, link > text, text
    if nested:
        kids.append(Spec(role=LIST, children=[Spec(role=ITEM, children=[Spec()]) for _ in range(5)]))
    return Spec(role=ITEM, children=kids)


def synth_doc(n_items: int = 10_000, total: int = 100_000, seed: int = 7) -> tuple[Doc, int]:
    rng = random.Random(seed)
    doc = Doc(seed)
    big = Spec(role=LIST, children=[item_spec(rng, i % 50 == 0) for i in range(n_items)])
    filler = []
    used = big.count()
    while used < total:
        para = Spec(children=[Spec(), Spec(children=[Spec()]), Spec()])
        if rng.random() < 0.02:
            para = Spec(role=LIST, children=[Spec(role=ITEM, children=[Spec()]) for _ in range(8)])
        filler.append(para)
        used += para.count()
    page = Spec(children=filler[: len(filler) // 2] + [big] + filler[len(filler) // 2 :])
    top = doc.insert_subtree(doc.root, 0, page)
    lst = doc.kids[top][len(filler) // 2]
    assert doc.kind(lst) == LIST
    return doc, lst


def pct(xs: list[int], q: float) -> int:
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))]


def summary(name: str, xs: list[int]) -> str:
    return f"{name:34s} n={len(xs):5d} mean={statistics.mean(xs):9.1f} p50={pct(xs, .5):7d} p99={pct(xs, .99):7d} max={max(xs):7d}"


def run_synth(rounds: int = 400, seed: int = 11) -> list[str]:
    rng = random.Random(seed)
    doc, lst = synth_doc()
    nodes = len(doc.parent)
    out = [f"synthetic page: {nodes} nodes, big list {doc.count(lst)} items, depth of items 3"]
    B = LazyCache(doc)
    costs: dict[str, list[int]] = {}

    def rec(k: str, v: int) -> None:
        costs.setdefault(k, []).append(v)

    def measure_query(x: int) -> None:
        doc.tsteps = doc.asteps = 0
        ra = doc.rank(x)
        cnt = doc.count(ra[1]) if ra else None
        rec("A key: rank+count", doc.tsteps + doc.asteps)
        B.steps = 0
        rb = B.query(x)
        rec("B key: rank+count (after patch)", B.steps)
        B.steps = 0
        B.query(x)
        rec("B key: rank+count (clean)", B.steps)
        rc, sc = direct_walk(doc, x)
        rec("C key: rank+count", sc)
        assert ra is None or rb == (ra[0], cnt), (ra, rb, cnt)
        if rc is not None and ra is not None:
            assert rc == (ra[0], cnt)
        doc.tsteps = doc.asteps = 0
        if ra:
            doc.kth(ra[1], rng.randrange(cnt))
        rec("A key: kth (jump to item k)", doc.tsteps + doc.asteps)

    synth: list[int] = []
    for r in range(rounds):
        items = doc.kids[lst]
        op = rng.choice(["insert", "remove", "role", "wrap", "dissolve", "move"])
        doc.tsteps = doc.asteps = 0
        i = rng.randrange(len(items))
        touched = lst
        if op == "insert":
            touched = doc.insert_subtree(lst, i, item_spec(rng, False))
        elif op == "remove" and doc.kind(items[i]) == ITEM:
            doc.remove_subtree(items[i])
        elif op == "role":
            touched = items[i]
            doc.set_props(touched, role=rng.choice([ITEM, OTHER]))
        elif op == "wrap" and len(synth) < 64:
            j = min(len(items), i + rng.randint(2, 256))
            if j - i >= 2:
                touched = doc.wrap(lst, i, j)
                synth.append(touched)
        elif op == "dissolve" and synth:
            n = synth.pop(rng.randrange(len(synth)))
            doc.dissolve(n)
        elif op == "move":
            j = rng.randrange(len(items))
            x = items[i]
            if doc.kind(x) == ITEM:
                touched = x
                doc.move_subtree(x, lst, j if j < len(items) - 1 else 0)
        rec(f"A patch: {op}", doc.tsteps + doc.asteps)
        B.steps = 0
        B.touch(touched if doc.alive[touched] else lst)
        rec("B patch: dirty marking", B.steps)
        # a keypress after every patch, on a random item of the big list (or of a synthetic one)
        cands = [y for y in doc.kids[lst][: 64] if doc.kind(y) == ITEM] or [y for y in doc.kids[lst] if doc.kind(y) == ITEM]
        x = rng.choice(doc.kids[lst])
        if doc.kind(x) == LIST and doc.kids[x]:
            x = rng.choice(doc.kids[x])
        if doc.kind(x) != ITEM and cands:
            x = rng.choice(cands)
        measure_query(x)

    # worst case for A's list-toggle: a plugin turns the 10k-item list off and on (role patch)
    doc.tsteps = doc.asteps = 0
    doc.set_props(lst, role=OTHER)
    rec("A patch: list role off (10k items)", doc.tsteps + doc.asteps)
    doc.tsteps = doc.asteps = 0
    doc.set_props(lst, role=LIST)
    rec("A patch: list role on (10k items)", doc.tsteps + doc.asteps)
    for k in sorted(costs):
        out.append(summary(k, costs[k]))
    return out


# ------------------------------------------------------------------ recorded fixtures
def load_fixture(name: str) -> list[dict]:
    return json.loads((ROOT / "tests/fixtures/web" / name).read_text())


def classes(x: dict) -> set[str]:
    return set(x["class"].split())


def plugin_role(x: dict, by: dict[int, dict]) -> str:
    """The Telegram plugin's re-role facts (plugins/telegram-web/src/lib.rs)."""
    c = classes(x)
    if "chat-list" in c or "MessageList" in c:
        return LIST
    if "chat-item-clickable" in c or "Message" in c:
        return ITEM
    if "ListItem-button" in c:
        p = by.get(x["p"])
        if p is not None and "chat-list" in classes(p):
            return ITEM
    return OTHER


def fixture_doc(rows: list[dict]) -> tuple[Doc, dict[int, int]]:
    """Source roles plain (OTHER): roles arrive later as plugin patches, like the live path."""
    kids: dict[int, list[dict]] = {}
    for x in rows:
        kids.setdefault(x["p"], []).append(x)

    def spec(x: dict) -> Spec:
        return Spec(children=[spec(k) for k in kids.get(x["i"], [])])

    doc = Doc(3)
    top = [x for x in rows if x["p"] == -1]
    ids: dict[int, int] = {}
    for t in top:
        n = doc.insert_subtree(doc.root, len(doc.kids[doc.root]), spec(t))
        # map fixture ids to doc ids by parallel preorder
        st = [(t, n)]
        while st:
            fx, dx = st.pop()
            ids[fx["i"]] = dx
            for fk, dk in zip(kids.get(fx["i"], []), doc.kids[dx]):
                st.append((fk, dk))
    return doc, ids


def run_fixtures(seed: int = 5) -> list[str]:
    rng = random.Random(seed)
    out = []
    for name in ["telegram-a-chat-list.json", "telegram-a-open-chat.json", "telegram-a-live.json"]:
        rows = load_fixture(name)
        by = {x["i"]: x for x in rows}
        doc, ids = fixture_doc(rows)
        doc.tsteps = doc.asteps = 0
        patched = 0
        for x in rows:
            r = plugin_role(x, by)
            if r != OTHER:
                doc.set_props(ids[x["i"]], role=r)
                patched += 1
        batch = doc.tsteps + doc.asteps
        check(doc)
        lists = [l for l in doc.c if l != doc.root]
        desc = ", ".join(f"{doc.count(l)} items" for l in lists)
        out.append(f"{name}: {len(rows)} nodes, {patched} role facts -> lists [{desc}]; load batch {batch} steps ({batch / max(1, patched):.1f}/fact)")
        if not lists:
            continue
        lst = max(lists, key=lambda l: doc.count(l))
        # virtual scroll churn: per step drop k rows at the top, append k clones at the bottom
        # (inserted with plain roles; the plugin's re-role facts arrive in the next batch)
        per_batch, per_key = [], []
        tmpl_rows = [y for y in doc.kids[lst] if doc.kind(y) == ITEM]
        for step in range(300):
            k = rng.randint(1, 4)
            doc.tsteps = doc.asteps = 0
            for _ in range(k):
                its = [y for y in doc.kids[lst] if doc.kind(y) == ITEM]
                if len(its) > 3:
                    doc.remove_subtree(its[0])
            fresh = []
            for _ in range(k):
                t = rng.choice(tmpl_rows)
                fresh.append(doc.insert_subtree(lst, len(doc.kids[lst]), clone(doc, t)))
            if rng.random() < 0.3:  # a chat moves to the top (new message)
                its = [y for y in doc.kids[lst] if doc.kind(y) == ITEM]
                doc.move_subtree(its[-1], lst, 0)
            b1 = doc.tsteps + doc.asteps
            doc.tsteps = doc.asteps = 0
            for f in fresh:
                doc.set_props(f, role=ITEM)
            per_batch.append(b1 + doc.tsteps + doc.asteps)
            for _ in range(3):
                its = [y for y in doc.kids[lst] if doc.kind(y) == ITEM]
                x = rng.choice(its)
                doc.tsteps = doc.asteps = 0
                rk = doc.rank(x)
                doc.count(rk[1])
                per_key.append(doc.tsteps + doc.asteps)
        check(doc)
        out.append("  " + summary("A churn batch (k<=4 rows + re-role)", per_batch))
        out.append("  " + summary("A key: rank+count", per_key))
    return out


def clone(doc: Doc, x: int) -> Spec:
    return Spec(role=OTHER, children=[clone(doc, k) for k in doc.kids[x]])


if __name__ == "__main__":
    which = sys.argv[1] if len(sys.argv) > 1 else "all"
    lines = []
    if which in ("synth", "all"):
        lines += run_synth()
    if which in ("fixtures", "all"):
        lines += run_fixtures()
    print("\n".join(lines))
