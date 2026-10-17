# M9 specification (final): Chromium and Chrome at the same browse-mode level as Firefox

Status: final after the implementation of 2026-10-17 (web-a11y-engineer; report `docs/reports/m9-impl.md`). Section 6 is the as-built state; sections 1 to 5 are the original plan. Facts and numbers: `docs/reports/m9-baseline.md`. Inputs: `docs/plan/phase-2.md` M9,
ADR-0003 (WF-4: AT-SPI only, no CDP), ADR-0008 (core engine, connector quirk rows as data, Chromium product rules in an extension), `docs/plan/m8-spec.md`
(the parity matrix and the 318-step corpus are the acceptance corpus for M9 too).

Nothing in this document is implemented in `gorca-core` (doc actor, browse state machine), `gorca-doc` or `gorca-doc-source-atspi`; those belong to the M8 work in flight. The
quirk rows below are already in `crates/gorca-connector-atspi/src/toolkit.rs` (data only, unit-tested, no consumer yet except `text_insert_slack`).

## 1. Goal and acceptance

Same gates as M7 and M8, run with `GORCA_M7_BROWSER=chromium` (`tests/web/m9/run_m9.sh`, `chain.sh`), with Chromium's tree arrival excluded from the latency budgets (ADR-0003 14, WF-4):

| Gate | Threshold |
|---|---|
| M8 corpus on Chromium | every step PASS, GAP or XFAIL with the same mark as Firefox; **0 unmarked FAIL**; Chromium-only differences carry an `xf` reason that names a Chromium bug with a filed reproduction (phase-2 M9 acceptance) |
| M7a set (B0 W1 BR1 BR2 W2 W2b W3 FM1 FM2) | same gates as Firefox; W2b gorca-side p99 at most 150 ms |
| Typing set (FC1h FC1 FE1 FCE2 FE2 FS2 FSA1 FSA3 FCE1 FT1 FP1 BSA1 BAR1 BL1 FR1) | same gates as Firefox, all failures 0; FC1 p99 at most 15 ms (Chromium needs a LINE round trip per key: 8 ms today) |
| No duplicate speech | a new step in B0: heading, link and label are said once (`not` token for the repeated word) |
| Wake | the real daemon reads a Chromium started *before* it, with only `IsEnabled` set: scenario `CR-WAKE` (below) |
| Browser coverage | Chrome for Testing (latest) and Debian `chromium`; Chrome stable and Electron are manual checklist items |

## 2. Work, in the order to do it

P0 = blocks every Chromium page; P1 = visible in daily use; P2 = polish. "Row" = data in the connector quirk table; "core" = generic web engine (ADR-0008 tier 1); "ext" = `ext-chromium` (tier 3).

