# Firefox tab switch and address bar (2026-10-17)

Harness: `tests/web/m7/run_m7.sh <label> --only TB1|UB1 --repeats 1` (private Firefox, loopback pages for UB1).
`GORCA_TAP_TRACE=1` adds window/property/state events to the bus tap (`EventTap::since`); `M7_RUST_LOG` sets the reader filter.

## 1. Ctrl+Tab / Ctrl+Shift+Tab (TB1)

Events on a switch: `selected` on the tab, `SelectionChanged`, window-frame `accessible-name` change, `showing` flips,
then `Focus` on whatever the target tab had focused: the document root, or a link/field inside it.

Root causes (reproduced, 2 of 5 switches silent about the page):
- Focus restored to a node INSIDE the page (no document focus event): the focus utterance said only "Link Gamma, link"; nothing carried the title.
- First visit of a tab (document not built yet): join miss -> `Loading` -> `FirstUseful` adoption; with a non-root focus the adoption said only the mode word.
Not dedupe/coalescing: every event reached the reader.

Fix (generic, in browse mode; no quirk needed): `web_on_focus` tracks `active` document; when the focus enters a different
document onto a non-root node, the page title leads the focus speech (`web_document_lead`, reuses `pending_tail` Lead items, like the dialog lead).
For a cold tab, `web_first_useful` re-speaks "mode word, title, focused node" in one utterance. Same document / return from chrome to the same page: no repeat.
Chromium: same code path (document focus); not run here.

Result TB1: 0 failures in 5 switches (was 2). Tests: browse::tests (3 new, 2 updated: first document focus now leads with its title).

## 2. Address bar (UB1)

Firefox exposes the URL bar as an editable `combo box` (Text iface) > popup `list` > `list item` rows (Focus + selected + active events per Down).
Found and fixed:
- "focus mode" was spoken on EVERY Down/Up/Escape in the bar and its suggestion list: chrome nodes took the web `Loading` path and its editable mode word.
  Fix: `outside_every_document`: when the whole cached ancestor chain reaches the Application with no document, treat as FocusAway (no word, no Ensure). Unit test added.
- BackSpace, and presumably Left/Right/selection, were silent: `trackable()` excluded `ComboBox`. Editable combo boxes are now text-tracked (BackSpace says the deleted char). Unit test added.
UB1 gate `ub_failures` = 0.

Open / unverified:
- Inline autofill (selected completed text) could not be triggered in a fresh profile (origin autofill needs frecency); not verified what gorca says. Insert-with-selection handling lives in the text/caret code another agent is changing.
- The first digit typed over the select-all URL leaves the next typed char double-echoed ("2","2"), deterministic in UB1 (typed claim vs. Gecko replace-selection events with no insert). Text/caret area, not touched.
- Suggestion rows: "list item" then a separate "selected" utterance sometimes; no "N of M" or suggestion count.
- Re-spoken cold-tab focus uses `describe_node` wording (role first: "link Link Gamma") unlike the focus presenter.

## Review fixes (2026-10-17)
- F1: `Tracked.replaced` (caret.rs): an unexplained delete with a fresh typed key sets it; the caret event that follows (no insert) consumes that key's expectation; an insert clears it. Regression test `a_key_that_replaced_a_selection_leaves_no_expectation_for_the_next_insert` (entry and combo, Gecko slack at 300/2200 ms and without). UB1 x3: "2" spoken once.
- F2: cold tab entry re-speaks via the focus presenter (word, title, "Link Gamma, link"), same wording as the first utterance. It is still two utterances (the first, short one goes out before the page is known; delaying it would break the focus-latency gate).
- F3: `web_retry_loading` now leads with the title (`web_document_lead`), no dedicated test.
- TB1 x3: 0 failures; UB1 x3: 0 failures.
- Known, pre-existing: after a document-root focus, its pending first-line tail can leak onto the next focus ("... Link Gamma, link, heading level 1, Heading Alpha").
- Gate note: `cargo clippy -D warnings` fails on main's `gorca-bench/src/e2e/ru.rs` (74, 109, 183), not on this branch's code.
