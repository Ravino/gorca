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
| 2026-10-12 | ~~Secret filtering deferred~~ — reverted the same day: it was a misunderstanding (reading vs. embedding). Final: speech/braille/navigation output is NEVER filtered (gorca reads passwords, tokens, terminal). The perception layer never embeds or persists secrets; terminal content encoding is opt-in (default off); the on-disk index is encrypted | ADR-0002 |
| 2026-10-12 | Enable gorca-helper extension now, without logout or reboot | user |
| 2026-10-12 | B-2 spec and shared harness done (no candidates yet): drain/ingress API fixed, executable coalescing model, mock/dbus-daemon/dbus-broker fixtures | docs/bakeoff/b2-ingress/SPEC.md |
| 2026-10-12 | B-2 reference ingress extended with hard bounds: per-app hard cap (rule H), global SlotBudget, tombstone compaction, announcement text clip 4 KiB, route-cache cap, exact counter invariants | docs/bakeoff/b2-ingress/SPEC.md section 3.6 |
| 2026-10-12 | voicego fast-fail open timeout (1.5 s), id-carrying errors on binary framing only, abandoned-open cap — merged and pushed (d049868). Follow-ups: SSIP async error as 7xx event (P1); flaky TestSynthesizeCtxCancel | docs/reports/2026-10-12-voicego-p0-fixes.md |
| 2026-10-12 | gorca-helper Shell extension enabled live (Looking Glass load + gsettings), ACTIVE; foreign callers verified denied | docs/reports/2026-10-12-screen-capture-options.md |
| 2026-10-12 | ADR-0003: background tabs build their mirrors in the background and are pre-warmed while the user walks the tab strip (confirmed) | ADR-0003 §6.6 |
| 2026-10-12 | ADR-0003: the active tab's mirror stays awake when focus leaves the browser for another app (confirmed) | ADR-0003 §6.6 |
| 2026-10-12 | B-2 bake-off: candidate A (raw-socket drain + lock-free ingress ring) wins (80 vs C 77, B 61) and is merged; blocking fixes: flat per-app reserve + preemption at any size, reaper-thread deallocation, ring records charged to SlotBudget, explicit budget wiring (no singleton) | docs/reports/2026-10-12-b2-bakeoff-judgement.md, ADR-0004 |
| 2026-10-12 | B-2: rejected B's sharded drain and `max_event_connections=4` default; harness defects (B-5 RSS, B-3 vs AU-3, unbudgeted rig, debug-flaky audit_latency, T4 feeder) to be fixed before re-measuring | docs/reports/2026-10-12-b2-bakeoff-judgement.md |
| 2026-10-12 | ADR-0004 accepted; per-app reserve R = 32 slots (IngressConfig default), global bound = budget + apps × (R+1); AU-3 restated (≤R never affected, above R only preempted with dirty-all); AU-8 added (counters equal real sizes) | ADR-0004, ADR-0001 §6.3 |
| 2026-10-12 | B-2 SPEC T4: cpu_ms_per_10k is gated at 100k events/s; the 10k/s figure becomes info-only (per-message wakeup cost dominates and varies 5x with core placement) | docs/reports/2026-10-12-b2-fixes-review.md |
| 2026-10-12 | B-2 SPEC T7: push4 measured as >=7 interleaved rounds, gated on the median; authoritative runs need load <1, performance governor, pinned cores (single-run p99 is host noise: 1.4-5.4 us on the same binary) | docs/reports/2026-10-12-b2-fixes-review.md |
| 2026-10-12 | B-2 closed: fixes merged after 4 review rounds (MERGE-WITH-FOLLOWUPS); follow-ups: authoritative T7 run on a quiet host, flaky realbus broker_fixture_reproduces_f3, inline free beyond MAX_PARKED=4 can exceed AU-6 when the reaper is backed up | docs/reports/2026-10-12-b2-fixes-review.md, ADR-0004 |
| 2026-10-13 | Phase 1 e2e latency gates: L1/L3/L4/K1 p99 <= 15 ms ceiling, L2 (stale-speech cancel) p99 <= 5 ms, lost = 0, median-of-interleaved-repeats gating; target to tighten to L1 <= 5 ms, L2 <= 2 ms once the daemon lands | bench/e2e/README.md |
| 2026-10-13 | Password fields: gorca fetches and speaks whatever text the app exposes for password fields, like any other field (user requirement: read everything); never logged | docs/reports/2026-10-13-phase1-slice-review.md |
| 2026-10-13 | Threat model for connector caps: goal is that a buggy or flooding app degrades only itself; a deliberately malicious same-uid process (e.g. 512 processes in separate pgids) can delay new apps via backoff and that is accepted, since same-uid code can already ptrace/kill the reader. Focused app and GNOME Shell are never evicted; protection fails closed, reading fails open | docs/reports/2026-10-13-phase1-slice-review.md |
| 2026-10-13 | Phase 1 vertical slice merged (MERGE-WITH-FOLLOWUPS after 4 review rounds): e2e p99 L1 2.0 ms, L2 0.9 ms, L3-50k 3.1 ms, lost=0; follow-ups R4-1 freeze eviction while protection is unverifiable, R4-2 group apps by systemd scope cgroup not pgid, R4-3 async /proc reads | docs/reports/2026-10-13-phase1-slice-review.md |
| 2026-10-13 | Accepted under the connector threat model: a same-uid process can move itself into another app's systemd scope (cgroup identity spoofing) or stall the bus to freeze eviction (fail-closed DoS); both only delay or mis-group apps, never mute reading or evict the focused app/Shell | docs/reports/2026-10-13-phase1-slice-review.md |
| 2026-10-13 | GNOME Shell Alt+Tab fix: Shell's switcher only toggles `state-changed:selected` (St `check_pseudo_class`, no Selection iface, no focus), so the connector flags `selected(1)` as `PropertiesChanged{SELECTED_NOW}` and a built-in app profile (`AppProfile`, stand-in for an extension hook) makes the Reader speak the newest selected item like a focus move, without touching focus protection; key echo follows Orca defaults (no echo under Alt/Ctrl/Super, no navigation keys, action keys on) | docs/reports/2026-10-13-rust-engineer-phase1-slice.md |
| 2026-10-13 | gorca modifier: Insert and CapsLock both (CapsLock disabled if it switches layout) | docs/plan/phase-2.md |
| 2026-10-13 | Keymap: NVDA-style default, Orca preset also shipped; desktop (numpad review) layout primary, laptop layout as alternative | docs/plan/phase-2.md |
| 2026-10-13 | Braille (M10) deferred out of Phase 2 | docs/plan/phase-2.md |
| 2026-10-13 | Phase 2 defaults taken by orchestrator: TOML settings (no GUI, no Orca import), typing echo chars on/words off, full-screen terminal apps best-effort, perception features out of Phase 2 | docs/plan/phase-2.md |
