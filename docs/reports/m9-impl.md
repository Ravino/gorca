# M9 implementation report: Chromium and Chrome browse mode

2026-10-17, web-a11y-engineer. Spec: `docs/plan/m9-spec.md` (final). Baseline: `docs/reports/m9-baseline.md`.
Checklist for the live walk: `docs/plan/checklists/M9.md`. Code head of the gate runs: `ebead6e` (M8 merged, `git_dirty=false`).

## 1. What changed

| # | Item | Where | Row / data |
|---|---|---|---|
| 1 | **Wake call**: one `GetAttributes` + `GetRelationSet` on the application root at first sight, deadline-bounded, best effort | `gorca-connector-atspi` `fetch/mod.rs` (`AppServer::run`), `caller.rs` (`Caller::wake`) | `Toolkit::needs_wake_call()` |
| 2 | **Doubled speech**: `RunLayer` drops the `static` children of a container whose own text is non-empty (and their runs, and the nodes already sent) | `gorca-doc-source-atspi` `runs.rs`, `SourceConfig.quirks.static_leaves_repeat_text` | `Toolkit::text_leaf_children_repeat_parent_text()` (the doc-source crate cannot import the connector: the factory sets the config from the same classification) |
| 3 | **Out-of-process iframes** (root cause below): the source scrolls each empty, not-showing `internal frame` into view once, waits for its tree, scrolls back | `source.rs` `note_sleeping_frames` / `wake_frames`, `quirks.wake_offscreen_frames` | Blink quirk |
| 4 | Frame announced by the iframe's title (the Blink iframe object above the frame document carries it) | `browse/present.rs` `describe_node` | none (shape rule: parent of Role::Unknown with a name) |
| 5 | A frame document taking focus on the way to its content (Chromium focuses it before the link inside) is not announced | `browse/commands.rs` `web_on_focus` | none |
| 6 | The page announced by the window activation: the cursor is placed on the first line that was spoken (Down then reads line 2) | `browse/commands.rs` `web_first_useful`, `web_place_first_line` | none |
| 7 | Tree-creation window: `load-complete` and the first useful document within `page_events_settle_ms` of the application appearing are the tree built for the wake call, not a page load (no auto-read of a page that was already open) | `AppInfo.page_events_settle_ms`, `doc_route.rs`, `directory.rs`, `web_first_useful` | `Toolkit::page_events_settle_ms()` = 3000 for Chromium |
| 8 | Zero-length delete with text: Chromium reports about 1 % of Backspaces as `delete(end, 0)` with the removed text; the drain turns it into `(end - n, n)` (was: an uncorrelated caret move spoke the whole line, "doubled echo") | `gorca-dbus` `drain/map.rs` | applies to any source; only Chromium sends it |
| 9 | Harness: `M9_AT=isenabled` is the default (the daemon wakes Chromium); `M9_BROWSER_FIRST=1` starts the browser before the reader (CR-WAKE); B0 once-checks, `doc_arrival_ms`; FP1 on Chromium opens the popup with F4; `probe_scn.py`, content-visibility page | `tests/web/m9/`, `crates/gorca-bench` | |

Dropped after the M8 merge (main had the same features): my dialog lead (M8 `web_dialog_lead`) and my anchor caret follow (M8 `web_caret`, gated by the click that caused it). Both work on Chromium unchanged (APP.compose/APP.alert/AN.* PASS).

## 2. Measurements

Wake (private session, only `IsEnabled`, no `ScreenReaderEnabled`, no flag): browser started after the reader: document 0.65 to 0.92 s after launch (`B0.doc_arrival_ms`, 853 ms median of runs, Firefox 1.03 s in the same matrix); browser started **before** the reader (`M9_BROWSER_FIRST=1`, CR-WAKE): document 25 to 40 ms after the reader starts (`m9-final-cr-first`, 3 repeats, B0 and BAR1 pass, BAR1 start 423 ms).

### Iframes: root cause
`probe_scn.py frames` on `tests/web/m8/pages/iframes.html`: the empty frames (Form frame, Sandboxed frame, Missing frame) are the **cross-process frames, outside the viewport**. They have no `showing` state and no children, however long we wait (6 s), in every AT mode. `Component.ScrollTo(ANYWHERE)` on the frame node makes the child document appear within one frame (`after_scroll`: 1 child each). Chromium does not render, so does not serialize, an off-screen out-of-process frame. This is Chromium behaviour, not a gorca or AT-SPI bug; the workaround is the scroll-in and scroll-back of item 3 (the page jumps for a moment after load; checklist row). A Chromium bug report is not filed from here (no network); the reproduction is `tests/web/m8/pages/iframes.html` with the probe above.

