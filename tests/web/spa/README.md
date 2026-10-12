# SPA test corpus and AT-SPI characterization

Static bundles of small but realistic single-page apps, plus a driver that records the AT-SPI event stream and object-identity
behaviour of Chromium and Firefox while a *trusted* user interacts with them.
Report: `docs/reports/2026-10-12-spa-behavior.md`. Tree dumps: `research/web-graph/real-trees/`.

## Build and serve
```sh
. ./env.sh                                  # repo root; project-local node is NOT needed, system node 24 is used
cd tests/web/spa && npm install && node build.mjs   # -> dist/{react,vue,angular,svelte}/ (gitignored)
node serve.mjs 8089                         # one static server, SPA fallback per /<fw>/ prefix
```
No dev server is needed; everything is bundled with esbuild (React JSX, Vue full build with string templates, Svelte 5 via
esbuild-svelte, Angular 20 JIT with `experimentalDecorators`; Angular is zone.js based and compiled to ES2017 so zone patches async).

## Apps and scenarios (URL = `/<fw>/<path>`)
| Path | React | Vue | Angular | Svelte | Exercises |
|---|---|---|---|---|---|
| `/` (+`?fm=1`) | x | x | x | x | route change; `?fm=1` = focus management (focus h1, `role=status` announcer) |
| `/route/products`, `/route/products/N`, `/route/about` | x | x | x | x | route targets (12-card grid, detail, text) |
| `/list/keyed`, `/list/unkeyed` | x | x | x | x | 50 `li#item-N`; prepend / reverse / remove middle / edit item 10; keyed by id vs by index (or no key) |
| `/list/virtual` | react-window 1.8 | hand-rolled | hand-rolled | hand-rolled | 10k rows windowed, absolutely positioned, `role=list/listitem` + posinset/setsize, overscan 5 |
| `/infinite` | x | | | | IntersectionObserver sentinel, 20 items per load, `aria-busy`, `role=status` |
| `/modal` | x | | | | `role=dialog aria-modal`, focus trap, `inert` background, focus restore |
| `/live` | x | | | | `role=log` chat flood (100 msgs / 2 s) and `role=status` atomic ticker (30 updates / 3 s) |
| `/form` | x | | | | `aria-invalid`, `aria-describedby`, `role=alert` summary, focus first invalid |
| `/combobox` | x | | | | ARIA 1.2 combobox, `aria-activedescendant`, listbox filtering, live count |
| `/tabs` | x | | | | tablist/tab/tabpanel, roving tabindex, panel unmounted when inactive |
| `/grid` | x | | | | `role=grid`, 100x6, roving focus, sortable keyed rows, `aria-sort` |

DOM contract shared by all apps (the driver relies on it): nav links `#nav-home|#nav-products|#nav-about`, `h1#page-title`,
list buttons `#btn-prepend|#btn-reverse|#btn-remove-mid|#btn-update`, `ul#items > li#item-N`, `div#vlist > [role=listitem]#row-N`.

## Driver (`driver/`)
```sh
tests/web/spa/run_characterize.sh chrome  [--only react/keyed,vue/virtual] [--no-dumps]
tests/web/spa/run_characterize.sh firefox            # headed inside a PRIVATE headless gnome-shell (Firefox has no ATK bridge when headless); never touches the user's focus
tests/web/spa/run_characterize.sh chrome --static    # ONLY the large static pages (gen_large.py output in $STATIC_DIR, default /tmp/static)
tests/web/spa/run_characterize.sh chrome --with-static   # all SPA scenarios + the static pages
```
Each run creates a private session bus + a11y bus + registryd (`tests/web/probes/private_a11y_env.sh`), launches its own browser
(Chromium: headless=new, `--force-renderer-accessibility`, CDP `Input.*`; Firefox: headed in the private gnome-shell, Marionette actions), registers for
`object:`/`window:`/`document:` events, and for every step records raw signals (sender path, kind, detail1/2, child reference)
with monotonic timestamps, DOM mutation counts (MutationObserver), a before/after tree snapshot and identity metrics
(`analyze.py`). Output: `results/<browser>/<scenario>.json` (summary) and `.events.json.gz` (raw events).
Needs system `python3` with `gi` + Atspi typelib (same as `tests/web/probes`).

Summaries: `driver/summarize.py <browser> [pattern]` and `driver/table.py [pattern]` print the per-step tables used in the report.
Static pages: `python3 tests/web/pages/gen_large.py 2000 /tmp/static/large2000.html` (and 10000).
