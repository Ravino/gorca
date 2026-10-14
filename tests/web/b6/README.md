# B-6 harness (web SourceLayer bake-off) and web-prep corpus

Skeleton of the harness for bake-off B-6 (ADR-0003 section 16) plus the measurements of the web-prep track.
Report: `docs/reports/2026-10-14-web-prep.md`. Results: `bench/results/web-prep/`.

## Pieces

| Path | What |
|---|---|
| `corpus.tsv` | The corpus manifest (name, kind, argument, expected title, approximate AX nodes, purpose). Loaded by `crates/gorca-bench-web/src/corpus.rs`. |
| `pages/spa-list.html` | Keyed list with vanilla DOM operations that give the same AT-SPI streams as the framework apps (prepend, reverse = DOM moves, recreate = key reset, rewrite = unkeyed in place, route swap). `id=item-N` on every row is the ground truth. |
| `pages/virtual-list.html` | Windowed list/grid, `mode=unmount|recycle|grid`, `aria-posinset/setsize/rowindex/colindex`. |
| `virt/` | Real virtual-scroll libraries (TanStack Virtual, vue-virtual-scroller, Angular CDK). `npm install && node build.mjs` writes `dist/*.html` (gitignored). |
| `driver.js` | Injected into every served page. The page long-polls the harness for commands and acknowledges them with its own wall-clock timestamps; no Marionette or CDP. |
| `run_b6.sh` | Private session (dbus, a11y bus, registryd, headless gnome-shell on a unique Wayland display) + `b6 run`. |
| `run_all.sh` | The whole suite into one output directory (`run_churn.sh`: the churn scenarios only). |
| `summarize.py` | Turns a run directory into Markdown tables (`SUMMARY.md`). |
| `../pages/gen_large.py`, `../pages/frames/` | Existing large-page generator (ported to Rust in the loader) and iframe/shadow/canvas pages. |

The harness crate is `crates/gorca-bench-web` (binary `b6`); the source under test is `crates/gorca-doc-source-atspi`.

## Running

```sh
. ./env.sh
cargo build --release -p gorca-bench-web
(cd tests/web/b6/virt && npm install && node build.mjs)      # once, for the TBD-WEB-3 library pages
target/release/b6 corpus                                       # lists entries and shows which are present
tests/web/b6/run_b6.sh --pages large10k --scenarios build --runs 10 --out bench/results/web-prep/mine
tests/web/b6/run_all.sh bench/results/web-prep/run2
```

`b6 run` refuses to start unless `XDG_RUNTIME_DIR` is not under `/run/user/` and the a11y bus address is not the live one;
`run_b6.sh` sets that up. Each (page, scenario) pair gets a fresh browser with a temporary `--profile` (Firefox:
`accessibility.force_disabled=0`, `GNOME_ACCESSIBILITY=1`, no proxy, no telemetry). Teardown signals only the process
group of the browser the harness itself started. Chromium (`--browser chromium --browser-bin ...`) runs headless=new with
`--force-renderer-accessibility` and is used for the cross-browser research scenarios.

## Scenarios and the metrics they produce

| Scenario | Metric | Output |
|---|---|---|
| `build` | ADR-0001 M-9 / ADR-0003 WM-1: full-tree build time, stage by stage (frontier, `GetTree`, roles, states, text), over a matrix of strategies (split, connections, window, state/text tiers) | `<page>.build-<browser>.json` |
| `churn` | ADR-0003 WM-4 (DOM mutation -> mirror latency, 1/100/1000 nodes and route swaps) and ADR-0001 M-13 (identity stability, wrong-node count, heuristic share); consistency of the mirror against a fresh fetch after every step; revalidate-on-read repair when it disagrees | `<page>.churn-<browser>.json` |
| `churn-nodomid` | the same with the DOM-id rung switched off (ablation of rung IL-2) | `<page>.churn-nodomid-<browser>.json` |
| `scale` | ADR-0001 M-15: decode scaling of `GetTree` replies over 1..16 threads, and fetch scaling over the number of P2P connections | `<page>.scale-<browser>.json` |
| `prewarm` | cache-domain activation is per application: warm on a tiny first page, navigate the same tab to the page under test, time its first block and full build | `<page>.prewarm-<browser>.json` |
| `research` | ADR-0003 TBD-WEB-1 (state flips and their events), TBD-WEB-2 (position attributes), TBD-WEB-3 (identity under DOM recycling) | `<page>.research-<browser>.json` |

## Arms

`crates/gorca-bench-web/src/arm.rs`: `AtspiTunedArm` (arm a, complete) and `AgpFirefoxArm` (arm b, skeleton: lists what the
bootstrap, the client crate and the aligner must provide and fails with `NotImplemented` so a B-6 run cannot silently
measure arm a twice). Both implement `B6Arm` (`build`, `attach`) against the same corpus, oracle and scenarios.
