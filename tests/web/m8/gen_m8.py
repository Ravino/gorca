#!/usr/bin/env python3
"""Generate the large M8 pages next to the static ones (not committed: see .gitignore).

usage: gen_m8.py OUT_DIR

  large-table.html   1000 rows x 12 columns, row and column headers, caption (table navigation
                     at size: Ctrl+Alt+arrows must stay O(1); the cell grid is 12k cells)
  large-100k.html    about 100,000 accessible nodes with headings every 50 nodes, landmarks and
                     links (W1 at 100k nodes, ADR-0003 14; quick nav must answer from the index)
  infinite.html      200 sections with a heading and two paragraphs each (long-document reading)
"""
import os
import sys


def page(title: str, body: str) -> str:
    return f'<!doctype html>\n<html lang="en"><head><meta charset="utf-8"><title>{title}</title></head><body>\n{body}\n</body></html>\n'


def large_table() -> str:
    cols = 12
    head = '<th scope="col">Row</th>' + "".join(f'<th scope="col">Col{c}</th>' for c in range(1, cols))
    rows = "\n".join(
        f'<tr><th scope="row">Row{r}</th>' + "".join(f"<td>r{r}c{c}</td>" for c in range(1, cols)) + "</tr>"
        for r in range(1000)
    )
    return page(
        "M8 Large table",
        "<h1>Large table</h1>\n"
        f'<table><caption>Big grid</caption><thead><tr>{head}</tr></thead><tbody>\n{rows}\n</tbody></table>\n'
        "<p>After the large table.</p>",
    )


def large_100k(nodes: int = 100_000) -> str:
    # ~11 nodes per unit: article, h3, 2 p (+ link, strong), ul with 3 li, button, img
    parts = ['<header><h1>One hundred thousand nodes</h1><nav aria-label="Top"><a href="#u0">Start</a></nav></header><main>']
    i = 0
    n = 0
    while n < nodes:
        parts.append(
            f'<article aria-label="unit {i}"><h3>Unit {i} heading</h3>'
            f'<p>Paragraph of unit {i} with <a href="#u{i}">link {i}</a> and <strong>bold</strong>.</p><p>Second paragraph {i}.</p>'
            f"<ul><li>a{i}</li><li>b{i}</li><li>c{i}</li></ul>"
            f'<button type="button">Do {i}</button><img alt="img {i}" src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7"></article>'
        )
        n += 14
        i += 1
        if i % 500 == 0:
            parts.append(f'<h2>Chapter {i // 500}</h2>')
            n += 1
    parts.append("</main><footer><p>End of the 100k page.</p></footer>")
    return page("M8 Large 100k", "\n".join(parts))


def infinite() -> str:
    parts = ["<h1>Long document</h1>"]
    for i in range(200):
        parts.append(f"<h2>Section {i}</h2><p>Section {i} first paragraph, with some words to read aloud.</p><p>Section {i} second paragraph.</p>")
    return page("M8 Long document", "\n".join(parts))


def main() -> None:
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for name, text in (("large-table.html", large_table()), ("large-100k.html", large_100k()), ("infinite.html", infinite())):
        with open(os.path.join(out, name), "w", encoding="utf-8") as f:
            f.write(text)


if __name__ == "__main__":
    main()
