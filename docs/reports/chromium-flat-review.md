# Flat review in Chromium (2026-10-18)

Phase 2 acceptance: e2e `M8-FL` (3 steps) and `M8-FLS` (1 step) failed deterministically on Chromium
while Firefox passed. Now green on both, 3 repeats each (`bench/results/e2e/cfl15`, `ffl15`).
Repro: `tests/web/m9/run_m9.sh m8 <label> --only M8-FL,M8-FLS` (Chromium),
`tests/web/m8/run_m8.sh <label> --only M8-FL,M8-FLS` (Firefox). Line/zone dump of every built model:
`M7_RUST_LOG="warn,gorca_core::review=trace"` (new `review line` / `review zone` trace events).
Probe of states and extents of a page: `M9_AT=poke M9_ARGS="--m9-max=300 --m9-textext=1" tests/web/m9/run_m9.sh probe-scn geom <page> <title>`.

## What Chromium exposes differently (verified on Chrome for Testing in the private rig)

1. **States.** Chromium sets SHOWING+VISIBLE on everything on screen (containers, text, links) and VISIBLE
   only on what is scrolled away. The Firefox rule (VISIBLE counts inside a `DocumentWeb`, entered only when
   the extents overlap the document) already handles it; no change needed there.
2. **Document extents** (0,143,1280,625): the document starts below the browser's own toolbar; screen and
   window coordinates are the same in `Component.GetExtents` (window origin is (0,0) under Wayland).
3. **Coordinate frame of `Text`: the real defect.** `Text.GetRangeExtents(.., WINDOW)` and
   `GetOffsetAtPoint(.., WINDOW)` are relative to the *web content*, i.e. `y - 143`, while
   `Component.GetExtents(WINDOW)` is not. Every text zone sat 143 px above its node. `SCREEN` agrees with
   the component extents. Fix: `Toolkit::text_extents_use_screen_coords` (row `blink-text-extents-frame`)
   sets the coordinate type of the connector's Text calls (`Caller::set_text_screen_coords`, set when the
   toolkit is identified). Gecko is unchanged (WINDOW).
4. **`static` children repeat the parent's text** (known row `blink-static-leaves`, so far only used by the
   browse layer). Flat review read "Filler paragraph number 1486" twice. Fix: `EditQuirks::text_leaves_repeat_parent`
   (filled from `Toolkit::text_leaf_children_repeat_parent_text`) reaches `ReviewIo::static_leaves_repeat`;
   `collect_with` does not walk `static` leaves under a text node. A `static` that holds children (a
   `<time>`, `<span>`: the parent only has a U+FFFC for it) is kept.
5. Child counts are fine (`Count(n)` on ChildSnapshots; main of the long page has 1502 children).

## Generic defects that Firefox's layout hid (not engine rows)

* **Container names sorted by the middle of the whole container.** A named container (document, landmark,
  list) put its name zone at the centre of its rectangle, so "M8 News article document" landed between
  unrelated rows, a different pair per engine ("Account menu" on Firefox was a coincidence of that).
  `cluster::name_strip`: the name sits on the row of the container's first child.
* **Transient text failures produced a "Complete" model with missing rows.** On Firefox eight parallel
  `VisibleLines` requests were refused with `Busy`; the rows were dropped silently and the model said
  Complete (the old FLS step passed only because the bottom line was then the heading). `lines_of` retries
  a busy app (4 tries, 5/10/15 ms) and a node that still failed marks the model `Truncated(Deadline)` so the
  continuation asks again.
* **R2-1 (child paging ignored the budget).** `expand` takes the build's soft deadline (`until`) and stops
  paging when it has passed. Because a build restarts on every pass, the pages already read are kept in
  `ReviewIo::partial_pages` and the next pass resumes behind them; a resumed call always reads one more page
  (otherwise a slow app never gets past the first pages: observed, builds stuck at 512 of 1502 children).
  Callers outside a build (`nav_fetch`, status bar, window presenter) pass `None`.

## Unit tests (Chromium-shaped fake trees)

