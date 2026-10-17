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

## Gates

See the final brief in the commit message of the closing commit and `bench/results/e2e/ug-*`.
