# Terminal review anchor fix: adversarial review

Date: 2026-10-17. Role: reviewer. Branch `worktree-agent-a4bdcdadbc4132acf`, head `904e383d`, base `main`.
Scope: ADR-0018 Option A (`docs/adr/0018-terminal-live-review.md`), implementer report
`docs/reports/terminal-review-anchor-fix.md`.

## Verdict: MERGE AFTER FIXES

The core idea works: word and character moves no longer reset while output scrolls (T-A1, the real
gnome-terminal VS run: same line read 4 s later has the same number in 5/5 cycles). The gates are
clean. Two correctness bugs in the new speak-first path make a blind user hear stale or wrong lines,
and the two rewritten unit tests hide the first of them.

## Gates (run by the reviewer)

`. ./env.sh`, `CARGO_BUILD_JOBS=4 RUST_TEST_THREADS=4`, host load ~11-12 (other agents), 14 GB free.

- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: all pass, 0 failed (gorca-core lib 707 passed, 1 ignored).
- e2e: not rerun. The committed results were read: `bench/results/e2e/rv-VS/20261017T152051Z` (FAIL,
  kept) and `20261017T152317Z` (PASS), `rv-VV/20261017T151914Z` (PASS).

## Probes

Seven probes were appended to `crates/gorca-core/src/review/tests.rs` as `mod reviewer_probes`, run
with `cargo test -p gorca-core --lib reviewer_probes -- --nocapture`, then removed (the file is restored
byte for byte; the branch is unchanged). The probe source is reproduced here in prose; the outputs are
quoted exactly.

| Probe | Scenario | Heard / state |
|---|---|---|
| P1 | The **original** input of the rewritten test 1: events lost, prompt `$`, PrevLine; the read returns `new 8\nnew 9\n$ ` | PrevLine: `["old 2"]`; after the read: `[]`; CurLine again: `["old 2"]`; NextLine: `["scrolled, new 8"]` |
| P2 | The **original** input of the rewritten test 2 (unprotected app); second read returns `newer 9\nnewer 10\n$ ` | PrevLine: `["new 9"]`; after the read: `[]`; CurLine again: `["new 9"]` |
| P3 | Speak-first on `prog 10%`, then the screen scrolls by one and the line is rewritten: `a1\nb2\nprog 10%\n$ ` to `b2\nprog 20%\nc3\n$ ` | First: `["prog 10 percent"]`; correction: `["c3"]` with the cursor at (1,0); CurLine: `["prog 20 percent"]` |
| P4 | Cursor on `ccc` (row 2, offset 1); VTE `text_modified` diff for a redraw of the top two rows: `Delete{offset 0, len 7}` then `Insert{0, 7}`; screen becomes `xxx\nyyy\nccc\nddd\n$ ` | CurLine: `["yyy"]` at (1,1) — want `ccc` |
| P5 | Clear (`$ ` only) while on the second word of a line | NextWord: `["scrolled, $"]`, NextLine: `["Bottom"]` (acceptable) |
| P6 | Alternate screen (less) and back | CurLine in less: held old line `["bbb word"]`, no notice; NextLine: `["scrolled, line one of file"]` (acceptable for Option A, see M3) |
| P7 | Cursor on a blank row, screen scrolls by one | follows to row 0 (correct) |
| P8 | Quiet terminal, five review keys | each key speaks exactly once (no double speech in normal use) |

## Findings (ranked)

### H1 (must fix) A stale line spoken first is never corrected when the screen was replaced

`crates/gorca-core/src/review/commands.rs:1355-1361` with `carry_terminal_cursor` (`:1380-1424`) and
`flat.rs:262-264`. When a review key speaks from a stale held screen (`speak_first`, events lost or app
unprotected) and the re-read shows nothing in common with that screen, `carry_terminal` returns `Gone`
(`(true, _) if score == 0`), the line becomes `held`, and `correct_spoken_line` is skipped. Failure (P1):
the user presses "previous line" at the prompt and hears `old 2`, a line that is no longer on the
screen (the screen now has `new 9` above the prompt); no correction follows, CurLine repeats `old 2`,
and nothing says the screen changed until a line move. On main the same keys said `new 9`. This
contradicts ADR-0018 decision 3 ("correct only a wrong line") and decision 2 ("no silent jumps").

