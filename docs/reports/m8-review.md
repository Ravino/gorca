# M8 review, round 1 (Firefox browse mode at NVDA parity)

2026-10-16, reviewer. Branch `worktree-agent-adcbae30d4d7a4ad0` at `e1e2a83` (code head `d50ca23`), merge base
`b8521b1`. Spec `docs/plan/m8-spec.md`; implementer reports `docs/reports/m8-impl.md`, `m8-gaps.md`.
Review worktree: `.claude/worktrees/m8-review` (detached at `e1e2a83`). Probes: `docs/reports/m8-review-probes.patch`
(applies to `e1e2a83`). Run results: `bench/results/e2e/m8-review-*` in this worktree.

## Verdict

**MERGE AFTER FIXES.** The browse-mode engine work is large, bounded and well tested, the 181 -> 308 jump is
honest (no step loosened to pass, the expected tokens were not tailored), and the merged tree with current
`main` builds clean and passes. But the D-2 substitute does not do what the report says, and on a real desktop it
is aimed at exactly the nodes where it can steal focus (H-1), and the new state words regress desktop speech in
two cases (M-1, M-2). One live-region rule silences alerts in SPA re-renders (M-3).

Must fix before merge: H-1 (stop `SetCaret` on focusable targets, or replace it with the adopted Tab override;
correct `m8-gaps.md` row 18), M-1, M-2, M-3. Should fix or record as follow-ups: M-4, M-5, M-6, L-1 (merge per the
resolution below), L-2, the PF1 quick-mode loss. No typing/caret regression was found (A/B below).

## Findings (ranked)

### H-1 High: the D-2 "caret follows the browse cursor" never fires on text and fires only on links, where Gecko focuses the link

`browse/commands.rs` `web_timer` (about lines 1520-1548) sends `SetCaret` only when
`up_to(node, FORM_FIELD | FOCUSABLE | EDIT, 8)` finds nothing or finds a link. Gecko's document accessible is
always `FOCUSABLE` (`refs/firefox/accessible/generic/DocAccessible.cpp:226`), and `translate.rs:190` turns that into
`Cat::FOCUSABLE` on the mirror root. So for any text within 8 levels of the root (almost every page) `up_to` returns
the root, `plain` is false and **no caret is set**; the only cursor positions that get `SetCaret` are inside links.

Probe `probe_m8r_caret_follow_targets` (browse/tests.rs, in the patch), same page with and without a focusable root:

| cursor move | fixture root (not focusable) | root `FOCUSABLE` (as Gecko) |
|---|---|---|
| Down onto a paragraph line | ScrollIntoView + SetCaret | ScrollIntoView only |
| K onto a link | ScrollIntoView + SetCaret on the link | ScrollIntoView + **SetCaret on the link** |
| Down onto a heading line | ScrollIntoView + SetCaret | ScrollIntoView only |

What a caret in a link does in Gecko: `HyperTextAccessibleBase::SetCaretOffset` ->
`TextLeafRange::SetSelection(kRemoveAll, aSetFocus = true)` (`base/TextLeafRange.h:339`), which calls
`hyp->TakeFocus()` when the leaf's HyperText parent is focusable (`base/TextLeafRange.cpp:2459`, "If the Accessible is
focusable, focus it before setting the selection"); the remote path forwards `aSetFocus`
(`ipc/DocAccessibleChild.cpp:248-255`). A link is a focusable HyperText, so on a live desktop every K (or other move that
leaves the cursor on a link) moves DOM focus to the link 150 ms later: BM-2 ("the browse cursor never moves
system focus") is broken, focus/blur handlers on the page run (menus that open on focus), and gorca then gets a
focus event for a node it already spoke. NVDA does not do this: `_set_selection` calls `setFocus()` only when the
target passes through (`refs/nvda/source/browseMode.py:1976-1985`).

The rig cannot show either effect: probe steps `PRB.*` (K onto a link, Shift+Right inside it, K x3) saw
`focus 0 caret 0` events in 2 repeats, while the control step (AT-SPI focus on a link) saw `focus 2`
(`bench/results/e2e/m8-review-probe-caret2`). The rig's Firefox apparently has no keyboard focus (`RIG_KEYS` in `m8_baseline.rs`: typed keys
land on a browser panel), so neither the claimed Tab benefit nor the side effect is observable there. `SY.nomove` passes
vacuously: no `SetCaret` is ever sent for its plain lines. No unit test covers `SetCaret` at all.

Failure scenario (live, predicted from the Gecko source, not observed): K onto a navigation link whose menu opens on
`focus` opens the menu 150 ms later; Down onto plain text afterwards sets no caret, so Tab starts from the last link
the cursor rested on, not from the cursor, which is the D-2 behaviour the feature was meant to give.

