# Editing polish: verification (reviewer, r1)

Branch `worktree-agent-acdd27aef2e6e0fee`, head `21de4788` (9 commits on `2a017ed3`). Reviewed 2026-10-18.
Verdict: **MERGE AFTER FIXES** (three must-fix items, all small).

## Gates (run by the reviewer)

| Gate | Result |
|---|---|
| toolchain | `.toolchain` is a symlink to `<repo>/.toolchain` |
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo test --workspace` | 2276 passed, 0 failed, 17 ignored (matches the author) |
| `cargo xtask deps` / `strings` | ok (37 crates, 303 packages; 324 literals, 106 allowed) |
| `cargo xtask build-ext` (4 jobs) | both components build |
| `git merge-tree --write-tree HEAD main` | clean; main has not moved since `2a017ed3` |
| `gorca-ext-api` deps | still only `gorca-i18n` (+ `wit-bindgen` on wasm32) |
| clipboard keys grabbed? | no: `app_clipboard_shortcuts_are_never_grabbed_and_ctrl_is_not_a_grab_modifier` is unchanged and passes; the classifier only runs on PassToApp keys |
| unwrap/panic in non-test diff | none |

Host: load ~13-15, 14 GB available, swap 17 GB used. Incremental caches cleared afterwards.

## Firefox ED targets (author did not run them)

`tests/edit/run_edit.sh target/release/e2e run --only <ids> --quick --allow-stale-reader --reader daemon:<abs>/target/release/gorca,--impersonate-orca --reader-env GORCA_ED=1 --reader-env GORCA_ED_STRICT=1 --reader-env GORCA_M7=1`.
`--allow-stale-reader`: the reader was built at `abd07915`; the only later commit touches docs and `gorca-bench` e2e.
Transcripts: `bench/results/e2e/ep-verify/` (run 1) and `bench/results/e2e/ep-verify/r2/` (run 2, with `gorca_text=debug`).

| Target | ed_failures | Failing step |
|---|---|---|
| ED-FFTA | 0 / 67 | none; the clipboard steps say `copied`, `cut, brave`, `pasted, brave`, `undo`, `redo` |
| ED-FFIN | 1 / 41 (2 runs out of 2) | `clip: Ctrl+V` silent. Debug log: Firefox sent **no edit event** for the paste (the next Ctrl+Z re-inserts the cut text). The reader correctly claims nothing. This is an app or harness issue, not a reader defect |
| ED-FFCE | 2 | `clip: Ctrl+X` = `cut, brave \| blank \| blank` (new, see F-3); `BackSpace selection` is the known `[known failing]` one |

Boundary keys in Firefox (`Left at start`, `Right at end`, `Ctrl+Left 4 at start` and so on) are now silent (cues: 6 per target), as designed for `boundary = "cue"`.

## Findings, ranked

### F-1 (Medium, must fix) The leaving fix is partial: with typeahead, the last character stays stale
`crates/gorca-core/src/commands/keys.rs:49-53`. The input lane advances the shared `EpochCell` at every press
(`gorca-input-api/src/lane.rs:586`). When key 2 is pressed before the Reader has handled key 1 (key 1 being
the one that leaves review), `ExitReview` advances to e2+1. Key 1 is then given `current()` (e2+1) and
spoken, but key 2 still carries e2 < current, so the lane drops its echo. The user hears the first
character and loses the newest one. In this case main dropped both, so the branch improves things without
fixing them. Also, `current()` can pick up the epoch of a later key, which makes key 1's echo outlive key 2's
interruption.
Probe (statemodel, two presses stamped before the Reader runs): TERM and ENTRY both gave
`["x@Epoch(6)", "y@Epoch(5)(STALE)"]`, current `Epoch(6)`.
Fix: do not advance the epoch again when a key leaves review and that key's own press already advanced it
(`interrupt_on_keys`). The press has already cancelled review speech. Keep `AdvanceEpoch` only when the
press did not advance. Add the two-press regression to `statemodel/regressions.rs`.

### F-2 (Medium, must fix) "copied" in a password field
`crates/gorca-core/src/text/caret.rs:976-984`: `Copy` is confirmed whenever `t.sel` is non-empty and does
not look at `t.kind.secret`. GTK and Gecko refuse to copy from password entries, so this is the false claim
the spec forbids. Probe: `track(Role::PasswordEntry, ..)`, select 0..6, Ctrl+C gives `["copied"]`.
Fix: add `&& !t.kind.secret` (cut and paste are already confirmed by events), plus a test.

### F-3 (Medium, must fix) Firefox contenteditable: Ctrl+X says `cut, brave`, then `blank`, `blank`
ED-FFCE run 2 debug log: `Delete{0,7}`, then `Insert{0,1}` (Gecko's placeholder) and a caret event at 0.
The placeholder insert is spoken as `Inserted(Unknown)` ("blank") and the caret as an uncorrelated line
move ("blank"). Undo and redo hide the same pair because the undo slot suppresses inserts; cut does not.
Firefox is a tier-1 polish target and this is the new feature's own key.
Fix: while a Cut slot is open, a one-scalar insert at the cut offset (a blank, newline or U+FFFC) and the
caret that lands on it count as the cut's. Add an ED-FFCE-shaped unit test.

### F-4 (Low, should fix) Terminals ignore `[editing] boundary`
ext-vte's `flush_edge` (`extensions/ext-vte/src/session.rs:479-491`) still speaks the unit at the caret.
`docs/config-reference.md` says the setting applies to "an arrow, Home or End key ... at the start or end
of a line or field". Either document that terminals keep `repeat`, or carry the setting in `TermConfig`
next to `word_level`.

### F-5 (Low) Coincidence false positives in the clipboard slots (by construction)
- PD: Ctrl+Z in a widget that cannot undo, then an unrelated insert 300 ms later (autocomplete or a script)
  gives `["undo"]`, **and the inserted text is not read** (`caret.rs:2045-2054` returns early).
- PE: a script insert of ≤8 characters held 3 ms before Ctrl+V with an empty clipboard gives
  `["pasted, cdef"]`.
Both need a coincidence inside the slot. A cheap tightening for PD is to suppress the insert text only
when the undo slot also saw a delete or replace.

### F-6 (Low) The statemodel oracle accepts a cue for a key that moved the caret
`statemodel/mod.rs:613-614`: `caret-silent` passes whenever `s.cues` is non-empty, so a false boundary
(cue on a real move) can never be caught. Tightening it to "cue only if the world caret of `n` did not
move" was probed: 5000 random sequences, 0 violations. Adopt that tightening; it costs nothing now.

### F-7 (Low) WIT record changed without a package version bump
`term-config` gained `word-level` under `package gorca:ext@0.2.0`. A component built against the old
0.2.0 shape fails to instantiate against the new host. Bump the version (0.3.0) or record that 0.x
carries no ABI promise.

### F-8 (Low) The SDK twin splits ZWJ emoji sequences at fine and medium
Probe: `"👨\u{200d}👩 x-y"`: gorca-text gives `👨‍👩 |x|-|y`, the SDK gives `👨|‍|👩 |x|-|y`. The SDK
works per scalar and gorca-text per cluster. 14 other probes (URL, paths, Russian, CJK, Hebrew, Arabic,
NBSP and tab, combining marks, `v1.2.3-rc.1`, `$HOME/bin:$PATH`, `C:\Users`) agreed at every level.
`WORD_LEVEL_VECTORS` has no cluster case, so the claim that the two "cannot drift apart" holds for ASCII
and simple scalars only.

### F-9 (Info) ext-vte Left edge on a command that wraps below the prompt
`blank_below(row)` is false when the command continues on the next row, so Left at the prompt start takes
the long edge time (240 ms instead of 100 ms). The speech still comes, only later. Not documented in the
author's report.

## Checked and fine
- Boundary from a caret event with an unchanged offset. Reached only in `Awaiting` with a non-select
  Move, after the wrong-way guard. Focus-in and selection events do not open a Move slot. In the probe
  "click, then Left within HOLD (5 ms)", a cue was followed by the right character, which takes an
  unrealistic timing. A move onto an empty line changes the offset, so it is not a boundary.
- A paste over a selection (delete, then insert) gives `pasted, there` once. A paste split into two
  inserts gives `pasted, one`, then `two` (Text priority), with no second "pasted".
- Shift+arrow selection, then Ctrl+C, gives `copied`. A collapsed selection is silent.
- Word levels on Russian, CJK, RTL, URLs and paths behave as documented. Medium makes `т.е.` into `т.е|.`
  and `~/.config` into `~|/|.config`.
- The cue seam never blocks (`NullCueSink`; `play` errors are dropped). `Core::with_cues` uses the same
  `Arc::get_mut` pattern as `with_speech`.
- en+ru catalogue entries exist; config rows, `load.rs` arms, the defaults test and the
  `print-default` docs block agree.

Probes were temporary (in `text/tests.rs`, `statemodel/regressions.rs` and `gorca-text/tests/`) and
have been reverted.

## Round 2 (head 3dcf9163, 2026-10-18)

Gates rerun with 4 jobs: fmt clean, clippy `-D warnings` clean, `cargo test --workspace` 2289 passed /
0 failed / 17 ignored, `xtask deps` ok (328 literals, 109 allowed), `build-ext` rebuilds both components.
Incremental caches cleared afterwards.

Verdict: **MERGE after a rebase onto main.** No must-fix items remain in the code.

### Merge-tree vs current main: CONFLICTS (blocking the merge itself, trivial to resolve)
Main now has the Chromium flat review merge (`eb6edfbf`), and `git merge-tree --write-tree HEAD main`
reports 3 conflicts:
- `crates/gorca-model/src/text.rs`: both sides add a field to `EditQuirks` (`word_level` here,
  `text_leaves_repeat_parent` on main). Keep both.
- `crates/gorca-connector-atspi/src/toolkit.rs:114`: the same struct literal. Keep both fields.
- `docs/decisions.md`: adjacent rows.
After resolving, rerun clippy and the tests: main's `EditQuirks` literals may need `word_level`, or
`..Default::default()`.

### (1) F-1 rework: can stale speech survive the exit from review?
`review_left_by_key` now calls `on_mode_epoch(ExitReview, advance = false)`. Probe (statemodel; review a
line, then one key press reaches a terminal):
- `keyboard.interrupt = "non-modifier"` (the default): the review utterance at Epoch(3) is invalid
  after the key at Epoch(4). It is cancelled, and the echo is current. The new regression
  `two_keys_typed_ahead_after_review_are_both_echoed_with_current_epochs` covers typing ahead.
- `keyboard.interrupt = "never"`: the lane does not advance the epoch at the press, and the Reader no
  longer does either, so the review utterance stays **valid** (`Epoch(3)`, current `Epoch(3)`). The echo
  (Prio::Interrupt, `gorca-speech/src/state.rs:247`) replaces the utterance being spoken. Any further
  queued review utterance of the same epoch plays after the echo; before this branch, leaving review
  purged it.
  (Low.) Fix: pass `advance = (keyboard.interrupt == Never)`. The Reader knows the policy
  (`compile.rs:105`).
- An edge with the default policy: review speech produced from a reply that the Reader handles after the
  key was stamped but before the key message takes `current()`, which is the key's epoch, so it is not
  stale. The echo replaces it if it is being spoken; if it is queued, it plays after the echo. This needs
  a reply landing inside the typeahead window. Info only.

### (2) WIT 0.3.0
- `package gorca:ext@0.3.0` and `WIT_PACKAGE` agree (a test asserts it).
- `build-ext` rebuilt `ext-vte` and `ext-gnome-shell`. `term_parity` loads the component and passes, and
  so do the hostile and ext host tests.
- A 0.2.0 component (for example main's `target/release/extensions/ext-vte/ext_vte.wasm`) no longer
  links. The host counts that as `instantiate_failures` (`gorca-ext-host/src/slot.rs:233`) and does not
  crash; I did not exercise this.
- Doc nit: ADR-0008 (line 327) plans the web hooks for 0.3, and ADR-0019 still names 0.2.0 as the
  current contract. Update both, or note that 0.3.0 was taken by the editing settings.

### (3) Boundary "say" wording and placement
`caret.rs:1700-1708`:
- "start"/"end" for Home/End (a line edge), and for every key in a one-line field;
- "top"/"bottom" for the other keys of a text view (Up on the first line, Ctrl+Home, Left at the very
  start, Page keys);
- ru: начало / конец / начало текста / конец текста.

This is sensible and close to NVDA. One wrinkle (Low): `cue+say+repeat` and `say+repeat` emit two
Interrupt utterances in a row, and the second replaces the first in the lane (`state.rs:247`). The word
"start" is cut off by the repeated unit. Join them into one utterance ("start, h"), or document that
`say` and `repeat` do not combine.
