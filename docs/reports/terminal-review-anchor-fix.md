# Terminal review anchor fix (ADR-0018 Option A)

Date: 2026-10-17. Role: rust-engineer. Design: `terminal-live-review.md`, `terminal-live-review-math.md`.

## What changed (all in gorca-core review, plus i18n and the VS e2e)

- `ZoneAnchor.line_hash`; `flat::carry_terminal` follows a terminal position by content and keeps the
  offset. Inputs: the rows VTE reported scrolled (exact, counted in `Reader::term_note_scroll` from
  `Delete{offset 0}` events while in review, `ScrollHint`), else a bounded shift estimate over the two
  screens (caret rows masked, <= 512 rows). Ambiguity (tied shifts, repeated content) never moves the
  cursor: the line becomes `Gone`.
- `Gone` lines are kept as a one-line held model (`review.held`): word/char moves go on reading them;
  a line move says "scrolled," and lands on the top row (`review-scrolled` message, en/ru).
- `term_review_model` does not rebuild when only the caret moved (screen text hash).
- A review key with a held screen no longer waits for a re-read (`speak_first`); the answer to the
  read corrects a changed line once (RV-1). Waiting stays only for an untracked terminal.
- Terminal review words use `gorca_text::seg::word_range` (`TermUnits`), spoken without trailing spaces.
- Shared word code was not refactored: `TermUnits` is a new adapter next to `SimpleUnits`.

## Tests

- Unit: `live_review_*` in `review/tests.rs` (T-A1 failed on main: said "output" instead of "charlie"),
  `flat::tests` (scroll, rewrite, repeats, exact scroll, bound), `segment` (TermUnits).
- Two older tests changed on purpose (lost events now speak first and correct after the read).
- State model: invariants `review-drift`, `review-reset`, `review-wait`, new op `TermScroll`; two
  regressions (`review_by_word_*`, `review_by_character_*`) fail with the offset dropped (mutation
  checked). 20 000 random sequences clean.
- e2e on a real gnome-terminal (private session): VV quick all gates PASS. VS: the first run failed
  (vs_stale 5, vs_silent 5) because it encoded the bug ("same row reads newer text"); VS now gates
  `vs_drift` (same line later has the same tick number) and accepts the "scrolled," prefix:
  vs_stale 0, vs_silent 0, vs_drift 0, 20 reads, 5 cycles, 60 s. Results: `bench/results/e2e/rv-VS`, `rv-VV`.
  The first failing VS result is kept next to it (host load ~10-12 from other agents).

## Not done / unproven

- Set VL (flooding producer, word/char reads at 150 ms, burst, clear, less) is not implemented; VS with
  a live loop plus the state model cover the symptom. Probes P-T1 (scrollbar Value), P-T2 (clear,
  Ctrl+L, alternate screen), P-T3 not run; no scrollback, no output-while-reviewing notice (Option C).
- Exact scroll counting trusts a delete at offset 0 to be a scroll; a user-visible clear that deletes
  the whole screen marks the hint inexact.
- No latency measurement of the review key (rebuild + shift estimate is O(rows^2) worst case, bounded).