Fix: implement D-2 as adopted (override Tab/Shift+Tab in Browse: next/previous `Cat::FOCUSABLE` after the cursor,
`Action::Focus`, pass the key through when none), which the spec recommended (m8-spec.md:409). If the caret
follow is kept for native selection mode, never target a focusable node (place the caret in the nearest
non-focusable HyperText ancestor, before the embedded object character), exclude the document root from the
`up_to` check, add a unit test of the targets, and move the rule to a Gecko quirk row (M-6). Until then
`m8-gaps.md` row 18 should say "not working", not "unverified".

### M-1 Medium: expand and collapse are spoken twice, desktop-wide

`gorca-connector-atspi/src/decode.rs:84` now synthesises `COLLAPSED` for `expandable && !expanded`. A state change
re-reads all states (`present/flow.rs` `on_state_changed`), and `present_change` speaks each flipped flag:

```
PROBE expand -> ["expanded", "not collapsed"]
PROBE collapse -> ["not expanded", "collapsed"]
```

(`probe_m8r_expand_change_words`, utterance.rs). On `main` the same change says "expanded" / "not expanded". NVDA
says "expanded" / "collapsed". Affects every GTK combo box, expander, tree row, menu button and every Firefox
`aria-expanded` control in focus mode. Fix: treat EXPANDED/COLLAPSED as one tri-state in `changed()` and speak only
the new value.

### M-2 Medium: a tri-state (mixed) check box is now spoken as "not checked"

`decode.rs` has no mapping for `Raw::Indeterminate` (there is no `StateSet` bit for it), and
`present/utterance.rs` adds "not checked" to every `CheckBox`/`RadioButton` without `CHECKED`.
`states_from_raw([Enabled, Sensitive, Checkable, Indeterminate])` = `ENABLED`, and its focus speech is
`["Select all", "check box", "not checked"]` (probes `probe_m8r_expandable_and_mixed`,
`probe_m8r_mixed_checkbox_focus_words`). Before M8 it was silent about the state; now it is wrong. NVDA suppresses
"not checked" when `HALFCHECKED` and says "half checked" (`controlTypes/processAndLabelStates.py:140-145`).
Affects `aria-checked="mixed"` and GTK inconsistent check buttons. Fix: map Indeterminate to a `MIXED` bit, say
"half checked", skip the negative word.

The rest of the "not checked / not pressed" change is NVDA-like: NVDA speaks "not checked" for check boxes, radio
buttons and check menu items, "not pressed" only for toggle buttons (same file, lines 140-147). gorca maps
CheckMenuItem/RadioMenuItem onto CheckBox/RadioButton (`role_map.rs:35-36`), and plain buttons stay
`["OK", "button"]` (probe). The connector harness shows the desktop effect: "Subscribe check box not checked",
"Submit button" (`tests/harness_focus.rs:163`).

### M-3 Medium: an alert inserted with an SPA re-render is silent

`doc_actor/live.rs:359-386`: every batch touching >= 64 nodes restarts the 600 ms load-quiet window, and a new
`alert` region found inside that window takes its present text as baseline (not news). An app that renders a view
and an error alert in one batch (form re-render plus "Payment failed") is therefore never announced. Probe
`probe_m8r_alert_in_a_bulk_batch`: alert alone after the load = 1 message; the same alert plus 63 other touched nodes
= **0 messages**. NVDA announces a shown alert regardless. Fix: tie the quiet window to the document load
(first settle, `busy`/load-complete), not to batch size; or exempt `alert` nodes that are not in the first settle.

### M-4 Medium: live speech has no TTL and bypasses the per-process fairness

Polite news is `Prio::Notification` with `survives_focus_change = true` (`browse/commands.rs` `web_live`); there is no
expiry (ADR-0003 §13.2 table: polite 5 s, assertive 10 s; nothing in `gorca-speech` or `browse/` implements a TTL).
The speech FIFO holds 32 and, when full, drops an equal-priority newcomer (`gorca-speech/src/queue.rs:135-152`).
Limits allow 5 live messages/s (reader) and 4/s per document; a 300-char announcement takes about 15 s to speak. A
page with 4 busy regions therefore fills the FIFO in about 8 s; from then on (a) a GNOME Shell notification
(also `Notification`) is dropped, which breaks the 2026-10-16 fairness invariant "one flooding app cannot silence
another app's notifications", and (b) the user hears news that is minutes old. Derived from code, not measured.
Fix: TTL on live utterances at dequeue, at most N (2) queued live items per document, and account live speech in
`present/fair.rs` under the browser's owner.

### M-5 Medium: the native selection mirror goes stale and is on by default

