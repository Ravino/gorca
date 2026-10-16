# Word navigation fix (Ctrl+Right at word ends)

## Bug
GTK (and Gecko on Linux) put the caret at the END of the next word on Ctrl+Right, Windows-style
toolkits (NVDA's targets) at its START. gorca spoke "the word starting at the caret" (ADR-0007
CR-2), which at a word end is the following punctuation or nothing. "Привет, как дела твои?" with
GTK stops 6, 11, 16, 21, 22 spoke `, как дела ? blank` (reproduced before the fix in
`gtk_ctrl_right_speaks_the_word_just_reached_through_the_end_of_the_text`). Ctrl+Left was already
right (GTK lands on word starts).

## Fix
`gorca-text::caret::present_word(space, from, at)` (used by `Tracked` for `Say::Word`, with the
caret before the move kept in `word_from`): a forward move whose landing follows a non-space scalar
and ends the word containing it speaks that word (`w.start..at`); everything else is the word at the
caret, and `blank` at the end of the text with nothing under the caret (NVDA `speakTextInfo` of an
empty unit). Orca does the equivalent by speaking the traversed text
(`refs/orca/src/orca/script_utilities.py` `get_word_at_offset_adjusted_for_navigation`). Words use
gorca's single word rule (ADR-0007 TL-1) over the window, not `GetStringAtOffset(WORD)`; punctuation
is its own word (so landing after "?" at the end speaks "?").

## Character edges (second report)
Checked, no defect found in the machine: Right onto the last position speaks `blank`, onto a line
break speaks `blank` (VB-2, NVDA), Left onto offset 0 speaks the first character, and a key that
moves nothing at either edge speaks the unit at the caret after `edge_at`. Regression tests added.
If the user still hears nothing at an edge in a real GTK app, the cause is outside this layer
(caret event not emitted by the toolkit for a no-op key is handled by CR-3; a late event is CR-3b).
Not verified against real GTK4: no PyGObject in this host's python.

## Evidence
- Unit: `cargo test -p gorca-core text::` (new tests at the end of `text/tests.rs`), `gorca-text`
  `caret::tests`. Two of the new tests fail on the old code.
- New e2e scenario C3 (`crates/gorca-bench/src/e2e/m3.rs`): Cyrillic sentence, GTK landings both ways,
  `c3_wrong` gate 0: 0 in 3 repeats.
- Gates: fmt, clippy -D warnings, `cargo test --workspace`, `cargo xtask deps` pass.
- M3 e2e (`bench/results/e2e/word-nav-m3/`): C1, C2, C3, E1 pass; C1h p99 3.66 ms (gate 3) and one E2
  missing/double in 3 repeats failed at host load 16-20 (known flake class, see decisions 2026-10-16);
  rerun at load 10-14 in `word-nav-m3-rerun`: C1h p99 0.41 ms, E2 0/0, all pass. Strict K2/K3/R1 with `--impersonate-orca`, 3 repeats: all pass
  (`word-nav-k`).