| # | Pri | Item | Where | Evidence | Done when |
|---|---|---|---|---|---|
| 1 | P0 | **Wake call**: on first sight of an application with `Toolkit::needs_wake_call()`, send `GetAttributes` and `GetRelationSet` on its root, deadline-bounded, once; then wait for `children-changed` on the window frame / `document:load-complete` and attach the document | connector discovery + row `blink-wake-call` (done) | `bench/results/m9/probe-at-isenabled.json`, `m9-trial`: no document in 60 s with the real daemon | `CR-WAKE` passes with `M9_AT=isenabled` |
| 2 | P0 | **Static leaves**: a container whose `Text` is non-empty does not get its `static` children as mirror children (they repeat the runs). Read the row `text_leaf_children_repeat_parent_text()`, not `silent_reorders`. Remove the orphaned `static` nodes from the mirror and keep their ids out of the identity map | `gorca-doc-source-atspi` `runs.rs` + `source.rs`; prototype patch `bench/results/m9/prototype-runlayer-drop-static-leaves.patch` | B0 words doubled; "It has a, Paragraph 1 starts here."; with the patch B0, BSA1, BAR1 text, RD.sa.stop, FT1 correct | a `RunLayer` unit test with the Blink shape `[static, link, static]` vs one U+FFFC; B0 no-duplicates step |
| 3 | P0 | Typing echo: `text_insert_slack` 2 (done, data) | row (done) | FE2 doubles 29 -> 1 -> 0 | FE2 `e2_double` 0 in 3 repeats at low load |
| 4 | P1 | Tree arrival: do not treat the first block or the quiet point as complete; build on `Document.LoadComplete`; keep the progressive first-block build for 100k-card pages (27 s to the document in the worst case); show "loading" feedback if the document is not there after 2 s | core (web actor) + connector | section 6 of the baseline: 100k page, `poke`: 27 s / 193k nodes, `force`: 1.6 s / 26k nodes | probe-load numbers stay in the report; a "still loading" utterance gate |
| 5 | P1 | In-page anchors: produce `CaretOrigin::ScrollToAnchor` from `text-caret-moved(0)` on a document and move the virtual cursor to the new sequential-focus start point | connector (event shape) + core (cursor) | AN.toc.go silent on Chromium; same gap class as M8 AN on Firefox | AN.toc.go PASS on both |
| 6 | P1 | First announce places the cursor on the line that was spoken (the page is announced by the window activation before any script focus) | core | B0 entry/Down | B0 PASS |
| 7 | P1 | Iframes: an empty `internal frame` is spoken as "frame" with no content and re-read when children appear; find out why cross-origin and sandboxed frames stay empty | core + investigation | 7 IF steps FAIL, `frames_summary.py` | IF steps PASS or `xf` with a Chromium bug id |
| 8 | P1 | Dialog name on focus entering a dialog; popup Escape (`expanded` state of Chromium combo boxes) | core | APP.compose, APP.alert, FP1 | steps PASS |
| 9 | P1 | Tab switch and window title: speak the page once (strip " - Google Chrome"/" - Chromium" product suffix, do not repeat the document name), wait for the document of a freshly shown tab | ext-chromium (product rule) | "M7 Start - Google Chrome for Testing, M7 Start, document" | W3 and W2 notes |
| 10 | P2 | Name-from-content names: a row-driven rule `name_source_attribute()` in the web presenter (do not speak a name that came from contents) | core + row (done) | `name-from=contents` on headings, links, list items | none visible once item 2 is in; keep as a guard |
| 11 | P2 | `kScreenReader` mode: decide whether gorca asks users to start Chrome with `--force-renderer-accessibility` or sets `comm`/argv `orca` (CLAUDE.md "Identity"): run the corpus with `M9_AT=orca` vs `poke` first | decision + docs | open question 1 | measurement in the report, decision in `docs/decisions.md` |
| 12 | P2 | Chromium UI (omnibox, tab strip, popups, find bar): probes, then ext-chromium rules; known broken omnibox popup ancestry | ext-chromium | not tested | a UI probe report |
| 13 | P2 | Event-rate gates under mutation (aria-live flood, SPA churn): self-mutating test pages keyed by query string (no CDP), `probe_load.py` extended | tests | open question 3 | E-rate table in the report |

## 3. Rows (connector quirk table, `toolkit.rs`): status

| Row | Consumer | State |
|---|---|---|
| `text_insert_slack` Chromium 2 | core (reads `AppInfo.text_insert_slack`) | live, measured |
| `needs_wake_call` | connector discovery | data only (item 1) |
| `text_leaf_children_repeat_parent_text` | `RunLayer` | data only (item 2) |
| `name_source_attribute` | web presenter | data only (item 10) |
| `text_unit` Scalar, `silent_reorders`, caret(0) | exist | unchanged |

The ADR-0008 `EngineProfile`/`QuirkRule` table replaces these accessor methods when M9 turns them into rows with ids (`blink.wake-call`, `blink.static-leaves`, `blink.insert-offset`); the accessors are the data contract until then.

## 4. Harness (done in the prep)

