# M9 baseline: Chromium / Chrome against `main` (M8 prep, before any M9 code)

2026-10-16, web-a11y-engineer. Spec draft: `docs/plan/m9-spec.md`. Harness: `tests/web/m9/` (commands in section 8), engine switch in
`crates/gorca-bench/src/e2e/web.rs` (`GORCA_M7_BROWSER=chromium`). Raw results: `bench/results/e2e/m9-*`, `bench/results/m9/`.

## 0. Summary

1. **gorca on `main` cannot read Chromium at all as shipped.** With only `org.a11y.Status.IsEnabled` set (what gorca writes) Chromium registers on the
   a11y bus but exposes no web content: no document appears in 60 s. The trigger is a `GetAttributes` or `GetRelationSet` call by an AT on any node;
   gorca never sends one before the document exists. One such call makes the whole tree appear within 0.8 s. W-6 (comm=orca) is answered in section 2.
2. **Once woken, Chromium works almost like Firefox, except that every heading, link and label is spoken twice** (and say-all chunks are scrambled).
   Cause: Blink exposes text leaves as real `static` children *and* repeats their text in the parent's hypertext; `RunLayer` pairs children with U+FFFC
   by position and so emits both. A 60-line prototype that drops the `static` children of a container with text removes the duplicates:
   M8 corpus 173 PASS vs Firefox 183 (172 before the prototype), BAR1 text and BSA1 now correct.
3. **Typing echo is fine after one data row**: Chromium's `text-changed` insert offset is 1 to 2 past the caret event in about 5 % of inserts (same shape
   as Gecko). `Toolkit::Chromium.text_insert_slack = 2` took FE2 from 29 doubled echoes to 1 and to 0 in the later run.
4. Latency is not the problem: key to speech p99 0.35 to 0.7 ms on Chromium (W1, BR1, M8-QN, PF1 100k nodes 0.53 ms); first focus 77 ms wall, 2 ms
   gorca-side. Tree arrival is: Chromium builds big trees slowly and lazily (100k-card page: 27 s to the first document, 193k nodes; section 6).
5. Residual Chromium-only gaps after the prototype: out-of-process iframes show no content (Form frame, sandboxed frame: 7 IF steps), in-page anchor jumps are
   silent (AN.toc.go), dialog names are not spoken on focus (APP.compose, APP.alert), `Escape` with an open popup does not reach the page (FP1).

## 1. Conditions (read before trusting a number)

| Item | Value |
|---|---|
| Tree | `main` at `7af16c8` plus the harness commits of this branch; core code untouched. Quirk-row commits (section 4) were added during the run, noted per result |
| Browser A | Chrome for Testing 153.0.8010.12 (Playwright `chromium-1243`), most runs |
| Browser B | Debian `chromium` 154.0.8037.92 (apt, installed for this task): B0, W1, W2b, FM1, FM2 only (`m9-deb154-m7`): identical speech and the same duplicates, W1 p99 0.51 ms, W2b p50 75.7 ms |
| Firefox | 153.4.0 ESR, same machine and period (`m9-ff-*`) |
| Session | private headless gnome-shell, private dbus and a11y bus; `IsEnabled` true, `ScreenReaderEnabled` false (`GORCA_HARNESS_SCREEN_READER=0`, new env of `run-isolated.sh`), throw-away profile deleted per browser |
| Host | 16 cores, governor performance, **busy**: loadavg 10 to 28 (other agents building). Latency numbers are labelled, not gated; status results are not load sensitive |
| Repeats | 1 per scenario (M8 `--quick`, typing and M7 set `--repeats 1`); single-run numbers |
| Wake mode | `M9_AT=poke` (default of `run_m9.sh`): the AT driver (`ax.py`) calls `GetAttributes`/`GetRelationSet` on the application node once, standing in for the reader's future wake-up call. `isenabled` is the unmodified main state (section 2) |

Runs kept, including invalid ones (rig bugs, see section 8): `m9-trial*` (first tries), `m9-cr-*` (before the prime fix), `m9-cr2-*`, `m9-cr3-*` (prime still wrong; their W1/BR/W2/FM failures are the harness, not gorca), `m9-cr4-*` (valid baseline), `m9-cr5-slack` (with the `text_insert_slack` row), `m9-proto-*` (with the drop-static-leaves prototype), `m9-deb154-m7`.

