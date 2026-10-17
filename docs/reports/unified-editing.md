# Unified editing navigation and text editors

Date 2026-10-17. Requirement (decisions.md 2026-10-17): the terminal's caret/word/char/line model applies
identically in every editor. Live checklist: `docs/plan/checklists/editors.md`.

## The model

One presenter, `gorca_text::caret::present_word(space, from, at, EditQuirks)` plus the char/line/selection paths of
`gorca-core::text`. Terminal (ext-vte) reaches it through `gorca_ext_api::text`, so readline is on the same word and
edge model. Toolkit facts are data: `Toolkit::edit_quirks()` in `gorca-connector-atspi` (`EditQuirks.word_landing`),
carried `AppInfo -> AppProfile -> MirrorDirectory -> Ctx.quirks` (ADR-0008: no toolkit name tested in core).

Measured Ctrl+Right landings in "hello, brave world" from 0: Pango 5,12,18; GtkSourceView and Blink 5,6,12,18; Gecko
6,12,18; readline 5,12,18. All land at the END of the word, so every row says `End`; the presenter speaks the word
passed (the last span with a letter or digit), which makes Gecko, Pango and readline sound alike. A `Start` row
exists for a future Windows-style (AccessKit/Newton) connector.

Also done on this branch: GTK4 text views keep the caret across typing and no longer drop line fetches on selection
events; Return in rich editables speaks `new line` once; auto-indent is spoken after `new line`; statemodel got an
`Op::Gtk4` world flavour and invariants `caret-silent` / `caret-unit`.

## Status of the previous run (resumed after the crash)

Done: presenter, quirk row, GTK4 fixes, matrix harness (`tests/edit/run_edit.sh`, `crates/gorca-bench/src/e2e/edit.rs`,
13 targets), checklist. Not done at the crash: this report, the decisions line, full matrix with 3 repeats, gates.
Oddities found while reviewing: the "wip" checkpoint commit added ~170 e2e result files (kept, they are results);
`editors.md` claimed automatic runs that had no recorded results (now recorded below); Text Editor restored the
drafts of the previous repeat, so repeat 2 of ED-TEDIT typed into an old document (28 failures). Fixed: the harness
gives Text Editor a fresh `XDG_DATA_HOME` per start.

## Matrix results (3 repeats each, `ED_ASSERT=1`, private headless session, voicego stub speech server)

| Target | Failures (median / max over 3) | Notes |
|---|---|---|
| ED-G3E GTK3 entry, ED-G3T GTK3 textview | 0 / 0 | |
| ED-G4E GTK4 entry, ED-G4T GTK4 textview | 0 / 0 | |
| ED-GEDIT (incl. find/replace, tabs, save dialog) | 0 / 0 | |
| ED-TEDIT GNOME Text Editor | 0 / 0 (after the fresh-data-dir fix) | r1: 28 in one repeat (restored draft); r2 and ug-edn: 0 |
| ED-TERM gnome-terminal readline | 0 / 0 | |
| ED-FFIN, ED-FFTA Firefox input, textarea | 0 / 0 | |
| ED-FFCE Firefox contenteditable | 0 / 3 | dropped typed letters ("one to", "las lie") |
| ED-CHIN Chromium input | 0 / 0 | |
| ED-CHTA Chromium textarea | 0 / 1 | dropped typed letter in the fixture text before Ctrl+Down |
| ED-CHCE Chromium contenteditable | 0 / 2 (r1), 0 / 1 (ug-edc) | `BackSpace selection` silent in 2 of 6 repeats; dropped letters |

The web failures that remain are (a) the known flake class of the Firefox/Chromium typist dropping a letter, so the
fixture text differs from the expected one, and (b) one real intermittent: Backspace over a selection in a Chromium
contenteditable is sometimes silent (no deletion event reaches the reader in time). Open item, not fixed here.

## Deliberate differences (also in the checklist)

* GtkSourceView (gedit, Text Editor) and Chromium stop between a word and its comma; they say `comma` for that stop.
* Firefox takes the comma along with a deleted or selected word (`ello,`).
* Wrapped lines depend on window width; a contenteditable announces selections inside one paragraph only.
* Text Editor's GTK4 find bar sends no focus or caret events: `Search` cannot be spoken, typed letters still echo.
* Tab switches speak `edit`, not the tab name (window title changes without a focus event).
* Terminal: no selection, no Up/Down lines (history), word motions are readline's.

## Spoken strings

New strings are few: `new line`, `{n} spaces` / `{n} tabs` (auto-indent), `all selected, N characters`, `selected` /
`unselected`. Grouped in `gorca-core/src/text/caret.rs` for the M12a catalogue migration.

## Gates (final tree)

* `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo xtask deps` ok;
  `cargo test --workspace`: 2097 passed, 0 failed, 17 ignored (includes the statemodel sequence tests).
* M3 typing/caret (C1,C1h,C2,C3,E1,E2,E2f,S2,SA1-3): 26 PASS, 0 FAIL (`bench/results/e2e/fin-m3`); an earlier run
  (`ug-m3`) showed the known E2 load flake.
* M6 VH/VT: VH pass; VT failed once at host load 15 with other agents' e2e running (`fin-m6`: vt_missing max 7),
  passed 0/0/0/0 on rerun (`fin-vt2`), as did the earlier run at lower load (`ug-m6`). Kept both results.
* M8 FC1/FE/FC typing: FC1 failed twice under load (`ug-m8`: p99 374 ms, lost 12; `ug-m8b`: lost 7 in one repeat),
  then passed 3/3 repeats twice (`ug-m8c`, `ug-m8d`, p99 0.79 ms); FC1h, FE1, FCE2, FE2, FS2, FCE1: 34 PASS, 0 FAIL
  (`ug-m8e`).
* Strict K2/K3 and R1 with `--impersonate-orca`: 6 and 3 PASS, 0 FAIL (`fin-k2k3`, `fin-r1`).
* ED matrix: see above (native, Firefox, Chromium groups, 3 repeats).

## After merging main (M12a catalogue), head of this section

Indent phrases moved into the catalogue (`text-indent-spaces`, `text-indent-tabs`, en and ru plural forms); a single
tab now reads `1 tab`. Gates on the merged tree: fmt, clippy `--all-targets -D warnings`, `cargo xtask deps` and
`strings` ok; `cargo test --workspace --no-fail-fast`: 2140 passed, 0 failed, 17 ignored. M3 (`mrg-m3`, host load
15-19 from other agents): 25 PASS, E2 `e2_double` max 1 in one repeat (the known E2 load flake class, median 0).
ED-GEDIT 0/0/0; ED-TEDIT 0/0/0 in `mrg3`, but one silent save-dialog step in each of two earlier runs (`mrg`, `mrg2`)
at load 19: kept, flaky class under load, not reproduced at lower load.
