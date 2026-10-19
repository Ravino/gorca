# CHR-0 implementation: Chromium-engine mirror freshness and the web-app mode policy

Date 2026-10-19, web-a11y-engineer. Spec: `chromium-base-design-review.md` (authoritative),
`chromium-engine-base-architecture.md`, `chromium-mirror-freshness-math.md`,
`chromium-electron-a11y-research.md`; decision rows of 2026-10-19 (CHR-0 scope, DP-3).
Commits on `worktree-agent-a508eca24e2626204`: `a2bca912` (code), `411f3f16` (pages, probes, scenarios),
plus the source tests.

## 1. Step 0: the deciding experiment

`tests/web/chr0/probe_growth.py` (via `run_probe.sh`, `step0_all.sh`) in a private session. The page
`slowload-auto.html` starts as a tiny document, builds ~900 nodes (1354 AT-SPI nodes) 300 ms after
DOMContentLoaded and keeps its load open with an image that a loopback server answers after 25 s. The
control (`slowload-auto-ctl.html`) has no slow image. Raw lines: `docs/reports/chr0-step0/*.jsonl`.

| Engine | adds after the growth, load held | load-complete / busy=false | adds, control | tree during the hold |
|---|---|---|---|---|
| Chrome 155 | 0 (1 add total, at 0.24 s, before growth) | 25.3 s, no add after | 46 (45 after load-complete) | 1354 nodes from 1 s |
| Chromium 154 | 0 (1 total) | 25.47 s and 26.51 s, none after | 46 | 1354 nodes |
| Electron 44.9 | 0 (1 total) | 25.48 s, none after | 46 | **4 nodes until load-complete**, 1354 at 25.8 s |

Root cause confirmed (RC-1, G-2, G-3 of the review): no `children-changed:add` during the load, and
none when it ends, so a mirror that exists before the end of the load is stale for good unless gorca
rebuilds it. Electron is stricter than the review assumed: it also withholds the new content from the
platform tree until load-complete, so only the end-of-load signal (not a focus-miss rebuild or the
young checks) can fix a mirror there. This matches Signal (4 nodes, never refreshed).

Further probes kept in `docs/reports/chr0-step0/`: growth at 12 s (`chrome-slowload-late`): the tree
jumps 1 to 1354 nodes with no add. Failing runs are kept in `docs/reports/chr0-step0/failed/` and
`bench/results/e2e/failed-runs/` (electron name fragment, missing `--ozone-platform`, a click probe
that never clicked because the harness gives the probe a private HOME).

**What the rig cannot show.** The M8-SLOW scenario does not fail on main (b4310146) in the rig
(Chrome: `chr0-f1-base-chrome`, all SLOW steps PASS). When the harness focuses the document, Chrome
publishes the page content and then sends add events normally; the stale case needs an unfocused
or Electron-style tree, and Electron cannot be used with a held load because the rig's `find_doc`
does not see the document (known, research report section 4). M8-SLOW therefore is a regression and
exercise scenario (the repair triggers fire: `repair_start` lines in the reader log), not a proof of
the fix. The proof is step 0 (mechanism) plus the unit and fake-peer tests below.

## 2. What was built

Freshness (all engines get the machinery; `RepairPolicy::for_engine` selects triggers, data not name checks):

| Piece | Where |
|---|---|
| Repair gate: one in flight, gap = max(base x 2^fruitless, 8 x last cost) capped at 60 s, coalescing by priority, `changed` measured as (nodes, text length) before and after | `crates/gorca-core/src/doc_actor/repair.rs`, wired in `doc_actor/mod.rs` |
| Triggers: load-complete of a mirrored doc (ignores the 3 s settle window), root state flip while young, focus miss, orphan events after 750 ms, young checks at 2/6/16 s while the mirror has < 32 nodes, two follow-ups after a load-end repair that found nothing | `app_actor/doc_route.rs`, `doc_registry.rs`, `doc_actor/mod.rs` |
| Policy: Blink all triggers; Gecko focus miss only; others none | `RepairPolicy::for_engine` |
| B-1: refetch as a background task over a forked path table; Action/Text/Revalidate served against the old mirror; overload resync uses the same path | `gorca-doc-source-atspi/src/source.rs` (`Flight`), `DocRequest::Repair` |
| B-2: loading-grace fallback remembers the waiting focus; a repair or update that mirrors it joins it if focus and cursor did not move | `browse/commands.rs` (`web_rejoin_after_grace`) |
| B-3: a `GetTree` over 1500 nodes marks its root heavy; the next fetch expands it with `GetChildren` (up to 6 extra levels) | `fetch.rs` (`FrontierSpec`, `heavy`) |
| Act only on what was spoken: before a click the source re-reads role and name and compares them with the mirror; a mismatch refuses the click, refreshes the node and the Reader says "that item changed" (fails open on a slow or failed read). Integrates with the existing displaced-cursor rule instead of duplicating it | `source.rs` (`displaced`), `SourceError::Displaced`, `DocNotice::ActionDisplaced` |

## 3. Mode policy (DP-3)

