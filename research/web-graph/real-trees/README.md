# Real accessibility trees (AT-SPI, Chromium and Firefox)

46 dumps of the showing `document-web` subtree, taken over AT-SPI from a private browser instance by
`tests/web/spa/driver/characterize.py` (SPA corpus in `tests/web/spa`, static pages from `tests/web/pages`).
Chromium 153.0.8010.12 (Chrome for Testing from Playwright `chromium-1243`, headless=new, `--force-renderer-accessibility`) and Firefox 153.4.0 ESR
(headed inside a private headless gnome-shell). Total 44066 nodes. Nothing here comes from the user's browsers.

## Format (one JSON object per file, `.json.gz` when it would exceed ~900 KB; no file is over 1 MB)
```
{"meta": {"browser", "scenario", "url", "state": "initial"|"after-steps", "n": <nodes>, "fields": [...]},
 "nodes": [[parent, role, name_len, [states], [x,y,w,h] | null, {attrs} | null, n_children, path_tail], ...]}
```
* `nodes` is in DOCUMENT ORDER (DFS pre-order); node index = position in the array; `parent` is an index (`-1` for the document root).
* `role` is the AT-SPI role nick (`heading`, `list-item`, `table-cell`, ...). Roles differ between browsers (Firefox `section` + `xml-roles=log`
  where Chromium says `log`; Chromium has `static` leaf nodes, Firefox folds text into the parent).
* `name_len` is the length of the accessible name in code points (the name text itself is intentionally dropped).
* `states`: AT-SPI state nicks except `enabled`/`sensitive`. `bounds`: screen extents (window coordinates of the private session).
* `attrs`: only the keys `id tag xml-roles level display class name-from live container-live relevant atomic busy explicit-name`
  that the browser exposes (`id` and `tag` are exposed by BOTH browsers; Chromium's `id` can be stale after an in-place DOM id change).
* `path_tail`: last path component of the AT-SPI object path (a per-process counter in both browsers; useful only for cross-dump identity checks
  in a before/after pair, e.g. `X__initial` vs `X__final`).

```python
import json, gzip
def load(p): return json.load(gzip.open(p, 'rt') if p.endswith('.gz') else open(p))
```

## Files
| file | nodes | max depth | KiB |
|---|---|---|---|
| chrome__angular-keyed__initial.json | 172 | 5 | 19 |
| chrome__angular-virtual__initial.json | 61 | 7 | 7 |
| chrome__react-combobox__final.json | 20 | 5 | 2 |
| chrome__react-combobox__initial.json | 20 | 5 | 2 |
| chrome__react-form__initial.json | 30 | 6 | 3 |
| chrome__react-grid__final.json | 1330 | 6 | 158 |
| chrome__react-grid__initial.json | 1330 | 6 | 158 |
| chrome__react-infinite__initial.json | 99 | 7 | 10 |
| chrome__react-keyed__initial.json | 172 | 5 | 19 |
| chrome__react-live__initial.json | 21 | 5 | 3 |
| chrome__react-modal__final.json | 22 | 5 | 2 |
| chrome__react-modal__initial.json | 22 | 5 | 2 |
| chrome__react-route-focus__initial.json | 50 | 6 | 5 |
| chrome__react-tabs__initial.json | 50 | 5 | 5 |
| chrome__react-virtual__final.json | 75 | 7 | 8 |
| chrome__react-virtual__initial.json | 75 | 7 | 8 |
| chrome__static-frames__initial.json | 18 | 7 | 2 |
| chrome__static-large10000__initial.json.gz | 21203 | 5 | 200 |
| chrome__static-large2000__initial.json | 4168 | 5 | 410 |
| chrome__svelte-keyed__initial.json | 174 | 5 | 19 |
| chrome__svelte-virtual__initial.json | 63 | 7 | 7 |
| chrome__vue-keyed__initial.json | 172 | 5 | 19 |
| chrome__vue-virtual__initial.json | 61 | 7 | 7 |
| firefox__angular-keyed__initial.json | 116 | 4 | 15 |
| firefox__angular-virtual__initial.json | 54 | 5 | 7 |
| firefox__react-combobox__final.json | 14 | 4 | 2 |
| firefox__react-combobox__initial.json | 14 | 4 | 2 |
| firefox__react-form__initial.json | 22 | 5 | 3 |
| firefox__react-grid__final.json | 725 | 6 | 120 |
| firefox__react-grid__initial.json | 725 | 6 | 120 |
| firefox__react-infinite__initial.json | 54 | 6 | 7 |
| firefox__react-keyed__initial.json | 117 | 5 | 15 |
| firefox__react-live__initial.json | 15 | 4 | 2 |
| firefox__react-modal__final.json | 15 | 4 | 2 |
| firefox__react-modal__initial.json | 15 | 4 | 2 |
| firefox__react-route-focus__initial.json | 34 | 5 | 4 |
| firefox__react-tabs__initial.json | 24 | 4 | 3 |
| firefox__react-virtual__final.json | 55 | 6 | 8 |
| firefox__react-virtual__initial.json | 55 | 6 | 7 |
| firefox__static-frames__initial.json | 22 | 7 | 3 |
| firefox__static-large10000__initial.json.gz | 10233 | 5 | 99 |
| firefox__static-large2000__initial.json | 2005 | 4 | 249 |
| firefox__svelte-keyed__initial.json | 117 | 5 | 15 |
| firefox__svelte-virtual__initial.json | 55 | 6 | 8 |
| firefox__vue-keyed__initial.json | 117 | 5 | 15 |
| firefox__vue-virtual__initial.json | 55 | 6 | 8 |

Regenerate: `tests/web/spa/run_characterize.sh chrome --with-static` and `... firefox --with-static` (see tests/web/spa/README.md).
