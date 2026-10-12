"""Synthetic DOM-like accessibility tree generator.

Produces deterministic (seeded) trees that mimic the shape of real web pages as
exposed through AT-SPI: banner/nav/main/aside/footer landmarks, article sections
with headings and paragraphs, product-card grids built from a few templates with
noise, data tables, forms, nested comment threads and link-heavy footers.

The tree is stored as a plain arena (lists indexed by node id), which is also the
layout we recommend for the Rust mirror (see the report).

Ground truth for the embedding experiments:
  `family[v]` is the template family id for the root of a repeated component
  (product card, comment, footer column, ...) and -1 otherwise.
"""
from __future__ import annotations

import random
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Roles (a small superset of what structural navigation cares about)
ROLES = [
    "document", "banner", "navigation", "main", "complementary", "contentinfo",
    "region", "form", "search", "generic", "heading", "paragraph", "link", "list",
    "listitem", "button", "textbox", "checkbox", "radio", "combobox", "table",
    "row", "cell", "columnheader", "image", "text", "article", "group", "label",
    "separator",
]
R = {name: i for i, name in enumerate(ROLES)}

LANDMARKS = {R[x] for x in ("banner", "navigation", "main", "complementary",
                             "contentinfo", "region", "form", "search")}
FORM_FIELDS = {R[x] for x in ("textbox", "checkbox", "radio", "combobox", "button")}

# Navigation categories (bit positions in a u64 mask). A node can be in several.
CATS = ["heading", "h1", "h2", "h3", "h4", "h5", "h6", "landmark", "link",
        "visited_link", "form_field", "table", "cell", "list", "listitem",
        "image", "button", "article", "separator"]
C = {name: i for i, name in enumerate(CATS)}


def category_mask(role: int, level: int, visited: bool) -> int:
    m = 0
    if role == R["heading"]:
        m |= 1 << C["heading"]
        if 1 <= level <= 6:
            m |= 1 << C[f"h{level}"]
    if role in LANDMARKS:
        m |= 1 << C["landmark"]
    if role == R["link"]:
        m |= 1 << C["link"]
        if visited:
            m |= 1 << C["visited_link"]
    if role in FORM_FIELDS:
        m |= 1 << C["form_field"]
    if role == R["button"]:
        m |= 1 << C["button"]
    if role == R["table"]:
        m |= 1 << C["table"]
    if role in (R["cell"], R["columnheader"]):
        m |= 1 << C["cell"]
    if role == R["list"]:
        m |= 1 << C["list"]
    if role == R["listitem"]:
        m |= 1 << C["listitem"]
    if role == R["image"]:
        m |= 1 << C["image"]
    if role == R["article"]:
        m |= 1 << C["article"]
    if role == R["separator"]:
        m |= 1 << C["separator"]
    return m


WORDS = ("lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod "
         "tempor incididunt ut labore et dolore magna aliqua enim ad minim veniam "
         "quis nostrud exercitation ullamco laboris nisi aliquip ex ea commodo "
         "consequat duis aute irure in reprehenderit voluptate velit esse cillum "
         "fugiat nulla pariatur excepteur sint occaecat cupidatat non proident").split()


@dataclass
class Tree:
    role: list = field(default_factory=list)
    name: list = field(default_factory=list)
    level: list = field(default_factory=list)
    visited: list = field(default_factory=list)
    parent: list = field(default_factory=list)
    children: list = field(default_factory=list)
    family: list = field(default_factory=list)   # ground-truth template family
    key: list = field(default_factory=list)      # stable identity (AT-SPI object ref)

    def add(self, role: str, parent: int | None, name: str = "", level: int = 0,
            visited: bool = False, family: int = -1) -> int:
        v = len(self.role)
        self.role.append(R[role])
        self.name.append(name)
        self.level.append(level)
        self.visited.append(visited)
        self.parent.append(-1 if parent is None else parent)
        self.children.append([])
        self.family.append(family)
        self.key.append(v)
        if parent is not None:
            self.children[parent].append(v)
        return v

    def __len__(self) -> int:
        return len(self.role)

    def mask(self, v: int) -> int:
        return category_mask(self.role[v], self.level[v], self.visited[v])

    def preorder(self, root: int = 0) -> list:
        out, stack = [], [root]
        while stack:
            v = stack.pop()
            out.append(v)
            stack.extend(reversed(self.children[v]))
        return out

    def depth(self) -> int:
        d = [0] * len(self)
        best = 0
        for v in self.preorder():
            p = self.parent[v]
            if p >= 0:
                d[v] = d[p] + 1
                best = max(best, d[v])
        return best