The two rewritten unit tests changed their **inputs**, not only their expectations: the read now
returns `old 1\nnew 9\n$ ` (test 1) and `new 8\nnewer 10\n$ ` (test 2), each keeping one shared line so
that the content estimate finds continuity. With the original inputs (P1, P2) the correction never
comes. The "speak first, correct after" expectation is consistent with ADR-0018; the input change is
not justified and masks the regression.

Fix: a line spoken from a screen that was untrusted at the time (the `rr_check` of a `speak_first`
step) must not turn into a held line when the read shows no continuity; fall back to the row (as main)
and correct, or say the screen changed and read the line at that row. Restore both tests with their
original inputs as regressions (expect `["old 2"]` then the corrected `["new 9"]`).

### H2 (must fix) The correction can speak a different line than the one the cursor is on

`commands.rs:1296-1311`. For a terminal, `correct_spoken_line` re-locates the spoken line with
`carry_terminal(None, …)` (content, else the same row, no shift), independently of where
`carry_terminal_cursor` just put the cursor (which used the old screen and the shift estimate).
Failure (P3): the user hears `prog 10 percent`, then the correction `c3`, while the cursor is on
`prog 20 percent`; the next CurLine says `prog 20 percent`. Two different lines, the second one wrong.
Fix: for terminals, correct from the cursor's carried position (`self.cursor()` after
`carry_terminal_cursor`), comparing its line text with `chk.text`.

### M1 A delete at offset 0 is not always a scroll; the exact path trusts it without a check

`crates/gorca-core/src/terminal.rs:503-534`, `flat.rs:229-231`. VTE's `text_modified` diff
(`refs/vte/src/vteaccess.cc:390-449`) emits `Delete{offset 0}` whenever the first character of the
screen changes, with a length up to the common suffix: a redraw of the top rows, a tmux pane scroll
(status line keeps a common suffix, so VTE takes the diff path instead of `text_scrolled`), Ctrl+L with a
prompt. The newlines in that span are counted as scrolled rows, and the exact path returns "same row
shifted, whatever it holds now" with no notice. P4: the cursor moves silently from `ccc` to `yyy`.
Under tmux every output line would mark the reviewed line as scrolled away (held), and the next line
move says "scrolled" and jumps to the top row although the line is still on screen.
Also, `gorca-ingress` turns a `Delete` followed by an `Insert` in one drain batch into `Replaced`
(`crates/gorca-ingress/src/table.rs:514`), which clears `exact`. VTE emits the pair back to back, so the
exact path only engages when the pair splits across drains: timing-dependent behaviour.
Fix: cross-check the hint with the content (accept it only when the rows the shift keeps match, e.g.
`best_shift` score at `d = -rows` > 0 or the anchor hash at the shifted row), else fall back to the
estimate; or drop the exact path until probe P-T2 has the real VTE event sequences.

### M2 Every text edit while in review fetches a full screen snapshot