### kScreenReader / Orca identity (item 4)
Chromium detects a screen reader by `/proc` argv element or comm `orca`. gorca's `--impersonate-orca` owns the bus name only (no process name) and `prctl(comm=orca)` stays off (decision R1, and so that a real Orca next to gorca is not confused). Measured: the M8 corpus with `M9_AT=orca` (decoy process `orca`) and with default mode: identical status counts and zero step differences (`m9i-m8orca` vs `m9i-m8b`, 318 steps each); `--force-renderer-accessibility` (which sets `kScreenReader` per `browser_accessibility_state_impl.cc`) and the decoy give the same tree for a `content-visibility` page (`tests/web/m9/pages/content-visibility.html`; the skipped sections are empty in all modes). The Blink uses of `kScreenReader` found in source are content-visibility pruning and exact focusability: neither matters for the corpus. **Decision: no process-name impersonation, no flag needed.** Unverified: that the decoy was detected (only the forced mode is certain by source); the live checklist has a `--force-renderer-accessibility` comparison row.

### Gate matrix (3 repeats, head `ebead6e`, `bench/results/e2e/m9-final-*`, host load 7 to 20, memory pressure `some avg10` below 1.1, see `m9-final-matrix.log`)
| Group | Chromium | Firefox (regression) |
|---|---|---|
| M7a set (B0 W1 BR1 BR2 W2 W2b W3 FM1 FM2) | all PASS; W1 p99 0.45 ms, BR1 0.45 ms, W2b gorca-side p99 3.0 ms, W3 0.18 ms, FM1 0.26 ms, FM2 0.45 ms | all PASS |
| M8 corpus `--quick` (339 steps) | **330 PASS / 2 GAP / 6 XFAIL / 1 XPASS / 0 FAIL** | 330 / 2 / 7 / 0 / 0 |
| Typing set | see below | FE2 double 1 in one repeat (Gecko astral insert offset), FS2 stray 1: median 0, load noise |
| CR-WAKE (browser first, B0 + BAR1) | PASS x3 | n/a |
| strict K2 K2s K3 K4 (`--impersonate-orca`) | K2 p99 0.315 ms, K2s 0.83, K3 2.0, K4 0 lost: all PASS | same run (not engine specific) |
| strict R1 | p99 0.34 ms, 0 lost: PASS | same |

Baseline for comparison: before M9, 172/183 steps and no document at all with the real daemon.

