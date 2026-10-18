# Editing polish batch

Owner decisions: `docs/decisions.md` rows "Editing polish batch" (2026-10-18), "Word granularity becomes a
setting" (2026-10-17) and the follow-up of "Unified editing merged". All five items are implemented and gated.

## 1. The key that leaves review is echoed (terminal and fields)

Root cause, found with the VR rig and the reader's debug log: `review_left_by_key` sends `ExitReview`, and a mode
change advances the epoch (`ModeEffect::AdvanceEpoch`). The echo of that very key was built from the key's own
epoch, which was now one behind, so the speech lane dropped it as stale. The reader log showed
`Say { Char, Urgency::Key }` with `speak=true` and the mock server heard nothing. The statemodel's `spoken` list
records effects, not lane staleness, which is why the model never saw it.

Fix: `Reader::on_key` takes the current epoch for the key after review was left (`commands/keys.rs`). It covers
native fields as well (same path).

Tests: `statemodel::regressions::the_character_that_leaves_review_is_echoed_with_a_current_epoch` (terminal and
entry, three review commands each; fails without the fix with `the echo (epoch Epoch(4)) is stale, current
Epoch(5)`); e2e VR gained two steps (fake VTE, `vr_failures` went from 2 to 0) and VV a real gnome-terminal step
`vv_review_echo`.

## 2. Clipboard and undo confirmations (`[editing] announce_clipboard = true`)

`gorca-input-api` classifies Ctrl+C and Ctrl+Insert as `ClipOp::Copy`, Shift+Delete as Cut (Ctrl+X, Ctrl+V,
Shift+Insert, Ctrl+Z, Ctrl+Y and Ctrl+Shift+Z were already classified). The keys are never grabbed (the existing
`app_clipboard_shortcuts_are_never_grabbed...` test covers c, x, v, z, y, insert, delete). Browse mode is untouched:
the text layer only handles keys in focus mode or pass-through.

| Key | Spoken | Evidence |
|---|---|---|
| Ctrl+C | `copied` | a selection is known (`t.sel`, fetched on the selection event); otherwise silent; the selection is left alone, so a second Ctrl+C is confirmed again |
| Ctrl+X | `cut, <text>` (a selection over 256 characters is counted: `cut, 1500 characters`; no readable text: `cut`) | the delete event, also when it beat its key |
| Ctrl+V | `pasted, <text>` | the insert event (also when it beat its key, up to 8 characters; an insert with no payload: `pasted`) |
| Ctrl+Z / Ctrl+Y | `undo` / `redo`, once per key, the changed text is not read | any text change (insert, delete or replace) while the key's slot is open |

Nothing is claimed without the event: an empty clipboard, a cut with no selection and an undo in a widget that
cannot undo are silent. Editable objects only (read-only text gets no cut/paste/undo claim). Messages
`text-copied/cut/pasted/undo/redo` in en and ru. With the setting off every path is exactly the old one
(`with_the_setting_off_the_old_speech_stays`). Russian: `скопировано`, `вырезано`, `вставлено`, `отмена`, `повтор`
(a guess at what is short and clear; easy to change in the catalogue).

ED matrix: new steps `clip: select all/Ctrl+C/Ctrl+X/Ctrl+V/Ctrl+Z/Ctrl+Y/restore` in the single-line phase of
every selecting target, with per-toolkit expectations (GTK 3 has no undo: silent; gedit has no Ctrl+Y: silent).
The E2 paste expectation became `pasted, <text>`.

## 3. Boundary cue (`[editing] boundary = "cue" | "repeat" | "silent"`)

A movement key that cannot move the caret used to read the unit at the caret again (the whole line for Up/Down).
Two paths lead to a boundary: no caret event after the edge time (GTK 3, Gecko), or a caret event that reports the
same offset (GTK 4 text views). Both go through `at_boundary`: `cue` asks the cue sink for `CueId::Boundary`,
`repeat` is the old behaviour, `silent` does nothing. A slow application's late caret event is still spoken with the
key's unit (CR-3b is unchanged).