`terminal.rs:504-512`. `term_note_scroll` calls `term_snapshot` before matching the delta, for Inserts
too. In production that is `gorca-ext-host/src/term.rs:150-166`: a WASM `term_snapshot` call and a full
screen `String` clone whenever the session is dirty, which it is after each applied edit. During a
flood this is O(screen) work per event on the reader, against the 10k events/s goal. Not measured.
Fix: match `Delete { offset: 0, .. }` first and only then take the snapshot (or count newlines in the
delete's own text when present).

### M3 ADR-0018 items not delivered (follow-ups, not blockers for Option A)

- Decision 1 "row anchoring remains for grid surfaces (alternate screen)": not implemented; P6 shows
  the old shell line held inside less with no notice.
- Metrics: VL e2e set, review key-to-speech p99 at 100 lines/s, state model 50k sequences (20k run)
  are not done. The implementer's report says so.

### L1 The VS gate for "line above later" accepts any numbered line

`crates/gorca-bench/src/e2e/m6.rs:2839-2850`: with `fresh = false` a numbered answer is neither lag- nor
relation-checked. The VS change itself is justified (ADR-0018 decision 1 inverts the old "same row
reads newer text" expectation; the first run's `vs_silent 5` and `vs_stale 5` are fully explained by the
unparsed `scrolled,` prefix and the old semantics, see the notes in `20261017T152051Z.txt`). Suggest:
require `n < last_fresh` or the `scrolled,` prefix.

### L2 The state model never reaches the exact path or identical lines

`statemodel/world.rs:877-899` emits only `Replaced`, with unique `flood N` lines; `review-drift` skips
any move that lands on a held line. Add an op emitting `Delete{0}` + `Insert`, and one with repeated
lines.

### L3 Terminal review word stops changed

`segment.rs:91-117`: `--color=auto` now has five word stops (`-`, `-`, `color`, `=`, `auto`). Intended by
ADR-0018 decision 4, but user-visible; confirm with the user.

## Questions from the brief

1. Test changes: VS change justified; the two unit tests are not (H1). In normal, non-streaming use
   there is no double speech (P8): `speak_first` only engages on lost events or an unprotected app.
2. Never-block: the speak-first path has no await; the re-read is the existing terminal read with its
   own deadline; no `unwrap`/`expect`/panic added outside tests; indexing in `carry_terminal` is guarded.
3. Anchor: off-by-one none found (`anchor_row` = `y_mid / ROW_H`, exact count = newlines in the dropped
   span = rows for a true VTE scroll). Shift estimate is bounded (512 rows, O(rows²) ≈ 5·10⁵ compares).
   Ambiguous and identical-line floods hold the cursor (unit tests). Problems: H1, H2, M1, M3.
   Resize/rewrap and soft-wrapped rows were not probed (VTE's newline policy for wrapped rows unverified).
4. Scope: no file under `crates/gorca-text` changed; `TermUnits` only calls `gorca_text::seg::word_range`.
   Caret navigation and typing echo paths are untouched apart from the per-edit cost in M2 (review mode
   only) and a flag write in focus mode. `spoken()` trims trailing spaces for every flat review model,
   a no-op for `SimpleUnits` words.

## Round 2 (head dd0e4253): MERGE

Re-verified on 2026-10-17. The same probes were appended to `review/tests.rs`, run, and removed again
(the file is restored and the branch is unchanged).

| Finding | Status | Evidence (probe output on dd0e4253) |
|---|---|---|
| H1 | Fixed | P1: `["old 2"]`, then after the read `["new 9"]`, CurLine `["new 9"]`. P2: after the read `["newer 10"]`. Both rewritten tests use their original inputs again. |
| H2 | Fixed | P3: the correction says `["prog 20 percent"]` with the cursor at (1,0), the same line. New test `live_review_the_correction_speaks_the_line_the_cursor_is_on`. |
| M1 | Fixed | P4: CurLine `["ccc"]` at (2,1). The scroll count is used only when the content agrees with it (`flat.rs`, `shift_score` > 0 or the anchor's text at the shifted row). The coalescing note (Delete+Insert in one batch becomes Replaced, so the exact path depends on timing) still stands as information. |
| M2 | Fixed | `term_note_scroll` returns before any snapshot unless the delta is `Delete{offset 0}`. |

P5 to P8 are unchanged; P8 still speaks exactly once per key.

Gates: `cargo fmt --check` clean; `clippy --workspace --all-targets -D warnings` clean. `cargo test --workspace`
was run three times. Run 1 had one failing test binary (cargo stopped there: 1278 passed, 1 failed; the
name was not captured). Runs 2 and 3 were clean (run 3: 2141 passed, 0 failed). Treat it as an
unidentified flake under load average ~11 and watch for it. `target/*/incremental` was cleared afterwards.

Remaining low items (not blocking): the `best_shift` doc comment now sits above `shift_score` (rustdoc
merges the two comments); the VS "line above later" gate is loose (L1); state-model coverage gaps (L2);
the word-stop change needs user confirmation (L3); M3 follow-ups stay open.