## 2. How Chromium finds out that an AT runs (W-6)

Source (`refs/chromium`): `ui/accessibility/platform/atk_util_auralinux.cc` `ShouldEnableAccessibility()` reads `org.a11y.Bus` `IsEnabled` on the **session** bus
(env `ACCESSIBILITY_ENABLED`/`GNOME_ACCESSIBILITY` first, `CHROME_HEADLESS=1` disables it) and, if true, initialises the ATK bridge. That only exposes the *browser UI*
(`CreateAtkObject` returns null for web content unless `kNativeAPIs` is in the process AX mode). The web tree is enabled by `AXPlatform::OnExtendedPropertiesUsedInWebContent`
which on Linux is called from `AtkGetAttributes` and `AtkRefRelationSet` (`ax_platform_node_auralinux.cc:2236-2261`, commented "Most AT does not call [it], but Orca does"). That change of the
AX mode also starts the `/proc` scan for `orca` (`browser_accessibility_state_impl_auralinux.cc`: an argv element equal to `orca`, or `comm == orca`); a hit adds `kAXModeComplete | kScreenReader`.
`RefreshAssistiveTech` runs only on that mode change, not at start-up.

Measured in the private session (`tests/web/m9/probe_at.py`, results `bench/results/m9/probe-at-*.json`, `m9-trial`):

| Mode | Result |
|---|---|
| `none` (IsEnabled forced false) | the application never registers |
| **`isenabled`** (IsEnabled only, no call) | application registers (toolkit `Chromium`, name "Google Chrome for Testing"), 7 nodes (frames), **no document in 40 s**. The e2e with the real daemon on this state: B0 "document not found" after 60 s (`m9-trial`) |
| `poke` (IsEnabled + one `GetAttributes`/`GetRelationSet` on the app root) | document after 0.82 s, 34 nodes on the test article |
| `orca` (poke + a decoy process with argv[0] `orca`) | identical tree on the test page (the `kScreenReader` difference was not observed) |
| `force` (`--force-renderer-accessibility`) | document after 0.62 s, same 34 nodes |