Cue seam (ADR-0017 plan, `docs/plan/phase-3-features.md` 7.3): `gorca-output-api::{CueId, CueSink, CueCaps,
NullCueSink}`, `ReaderEffect::Cue`, `Core::with_cues`, counter `CoreStats::cues`. `NullCueSink` is the silent
placeholder; M13 replaces it with the sound theme and adds the other `CueId`s.

## 4. Word granularity (`[editing] word = "auto" | "fine" | "medium" | "coarse"`)

`gorca_model::WordLevel` (the generic text-unit option), `gorca_text::seg::word_range_level`,
`CharsSpace::with_word_level`, `NativeText::with_word_level`; per-app default in `EditQuirks::word_level` (the
connector's toolkit row; every row says fine today); the user's setting wins unless it is `auto`.

- fine: the caret word rule (`--color=auto` has five stops: `-`, `-`, `color`, `=`, `auto`).
- medium: option tokens, identifiers and file names stay whole. The joiners `- . = : @ ~` join the token characters
  around them when a letter or digit follows (`--color=auto`, `foo_bar`, `file.tar.gz`, `user@host`, `10:30`,
  `.bashrc`, `-l`); a path separator, a quote, a comma, a trailing dot (`end.`) and a bare dash (`a - b`) are words
  of their own, so paths read `/`, `usr`, `/`, `local`.
- coarse: everything between blanks (Orca-like).
- Every level keeps trailing blanks in the word, and never splits a grapheme cluster.

Terminal: ext-vte reads **medium** words by default (`gorca_ext_api::terminal::DEFAULT_WORD_LEVEL`) because shell
words are option tokens and paths that fine words chop into pieces; I found no reason against it (paths stay at
fine granularity either way). `TermConfig::word_level` (WIT `term-config.word-level: option<word-level>`) carries the
user's choice; the host maps `[editing] word` to it, and terminal review (`TermUnits`, the current-word command)
uses the same level. The SDK's slice implementation (`gorca-ext-api::terminal::word_range_level`, MIT OR Apache-2.0)
and `gorca-text` agree on `WORD_LEVEL_VECTORS` (run by both crates), and `word_motion` takes the level.

Not affected: the application's own caret motion (Ctrl+Right lands where the toolkit puts it), browse mode (its
`CharsSpace` is still fine; the option is there for it), and `word_at` (whitespace-delimited by design: it names the
word a Tab completed).

## 5. ext-vte Left edge

`cannot_move(Back)` now needs the caret row to be the first row of the command (not the continuation of a command
that wrapped from a prompt row: full rows above up to a prompt) and every row below to be blank, like Right. Tests:
`left_on_a_comment_line_of_an_editor_is_not_a_cannot_move_edge` (vim `# comment`),
`left_on_a_wrapped_row_that_looks_like_a_prompt_is_not_a_cannot_move_edge` (`> out.txt`) and the positive
`left_at_the_start_of_a_command_below_output_still_speaks_at_the_short_edge_time`. A wrapped continuation is
recognised by the rows above being as long as the longest row of the screen; a screen whose widest ordinary output
line is directly above the prompt and contains a prompt marker is misread as wrapped, which only costs the longer
edge time (240 ms instead of 100 ms).

## Gates

- `cargo fmt`; `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: 2276 passed, 0 failed.
- `cargo xtask deps` (dependency, environment and strings rules) ok (`xtask/strings.allow` gained the four
  assertion messages of the new regression); `cargo xtask build-ext`: both WASM extensions build with the new WIT field.
- e2e (release, private sessions, `--quick`): ED matrix native targets ED-G3E, ED-G3T, ED-G4E, ED-G4T, ED-GEDIT,
  ED-TEDIT all pass (80 and 81 steps for the two editors, full phases); M3 C1, C1h, C2, C3, E1, E2, E2f, S2, PW
  pass; M6 fake VTE V1, VT, VH, VR, VF, VP pass; VV (real gnome-terminal, with the new review-echo step) passes with 0 failures.

## Open points

- The Russian words for undo/redo are my choice.
- Boundary cue is silent until M13: with the default a boundary key now says nothing. `boundary = "repeat"` brings
  the old speech back.
- Web targets of the ED matrix (Firefox, Chromium) were not run; the model applies the same expectations there.
- Statemodel `caret-silent` now accepts a cue as the answer of a key that moved nothing, and `caret-unit` is only
  judged when something was spoken.
