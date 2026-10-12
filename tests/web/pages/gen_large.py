#!/usr/bin/env python3
"""Generate a large, realistic-ish static page with ~N accessible nodes.

usage: gen_large.py N out.html
Each "card" yields ~12 AX nodes (article, heading, 2 paragraphs with link/strong,
list of 3 items, button, image with alt). Every 200 cards a table block and a form
block are inserted; one aria-live region and one open shadow root host are included.
The real AX node count depends on the browser; the probes report it.
"""
import sys, html

def card(i):
    return f"""<article aria-label="card {i}"><h3>Item {i} heading</h3>
<p>Paragraph one of card {i} with a <a href="#c{i}">link {i}</a> and <strong>strong text</strong>.</p>
<p>Second paragraph, plain text for card {i}.</p>
<ul><li>alpha {i}</li><li>beta {i}</li><li>gamma {i}</li></ul>
<button type="button">Action {i}</button><img alt="image {i}" src="data:image/gif;base64,R0lGODlhAQABAAAAACw="></article>
"""

def table(i):
    rows = "".join(f"<tr><th scope=row>r{r}</th><td>{i}.{r}.a</td><td>{i}.{r}.b</td></tr>" for r in range(10))
    return f"<table><caption>Table {i}</caption><thead><tr><th>h</th><th>a</th><th>b</th></tr></thead><tbody>{rows}</tbody></table>\n"

def form(i):
    return (f'<form aria-label="form {i}"><label for="t{i}">Name {i}</label><input id="t{i}" type="text">'
            f'<label><input type="checkbox"> check {i}</label><select aria-label="sel {i}"><option>one</option><option>two</option></select>'
            f'<div role="combobox" aria-expanded="false" aria-label="combo {i}" tabindex="0"></div></form>\n')

def main():
    n = int(sys.argv[1]); out = sys.argv[2]
    cards = max(1, n // 12)
    parts = ['<!doctype html><html lang="en"><head><meta charset="utf-8"><title>large %d</title></head><body>' % n,
             '<header><nav aria-label="main"><a href="#a">A</a><a href="#b">B</a></nav></header><main id="main"><h1>Large page</h1>',
             '<div id="live" role="status" aria-live="polite"></div>',
             '<div id="shadowhost"></div><script>document.getElementById("shadowhost").attachShadow({mode:"open"}).innerHTML="<h2>in shadow</h2><button>shadow button</button>";</script>']
    for i in range(cards):
        parts.append(card(i))
        if i % 200 == 199:
            parts.append(table(i)); parts.append(form(i))
    parts.append('</main><footer><p>end</p></footer>')
    # mutation helpers for update-granularity probes
    parts.append('''<script>
window.mut = {
  addCards(k){const m=document.getElementById("main");const f=document.createDocumentFragment();for(let i=0;i<k;i++){const a=document.createElement("article");a.innerHTML="<h3>new "+i+"</h3><p>added paragraph</p>";f.appendChild(a);}m.appendChild(f);},
  rename(k){const bs=document.querySelectorAll("button");for(let i=0;i<k&&i<bs.length;i++)bs[i].textContent="renamed "+i;},
  live(k){const l=document.getElementById("live");for(let i=0;i<k;i++){setTimeout(()=>{l.textContent="msg "+i;},i);}},
  removeCards(k){const a=document.querySelectorAll("article");for(let i=0;i<k&&i<a.length;i++)a[i].remove();},
  route(){document.getElementById("main").innerHTML="<h1>Route 2</h1><p>new view</p>";history.pushState({},"","#route2");document.title="route 2";}
};
</script></body></html>''')
    open(out, "w").write("\n".join(parts))
main()