* Default for every web document, Electron included: browse mode with automatic focus mode on widgets (unchanged code).
* The forced focus mode is the existing `browse.toggle` (gorca+Space NVDA presets, gorca+A Orca presets),
  `WebMode::Focus{cause: Manual}`; no parallel command. Verified end to end: in Chrome 155, Chromium 154
  and Electron 44 with real arrow keys (`M8-MSGK`: Down/Up/End reach the page and the reader speaks the
  message the page focuses, feed of articles with roving tabindex), and on Firefox with AT-SPI focus moves
  (`M8-MSG`; real keys cannot reach a Firefox page in the rig, RIG_KEYS). A listbox with
  `aria-activedescendant` and a roving list of list items work through auto focus mode (NVDA behaviour).
  Main failed the real-key case on Chrome: the toggle focused the document because the mirror does not
  mark a roving `tabindex` article focusable (`chr0-f1-base-chrome`, MK.fm.down1..end silent). Fix: the
  toggle keeps the page focus when the mirror knows no focusable node but the document and the page
  already has focus inside it.
* Per-app presets: data table `crates/gorca-config/src/app_modes.rs` (`AppModeRow` with app matchers, optional
  site host, `BrowseDefault`); `resolve(layers, app, host)` implements "later layer wins, host row beats app
  row", the shape ADR-0016 / M14a profile triggers (App, Site) and M18a add-on rows can supply. First-party
  row: `vscode` (code, visual studio code, code - oss, vscodium, cursor) = no buffer; the hub does not host
  such an app as a web application. Host matching waits for `DocMeta.url`.

### Existing behaviour that changes by default

1. Electron/Chromium documents whose mirror was stale now get browse mode working (the goal).
2. Apps named like VS Code no longer get a buffer.
3. Firefox: a focus miss inside a known document triggers one rate-limited background rebuild.
4. Enter refuses to click an object whose live role or name differs from what was spoken (all engines) and says "that item changed".
5. After the loading grace, a late repair moves the cursor to the focused element and speaks it.
6. gorca+Space no longer moves the page focus to the document when the page focus is already inside it and the mirror knows no focusable node at the cursor.
7. Rebuilds (overflow, wake) no longer block the source's request loop.

Terminal and desktop code is untouched.

## 4. Verification

Unit and integration: `cargo test --workspace` (core 857 tests incl. 3 gate-state tests, doc actor repair tests,
router tests, hub tests, B-2 tests; doc source: `text_requests_are_served_while_a_repair_fetches` (a text
request answers in < 300 ms while a repair runs 400 ms), `growth_without_events_stays_invisible_until_a_repair_finds_it`,
`a_heavy_subtree_is_expanded_by_the_next_fetch...`, `a_click_on_an_object_that_no_longer_says_what_was_spoken_is_refused`).
Gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask strings` (336 literals ok).

E2E (private sessions, `tests/web/chr0/final.sh`, results `bench/results/e2e/chr0-f1-*`):

| Run | Result |
|---|---|
| New reader, Chrome 155, SLOW/SLOW0/MSG/MSGK, 3 repeats | all PASS (54 steps/repeat set) |
| Main, same on Chrome | SLOW PASS, MSG PASS, MSGK: 4 forced-focus steps FAIL (silent) |
| Firefox new / main, SLOW, SLOW0, MSG | all PASS both |
| Chromium 154 new | all PASS |
| Electron 44 new, SLOW0, MSG, MSGK | all PASS (49 steps) |
| A/B existing M8 corpus, Firefox, 1 repeat | identical to main (section 5) |
| A/B existing corpus through the M9 rig, Chrome, 1 repeat | identical to main (section 5) |

## 5. Regression A/B

Same host, same minute-scale load, main (b4310146 binary) against this tree, 1 repeat, existing corpus
(`bench/results/e2e/chr0-f1-ab-*`):

| Rig | main | CHR-0 |
|---|---|---|
| M8, Firefox | 535 PASS, 0 FAIL, 1 GAP, 7 XFAIL | 535 PASS, 0 FAIL, 1 GAP, 7 XFAIL |
| M8 through the M9 rig, Chrome (Playwright Chromium) | 526 PASS, 3 FAIL, 7 GAP, 6 XFAIL | 526 PASS, 3 FAIL, 7 GAP, 6 XFAIL (same 3 steps) |

No regression. `click_displaced` fired 0 times in both corpus runs (no false positive of the click guard).
The 3 Chrome FAIL steps are pre-existing and identical on main.

## 6. Open

* Main does not fail M8-SLOW in the rig; no e2e proves the fix for the stale case. A live Signal check after the owner's
  restart (M-F7) is the acceptance test. Look for `repair_start reason="load_complete"` and `web_focus hit=true` in the journal.
* Orphan repairs fire on a quiet page once after load-complete (Chrome emits events on nodes it never lists); with
  the change-based back-off this costs one ~3 ms refetch per document, not a loop. Revisit with real Signal numbers.
* The click guard compares role and name; a dynamic name (an unread counter) makes the first Enter say "that item changed".
  Watch for complaints; the guard fails open on read errors.
* Subtree diff, sweep, M2 identity stay with M7b/M22a as decided.
