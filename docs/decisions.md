# Decision log

| Date | Decision | Reference |
|---|---|---|
| 2026-10-12 | Core language: Rust (tokio, zbus, atspi); Go/Python/C for tooling | chat; ADR-0001 pending |
| 2026-10-12 | Bake-offs only for key architectural decisions | CLAUDE.md |
| 2026-10-12 | Scope: GNOME only; polish order Shell → Terminal → Firefox → Chromium; KDE/LibreOffice later | CLAUDE.md |
| 2026-10-12 | Core is a backend-agnostic API; AT-SPI2 and Newton are pluggable connectors that can run together | CLAUDE.md; ADR-0001 pending |
| 2026-10-12 | App-specific behavior lives in modular extensions, not in the core | CLAUDE.md |
| 2026-10-12 | R1: gorca may impersonate Orca (bus name + process name); Chrome via `--force-renderer-accessibility`; upstream fixes later | ADR-0001 |
| 2026-10-12 | R2: message-based connector boundary, mandatory deadline per request | ADR-0001 |
| 2026-10-12 | R3: non-blocking Reader actor, epoch-tagged result messages | ADR-0001 |
| 2026-10-12 | R4 (amended by user): speech is a separate service — voicego primary (native protocol), speech-dispatcher/SSIP compat; no in-process synthesis; PipeWire tuning deferred | ADR-0001 |
| 2026-10-12 | R5: no Newton runtime dependency; connector API designed for it; Newton connector deferred | ADR-0001 |
| 2026-10-12 | R6: per-document mirror for web, viewport-first build; data source layer (AT-SPI vs own gorca protocol) to be researched/benchmarked | ADR-0001 |
| 2026-10-12 | Performance philosophy: speed over memory/CPU frugality, fully multi-threaded | CLAUDE.md |
| 2026-10-12 | Test harness: private dbus-daemon session + real at-spi-bus-launcher (forced dbus-daemon) + registryd; fake apps speak AT-SPI directly via python3-dbus (no toolkit); system python3 only | tests/harness/README.md |
| 2026-10-12 | W: `DocumentSource` abstraction; tuned AT-SPI is the baseline; AGP-Firefox opt-in accelerator in hybrid mode, decided by bake-off B-6; no CDP. SPA frameworks (React/Vue/Angular/Svelte) are the primary web test target | docs/reports/2026-10-12-web-source-layer.md |
| 2026-10-12 | G: typed graph + treap over document order for navigation indexes | docs/reports/2026-10-12-math-web-graph-model.md |
| 2026-10-12 | V: fix voicego upstream (P0 first); automatic fallback to speech-dispatcher when voicego is unavailable | docs/reports/2026-10-12-voicego-audit.md |
| 2026-10-12 | A1: gorca never kills or replaces Orca; the user switches manually. Super+Alt+G toggles gorca (like Super+Alt+S for Orca) | ADR-0001 §17.2 |
| 2026-10-12 | A2: minimal key grab sets; any non-modifier key press silences speech | ADR-0001 §17.2 |
| 2026-10-12 | A5: memory is not a constraint (600 MB guard rail, no unbounded growth) | ADR-0001 |
| 2026-10-12 | Patching/rebuilding at-spi2-core is allowed when it buys speed | user |
| 2026-10-12 | Dev toggle = user unit `contrib/systemd/gorca.service` (not enabled at login) + `tools/gorca-toggle.sh` (flock, spd-say/espeak-ng feedback) bound to Super+Alt+G via media-keys custom1; `tools/install-dev.sh [--uninstall]` | ADR-0001 §17.2 |
| 2026-10-12 | SPA corpus + AT-SPI characterization: frameworks look identical over AT-SPI; mirror needs parent revalidation (Chromium silent reorder), remove/add re-matching (Firefox recreates moved nodes), identity ladder path > DOM id (stale in Chromium) > (role,name,tag); AGP not required by SPA data. Firefox tests run in a private headless gnome-shell | docs/reports/2026-10-12-spa-behavior.md |
| 2026-10-12 | voicego P0 fixes (STOP/SPEAK race, panic containment, sink open retry, sink reopen, volume -100) made on branch fix/p0-stability of refs/voicego; not merged or pushed, pending reviewer | docs/reports/2026-10-12-voicego-p0-fixes.md |
| 2026-10-12 | voicego P0 stability fixes merged to main and pushed (7cf7a1c) after 3 review rounds | docs/reports/2026-10-12-voicego-p0-fixes.md |
| 2026-10-12 | Multimodal: core has feature store (content-keyed, privacy filter) + exact VectorIndex + MapSurface (SOM behind a flag); encoders in a native no-network sidecar `gorca-perceive`; first feature = semantic find, then unlabeled-button naming cascade | docs/reports/2026-10-12-multimodal-vectors-som.md; ADR-0002 pending |
| 2026-10-12 | License: deferred (Cargo `license = "TBD"`) | user |
| 2026-10-12 | Super+Alt+G toggle verified by the user on a live keypress | user |
| 2026-10-12 | Web node identity: variant C — AT-SPI path → validated DOM id → (role, name, tag)+sibling order, plus GetChildren revalidation of cursor's parent on read | docs/reports/2026-10-12-spa-behavior.md; ADR-0003 pending |
| 2026-10-12 | ADR-0002 approved with amendment: terminal content encoding is user-toggleable (default off); obvious secrets always filtered | ADR-0002 |
| 2026-10-12 | ADR-0003 approved with amendment: one mirror per tab with no fixed cap (N mirrors = M tabs); only the active tab's mirror is awake, all others dormant | ADR-0003 |
| 2026-10-12 | voicego: cut the sink open timeout to 1–2 s so gorca can fall back to speech-dispatcher quickly | voicego |
| 2026-10-12 | Secret filtering deferred: no secret/terminal filtering for now (the user works with secrets and must read them); revisit later. Speech output is never filtered | user |
| 2026-10-12 | Enable gorca-helper extension now, without logout or reboot | user |
