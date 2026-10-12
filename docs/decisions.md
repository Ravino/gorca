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