`web_native_selection` (`browse/commands.rs:1982-1996`) mirrors a virtual selection only when both ends are in one
text node and never clears the page selection otherwise. Select a word (mirrored), extend over the next link
(two nodes): the page keeps the first word selected and the page's own Ctrl+C copies it silently. Ctrl+A on a real
page (many nodes) mirrors nothing. D-3 as adopted says virtual by default, Ctrl+C grabbed while a range exists,
native selection behind gorca+Shift+F10. Fix: collapse the page selection whenever the range is not mirrored, or
implement the grabbed Ctrl+C; keep the mirror behind the toggle. Code-read, not run.

### M-6 Medium (architecture): a Gecko rule in the generic core (ADR-0008)

The D-2 caret follow is justified only by Gecko behaviour ("Firefox starts Tab from its caret", "Firefox focuses an
editable that gets a caret", comments in `web_timer`) but runs for every web engine. ADR-0008 puts engine
behaviour in the connector quirk table (W1) and product rules in `ext-firefox` (W2). No other Firefox product rule
was found in core: `git diff main...branch -- crates/gorca-core crates/gorca-doc | grep -i 'firefox|gecko'` hits only
these comments and a doc comment in `gorca-doc/src/web/table.rs`.

### L-1 Low: the merge with `main` has six conflicts, not one

`m8-impl.md` §7 names only `text/mod.rs`. A trial merge with `main` `7af16c8` conflicts in `text/mod.rs`,
`present/flow.rs`, `commands/compile.rs`, `commands/mod.rs`, `lib.rs` and `docs/decisions.md`. Resolution used here:

- `text/mod.rs` `TextMsg::Caret`: `if self.web_caret(..) { return; }` first, then main's `term_is` / `term_caret`
  branch, then the text-tracking branch (a terminal is never in the web join, so the order is safe).
- `present/flow.rs`: `if silent {} else if self.web_suppress_focus(key) {} else if hold { window_hold } else { Speak }`
  (keeps M4b's policy silence and window hold; the suppress flag is taken only when the focus would speak).
- `commands/mod.rs`, `lib.rs`: union of the re-exports (`compile_with` and `letter_grabs`).
- `compile.rs` tests: keep both sides' tests. `decisions.md`: keep both sides' rows.

Merged tree: `cargo clippy --workspace --all-targets -D warnings` clean; `cargo test -p gorca-core -p gorca-daemon
-p gorca-connector-atspi` 796 passed, 0 failed (core 585, incl. the daemon e2e with "Subscribe, check box, not
checked"). The command-table recheck (`COUNT`, `impl_names`) passes in that run.

### L-2 Low: test gaps

`SY.nomove` cannot fail for D-2 (see H-1); no unit test asserts where `SetCaret` / `SetSelection` go; the live
"assertive goes stale on focus change" rule drops an alert that arrives just before a focus move (common in form
validation) and has no test. Live latency p99 81-96 ms sits close to the 100 ms gate at load 11-14.

## Typing and caret gates: interleaved A/B (FS2, FC1h, FE2)

The implementer compared against `main` `a456491`, which also contains M4b and its follow-ups, so the comparison
was confounded. Here: base = merge base `b8521b1`, branch = `e1e2a83`, same harness binary (branch), same host,
`--only FC1h,FS2,FE2 --repeats 1`, four rounds in ABBA order (`bench/results/e2e/m8-review-ab-*`, driver log
`m8-review-ab.log`). Load is the 1-minute average at each arm's start. Max over the run:

| metric | base r1 | base r2 | base r3 | base r4 | branch r1 | branch r2 | branch r3 | branch r4 |
|---|---|---|---|---|---|---|---|---|
| load at start | 15.8 | 9.9 | 9.8 | 11.1 | 8.7 | 7.9 | 11.3 | 16.8 |
| FC1h p99 ms | 2.27 | 0.23 | 0.32 | 0.26 | 0.26 | 0.23 | 0.27 | 5.07 |
| FC1h fc_double / fc_extra | 1 / 1 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 | 0 / 0 |
| FC1h page_dropped | 15 | 0 | 0 | 1 | 0 | 0 | 1 | 41 |
| FS2 p99 ms | 5.13 | 1.27 | 9.11 | 10.54 | 1.16 | 1.11 | 1.43 | 20.75 |
| FS2 fc_extra / fc_late / lost | 3 / 1 / 0 | 0 / 0 / 0 | 1 / 0 / 0 | 1 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 15 / 2 / 6 |
| FS2 page_dropped | 9 | 0 | 8 | 20 | 0 | 0 | 0 | 28 |
| FE2 e2_missing / double / wrong | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 1 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0 |
| FE2 e2_resync | 0 | 0 | 1 | 12 | 0 | 0 | 1 | 1 |

Clean runs: base 1 of 4, branch 3 of 4. The one red branch run started at the highest load of the series (16.8) and
fails the same way base does under load (Firefox drops keys, `lost` rows are keys whose page caret did not move,
with the speech 250 ms late). **No evidence of a typing/caret regression on the branch**; the 36 vs 14
`page_dropped` of `m8-impl.md` §6 is load, not M8. Not a quiet-host measurement: one repeat per arm, load 8-17.


## Harness honesty (the 181 -> 308 jump)

Checked every hunk of `m8_steps.rs`, `m8_baseline.rs`, `m8.rs`, `web.rs` and `tests/web/m8/pages/spa.html` in
`git diff main...branch`:

- Removed marks: every removed `.xf(...)` and every `MARKS` row belongs to a feature that landed; the three new
  marks (QN.o.2, QN.w.1, RD.copy.say) replace older marks on the same steps. No token moved from `say` to `extra`,
  no `not`/`max` relaxed.
- Token changes: only `IF.m.2` ("Nested level two" -> "frame", "Level two frame"). Verified: the frame's title is
  "Level two frame" (`tests/web/m8/pages/frames/level1.html:5`) and "Nested level two" occurs nowhere in the corpus.
- `TB.into` / `TB.span.hdr` Down x2 -> x1: follows from one-line-per-row (main had one cell per line); the checked
  token ("Region", "Item") is unchanged and matches NVDA (T lands on the caption line).
- `APP.toast` folded into `APP.close` with a 3000 ms window: the old step listened after the toast was spoken (the
  page sets it in the same click handler). Order is not asserted; acceptable.
- `esc()` before `APP.menu.item` / `APP.combo`, `top()` before QN.a/w/p and FG.b.1: start-position fixes; the mode
  word is spoken only on a change, as in NVDA.
- `spa.html` re-renders 1200 ms after the click so the cursor can be in the list: a corpus change made for the
  harness. Justified (the old step put the cursor on the button), but `SP.restore` now only covers a delayed
  re-render.
- Bench fixes (char-boundary truncation, profile removal) are correct.

## Full M8 gate rerun at the clean head

`tests/web/m8/run_m8.sh m8-review-full --quick --repeats 1` at `e1e2a83` (code `d50ca23`), load 13-16
(`bench/results/e2e/m8-review-full`): **308 PASS / 2 GAP / 8 XFAIL / 0 XPASS / 0 FAIL**, the implementer's count.
GAP `FM.invalid`, `FG.b.1`. Key p99 0.39-0.61 ms, FM mode p99 7.3 ms, live p99 84 ms (LV and FORGE).

Two gates red: `M8-PF1.w1_100k_ms` p99 15.5 ms (max 61 ms) and `lost_w1_100k_ms` 8 of 150, i.e. H/Shift+H on the
100k-node page lost 8 keys *after* the steady-state check passed. The same 8-key loss is in one of the three
repeats of the implementer's `m8-final-full` (head `ecebf72`); `m8-final2-full` shows a 6.99 ms p99 repeat; on
`main`, `m8-baseline-perf` lost 7 in one repeat and `m8-main2-perf` 1. So it is intermittent and pre-existing in
kind, but the "100k-node H p99 0.52 ms, lost 0" headline comes from the perf-mode run only. Not diagnosed (no
warnings in the reader log); follow-up for the perf owner.


## Gates run here

| Gate | Where | Result |
|---|---|---|
| `cargo fmt --all --check` | branch `e1e2a83` | pass |
| `cargo clippy --workspace --all-targets -D warnings` | branch | pass |
| `cargo test --workspace --no-fail-fast` | branch | 109 test binaries, 0 failed |
| clippy + tests of core/daemon/connector | trial merge with `main` `7af16c8` | pass, 796 tests |
| No `unwrap`/`expect`/`panic` added in non-test code | `git diff` grep | none; new IPC (`SetCaretOffset`, `SetSelection`) goes through `self.app.call(lane, deadline, ..)` with a deadline; live state capped (64 regions, ring 8, 16 KiB snapshot, 300-char text) |

## Probes

`docs/reports/m8-review-probes.patch` (apply to `e1e2a83`):
`decode.rs` `probe_m8r_expandable_and_mixed`; `utterance.rs` `probe_m8r_expand_change_words`,
`probe_m8r_mixed_checkbox_focus_words`; `live.rs` `probe_m8r_alert_in_a_bulk_batch`; `browse/tests.rs`
`probe_m8r_caret` module; `m8_steps.rs` `PRB.*` steps in M8-SY with focus/caret event counts in `heard` (`m8.rs`).
Run: `cargo test -p gorca-core --lib probe_m8r -- --nocapture`, `cargo test -p gorca-connector-atspi --lib probe_m8r --
--nocapture`, `GORCA_M8_STEPS=PRB tests/web/m8/run_m8.sh <label> --quick --only M8-SY --no-fail`.