`tests/web/m9/`: `run_m9.sh` (m7, m8, probe, probe-load, probe-typing), `chain.sh`, `probe_at.py`, `probe_load.py`, `probe_typing.py`, `compare_m8.py`, `insert_offsets.py`, `frames_summary.py`. Engine switch in `crates/gorca-bench/src/e2e/web.rs` (`chromium_selected()`), text units in `webtext.rs`.
Wake modes: `M9_AT=isenabled|poke|orca|force`; the default `poke` emulates item 1 in the AT driver, so the corpus can be run before item 1 exists. When item 1 lands, add scenario `CR-WAKE` (driver without a poke) and make `isenabled` the default.

Rig facts for Chromium: the first key into a freshly activated window is eaten by the activation (as in Firefox); Chromium announces the page by itself on window activation (the M7 `prime` step accepts that); a background tab has no document; `find_doc` matches the application name `chrom` (CfT is "Google Chrome for Testing").

## 5. Decisions the owner must take

| # | Question | Recommendation |
|---|---|---|
| D-1 | Wake-up call by the connector for every Chromium-toolkit application (also Electron), or only when a browser window is focused? | At first sight of the application, one call, deadline 800 ms; it is what Orca effectively does (it reads attributes on the first focus). Cost: none measured |
| D-2 | `orca` identity for Chromium's `kScreenReader` mode: set argv/comm `orca` in the daemon, or document `--force-renderer-accessibility`? | Measure first (item 11); do not impersonate by process name until the difference is shown, because a running real Orca is detected the same way |
| D-3 | Ship Chromium support with out-of-process iframes silent? | Yes, with a visible "frame, no content" and a filed Chromium bug; most pages' primary content is the top document |
| D-4 | Browser matrix for the gates: Chrome for Testing + Debian chromium only | Yes; Chrome stable by hand once per release |

## 6. As built (2026-10-17)

| Item | State |
|---|---|
| 1 wake call | done: connector `Caller::wake` at first sight (row `needs_wake_call`); document 25 to 40 ms after the reader starts with Chrome already open, 0.65 to 0.9 s after launch otherwise; `M9_AT=isenabled` is the harness default; scenario CR-WAKE = `M9_BROWSER_FIRST=1` (B0 + BAR1) |
| 2 static leaves | done: `RunLayer::dropping_static_children`, config `quirks.static_leaves_repeat_text`; unit tests; B0 once-checks |
| 3 typing echo | row `text_insert_slack` 2 plus the zero-length-delete repair in the drain (FE2 doubles 1 to 3 per run -> 0 in 5 repeats) |
| 4 tree arrival | not changed (progressive first block exists); settle window `page_events_settle_ms` added so the tree built for the wake call is not a page load |
| 5 anchors | done by M8 (`web_caret`, gated by our click); passes on Chromium |
| 6 first announce places the cursor | done (`web_place_first_line`) |
| 7 iframes | root cause found and worked around: off-screen cross-process frames are not serialized until scrolled into view; the source scrolls them in and back (`wake_offscreen_frames`); frame named by its title; frame document focus not announced. 17/17 M8-IF steps PASS. Chromium bug report not filed (no network): reproduction in the report |
| 8 dialog name, popup Escape | dialog name by M8; FP1 passes on Chromium when the popup is opened with F4 (Alt+Down released Alt starts a Chromium menu gesture that swallows the next key: rig reproduction, not gorca) |
| 9 tab switch / title suffix | not done (ext-chromium product rule) |
| 10 name-from rule | not needed (no visible effect after item 2); row kept as a guard |
| 11 kScreenReader | measured: no difference on the corpus; decision D-2 below |
| 12 Chromium UI probes | not done |
| 13 event-rate gates | not done |

Decisions: D-1 wake at first sight of every Chromium-toolkit application (adopted). D-2 no process-name impersonation; `--force-renderer-accessibility` is a comparison aid only (adopted, measured). D-3 frames ship with the scroll-in workaround (adopted). D-4 matrix CfT + Debian chromium: only CfT 153 was run on the final head.

Open: FCE1 loses ~15 keys in one of three repeats when it follows FS2/FSA (browser stall, not understood); Chromium browser UI; tab-switch announcement; Debian chromium re-run.
