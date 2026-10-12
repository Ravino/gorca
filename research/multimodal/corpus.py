"""Real-text a11y-like trees from the local GNOME help corpus (Mallard .page XML).

/usr/share/help/<lang>/<app>/*.page holds real UI documentation for accerciser,
gedit, gnome-terminal and orca, in English and Russian. Each page becomes a
document subtree: document > section > heading / paragraph / listitem / link /
table cell, i.e. the shape a browser exposes for a help page. Ground-truth labels:
`app` (4 classes) and `page` (~126 classes). Items are aligned across languages by
(app, page, ordinal) where the translated page has the same block structure.

Geometry is synthesised by a simple vertical block-flow layout (stated as an
assumption: real bounds come from AT-SPI Component.GetExtents).
"""
from __future__ import annotations

import glob
import os
import re
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field

NS = "{http://projectmallard.org/1.0/}"
ROLE_OF = {"title": "heading", "p": "paragraph", "item": "listitem", "link": "link",
           "td": "cell", "code": "paragraph", "screen": "paragraph", "note": "note"}
ROLES = ["document", "section", "heading", "paragraph", "listitem", "link", "cell", "note", "list", "table"]


@dataclass
class Items:
    role: list = field(default_factory=list)
    text: list = field(default_factory=list)
    depth: list = field(default_factory=list)
    parent: list = field(default_factory=list)
    app: list = field(default_factory=list)
    page: list = field(default_factory=list)
    ordinal: list = field(default_factory=list)   # index of the leaf item inside its page
    bounds: list = field(default_factory=list)    # (x, y, w, h) synthetic block flow

    def __len__(self):
        return len(self.role)


def _text(e) -> str:
    return re.sub(r"\s+", " ", "".join(e.itertext())).strip()


def load(lang: str = "C", apps=("accerciser", "gedit", "gnome-terminal", "orca")) -> Items:
    it = Items()
    pages = []
    for a in apps:
        pages += [(a, f) for f in sorted(glob.glob(f"/usr/share/help/{lang}/{a}/*.page"))]
    for pi, (app, f) in enumerate(pages):
        try:
            root = ET.parse(f).getroot()
        except ET.ParseError:
            continue
        page = f"{app}/{os.path.basename(f)}"
        y = 0.0
        ordinal = 0

        def add(role, text, depth, parent):
            nonlocal y, ordinal
            h = 18.0 if role in ("heading", "link", "cell") else 18.0 * (1 + len(text) // 90)
            it.role.append(role); it.text.append(text); it.depth.append(depth)
            it.parent.append(parent); it.app.append(app); it.page.append(page)
            it.ordinal.append(ordinal); ordinal += 1
            x = 20.0 * depth
            it.bounds.append((x, y, 900.0 - x, h)); y += h + 6
            return len(it.role) - 1

        doc = add("document", page, 0, -1)

        def walk(e, depth, parent):
            tag = e.tag.replace(NS, "")
            if tag in ("info", "comment", "media"):
                return
            if tag in ("section", "list", "terms", "table", "steps", "tree"):
                role = "section" if tag == "section" else ("table" if tag == "table" else "list")
                p = add(role, "", depth, parent)
                for c in e:
                    walk(c, depth + 1, p)
                return
            if tag in ROLE_OF:
                t = _text(e)
                if t:
                    add(ROLE_OF[tag], t[:300], depth, parent)
                return
            for c in e:
                walk(c, depth, parent)

        for c in root:
            walk(c, 1, doc)
    return it


if __name__ == "__main__":
    for lang in ("C", "ru"):
        it = load(lang)
        from collections import Counter
        print(lang, len(it), Counter(it.role).most_common(), len(set(it.page)))