Answers for the roadmap: **(1)** `IsEnabled` alone is not enough, and `comm=orca` alone is not enough either: the AT must call `GetAttributes`/`GetRelationSet` first
(the scan for `orca` hangs on it). **(2)** gorca does not set `prctl(comm=orca)` (daemon `lib.rs`), so Chromium never sets `kScreenReader`; whether that changes names, live-region events or
text boxes on real pages is **not measured** (open question 1). **(3)** The wake-up call is cheap and belongs to the connector (quirk row `blink-wake-call`, section 4).
**(4)** `--force-renderer-accessibility` remains the fallback for users whose Chrome starts before gorca; it exposes the same tree and also the full mode at once.
A tab that is not shown has no document in the tree (W3: the second tab's document is absent until the tab is selected).

## 3. Results against Firefox, same period

### 3.1 M8 corpus (318 steps, `--quick`, 1 repeat). `compare_m8.py` over the step reports

| | Firefox (`m9-ff-m8`) | Chromium `main` (`m9-cr4-m8`) | Chromium + drop-static-leaves prototype (`m9-proto-m8`) |
|---|---|---|---|
| PASS / GAP / XFAIL / XPASS / FAIL | 183 / 53 / 82 / 0 / 0 | 172 / 53 / 80 / 2 / 11 | 173 / 53 / 80 / 2 / 10 |
| p99 key (QN) | 0.43 ms | 0.57 ms | n/a |
| PF1, 100k nodes, H/Shift+H p99 | 6.2 ms (one loaded sample, max 14 ms) | **0.53 ms** | n/a |

Marks (XFAIL) are the Firefox baseline of `m8_baseline.rs` and apply to both engines, so a FAIL on Chromium is by construction a Chromium difference (or an XPASS: Chromium passes what Firefox
does not). The duplicate words do not fail steps (tokens match in any order), which is why the PASS count barely moves: the effect of the duplicates is on what is *heard*
(section 5.1). Per scenario: only M8-QN (2), M8-IF (7), M8-AN (1+1 XPASS), M8-RD (1, fixed by the prototype), M8-APP (2 GAP, 1 XPASS, 1 PASS where Firefox has a GAP) differ.

| Chromium-only difference (step) | What was heard on Chromium | Group |
|---|---|---|
| IF.h.form, IF.h.srcdoc, IF.f.1, IF.b.1, IF.focus, IF.esc, IF.m.1, QN.m.1, QN.m.2 | content of "Form frame" and "Sandboxed frame" is missing from the tree (the `internal frame` has 0 children 6 s after load, in `poke`, `orca` and `force`); the other frames work | (a)/(b), open |
| AN.toc.go | silent: Enter on an in-page link moves no virtual cursor (Chromium sends `text-caret-moved(0)` on the document, no focus; `CaretOrigin::ScrollToAnchor` exists in the model but nothing produces or consumes it) | (a) + (b) |
| APP.compose, APP.alert | focus enters a dialog, the dialog name is not spoken (Firefox passes) | (a), open |
| RD.sa.stop | says the previous line after a key during say-all (fixed by the prototype: duplicated chunks) | (a) |
| AN.foc.go, APP.alert.k | XPASS: pass on Chromium although marked on Firefox | none |

### 3.2 M7a set (`m9-cr4-m7`, `m9-ff-m7`)

| Scenario | Firefox | Chromium (valid run) |
|---|---|---|
| B0 (script: heading, words, quick nav, form, link) | PASS | 2 checks fail: the first line at entry is not spoken (the page is announced before the harness' first focus, section 8) and Down repeats the heading; every heading/link/label is said twice ("heading level 1, Gorca article, Gorca article", "Your name, Your name, edit") |
| W1 H/Shift+H, 10k page (p99, gate 5 ms) | 0.53 ms | 2.16 ms (p999 40 ms, load) PASS |
| BR1 (key to speech, 10k) | 0.50 / 0.50 / 0.55 ms | 0.66 / 0.55 / 0.54 ms PASS |
| BR2 (90 keys held, stale) | 0 | 0 PASS |
| W2 link click loads the 10k page, wall | 307 ms | 622 ms (information) |
| W2b first focus into a loaded 10k document | wall p50 84 ms, gorca-side p50 62 ms (p99 166 ms, gate 150 ms FAIL at load) | wall p50 77.5 ms, gorca-side 2.2 ms (6 trials; measured from the first focus event, harness adapted) |
| W3 tab switch | 2 lost of 3 (load) | 0 lost, p99 0.32 ms (after the harness stopped waiting for a hidden tab's document) |
| FM1 / FM2 (field focus, Escape) p99 | 2.0 / 0.67 ms | 3.6 / 4.3 ms PASS (gates 15 / 5) |

### 3.3 Typing and reading gates (`m9-cr4-typing` and `m9-ff-typing`; `m9-cr5-slack` and `m9-proto-typing` with the rows)

| Gate | Firefox | Chromium `main` | Chromium + `text_insert_slack` row (+ prototype) |
|---|---|---|---|
| FC1h Right/Left through an emoji line, p99 | 0.27 ms, 0 lost | 0.40 ms, 0 lost (scenario adapted to code-point offsets) | 0.24 ms |
| FC1 Down through 300 lines, p99 (gate 15 ms) | 0.81 ms | **7.9 ms** PASS (one `GetStringAtOffset` LINE per key is a real IPC round trip on Chromium) | 8.1 ms |
| FE1 typed keys into an input, p99 (gate 3 ms) | 0.47 ms | 3.8 ms FAIL (load, 5 ms p999) | 0.58 / 0.48 ms PASS |
| FCE2 contenteditable, p99 | 0.79 ms | 0.61 ms | 0.49 ms |
| FE2 randomised session | 0 missing, 0 double, resync 13 (load) | **20 doubled echoes**, 0 missing, 0 wrong, resync 7 | **1** (`m9-cr5-slack`), **0** (`m9-proto-typing`), resync 3 and 4 |
| FS2 selection by Shift+arrows, p99 | 1.9 ms | 1.5 ms | 4.1 ms (1 stray speech, load) |
| FSA1 / FSA3 (say-all in a field, stop) | 3.5 / 0.52 ms | 1.4 / 0.42 ms | 1.3 / 0.45 ms |
| FCE1, FT1, FP1 | pass | FCE1 1.0 ms; FT1 2 steps fail (first key after focus eaten by window activation: rig); FP1 not reached | FT1 pass; **FP1 fails**: Escape with an open popup does not reach the page |
| BSA1 browse say-all | 0.37 ms | 0.69 ms, 2 trials silent | 0.35 ms, 0 failures (after the prototype) |
| BAR1 auto-read on load | pass | fail: duplicated chunks | text now correct; still fails the "page that was already open must not read itself" check (rig ordering, section 8) |
| BL1, FR1 | pass | pass | pass |

## 4. Root causes

### (b) Chromium AT-SPI quirks: connector data rows (`crates/gorca-connector-atspi/src/toolkit.rs`)

| Row (this branch) | Evidence | Consumer still to write |
|---|---|---|
| `Toolkit::needs_wake_call()` = Chromium: one `GetAttributes` + `GetRelationSet` on the application root when the application is first seen | section 2 | connector discovery (P0) |
| `Toolkit::text_leaf_children_repeat_parent_text()` = Chromium | the dump of a heading: Text "Gorca article" (13 chars, no U+FFFC), 1 child `static` "Gorca article"; a paragraph with one link: hypertext has 1 U+FFFC and 3 children `[static, link, static]`; `runs.rs` pairs by position: `[run0, static, run1, link, static]`. Heard: "It has a, Paragraph 1 starts here." | `RunLayer` drops `static` children (P0, patch below) |
| `Toolkit::name_source_attribute()` = `Some("name-from")` | headings, links, list items have `name-from=contents`; the name repeats the text | consumer in the web presenter: do not speak a content-derived name (P2; today the doubles come from the runs, not from names) |
| `Toolkit::text_insert_slack()` = 2 for Chromium (was 0) | 24 of 446 inserts into an `<input>` (5.4 %) named an offset 1 or 2 past the next caret event (`insert_offsets.py` on the FE2 reader log); FE2 doubles 29 (debug run) -> 1 | none: the core already reads the row (ADR-0008) |

Unit test `toolkit::tests::blink_rows_belong_to_chromium_only` and the updated `only_the_browser_engines_have_the_insert_offset_quirk`. Not added because nothing proves them yet: `caret0` as a data row
(the model has `CaretOrigin::ScrollToAnchor`, no producer), a LINE-boundary row (FC1 7.9 ms is slow but correct), a Chromium event-flood row.

Other Chromium facts used above: Text offsets are code points (no phantom after astral characters, `Toolkit::text_unit` already says `Scalar`); text-changed is sent for editable nodes with the insert before the caret event in
the same millisecond (`probe_typing.py`); `showing` never changes (B-6).

### (a) Generic web-engine gaps (core and doc source), ordered

1. **Duplicated text leaves** (P0). Prototype patch `bench/results/m9/prototype-runlayer-drop-static-leaves.patch` (uses `silent_reorders` as the Chromium flag; the real change should read the quirk row). Verified: B0 words, BSA1, BAR1 text, RD.sa.stop, FT1 all correct.
2. **Wake-up call** (P0), section 2.
3. **First announce does not place the cursor** (B0 "entry"/"Down"): Chromium announces the page by itself when its window is activated ("title, document" then "browse mode, heading level 1, ..."), before any script focus; the cursor then sits before the heading and Down says the heading again. Same family as `CONTAINER_JUMP` in `m8_baseline.rs`.
4. **In-page anchors** (AN.toc.go): produce `CaretOrigin::ScrollToAnchor` from `text-caret-moved(0)` on a document and move the virtual cursor to the new sequential-focus start point (also M8's AN gap for Firefox).
5. **Iframes**: the cross-origin / sandboxed `internal frame` has no child document in the tree (no event either): treat an empty frame as "frame, no content" and re-read when `children-changed` arrives; needs a Chromium bug report if it does not (open question 2).
6. **Dialog name on focus** (APP.compose/APP.alert) and **popup Escape** (FP1: the mirrored `expanded` state is not seen for Chromium comboboxes).
7. **Page-load reading** (BAR1/W2): `document:load-complete` (4 per load) is delivered without a focus event after navigation; the Firefox path keys on focus. Chromium is announced from the window activation. Needs a deliberate rule.

### (c) Chromium product rules (a future `ext-chromium`; no row can express them)

* Window title is "page - Google Chrome for Testing" and the document name repeats the page: speak the page once ("M7 Start - Google Chrome for Testing, M7 Start, document").
* The browser UI (omnibox, tab strip, popups, find bar, downloads) is exposed by the same process in `kNativeAPIs` mode: not tested here; known broken omnibox popup ancestry (memory: chromium-atspi-facts). Needs its own probes before rules.
* Background tabs have no document until shown: tab switching must wait for the new document and announce it.
* The app-name test (`chrom`) also matches Electron apps, Chromium-embedded apps and `Google Chrome for Testing`; Electron needs `--force-renderer-accessibility` or the same wake call. Policy decision per product.
* User guidance: Chrome started before gorca stays unwoken until the wake call lands; `chrome://accessibility` shows the current mode (not checked here).

## 5. What the user hears

### 5.1 Before and after the prototype (B0, BAR1)

Before: `heading level 1, Gorca article, Gorca article`, `link, details below, details below`, `browse mode, Your name, Your name, edit, blank, button, Send`; a say-all chunk
"It has a, Paragraph 1 starts here." (out of order). After: `heading level 1, Gorca article`, `link, details below`, `browse mode, Your name, edit, blank, button, Send`; the same wording as Firefox, say-all in order.

### 5.2 Latency (labelled: busy host, single runs; Firefox in the same period for reference)

| Moment | Firefox | Chromium (CfT 153 / Debian 154) |
|---|---|---|
| first focus into a loaded 10k document, focus event to first speech | 84 ms wall (62 ms gorca-side) | 77.5 ms wall (2.2 ms gorca-side) / 75.7 ms |
| quick nav key, 10k page (W1 p99) | 0.53 ms | 2.2 ms / 0.51 ms |
| quick nav key, 100k nodes (PF1 p99) | 6.2 ms (loaded) | 0.53 ms |
| typed-key echo, input (FE1 p99) | 0.47 ms | 0.58 ms with the slack row (3.8 ms in the loaded first run) |
| typed-key echo, contenteditable (FCE2 p99) | 0.79 ms | 0.49 ms |
| browse say-all start, field say-all start (BSA1 / FSA1 p99) | 0.37 / 3.5 ms | 0.35 / 1.3 ms |
| mode switch (FM1 / M8-FM `mode_ms` p99) | 2.0 / 5.7 ms | 3.6 / 3.6 ms |
| load of the 10k page by link, click to speech (W2 wall) | 307 ms | 622 ms |

## 6. Tree size and event rate (`tests/web/m9/probe_load.py`, `bench/results/m9/probe-load.jsonl`)

`gen_large.py N` pages in `poke` mode and in `force` mode; "first document" is the time from browser start. Events are all `Object:`, `Window:`, `Document:` classes registered by the probe.

| Page (cards) | Mode | First document | Nodes at quiet (GetTree) | GetTree call (steady) | Events during load, peak rate |
|---|---|---|---|---|---|
| 1k | poke / force | 1.3 s / 0.8 s | 2,093 / 2,093 | 34 / 30 ms | 7 / 24, 40 / 190 per s |
| 10k | poke / force | 3.3 s / 1.5 s | 21,203 / 21,203 | 562 / 204 ms | 23 / 33, 110 per s |
| 50k | poke / force | 7.9 s / 4.3 s | **105,968 / 64,864** | 1.09 s / 0.67 s | 66 / 49, 430 / 270 per s |
| 100k | poke / force | **27 s** / 1.6 s | **193,435 / 25,965** | 2.3 s / 0.28 s | 84 / 30, 790 / 190 per s |

* Chromium has about 2 AT-SPI nodes per page node (a `static` child per text run). No event storm during load: tens of events total, peak under 800/s (the 100k page spreads over 27 s).
  The "events storm" of Chromium is not at load; it needs a mutation workload (live regions, SPA churn), which needs JS execution in the page and was not built (no CDP, open question 3).
* **Trees are lazy and the arrival time is not monotonic in size**: the document node of the 100k page appeared after 27 s in `poke` mode (full tree: 193k nodes) but after 1.6 s in `force` mode with only 25,965
  nodes that did not grow in the following 3 s (`getree_second` identical), and the 50k page in `force` had 64,864 vs 105,968 nodes. So a build must not assume that "quiet" or the first block means complete, and `Document.LoadComplete` (1 to 2 per load) is the only completeness signal. This matches WF-4 (Chromium serialises a 100k-element page in tens of seconds). The M8-PF1 corpus page (100k) was usable in 1.4 s because its nodes per element are fewer.
* A GetTree reply of 106k nodes takes 1.1 s and 193k nodes 2.3 s: the B-6 frontier split (5 ms longest main-thread call) still applies; Chromium answers calls from its UI thread.

## 7. Open questions

1. What does `kScreenReader` (set only when an `orca` argv/comm exists) change on real pages: live-region events, names, inline text boxes? The probe page showed no difference. Needs a corpus run with `M9_AT=orca` vs `poke` on `news`, `webapp`, `spa` (harness ready).
2. Why do cross-origin and sandboxed iframes show no children? Site isolation (OOPIF) stitching, a mode bit, or a Chromium bug; check with `chrome://accessibility` and a same-origin http server.
3. Event rates under mutation (SPA churn, aria-live floods): needs in-page script control without CDP; candidates: a local page that mutates itself on a timer, keyed by query string.
4. Chromium UI (omnibox, popups), Electron and Chrome (branded) were not tested; Wayland-native vs XWayland Chrome not compared.
5. 1 doubled echo remains in one FE2 run with the slack row (note in `m9-cr5-slack`); not root-caused.
6. FT1 and BAR1 differences might be rig (first key after window activation is eaten by the activation, see `probe_typing.py` first key); not separated yet.

## 8. Harness changes and rig adaptations (all engine-switched, Firefox behaviour unchanged)

* `GORCA_M7_BROWSER=chromium`: `Firefox::launch` starts Chromium (`--user-data-dir`, `--ozone-platform=wayland`, a throw-away profile removed in `Drop`; `GORCA_M9_AT` = `isenabled|poke|orca|force`; `orca` starts a decoy `exec -a orca sleep`, killed with the browser).
* `ax.py`: app match by `chrom`, wake call in `poke`/`orca` mode. `run-isolated.sh`: `GORCA_HARNESS_SCREEN_READER=0` writes `IsEnabled` only.
* `prime()` accepts a mode word spoken before its first focus (Chromium announces by itself; the first runs failed all W1/BR/W2/FM/typing scenarios on this: `m9-cr-*`, `m9-cr2-*`, `m9-cr3-*` are kept as invalid).
* FC1h and FE2 use code-point offsets for Chromium (`text_unit_len`), W2b measures from the first focus event, W3 does not look for the hidden tab's document, the `untagged_warm` gate is Gecko-only.
* Commands: `tests/web/m9/run_m9.sh m7|m8|probe|probe-load|probe-typing`, `tests/web/m9/chain.sh chromium|firefox <prefix> [m7 typing m8]`, `compare_m8.py <cr-steps.md> <ff-steps.md> --diff`, `insert_offsets.py <reader.log>`, `frames_summary.py`.

## 9. Reproduce

```
tests/web/m9/chain.sh chromium m9-cr      # M7a, typing and M8 sets, poke mode, about 35 min on a busy host
tests/web/m9/chain.sh firefox  m9-ff
M9_AT=isenabled tests/web/m9/run_m9.sh m7 m9-isenabled --only B0 --quick --repeats 1   # fails: no document
tests/web/m9/run_m9.sh probe isenabled|poke|orca|force
M9_CHROMIUM=/usr/bin/chromium tests/web/m9/run_m9.sh m7 m9-deb --only B0,W1,FM1
```