## 3. Open items (honest list)
* **FCE1 (contenteditable) loses the first ~15 keys of one repeat in three** when it follows FS2/FSA in the same session (`m9-final-cr-typing`, `-typing2`, `m9i-fce1b`; alone 3/3 pass, `m9i-fce1`). The lost keys never appear as `text key` in the reader log (`gorca_text=debug`): the browser stalls or drops them (the rig's known dropped-keys-under-load class); not understood, not seen on Firefox. Gate FCE1 is unmet on Chromium for this reason.
* FCE2 double 1 and FE2 wrong 1 in single repeats of `typing2` (max over 3), medians 0; FE2 doubles were 0 in a 5-repeat run after item 8 (`m9i-fe2fix`) and 1 to 3 per run before it.
* Chromium browser UI (omnibox, tabs, popups, find bar): not probed. Background tabs have no document until shown. Window title suffix " - Google Chrome" is spoken as is (item 9 of the spec, ext-chromium).
* `kScreenReader` decoy detection unverified (above). Chrome stable and Electron: checklist only.
* Debian `chromium` 154 not re-run on the final head (CfT 153 only).
* Main compiled badly at one point after two merges (`present/flow.rs` `Changed` lacked `description`); fixed here with `description: false` in the late-name re-read.

## 4. Process facts
Build and test: `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` and `cargo xtask deps` were green before the M8 merge; after it `cargo test -p gorca-core -p gorca-doc-source-atspi -p gorca-dbus -p gorca-connector-atspi` and clippy were green, the full workspace test is re-run at the final head (see the head SHA in the brief). Aborted matrices are kept: `m9-final-matrix-aborted*.log`, results at heads `e44b51d` and `83e4992`.

## 5. Review fixes (round 1, `docs/reports/m9-review.md`)

Code head of the fixes: see section 5.3. All fixes are data-driven (no engine names in the core).

- **H-1 (text layer, FCE1 key loss)**. Root causes, both fixed: (a) `text_mode_changed` sent `In::Dirty` in the same
  turn as the new tracking (`tracked_this_turn`), so the window prefetch, which carries whether the root hosts
  paragraphs (`hosts`), was always stale and discarded; a tracking started in the turn now keeps its reply.
  (b) A caret event of a paragraph that came before the focus (Chromium, separate lanes) was dropped in browse mode,
  and one that came while the root's window was still in flight was refused (`hosts` unknown). Both are kept
  (`early_caret`, valid 1.5 s, latest wins) and replayed when the fill says the root hosts paragraphs, as if the
  browser had sent them after the focus. This also removed a Firefox-only artefact the first fix exposed: Right
  keys that reached a stalled page spoke the embedded object ("object") through the root's Boundary logic, because
  the tracked node stayed the root. Tests: `text::tests::probe_m9r::*` (4).
- **H-2**: main merged; `page_events_settle_ms` added to the two terminal fixtures; one `description: false` in
  `present/flow.rs`; `decisions.md` keeps both sides.
- **M-1/M-2 (frame wake, `crates/gorca-doc-source-atspi/src/wake.rs`)**: a reference node in view (any role, real
  `SHOWING` state asked from the app) is measured with `Component.GetExtents` before the first scroll; the end is
  `ScrollToPoint` of that node to the measured position (checked, one retry), so the original offset returns
  exactly. Between steps the reference must still sit where our scroll put it, otherwise the task stops and restores
  nothing (the user scrolled). No node in view: frames stay asleep. The task ends with the document (`shutdown`).
  Residual risk: a layout shift above the reference during the wake reads as a user scroll (page left at the frame);
  not measured on real pages. Tests: `frame_wake::*` in `tests/fake_peer.rs` (4).
- **L-1**: a nested document taking focus in browse mode is still swallowed (Chromium focuses the frame document and
  then the link inside), but after 250 ms with nothing newer it is announced like any focus, so a Firefox Tab stop on
  a frame speaks its title and role. Tests: `browse::tests::a_frame_document_*`, `a_tab_stop_on_a_frame_document_*`.
- **L-2**: `AppInfo::page_events_settle_ms` is 0 for an application that was started after the reader did and less than
  the window before it appeared (process start from the registration identity, `/proc/uptime` and the reader's own start),
  so the page Chrome opens during its first 3 s is auto-read. A browser that was already running when the reader started
  keeps the window (first version without the reader-start condition broke BAR1 "a page that was already open must not
  read itself", 3 of 3: `bench/results/e2e/m9r-final-cr-typing`, kept).
- **L-4**: new `AppInfo::web_engine` (`gorca_model::WebEngine`, filled from `Toolkit::web_engine()`); the factory's
  `is_gecko/is_blink` read it instead of sniffing names. `Toolkit::classify` matches Chromium's `ToolkitName` first
  (the same for Chrome, Edge, Brave and Electron) and then names (chrom, electron, brave, msedge, vivaldi).
- L-3 stays: the checklist row Alt+Down on the Combo field is still manual.

### 5.3 Gate results of the review fixes (code head eff0ad4c, results-only commits after it)

- Unit gates at head: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
  (124 suites, 0 failed), `cargo xtask deps`: all OK.
- H-1 sequence FS2, FSA1, FSA3, FCE1 x 5 (`bench/results/e2e/m9r4-seq-cr`, `m9r4-seq-ff`): FCE1/FSA1/FSA3/FS2 lost 0 on Chromium
  and Firefox.
- Strict K2/K3/K4/R1 with `--impersonate-orca`, 3 repeats (`m9r4-strict`): all pass. M7 set and M8 corpus on both
  engines, typing set on both: see `bench/results/e2e/m9r4-*` and the m9r3 runs (same code). Host load 5-18 (matrix log
  `m9r4-matrix.log`).
- Failing gates are all single-repeat "max over repeats" gates with median 0, same code as the passing m9r3 runs, and
  the same shapes on main baselines: triage in `docs/reports/m9-gate-triage.md`.

### 5.4 Known issues (not caused by this branch)

- **K-1** `FS2.page_dropped` (and FC1h) measure the browser dropping keys under load, not gorca. Not scored; `FS2.lost` is 0.
- **K-2** The 100 ms boundary speech (`caret.rs:80`, CR-3) repeats the old line while a page stalls: FC1 lost/double/late
  and FC1h fc_extra/fc_double in one repeat (Chromium `m9r4-cr-typing`: keys 204-208 heard "Line 0204"). Main shows the same
  shape on Firefox.
- **K-3** A bare-letter stray speech in FS2 (`fc_extra`, "q" at +13..20 ms) already exists on main.
- L-3 stays: the Alt+Down checklist row on the Combo field is manual.

### 5.5 Fix round after the short review (`docs/reports/m9-fixround-verify.md`)

- **F-1 (fixed)**: the document actor cancels its source token when the main loop ends (`doc_actor/mod.rs`) and the registry
  cancels a document's token in `on_doc_ended` (`doc_registry.rs`), so the detached frame wake stops with its document.
  Test: `doc_actor::tests::a_task_the_source_detached_stops_when_the_document_ends` (fails without the fix, checked).
  Nit: `proc.rs` uses `.get(..2)`.
- Follow-ups, not fixed: **F-2** the deferred frame-focus announcement drops a browse-mode word queued on the swallowed focus
  (cosmetic); **F-3** the wake restore assumes an instant scroll (CSS `scroll-behavior: smooth` would leave the page scrolled;
  needs one e2e probe); **F-4** the early caret replay accepts an `Ancestry::Unknown` node (Firefox is safe, mirrored nodes are
  rejected).
