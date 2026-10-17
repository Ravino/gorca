# Reports

Working reports behind the decisions in `../decisions.md`: designs, reviews, measurements and
verification runs. They are a record of how the project got here, not user documentation. A report is
a snapshot of the day it was written (dates are in the file name or the first lines); the code and the
ADRs in `../adr/` are authoritative when they differ.

Measurements name their conditions (host load, CPU governor, repeat count) in the report. Latency
numbers taken on a loaded host are labelled as such and are not gate results; see
`../decisions.md` (2026-10-16, latency gates under load).

Host paths and user names in logs were replaced with `<repo>` and `user` before publication.

Raw benchmark output is in `../../bench/results/` (see its README).

## Index

| File | Title |
|---|---|
| [2026-10-12-architect-prior-art.md](2026-10-12-architect-prior-art.md) | Prior art for gorca: Orca failure catalog, Odilia lessons, Newton analysis |
| [2026-10-12-b2-bakeoff-judgement.md](2026-10-12-b2-bakeoff-judgement.md) | B-2 bake-off judgement: AT-SPI ingress drain + coalescer |
| [2026-10-12-b2-fixes-review.md](2026-10-12-b2-fixes-review.md) | B-2 blocking fixes review (branch `worktree-agent-a3d5ba691ea04e152`, head 9d4f641) |
| [2026-10-12-linux-platform-foundation.md](2026-10-12-linux-platform-foundation.md) | Linux platform foundation research (2026-10-12) |
| [2026-10-12-math-web-graph-model.md](2026-10-12-math-web-graph-model.md) | Web document as a typed graph, and where N-dimensional embeddings help |
| [2026-10-12-multimodal-vectors-som.md](2026-10-12-multimodal-vectors-som.md) | Multimodal node vectors, vector search and Kohonen maps for gorca |
| [2026-10-12-rust-engineer-speech-stack.md](2026-10-12-rust-engineer-speech-stack.md) | Speech stack (Phase 1): notes and measurements |
| [2026-10-12-screen-capture-options.md](2026-10-12-screen-capture-options.md) | Screen-pixel acquisition for gorca: options (2026-10-12, linux-platform-engineer) |
| [2026-10-12-spa-behavior.md](2026-10-12-spa-behavior.md) | SPA behaviour over AT-SPI: Chromium 153 and Firefox 153 ESR |
| [2026-10-12-voicego-audit.md](2026-10-12-voicego-audit.md) | voicego audit for gorca (v2.0.4, commit 3a0f1f2, AGPL-3.0) |
| [2026-10-12-voicego-p0-fixes.md](2026-10-12-voicego-p0-fixes.md) | voicego P0 fixes (branch fix/p0-stability) |
| [2026-10-12-web-a11y-browsers.md](2026-10-12-web-a11y-browsers.md) | Web accessibility in Firefox and Chromium on Linux, and requirements for gorca's virtual document model |
| [2026-10-12-web-source-layer.md](2026-10-12-web-source-layer.md) | Web document source layer: AT-SPI (tuned) vs own protocol (AGP) for Firefox and Chromium |
| [2026-10-13-b1-prework-review.md](2026-10-13-b1-prework-review.md) | B-1 shared prework review (PW-1…PW-12 without PW-6) |
| [2026-10-13-e2e-lost-main.md](2026-10-13-e2e-lost-main.md) | e2e `lost` gate failures on main (a8686d0): noise plus an accounting bug in the harness |
| [2026-10-13-m1-platform-measurements.md](2026-10-13-m1-platform-measurements.md) | M1 platform measurements: P-7, P-8, P-10, P-13, P-14, P-15, XKB CapsLock |
| [2026-10-13-m1-review.md](2026-10-13-m1-review.md) | M1 review: command layer (branch `worktree-agent-a4851ad62a46d65fe`, head 522bdf7) |
| [2026-10-13-mb1-mirror-store.md](2026-10-13-mb1-mirror-store.md) | MB-1: mirror store microbenchmark (input to MS-3) |
| [2026-10-13-p12-realtime-scheduling.md](2026-10-13-p12-realtime-scheduling.md) | P-12: real-time scheduling for gorca hot threads (host: Debian 13, 7.1.8+deb13-rt-amd64) |
| [2026-10-13-p9-cache-p2p.md](2026-10-13-p9-cache-p2p.md) | P-9 (Cache AddAccessible/RemoveAccessible/GetItems consistency) and P2P reachability (ADR-0001 D-7) |
| [2026-10-13-phase1-slice-review.md](2026-10-13-phase1-slice-review.md) | Phase 1 vertical slice: adversarial review (review-r1) |
| [2026-10-13-rust-engineer-phase1-slice.md](2026-10-13-rust-engineer-phase1-slice.md) | Phase 1 vertical slice: notes |
| [2026-10-14-adr0006-platform-checks.md](2026-10-14-adr0006-platform-checks.md) | ADR-0006 platform checks V-1 .. V-5 (2026-10-14, linux-platform-engineer) |
| [2026-10-14-b1-harness-gaps.md](2026-10-14-b1-harness-gaps.md) | B-1 harness gaps: what was wrong, what is fixed, what needs a decision |
| [2026-10-14-b1-judgement.md](2026-10-14-b1-judgement.md) | B-1 judgement: native mirror fill strategy (arm A lazy vs arm C hybrid) |
| [2026-10-14-gorca-doc-core.md](2026-10-14-gorca-doc-core.md) | gorca-doc web-prep core: arena, treap index, flat text, left-right publication |
| [2026-10-14-m4a-review.md](2026-10-14-m4a-review.md) | M4a review (review mode: object navigation and flat review) |
| [2026-10-14-web-prep.md](2026-10-14-web-prep.md) | Web prep: AtspiDocSource, the B-6 harness skeleton, TBD-WEB-1/2/3 |
| [2026-10-16-perf-engineer-k2-loss-under-load.md](2026-10-16-perf-engineer-k2-loss-under-load.md) | K2/K2s/R1 "lost" under host load: attribution |
| [addon-platform-architecture.md](addon-platform-architecture.md) | Add-on platform architecture: gap analysis and design |
| [addon-platform-math.md](addon-platform-math.md) | Add-on platform math: composition, latency budgets, versioning, store |
| [addon-platform-security.md](addon-platform-security.md) | Add-on platform: security and stability review |
| [alt-tab-unknown.md](alt-tab-unknown.md) | Alt+Tab and Ctrl+Alt+Tab speak "unknown" (2026-10-16) |
| [b1-adopt-rereview.md](b1-adopt-rereview.md) | Re-review (round 2): B-1 adoption |
| [b1-adopt-review.md](b1-adopt-review.md) | Review: B-1 adoption (focus-window prefetch as default fill policy) |
| [b1-adopt.md](b1-adopt.md) | B-1 adoption: the focus-window prefetch becomes the default fill policy |
| [b5-judgement.md](b5-judgement.md) | B-5 runtime topology: judgement |
| [b5-pilot.md](b5-pilot.md) | B-5 runtime topology: pilot (NON-AUTHORITATIVE) |
| [b5-verdict.md](b5-verdict.md) | B-5 runtime topology: reviewer verdict |
| [capital-indication-verify.md](capital-indication-verify.md) | Capital indication: adversarial review |
| [capital-indication.md](capital-indication.md) | Capital indication: why the pitch was silent, and the independent options |
| [ext-gnome-shell-wasm-data/](ext-gnome-shell-wasm-data/) | Raw data and logs for the report of the same name. |
| [ext-gnome-shell-wasm.md](ext-gnome-shell-wasm.md) | ext-gnome-shell as a WASM component (M5 follow-up) |
| [ext-vte-wasm-data/](ext-vte-wasm-data/) | Raw data and logs for the report of the same name. |
| [ext-vte-wasm.md](ext-vte-wasm.md) | ext-vte as a WASM component (M6 follow-up) |
| [f0-quick-wins.md](f0-quick-wins.md) | F0 quick wins |
| [ff-tabs-urlbar-verify.md](ff-tabs-urlbar-verify.md) | Review: Firefox tab-switch title and address bar (760e460e), 2026-10-17 |
| [ff-tabs-urlbar.md](ff-tabs-urlbar.md) | Firefox tab switch and address bar (2026-10-17) |
| [licence-provenance.md](licence-provenance.md) | Licence provenance audit (pre-publication) |
| [live-keys-swallowed.md](live-keys-swallowed.md) | Live bug: ordinary keys swallowed, gorca speaks the key names (2026-10-14) |
| [m12a-catalogue.md](m12a-catalogue.md) | M12a: message catalogue and localisation sweep |
| [m12a-review.md](m12a-review.md) | M12a review: localisation sweep (message catalogue) |
| [m12a-verify.md](m12a-verify.md) | M12a fix round: verification review |
| [m3-e2-flake.md](m3-e2-flake.md) | M3 E2 flake (`e2_missing`), investigation 2026-10-14 |
| [m3-review.md](m3-review.md) | M3 review: native caret, typing echo, selection, say-all |
| [m3b-m7b-impl.md](m3b-m7b-impl.md) | M3b and M7b implementation report: Firefox fields, browse say-all, layout-aware quick navigation |
| [m3b-m7b-rereview.md](m3b-m7b-rereview.md) | M3b + M7b re-review (round 2) |
| [m3b-m7b-review.md](m3b-m7b-review.md) | M3b + M7b review (Firefox caret/typing echo, browse say-all, layout-aware quick nav) |
| [m4b-followups.md](m4b-followups.md) | M4b follow-ups (N-1 to N-6) |
| [m4b-impl.md](m4b-impl.md) | M4b implementation report: GNOME Shell and desktop polish |
| [m4b-rereview-probes/](m4b-rereview-probes/) | Raw data and logs for the report of the same name. |
| [m4b-rereview.md](m4b-rereview.md) | M4b review, round 2 (fix round) |
| [m4b-review-probes/](m4b-review-probes/) | Raw data and logs for the report of the same name. |
| [m4b-review.md](m4b-review.md) | M4b review, round 1 (GNOME Shell and desktop polish) |
| [m4b-shell-probe-logs/](m4b-shell-probe-logs/) | Raw data and logs for the report of the same name. |
| [m4b-shell-probe.md](m4b-shell-probe.md) | M4b ground truth: what GNOME Shell 48.7 emits over AT-SPI (private headless session) |
| [m5-impl.md](m5-impl.md) | M5 extension host: implementation report |
| [m5-rereview.md](m5-rereview.md) | M5 extension host: adversarial review, round 2 |
| [m5-review.md](m5-review.md) | M5 extension host: adversarial review (r1) |
| [m6-focus-fix-review.md](m6-focus-fix-review.md) | M6 focus fix review (78fa165 / docs 5c99628) |
| [m6-focus-fix.md](m6-focus-fix.md) | M6 focus fix: terminal output after a focus change, before any key |
| [m6-impl.md](m6-impl.md) | M6 implementation report: GNOME Terminal (VTE) |
| [m6-review.md](m6-review.md) | M6 review (GNOME Terminal / VTE), round 1 |
| [m7a-impl.md](m7a-impl.md) | M7a implementation report: browse mode for static pages in Firefox |
| [m7a-rereview.md](m7a-rereview.md) | M7a re-review: Firefox browse mode, round 2 |
| [m7a-review.md](m7a-review.md) | M7a review: Firefox browse mode for static pages |
| [m8-baseline.md](m8-baseline.md) | M8 baseline: NVDA browse-mode parity scenarios against `main` |
| [m8-gaps.md](m8-gaps.md) | M8 gap table: where each baseline root cause stands |
| [m8-impl.md](m8-impl.md) | M8 implementation report: Firefox browse mode at NVDA parity |
| [m8-rereview-probes.patch](m8-rereview-probes.patch) | Probe patch used by the review of the same name. |
| [m8-rereview.md](m8-rereview.md) | M8 review, round 2 (head 14a82da) |
| [m8-review-probes.patch](m8-review-probes.patch) | Probe patch used by the review of the same name. |
| [m8-review.md](m8-review.md) | M8 review, round 1 (Firefox browse mode at NVDA parity) |
| [m8b-impl.md](m8b-impl.md) | M8b: ARIA names, roles, states and descriptions at NVDA parity |
| [m8b-review.md](m8b-review.md) | M8b review: ARIA names, roles, states, descriptions (NVDA parity) |
| [m9-baseline.md](m9-baseline.md) | M9 baseline: Chromium / Chrome against `main` (M8 prep, before any M9 code) |
| [m9-fixround-verify.md](m9-fixround-verify.md) | M9 fix round verification (18df6725..e5becadb) |
| [m9-gate-triage.md](m9-gate-triage.md) | M9 review-fix e2e gate triage (m9r .. m9r4) |
| [m9-impl.md](m9-impl.md) | M9 implementation report: Chromium and Chrome browse mode |
| [m9-review.md](m9-review.md) | M9 review (Chromium/Chrome), round 1 |
| [public-push-prep.md](public-push-prep.md) | Public push preparation (2026-10-17) |
| [punctuation-symbols.md](punctuation-symbols.md) | Punctuation and symbol speech |
| [reader-state-proptest.md](reader-state-proptest.md) | Model-based tests of the Reader's mode and state transitions (2026-10-17) |
| [resource-usage.md](resource-usage.md) | gorca whole-daemon CPU and memory under event storms |
| [review-breaks-echo.md](review-breaks-echo.md) | Review mode left the typing echo broken (the "degraded state") |
| [selection-mapping-math.md](selection-mapping-math.md) | Browse-mode selection mapping: formal model, algorithms, copy spec (2026-10-17) |
| [selection-modes-architecture.md](selection-modes-architecture.md) | Browse-mode selection and copy: two models, one Ctrl+C (2026-10-17) |
| [selection-modes-risks.md](selection-modes-risks.md) | Selection modes (gorca+Shift+F10): adversarial risk review |
| [speech-lane-priority.md](speech-lane-priority.md) | Speech lane: priority-aware slot overwrite |
| [terminal-degraded-state.md](terminal-degraded-state.md) | Degraded terminal / page state: key names spoken, output silent (2026-10-17) |
| [terminal-history-recall.md](terminal-history-recall.md) | Terminal history recall (Up/Down) sometimes silent |
| [terminal-live-review-math.md](terminal-live-review-math.md) | Terminal live review: the math (anchoring, incremental diff, flood speech) |
| [terminal-live-review.md](terminal-live-review.md) | Reviewing a live terminal (flat review while output streams) |
| [terminal-review-anchor-fix.md](terminal-review-anchor-fix.md) | Terminal review anchor fix (ADR-0018 Option A) |
| [terminal-review-anchor-verify.md](terminal-review-anchor-verify.md) | Terminal review anchor fix: adversarial review |
| [terminal-review-stale.md](terminal-review-stale.md) | Terminal flat review reads a stale screen |
| [terminal-stuck-states.md](terminal-stuck-states.md) | Terminal stuck states (found by the state model) |
| [test-hang-fix.md](test-hang-fix.md) | Why `tabbing_through_a_form_is_heard` hung the test binary, and the fail-fast pattern |
| [test-load-robustness.md](test-load-robustness.md) | Load-robust unit and integration tests |
| [wasm-hook-overhead.md](wasm-hook-overhead.md) | wasmtime component-model hook overhead for web quirks (ADR-0008 evidence) |
| [word-nav-fix.md](word-nav-fix.md) | Word navigation fix (Ctrl+Right at word ends) |