class Gen:
    """Page generator. `n_target` is approximate (we stop adding blocks once reached)."""

    def __init__(self, seed: int = 0):
        self.rng = random.Random(seed)
        # Card templates: list of (role, optional) parts. Families 0..K-1.
        self.card_templates = [
            [("image", 0.1), ("heading", 0.0), ("text", 0.0), ("button", 0.0)],
            [("link", 0.0), ("image", 0.0), ("text", 0.2), ("group", 0.3), ("button", 0.1)],
            [("heading", 0.0), ("paragraph", 0.0), ("paragraph", 0.5), ("link", 0.0)],
            [("image", 0.0), ("link", 0.0), ("text", 0.0), ("text", 0.0), ("checkbox", 0.3), ("button", 0.0)],
        ]
        self.next_family_offset = len(self.card_templates)

    def words(self, lo: int, hi: int) -> str:
        return " ".join(self.rng.choice(WORDS) for _ in range(self.rng.randint(lo, hi)))

    # -- components ---------------------------------------------------------
    def wrap(self, t: Tree, p: int, hi: int = 3) -> int:
        """Insert 0..hi anonymous `generic` wrappers (div soup) and return the innermost."""
        for _ in range(self.rng.randint(0, hi)):
            p = t.add("generic", p)
        return p

    def nav(self, t: Tree, p: int, n_links: int) -> None:
        nav = t.add("navigation", p, "Main")
        lst = t.add("list", nav)
        for _ in range(n_links):
            li = t.add("listitem", lst, family=100)
            a = t.add("link", li, self.words(1, 2), visited=self.rng.random() < 0.3)
            t.add("text", a, t.name[a])

    def card(self, t: Tree, p: int, fam: int) -> None:
        rng = self.rng
        root = t.add("article" if fam % 2 == 0 else "listitem", p, family=fam)
        inner = self.wrap(t, root, 2)
        for role, p_skip in self.card_templates[fam]:
            if rng.random() < p_skip:
                continue
            if role == "heading":
                h = t.add("heading", inner, self.words(2, 5), level=3)
                a = t.add("link", h, t.name[h], visited=rng.random() < 0.2)
                t.add("text", a, t.name[h])
            elif role == "group":
                g = t.add("group", inner, "rating")
                for _ in range(rng.randint(1, 3)):
                    t.add("image", g, "star")
            elif role in ("link", "button", "checkbox"):
                x = t.add(role, inner, self.words(1, 3), visited=(role == "link" and rng.random() < 0.2))
                t.add("text", x, t.name[x])
            elif role == "paragraph":
                x = t.add("paragraph", inner)
                t.add("text", x, self.words(5, 20))
            else:
                t.add(role, inner, self.words(1, 4))
        # rare noise: an extra badge
        if rng.random() < 0.1:
            t.add("text", root, "new")

    def grid(self, t: Tree, p: int, n_cards: int) -> None:
        reg = t.add("region", p, "Products")
        t.add("heading", reg, self.words(1, 3), level=2)
        fam = self.rng.randrange(len(self.card_templates))
        lst = t.add("list", reg)
        for _ in range(n_cards):
            self.card(t, lst, fam)

    def section(self, t: Tree, p: int, level: int, depth: int) -> None:
        sec = t.add("generic", p)
        h = t.add("heading", sec, self.words(2, 6), level=level)
        t.add("text", h, t.name[h])
        for _ in range(self.rng.randint(1, 5)):
            para = t.add("paragraph", sec)
            for _ in range(self.rng.randint(1, 4)):
                if self.rng.random() < 0.3:
                    a = t.add("link", para, self.words(1, 3), visited=self.rng.random() < 0.3)
                    t.add("text", a, t.name[a])
                else:
                    t.add("text", para, self.words(3, 15))
        if depth < 3 and self.rng.random() < 0.5:
            for _ in range(self.rng.randint(1, 3)):
                self.section(t, sec, min(level + 1, 6), depth + 1)

    def table(self, t: Tree, p: int, rows: int, cols: int) -> None:
        tb = t.add("table", p, self.words(1, 3))
        hr = t.add("row", tb)
        for _ in range(cols):
            c = t.add("columnheader", hr, self.words(1, 2))
            t.add("text", c, t.name[c])
        for _ in range(rows):
            r = t.add("row", tb, family=101)
            for _ in range(cols):
                c = t.add("cell", r)
                t.add("text", c, self.words(1, 3))

    def form(self, t: Tree, p: int, n_fields: int) -> None:
        f = t.add("form", p, "Sign in" if self.rng.random() < 0.5 else "Contact")
        for _ in range(n_fields):
            g = t.add("group", f, family=102)
            lbl = t.add("label", g, self.words(1, 2))
            t.add("text", lbl, t.name[lbl])
            t.add(self.rng.choice(["textbox", "textbox", "combobox", "checkbox"]), g, t.name[lbl])
        b = t.add("button", f, "Submit")
        t.add("text", b, "Submit")

    def comments(self, t: Tree, p: int, n: int, depth: int = 0) -> None:
        for _ in range(n):
            c = t.add("article", p, family=103)
            a = t.add("link", c, self.words(1, 2))
            t.add("text", a, t.name[a])
            para = t.add("paragraph", c)
            t.add("text", para, self.words(5, 30))
            t.add("button", c, "Reply")
            if depth < 4 and self.rng.random() < 0.3:
                self.comments(t, c, self.rng.randint(1, 3), depth + 1)

    def footer(self, t: Tree, p: int) -> None:
        f = t.add("contentinfo", p)
        for _ in range(4):
            col = t.add("generic", f, family=104)
            t.add("heading", col, self.words(1, 2), level=4)
            lst = t.add("list", col)
            for _ in range(self.rng.randint(4, 8)):
                li = t.add("listitem", lst)
                a = t.add("link", li, self.words(1, 2))
                t.add("text", a, t.name[a])

    def page(self, n_target: int) -> Tree:
        t = Tree()
        doc = t.add("document", None, "Synthetic page")
        ban = t.add("banner", doc)
        t.add("link", ban, "Home")
        self.nav(t, ban, self.rng.randint(5, 12))
        main = t.add("main", doc)
        t.add("heading", main, "Synthetic page", level=1)
        aside_at = self.rng.random()
        while len(t) < n_target - 200:
            k = self.rng.random()
            main_p = main
            main = self.wrap(t, main_p, 3)
            if k < 0.35:
                self.section(t, main, 2, 0)
            elif k < 0.60:
                self.grid(t, main, self.rng.randint(8, 48))
            elif k < 0.72:
                self.table(t, main, self.rng.randint(5, 40), self.rng.randint(3, 8))
            elif k < 0.80:
                self.form(t, main, self.rng.randint(2, 8))
            else:
                sec = t.add("region", main, "Comments")
                t.add("heading", sec, "Comments", level=2)
                self.comments(t, sec, self.rng.randint(3, 15))
            main = main_p
            if aside_at and len(t) > aside_at * n_target:
                aside = t.add("complementary", doc)
                self.nav(t, aside, 10)
                aside_at = 0.0
        self.footer(t, doc)
        return t


def page(n: int, seed: int = 0) -> Tree:
    return Gen(seed).page(n)


def random_subtree(seed: int, size: int) -> Tree:
    """A fresh component subtree (for insert/replace mutations); root is node 0."""
    g = Gen(seed)
    t = Tree()
    root = t.add("generic", None)
    while len(t) < size:
        k = g.rng.random()
        if k < 0.5:
            g.card(t, root, g.rng.randrange(len(g.card_templates)))
        elif k < 0.8:
            g.section(t, root, 3, 2)
        else:
            g.comments(t, root, 1, 3)
    return t


if __name__ == "__main__":
    import collections
    import sys
    for n in (10_000, 100_000):
        t = page(n, seed=1)
        cnt = collections.Counter(ROLES[r] for r in t.role)
        print(f"n={len(t)} depth={t.depth()} max_children={max(map(len, t.children))}")
        print("  ", dict(cnt.most_common(12)))
        fam = collections.Counter(f for f in t.family if f >= 0)
        print("   families:", dict(fam))
    sys.exit(0)