* `collect::a_blink_shaped_page_skips_repeating_static_leaves_and_off_screen_nodes`
* `cluster::a_named_container_is_said_on_the_row_where_its_content_starts`, `a_name_without_a_child_row_keeps_its_own_rectangle`
* `build_tests::child_paging_stops_when_the_build_budget_is_used_up` (stop, resume, one page owed, no first page again)
* `toolkit::only_blink_has_the_review_text_quirks`
Not unit-tested: the busy-retry in `lines_of` (covered by the Firefox e2e, where it was found).

## Changed e2e expectations (`crates/gorca-bench/src/e2e/m8_steps.rs`)

The old expectations encoded Firefox's accidental layout. They are replaced by assertions that walk the page
in reading order and name specific content on every row, valid on both engines (the browser chrome above the
document differs, so no step depends on a row count from the top).

| step | old | new | why |
|---|---|---|---|
| FL.cur | "News article" + "Account menu" | "News article" + "Skip to main content" | "Account menu" came from the document name sorting at the page's middle; the document row is now the first content row. Both engines. |
| FL.top2, FL.next1..4 (unjudged positioning) | jump to top, 4 silent next | FL.next1..FL.next5 judged: "Daily Gorca" -> "Sections"+"World" -> "Politics" -> "Business" -> "Technology" | strictly stronger: each row down the nav list is asserted, in order |
| FL.next5 | "Skip to main content" after top + 5 nexts | "Technology" (fifth next from the document row) | old count depended on the chrome rows of the engine |
| (new) FL.back | - | previous line says "Business" | previous-line move |
| FL.end | "Breadcrumb" | at least one utterance, none of the browser chrome ("toolbar", "tab list", "Address and search", "Open tab in split") | "Breadcrumb" was a page-wide container zone reached as the last line; the real bottom row differs by viewport and engine ("14 October 2026" vs "Copy link"). Weaker on content, stronger on "page, not chrome"; see risk below |
| FL.prev | "Copy link" | same constraint as FL.end | same reason |
| FLS.bottom | "Final heading marker" | "The very last line" | the old value was the second-to-last row; Firefox had lost the last 12 rows (Busy) and the step still passed |
| (new) FLS.up | - | previous line: "Final heading marker" | reading order at the end of a page that is 1500 paragraphs long |
| (new) FLS.up2 | - | previous line: "Filler paragraph number 1499" | the rows between are reachable |
| FL.objn and the rest | unchanged | unchanged | |

Residual risk: FL.end/FL.prev assert page-not-chrome rather than a named row, because the last visible row of
`news.html` depends on the window height of each engine. A fixed-height fixture would allow naming it.

## Gates
fmt; `clippy --workspace --all-targets -D warnings`; `cargo test --workspace` all pass except the two known
environmental `gorca-daemon/tests/shell_parity.rs` tests (`wasm32-wasip2` not installed); `cargo xtask strings` ok.
Result dirs of this work: `bench/results/e2e/cfl0..cfl15` (Chromium, cfl0 = the failing baseline), `ffl3..ffl15` (Firefox).

## Review fixes (chromium-flat-review-verify.md)

* **M-1.** `lines_of` retries only `Busy` (4 tries, stops when the build's soft deadline is spent). A
  `Timeout` is asked once and marks the model `Truncated(Deadline)`: four class deadlines in a row would have
  counted as three timeouts and made a slow-but-working app "hung". Tests:
  `a_busy_app_is_asked_again_for_its_lines`, `a_timeout_on_lines_is_not_retried_and_marks_the_model_truncated`,
  `busy_retries_stop_at_the_build_budget`.
* **M-2.** Saved pages carry the parent's child count; a resume re-reads the last saved child together with the
  next page (range starts one early) and starts again at page 0 if the count differs or that child is not the
  same. Tests: `a_list_that_grows_between_passes_is_read_again_from_the_start` (600 -> 601, new first child,
  no duplicates), `a_list_reordered_between_passes_is_read_again_from_the_start`.
* **Known limitation.** The Chromium screen-coordinate quirk assumes the toplevel's origin is (0,0) so that
  `Component.GetExtents(WINDOW)` equals screen coordinates. Verified on native Wayland only; Electron or
  XWayland sessions, where the window origin is known and non-zero, may put text rows off by that origin.
  A cheap correction (not done): compare one node's `Text` and `Component` extents and use the difference
  as the offset.
