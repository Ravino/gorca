# M8 specification: Firefox navigation at NVDA browse-mode parity

Status: prep, 2026-10-15 (web-a11y-engineer). Owner of the implementation: rust-engineer on top of the M3b/M7b merge.
Inputs: `docs/plan/phase-2.md` M7 and M8, ADR-0003 (11 browse mode, 12 structural navigation, 13 live regions, 14 budgets),
ADR-0008 (engine placement), `docs/design/browse-mode.md`, `docs/design/modes.md` 4.4, `docs/plan/checklists/M7a.md`,
`refs/nvda/source/browseMode.py`, `documentBase.py`, `virtualBuffers/`, `refs/nvda/user_docs/en/userGuide.md`,
`refs/orca/src/orca/structural_navigator*.py`.
Companion files: corpus `tests/web/m8/pages/` (see `tests/web/m8/README.md`), gate harness `crates/gorca-bench/src/e2e/m8.rs`, `m8_steps.rs`, `m8_baseline.rs`, `bin/e2e_m8.rs`,
runner `tests/web/m8/run_m8.sh`, baseline `docs/reports/m8-baseline.md`.

Nothing here edits `gorca-core` (doc actor, browse state machine), `gorca-text` or the connector `text/` and `doc_source/`
modules: they belong to the M3b/M7b branch. This document says what those modules must do, and the harness says how it is measured.

## 1. How to read the matrix

Every row is one observable behaviour:

| Column | Meaning |
|---|---|
| ID | Stable id; the gate step with the same id (or id prefix) in `m8_steps.rs` checks it. `-` means no automated step (manual checklist or unit test only) |
| Key | The gorca key. gorca keeps NVDA's keys (`docs/design/modes.md` 4.4); "gorca" is the gorca modifier (Insert or Caps Lock) where NVDA uses "NVDA" |
| NVDA rule | What NVDA does and where (`refs/nvda/...`) |
| Expected speech | gorca's wording. Matching in the harness is on lower-cased tokens in any order, so the contract is the words, not their order. NVDA word order is `name role state ... value description level`, but in browse mode text is read as content with the role first ("link Home", "heading level 2 Products", "button Submit"), which is what gorca's line presenter does (browse-mode.md LP-1, LP-4) |
| Today | State on `main` (commit `ce548af`), from the code and from the baseline run: **done**, **partial** (what exists, what is missing) or **missing** (the command is declared `Planned` or absent) |
| Pri | P0 = in the M8 acceptance checklist (the 60+ scripted cases), P1 = used by every NVDA user every day, P2 = polish |

"No wrap" is NVDA's rule everywhere: at the end of the document a quick-nav key says "no next X" and the cursor stays
(`browseMode.py` `nextError` strings). gorca has the same rule (ADR-0003 SN-1).

## 2. Parity deltas that need a decision

Places where the ADR or the current key map differs from NVDA's behaviour. Each is a recommendation; none is changed in code here.

| Id | NVDA | ADR / gorca today | Recommendation |
|---|---|---|---|
| D-1 | `SWITCH_TO_PASS_THROUGH_ON_FOCUS_ROLES` = list item, **radio button**, **tab**, menu item, radio/check menu item: on a *focus* event (Tab, click, script) NVDA enters focus mode so the arrow keys can change the selection. `ALWAYS_SWITCH_TO_PASS_THROUGH_ROLES` adds combo box, edit, list, slider, tab control, menu bar, popup menu, tree, tree item, spin button, table row, grid cells (`browseMode.py:327-350`) | ADR-0003 11 lists radios as "not passing through" | Adopt NVDA's two sets in `P(n, reason)` (reason `Focus` only; `QuickNav` stays false). Without it a radio group or a tab list cannot be driven with the arrows from browse mode. Gate steps `FM.radio`, `APP.tab`, `APP.menu.item` encode NVDA |
| D-2 | **Tab** and Shift+Tab are overridden in browse mode: NVDA finds the next focusable node after the *browse cursor* in its own buffer and focuses it (`_tabOverride`, `browseMode.py:2121-2180`) | `modes.md` 4.4: Tab is "not grabbed", it reaches Firefox, which starts from its own focus point | Implement the override in the Reader: `next(cur, FOCUSABLE)` on the index plus `Action::Focus` (IR-5), falling back to the app's Tab when none. The mirror already has `Cat::FOCUSABLE`. Step `SY.tab` |
| D-3 | By default the browse-mode selection is **virtual** (Shift+arrows select in NVDA's buffer; Ctrl+C copies NVDA's plain text). `NVDA+Shift+F10` toggles *native selection mode*, which makes Firefox's own selection follow, so formatting and links are copied | `modes.md` lists select and copy rows and `gorca+Shift+F10`; nothing implemented | Virtual selection first (pure flat-text range in the doc cursor), Ctrl+C grabbed in Browse while a range exists, text to the clipboard through the output side. Native selection (`Text.AddSelection` or `SetSelection` on the Firefox document) second, behind the toggle |
| D-4 | `dialog` and `application` are `APPLICATION_ROLES`: focus enters pass-through for the whole dialog (`browseMode.py:302`) | ADR-0003 11 Modal: `aria-modal` keeps browse mode and confines the cursor | Keep gorca's design (long dialogs stay readable). Steps `APP.alert.*` test it. Non-modal `role=dialog` (cookie banners) stay in the document flow |
| D-5 | Live-region removals are never spoken | ADR-0003 LR-2: removals are spoken when `aria-relevant` has `removals` or `all` | Keep the ADR (it follows the ARIA default semantics); step `LV.remove` is P2 |
| D-6 | Quick-nav digits `1`..`9` jump to heading levels 1 to 9 (`browseMode.py:931`) | gorca has 1..6 implemented, 7..9 `Planned(M7)` | Add 7..9 (trivial: `Cat::heading_level`); the corpus has no level 7+ heading, P2 |
| D-7 | While in focus mode single letters go to the page; there is no way to use quick nav there except the toggle | same | none; step `FM.letters.key` |
| D-8 | Escape leaves *automatic* focus mode only; in manual focus mode and in `application` it is passed to the page (`disablePassThrough`, `browseMode.py:771`) | ADR-0003 11 has the same rule, plus "an expanded popup gets the first Escape" | none; the popup exception needs the `expanded` state (known gap in M7a checklist) |
| D-9 | `NVDA+Ctrl+Space` moves out of an embedded object back to the containing document | not planned | add `browse.leave_object` (P2) |
| D-10 | Orca differs: landmark = `M`, live region = `D`, last live region = `Y`, container start/end = `Shift+,` / `,`, entity list dialogs on `Shift+Alt+key`, structural navigation is a mode toggled by `Orca+Z`; Orca has no virtual buffer in Firefox, only caret navigation (`refs/orca/src/orca/structural_navigator_command_definitions.py`) | gorca's Orca preset has its own table (`modes.md` 5) | The preset keeps Orca's keys; the engine is the same. Only NVDA's keys are gated here |

## 3. Matrix

Today-column legend: **done** = implemented and covered by a unit test on main; **partial**; **missing** = the command is declared `Planned` (see `crates/gorca-core/src/commands/table.rs`) or no handler exists.
Speech is "‹…›"-quoted where a literal word matters. NVDA wording is from the strings in `browseMode.py` (`nextError`, `prevError`), `documentBase.py` (table messages) and `speech/speech.py` `getControlFieldSpeech`.

### 3.1 Quick navigation (single letters, Shift = backwards, no wrap)

Source: `BrowseModeTreeInterceptor.addQuickNav` registrations, `browseMode.py:916-1445`; user guide "Single Letter Navigation". The speech for a hit is the target's line with the container field diff (3.5); wrong-side end: `nextError` / `prevError`.

| ID | Key | NVDA rule | Expected speech | Today | Pri |
|---|---|---|---|---|---|
| QN.h.* | H | next heading | "heading level 2 ‹text›"; none: "no next heading" | done | P0 |
| QN.hl.* | 1..6 | next heading at that level; none: "no next heading at level N" | "heading level N ‹text›" | done (gorca: "no next heading level N", same tokens) | P0 |
| QN.hl.s1 | Shift+1..6 | previous at that level | as above | done in the table; the keysym for Shift+digit is the shifted symbol (`!@#$%^`): **verify the grab** | P1 |
| - | 7, 8, 9 | levels 7..9 (NVDA `range(1,10)`) | as above | missing (`Planned`) | P2 |
| QN.k.* | K | next link | "link ‹text›"; "no next link" | done | P0 |
| QN.u.1 / QN.v.1 | U / V | unvisited / visited link | "link ‹text›" / "visited link ‹text›"; "no next visited link" | done; visited state needs `VISITED` from the source (Firefox exposes it as the `visited` state) | P1 |
| QN.f.* | F | next form field: edit, button, check box, radio, combo box, list, slider... | role, name, state, value ("edit Search term blank"; "check box Subscribe not checked") | **partial**: role, state and value are spoken, the control's name is not ("edit, blank"); never switches to focus mode (baseline) | P0 |
| QN.e.* | E | next edit field (text boxes, not buttons) | "edit ‹label› ‹value or blank›" | **partial**: name missing as above | P0 |
| QN.b.* | B | next button (incl. toggle and menu buttons) | "button ‹name›" (+ "pressed" / "collapsed" states) | done; states partial | P0 |
| QN.x.* | X | next check box | "check box ‹name› checked / not checked" | **partial**: name missing | P0 |
| QN.r.* | R | next radio button | "radio button ‹name› checked / not checked" (+ "1 of 2" at verbose) | **partial**: name missing | P0 |
| QN.c.* | C | next combo box | "combo box ‹name› ‹value›" | **partial**: "combo box" only, no name, no value | P0 |
| QN.l.* | L | next list | "list with N items" then the first line | **partial**: moves to the list and speaks its items; no "list", no count (field diff, 3.5) | P0 |
| QN.i.* | I | next list item | the item text (marker first for ordered lists) | done | P0 |
| QN.t.* | T | next table; layout tables are skipped unless included | "table with C columns and R rows ‹caption›" | **partial**: moves to the table and reads its text; no "table", no counts; layout tables are skipped | P0 |
| QN.g.* | G | next graphic; empty-alt images are not graphics | "graphic ‹alt›" | done | P0 |
| QN.q.* | Q | next block quote | "block quote ‹text›" | **partial**: text only, no "block quote" | P0 |
| QN.s.* | S | next separator | "separator" | **partial**: finds it, speaks nothing | P1 |
| QN.d.* | D | next landmark; unnamed `section`/`form` are not landmarks | "‹name› ‹role› landmark" (e.g. "Primary navigation navigation landmark") | **partial**: unnamed ones are skipped, named forms count; the hit is read as the content at its start, with neither name nor role word | P0 |
| QN.m.* | M | next frame | "‹title› frame" | missing (`quicknav.next.frame` Planned) | P1 |
| QN.o.* | O | next embedded object: object, application, dialog | "embedded object" / "application ‹name›" / "dialog ‹name›" | **missing in practice**: "no next embedded object" on a page with `role=application` and `<object>` (baseline) | P1 |
| QN.n.1 | N | skip past a block of links to the next text | the text line; none: "no more text after a block of links" | missing | P1 |
| QN.a.1 | A | next annotation: comment, insertion, deletion, details | "‹text› ‹annotation role›" | missing | P2 |
| QN.w.1 | W | next spelling or grammar error | the word, "spelling error" | missing (needs `invalid:spelling` text attribute from the source) | P2 |
| QN.p.1 | P | next text paragraph | the paragraph | missing | P2 |
| - | unbound | slider, clickable, article, grouping, tab, figure, menu item, toggle button, progress bar, math, same/different style, reference (NVDA `key=None`) | as the item | missing (assignable commands declared `Planned`); live region is gorca's own `quicknav.next.live_region` | P2 |
| QN.comma.* | `,` / Shift+`,` | past the end / to the start of the container (`script_movePastEndOfContainer`, `browseMode.py:2538-2580`) | the line there; none: "Not in a container"; end of document: "Bottom" | missing (`browse.container_*` Planned) | P1 |
| QN.sl.* | gorca+Shift+Space | toggle single-letter navigation | "single letter navigation on / off" | missing | P1 |
| QN.edge.* | any | no wrap | "no next ‹type›" / "no previous ‹type›" | done | P0 |

Latency: every row above is `key_ms`, p99 5 ms (ADR-0003 14, SN-5), gate `<scn>.key_ms.p99`. PF1 repeats H / Shift+H on a 100k-node page.

### 3.2 Reading by the virtual cursor

Source: NVDA review of the buffer text (`BrowseModeDocumentTreeInterceptor` caret scripts), `browse-mode.md` 8.1. Lines are the flat text split at block nodes and soft-wrapped at 100 scalars; blank lines are skipped.

| ID | Key | NVDA rule | Expected speech | Today | Pri |
|---|---|---|---|---|---|
| RD.line | Down / Up | next / previous line | the line with inline role prefixes (link, heading level, button, edit with value); "bottom" / "top" at the ends | done | P0 |
| RD.char | Right / Left | next / previous character; spaces are "space", empty lines "blank" | the character (phonetic on hold: M3) | done; at the start of a line the role prefix is spoken again ("heading level 1, S") | P0 |
| RD.word | Ctrl+Right / Ctrl+Left | next / previous word | the word | done; same prefix repeat on Ctrl+Left to the line start | P0 |
| RD.end, RD.home | End / Home | line end / start | the character there | done; Home repeats the prefix | P1 |
| RD.bottom, RD.topkey | Ctrl+End / Ctrl+Home | document end / start | the line | done | P0 |
| RD.pgdn | Page Down / Up | 25 lines (`linesPerPage`) | the line | done | P1 |
| RD.para | Ctrl+Down / Ctrl+Up | next / previous paragraph | the paragraph | missing (M7b) | P1 |
| RD.sent | Alt+Down / Alt+Up | sentence | the sentence | missing (M7b) | P1 |
| - | gorca+Up / info keys | say line, word, character, spell | as in focus mode | done (`info.*` on the cursor in Browse, ADR-0007) | P1 |
| - | scrolling | the target is scrolled into view, debounced | no speech | done (`ScrollIntoView`, 150 ms) | P1 |

### 3.3 Tables

Source: `documentBase.py:362-606` (`_tableMovementScriptHelper`, "Not in a table cell", "Edge of table"), `virtualBuffers/gecko_ia2.py` `_getTableCellAt`. NVDA announces the *changed* headers: the row header when the row changes, the column header when the column changes (`reportTableHeaders`, default on); cell coordinates are off by default (`reportTableCellCoords`).

| ID | Key | NVDA rule | Expected speech | Today | Pri |
|---|---|---|---|---|---|
| TB.t.1 | T | enter the table by name | "table ‹caption›" + "with C columns and R rows" | partial (caption and the table text are spoken; no role word, no counts) | P0 |
| TB.into | Down | with screen layout on (NVDA default) a table row is one line | "‹cell› ‹cell› ‹cell›" | **partial**: every cell is its own line; the caption is a line before the header row | P0 |
| TB.right.*, TB.left.* | Ctrl+Alt+Right / Left | next / previous column, same row | "‹column header› ‹cell›" when the column header changes, else the cell | missing (`table.*` Planned M8) | P0 |
| TB.down.*, TB.up.* | Ctrl+Alt+Down / Up | next / previous row, same column | "‹row header› ‹cell›" when the row header changes | missing | P0 |
| TB.edge.* | any of the four at the edge | no move | "Edge of table" | missing | P0 |
| TB.notcell | any, outside a table | no move | "Not in a table cell" | missing | P1 |
| TB.first.*, TB.last.* | Ctrl+Alt+PageUp / PageDown / Home / End | first / last row, first / last column | the cell (+ changed headers) | missing | P1 |
| TB.say.row, TB.say.col | gorca+Ctrl+Alt+Left / Up | speak the row / column | all cells, headers first | missing | P2 |
| - | gorca+Ctrl+Alt+Right / Down | say-all row / column | as above, interruptible | missing | P2 |
| TB.span.* | same keys | `rowspan` / `colspan`: move to the cell in the next row or column *after* the span; the spanning cell is one cell | cell, headers | missing (the grid lookup must be built from `rowindex`/`colindex` + spans, GM 4.6) | P2 |
| TB.t.3 | T | layout tables (`role=presentation`, no headers) are not tables | not announced; content read as text; "layout tables on/off" toggle exists in NVDA (`NVDA+Ctrl+Alt+?` unbound by default) | partial: `LAYOUT_TABLE` flag exists | P0 |
| TB.grid.focus | Tab / focus | interactive `grid` cells enter focus mode (`TABLECELL`, `TABLEROW` in the always-pass-through set) | "focus mode" + the cell | partial | P1 |
| PF2 | Ctrl+Alt+Down at 12k cells | O(1) grid lookup | p99 5 ms | missing | P0 |

Header reading rule to implement (NVDA `speakTableCellHeaders`): column headers come from `th scope=col`/`columnheader`/the first row of `thead`; row headers from `th scope=row`/`rowheader`; the nearest explicit header cell wins; an `headers=` attribute is honoured when Firefox exposes `Table.GetColumnHeader`. The source already fetches row/col indices for `Cat::TABLE`; the missing piece is the cell grid (`gorca-doc::web` tables, ADR-0003 4.6) and the speech.

### 3.4 Forms, focus mode and browse mode

Source: `shouldPassThrough`, `browseMode.py:366-440`; `script_disablePassThrough`, `:771`; `autoPassThroughOnFocusChange = true`, `autoPassThroughOnCaretMove = false` (`configSpec.py:220-221`); user guide "Browse Mode". NVDA plays a sound on automatic mode changes; gorca speaks "focus mode" / "browse mode" until earcons exist (AM-11).

| ID | Trigger | NVDA rule | Expected speech | Today | Pri |
|---|---|---|---|---|---|
| FM.link, FM.btn, FM.cb | focus (Tab, script, click) on link, button, check box | no mode change | role, name, state | done | P0 |
| FM.edit, FM.textarea, FM.pw | focus on edit, multi-line, password | auto focus mode; the password value is never spoken | "focus mode", name, "edit" | done | P0 |
| FM.select, FM.slider, FM.spin, FM.aria.combo, FM.tree, FM.ce, FM.textarea, FM.readonly | focus on combo, slider, spin button, ARIA combo, tree, rich editor, multi-line, read-only edit | auto focus mode | "focus mode" + the control | done (baseline: all enter focus mode) | P0/P1 |
| FM.listbox | focus on a list box (Firefox focuses the selected option) | auto focus mode | "focus mode" + the option | missing: the option is spoken ("Cheese, list item, selected"), no mode change | P1 |
| FM.app | focus on `role=application` | focus mode | "focus mode" | missing (only the name is spoken) | P1 |
| FM.radio, APP.tab, APP.menu.item | focus on radio, tab, menu item, list item | focus mode **on focus only** (D-1) | "focus mode" + the control | missing (xfail D-1) | P1 |
| FM.app, FM.app.esc, FM.app.toggle | focus on `role=application` | focus mode; Escape does **not** leave; only the toggle does | "focus mode"; toggle: "browse mode" | partial | P1 |
| FM.esc* | Escape in automatic focus mode | back to browse mode, cursor on the control, Escape not sent to the page | "browse mode" + the line | done; the expanded-popup exception is a known gap | P0 |
| FM.toggle.* | gorca+Space | manual focus mode; sticky until toggled | "focus mode" / "browse mode" | done | P0 |
| FM.enter | Enter or Space on an edit in browse mode | switch to focus mode and focus the control | "focus mode" | done | P0 |
| FM.space | Space / Enter on a check box, button, link | activate (`DoDefault`), speak the new state | "checked" / "not checked" | done | P0 |
| FM.pass.* | Tab, Shift+Tab, Ctrl+Enter, Shift+Enter, Alt+Enter, Menu, Shift+F10 | not grabbed in browse mode (`passThrough` gesture list, `browseMode.py:899-911`) | none | done | P0 |
| FM.letters.key | letters in focus mode | reach the control | none | done (L3, FM1) | P0 |
| FM.tab.real | real Tab onto an edit | auto focus mode | "focus mode" + field | done | P1 |
| AF.script | a page script moves focus from an edit to a link | auto back to browse mode | "browse mode" + the link | done | P0 |
| SY.script.field | a script moves focus into an edit | auto focus mode | "focus mode" + field | done | P0 |
| FM.required, FM.invalid, FM.readonly | focus on required, invalid, read-only field | NVDA speaks "required", "invalid entry", "read only", the description and `aria-errormessage` | the words | missing (state and description speech is M3/M7b polish) | P1 |
| - | autofocus on load | `Loading` enters focus mode at once (AM-11) | "focus mode" | done in unit tests; not gated (needs a reader-ready handshake before the load, which the rig has not) | P1 |

### 3.5 Landmarks, regions and container field diff

Source: `speech/speech.py:getControlFieldSpeech` (`reportLandmarks`, `reportLists`, `reportTables`, `reportBlockQuotes`...), `browseMode.py` landmark quick nav, ADR-0003 11 BM-3, browse-mode.md LP-2.

| ID | Behaviour | NVDA rule | Expected speech | Today | Pri |
|---|---|---|---|---|---|
| LM.d.* | D / Shift+D walk | banner, navigation, search, main, region (named), complementary, form (named), contentinfo; unnamed `section` and unnamed `form` are skipped | "‹name› ‹landmark role›" | done | P0 |
| LM.enter | entering a landmark by arrows or quick nav | spoken once on entry ("main landmark", "Named region one region"); **leaving is silent** for landmarks | name + role before the line | missing (field diff is M7b) | P1 |
| QN.l.1, QN.t.1 | entering a list, table, block quote | "list with N items", "table with C columns and R rows", "block quote" | before the line | missing | P0 |
| - | leaving a list, table, block quote | "out of list", "out of table", "out of block quote" | after the move | missing | P1 |
| LM.skip.enter | skip link (`#main`, `tabindex=-1`) | focus event moves the cursor to the target | the target heading | done | P1 |
| - | `role=dialog` non-modal (cookie banner) | stays in the flow; entering it speaks "dialog ‹name›" | name | missing | P2 |

### 3.6 Live regions

Source: NVDA `NVDAObjects/IAccessible/mozilla.py` / `ia2Web.py` live-region events (`liveRegionChange` as text updates; `role=alert` as an alert event), gorca policy ADR-0003 13 (LR-1..LR-7, throttling table), browse-mode.md 11. Gate LV1: last mutation to speech start ≤ 65 ms wall (phase-2 M8 acceptance); the harness gates `live_ms` p99 at 65 ms plus a 35 ms rig allowance because it times from the `DoAction` call.

| ID | Region | Expected | Today | Pri |
|---|---|---|---|---|
| LV.polite | `aria-live=polite` text replaced | new text spoken when idle (priority `Notification`, never interrupts) | missing (M7b) | P0 |
| LV.assertive | `aria-live=assertive` | spoken at once, interrupting (except user speech < 1 s old) | missing | P0 |
| LV.alert | `role=alert` node added | spoken assertively, even if present at insertion (LR-4) | missing | P0 |
| LV.status, LV.log | `role=status`, `role=log` | status: text change spoken; log: only the appended entry | missing | P0 |
| LV.atomic | `aria-atomic=true` | the whole region once, last-writer-wins slot | missing | P1 |
| LV.busy | `aria-busy=true` then false | deferred, final diff once (LR-5) | missing | P1 |
| LV.off | `aria-live=off` | silent | done (nothing is spoken) | P0 |
| LV.remove | `aria-relevant=all` removal | spoken per ADR LR-2 (NVDA: not) | missing | P2 |
| LV.same, LV.same2 | identical text again | second within 10 s suppressed (dedupe) | missing | P1 |
| LV.new | region inserted with text | initial text not spoken, later change is (LR-4) | missing | P1 |
| LV.hidden | `display:none` region | silent | done | P2 |
| LV.flood, LV.flood.a | 200 updates in 1 s | bounded: polite 1/s burst 3, assertive 2/s burst 3, 4/s per document, then "N updates in ‹region›" at most every 5 s | missing | P0 |
| LV.key | key after a flood | still answered at p99 5 ms | done | P0 |
| FG.star, APP.delete, APP.toast, SP.rerender | toast / status after an action | spoken | missing | P0/P1 |
| - | background tab | muted | missing | P1 |
| - | `browse.live_regions_toggle` (gorca+5), mute a region, history | user controls | missing | P2 |

### 3.7 iframes

Source: ADR-0003 12 and browse-mode.md 12 (IF-1: one mirror per tab, nested documents are subtrees with the `FRAME` flag); NVDA buffers frames inline and reports "frame" with the title on entry.

| ID | Behaviour | Expected | Today | Pri |
|---|---|---|---|---|
| IF.walk | reading through the outer page into a frame | continuous flow; frame content is in the same line sequence | partial (AM-12 recognises nested `DocumentWeb`; verified only on `frames/` of the B-6 corpus) | P0 |
| IF.h.deep | H through four nested levels | headings of every level reachable | to measure | P0 |
| IF.f.1, IF.focus, IF.esc | field in a frame: F, focus, Escape | works as in the main document | to measure | P0 |
| IF.m.* | M next frame | "‹title› frame" | missing | P1 |
| IF.h.srcdoc, IF.end | srcdoc, sandbox, untitled, empty, missing-source frames | no crash, content reachable | to measure | P1 |
| IF.late | frame content replaced by script | silent; mirror updated | to measure | P2 |
| - | cross-process frames (Fission) | `GetTree` over each process's bus | verified in the B-6 corpus only | P1 |

### 3.8 Hidden, presentational and visually hidden content

Source: NVDA's buffer drops `aria-hidden`, `display:none`, `visibility:hidden`, the `hidden` attribute; keeps off-screen and screen-reader-only text; `role=presentation`/`none` removes the semantics but keeps the text (`ia2Web`/`gecko_ia2.py` `_normalizeControlField`).

| ID | Content | Expected | Today | Pri |
|---|---|---|---|---|
| HD.walk | `aria-hidden`, `display:none`, `visibility:hidden`, `hidden` | never read | to measure (Firefox does not expose most; `aria-hidden` subtrees are the risk, TBD-WEB-1) | P0 |
| HD.walk | sr-only, off-screen, `opacity:0` text | read | to measure | P0 |
| HD.k, HD.b | hidden links and buttons | not reachable by K, B, F | to measure | P0 |
| HD.h, HD.l | `role=presentation` on heading and list | no longer a heading or list; text kept | to measure | P1 |
| HD.walk | `alt=""` image | skipped; an image with alt text is read | to measure | P0 |
| HD.late | `hidden` toggled by script | mirror follows | to measure | P1 |

### 3.9 Same-page anchors and focus/caret sync

Source: NVDA `gecko_ia2.event_scrollingStart` (moves the cursor to a scrolled-to anchor target), `_tabOverride` (D-2), `event_caret`/`event_gainFocus` in `browseMode.py:1859-2030`; gorca BM-2: the browse cursor never moves system focus.

| ID | Behaviour | Expected | Today | Pri |
|---|---|---|---|---|
| AN.toc.go | Enter on a link to a heading | cursor at the target, target line spoken | **done** (baseline: "Section two" is spoken; Firefox reports the move) | P0 |
| AN.para.go, AN.named.go | Enter on a link to a paragraph id or a name anchor (no focus; Firefox moves the DOM caret and scrolls) | cursor at the target, target line spoken; Down continues from there | missing: `CaretMoved` is ignored in Browse. Needs a rule: a caret or selection event on the document within 300 ms after our `DoDefault` is "our own action" and moves the cursor (BM design already correlates Reader actions) | P0 |
| AN.skip, AN.after, AN.foc.go, LM.skip.enter | Enter on a link whose target takes focus (`main[tabindex=-1]`, a `div[tabindex=-1]`) | cursor at the target's first text line, spoken; Down continues after it | **partial**: the cursor lands on the container (speech: "landmark" or "panel"), the first line is not spoken, Down then reads it | P0 |
| AN.miss.go | link to a missing id | nothing, no error | to measure | P1 |
| AN.after | Down after a jump | continues from the target | partial | P0 |
| SY.focus, SY.follow | Tab (AT-SPI focus) to a link | spoken; the browse cursor is there, so Down continues after it | done | P0 |
| SY.script | script focus | cursor follows | done | P0 |
| SY.nomove | cursor moves | no focus events on the bus | done by design | P1 |
| SY.tab | Tab after moving the cursor | next focusable after the cursor (D-2) | missing | P1 |
| - | mouse click into text | cursor follows the click (caret event) | missing | P1 |

### 3.10 Say-all with navigation

Source: NVDA `sayAll.py` `CURSOR.CARET` for browse mode; any key stops; after a stop the cursor is at the spoken position. gorca: browse-mode.md 9 (`SayAllSrc::Doc`, zero IPC, chunks from flat text), `text.say_all` on gorca+Down (NVDA+Down).

| ID | Behaviour | Expected | Today | Pri |
|---|---|---|---|---|
| RD.sa.start | gorca+Down reads from the cursor to the end | continuous, paragraph by paragraph, list items, headings with their level | missing for browse (`text.say_all` works on focus-mode text) | P0 |
| RD.sa.inline | links, tables, form fields inline | as in line reading | missing | P1 |
| RD.sa.stop | any key | silence within 2 ms (L2), cursor at the chunk being spoken | missing | P0 |
| - | quick nav during say-all | stops say-all, moves, speaks the target; NVDA resumes say-all for some commands (`resumeSayAllMode`, e.g. `moveToStartOfContainer`); default: stop | missing | P1 |
| - | say-all on page load (`autoSayAllOnPageLoad = true`, setting) | starts after "browse mode ‹title›" when the setting is on | missing; default to be set (NVDA: on) | P1 |
| - | speed: utterances run ahead of the audio (SA2) | next chunk at the server before the previous ends | exists for text (SA2) | P1 |

### 3.11 Selection, copy and the clipboard

Source: NVDA virtual selection, `NativeSelectionMode` (user guide), D-3.

| ID | Key | NVDA rule | Expected speech | Today | Pri |
|---|---|---|---|---|---|
| RD.sel.right, .left, .word, .down | Shift+Right / Left, Ctrl+Shift+Right, Shift+Down | extend the virtual selection | "‹unit› selected" / "‹unit› unselected" (`speakSelectionChange`) | missing | P1 |
| RD.sel.all | Ctrl+A | select everything | "selected" | missing | P1 |
| RD.copy.say | Ctrl+C, then gorca+C | the selected plain text is on the clipboard | clipboard text | missing | P1 |
| - | gorca+Shift+F10 | toggle native selection mode | "native selection on/off" | missing | P2 |
| - | Escape / any move without Shift | clears the selection | none | missing | P1 |

### 3.12 Elements list, find, refresh and other commands

| ID | Key | NVDA rule | Expected | Today | Pri |
|---|---|---|---|---|---|
| EL.open | gorca+F7 | dialog with radio buttons (links, headings, form fields, buttons, landmarks), a filter edit, Move to / Activate buttons | opens, speaks "elements list"; selecting moves the cursor | missing | P0 |
| EL.find | gorca+Ctrl+F, gorca+F3, gorca+Shift+F3 | find text in the buffer, next, previous | dialog; hit: the line; none: "text not found" | missing | P1 |
| - | gorca+F5 | refresh the buffer | "refreshed" | missing (`browse.refresh`; here: `Rebuild`) | P2 |
| - | NVDA+d (long description), gorca+K (link destination), gorca+D (details) | `info.link_destination`, `info.details` | the text | missing (`Planned(M7)`) | P2 |
| - | gorca+Ctrl+Space | leave embedded object (D-9) | | missing | P2 |
| - | Enter on an embedded object | enters it | | partial | P2 |

The elements list is a gorca dialog (UI toolkit TBD in M8; M10 braille needs an equivalent): the gate only checks the spoken entry and exit until the dialog exists.

### 3.13 Dialogs, modal scope and web apps

Source: D-4, ADR-0003 11 Modal.

| ID | Behaviour | Expected | Today | Pri |
|---|---|---|---|---|
| APP.compose | opening a modal dialog moves focus into it | "‹dialog name›", then the focused control (focus mode for an edit) | partial | P0 |
| APP.esc | Escape in a dialog's edit | "browse mode", dialog stays | done | P0 |
| APP.alert.h2, APP.alert.k | `aria-modal` dialog **without** `aria-hidden` on the page: Shift+H, Shift+K | confined: "no previous heading", nothing from the page behind | missing (xfail D-4 / M7b) | P0 |
| APP.compose (aria-hidden app) | page hidden by `aria-hidden` | already confined by the hidden filter | to measure | P0 |
| APP.close, APP.alert.close | closing a dialog | focus returns to the opener: "Compose"; cursor there | to measure | P0 |
| APP.tab, APP.tab.sel | tab list | D-1; selection state "selected" | missing | P1 |
| APP.menu* | menu button, menu items | focus mode on menu items, "expanded" state | partial | P1 |
| APP.combo | ARIA combo box | focus mode | partial | P1 |

### 3.14 Dynamic pages and SPA

Source: ADR-0003 10 (burst classification, route swap, position restore), phase-2 M8 acceptance (cursor restore ≥ 85 %), `docs/design/browse-mode.md` 5.

| ID | Behaviour | Expected | Today | Pri |
|---|---|---|---|---|
| SP.route | client-side route change (pushState, new title, focus to the new `h1`) | new view spoken, **no stale speech** from the old view | partial (focus path) | P0 |
| SP.after | Down from the new heading | the first row of the new view | to measure | P0 |
| SP.more, SP.lastlink | infinite scroll append | silent; appended rows navigable | to measure | P1 |
| SP.rerender, SP.restore | list re-rendered with the same text and new nodes | the cursor stays on "Row 3" (matcher, position restore) | missing (M7b) | P0 |
| SP.back | browser Back | previous view spoken | to measure | P1 |
| - | virtualised list (`aria-posinset`, `aria-setsize`) | "‹item› 8 of 10000" at verbose; the mirror holds only the rendered rows | missing (TBD-WEB-2/3) | P2 |
| - | `aria-busy` skeleton | no speech until idle | missing | P2 |
| - | shadow DOM (open roots, slots, nested, `delegatesFocus`) | all content reachable as in light DOM | to measure | P0 |

### 3.15 Performance rows (gates)

| ID | Stimulus | Gate | Source |
|---|---|---|---|
| W1 (exists) | H / Shift+H at 10k nodes | p99 5 ms | ADR-0003 14 |
| PF1 | H / Shift+H at **100k** nodes | p99 5 ms, `lost` 0 | phase-2 M8 acceptance |
| PF2 | Ctrl+Alt+Down / Up at **12k cells** | p99 5 ms, `lost` 0 | ADR-0003 14 (table move) |
| LV.* | last mutation to speech | p99 65 ms (+35 ms rig allowance) | ADR-0003 21 AM-9 |
| FM.edit, FM.esc | mode switch to speech | p99 15 ms / 5 ms | ADR-0003 14, FM1/FM2 |
| (not here) W2, W2b, W3 | load, first line, tab switch | existing M7 gates | phase-2 2.1 |

### 3.16 Baseline on `main` (summary; details in `docs/reports/m8-baseline.md`)

318 scripted steps, run 2026-10-15 against `ce548af` on a busy host: 174 PASS, 55 GAP, 89 XFAIL, 0 unmarked failures. By priority: P0 105 of 176 pass, P1 62 of 127, P2 7 of 15. The 18 root causes, in the order they cost M8 the most steps:
quick nav and focus-follow do not speak the control's accessible name; container hits (list, quote, table, landmark, dialog) lack role word, name and count; table navigation past `T` does not exist and cells are one line each; live regions and browse say-all are not spoken; selection and copy are absent; a jump to a container leaves the cursor on it; anchors without focus re-announce the document; modal scope; pass-through gaps (application, grid cell, list box option, D-1); state words; position restore; nested-landmark Shift+D; silent separators; `O` finds nothing; repeated role prefix; Tab not from the cursor (D-2).
Latency: key to speech p99 0.3 to 0.7 ms in the quiet scenarios, 0.56 ms for H / Shift+H at 100k nodes (3 repeats, load 13), mode switch 1.9 ms warm and 29 to 121 ms for the first focus into an edit of a fresh page.

## 4. Corpus (`tests/web/m8/pages/`)

All pages are local static HTML, title prefix `M8`. Generated pages are produced by `gen_m8.py` on first use and are git-ignored.

| Page | Covers | Scenario |
|---|---|---|
| `quicknav.html` | every quick-nav type at least twice in a fixed order, unbound types, edges | M8-QN |
| `tables.html` | caption, `thead`/`th scope`, row headers, `colspan`/`rowspan`, layout table, ARIA grid, sparse and single-cell tables | M8-TB |
| `forms.html` | every native control, ARIA widgets that switch to focus mode, password, read-only, required, invalid, application, contenteditable | M8-FM |
| `autofocus.html` | script focus in and out of an edit | M8-AF |
| `landmarks.html` | banner, navs, search, main, named and unnamed regions, nested aside, forms, contentinfo, skip link | M8-LM |
| `live.html` | polite, assertive, alert, status, log, atomic, busy, off, removals, duplicates, new region, hidden region, floods, a timer | M8-LV |
| `iframes.html` + `frames/` | four nested levels, form frame, srcdoc, sandbox, untitled, empty, missing, late reload | M8-IF |
| `hidden.html` | aria-hidden, display:none, visibility, `hidden`, inert, sr-only, off-screen, opacity, presentation, `alt=""` | M8-HD |
| `anchors.html` | skip link, ToC links to heading, paragraph, name anchor, focusable target, missing target, query link | M8-AN |
| `sync.html` | Tab/script focus vs browse cursor, scroll | M8-SY |
| `selection.html` | line/char/word/paragraph reading, selection, copy, say-all through inline roles | M8-RD |
| `shadow.html` | open shadow roots, slots, nested components, `delegatesFocus` | M8-SH |
| `news.html` | news article: cookie dialog, banner/nav/search, figure, quote, ads, ordered list, results table, comments, forms, footer | M8-NEWS, M8-EL |
| `wiki.html` | encyclopedia: presentational infobox, ToC, `h2`/`h3` with edit links, references, data table, navbox | M8-WIKI |
| `forge.html` | code forge: tab navigation, file table with links, markdown readme, issue list with labels, pagination, diff table, toast | M8-FORGE |
| `webapp.html` | mail-like app: toolbar, tabs, menu, combo box, modal `dialog` (page hidden), modal `alertdialog` (page exposed), toasts, focus return | M8-APP |
| `spa.html` | route swap with `pushState`, in-place re-render, infinite scroll, virtualised list, busy skeleton | M8-SPA |
| `generated/large-table.html` | 1000 rows x 12 columns | M8-PF2 |
| `generated/large-100k.html` | about 100k nodes with headings, landmarks, links | M8-PF1 |
| `generated/infinite.html` | long document (200 sections) | manual, say-all soak |

Not covered by the automatic corpus, on purpose: visited links (needs history), mouse interaction, PDF.js, Fission cross-site frames (B-6 corpus), page-load autofocus (needs the reader-ready handshake before the load). Each has a line in the manual checklist (section 7).

## 5. Gate harness

`tests/web/m8/run_m8.sh <label> [--quick] [--repeats N] [--only M8-QN,...] [--no-fail]` starts the two private worlds of `run_m7.sh` (a private dbus + headless gnome-shell for Firefox's display; a private AT-SPI session with the daemon, the emulated Mutter, the mock speech server and a throw-away-profile Firefox), runs `target/release/e2e-m8`, and writes `bench/results/e2e/<label>/<UTC>.{json,txt}` and `<UTC>-steps.md`. `run_m8.sh --list` prints every scenario and step without a rig; `GORCA_M8_STEPS=QN.h,QN.k` restricts a scripted scenario to the steps whose id starts with one of the prefixes. Teardown signals only the recorded PIDs and the Firefox process group (CLAUDE.md live-session rules); nothing touches the live session.

### 5.1 Scenario ids and what they gate

| Scenario | Steps | Gates |
|---|---|---|
| M8-QN, TB, FM, AF, LM, LV, IF, HD, AN, SY, RD, SH, EL | scripted | `fail` = 0 in every repeat; `key_ms` / `mode_ms` / `live_ms` p99 |
| M8-NEWS, WIKI, FORGE, APP, SPA | scripted, real-world shaped | same |
| M8-PF1 | H / Shift+H on 100k nodes | `w1_100k_ms` p99 5 ms, `lost_w1_100k_ms` 0 |
| M8-PF2 | Ctrl+Alt+Down/Up on 12k cells | `tb1k_ms` p99 5 ms, `lost_tb1k_ms` 0; **expected fail** until tables land |

Thresholds are the ADR-0003 14 budgets: `key_ms` 5 ms (index query < 2 us, presentation < 1 ms, zero IPC), `mode_ms` 15 ms, `live_ms` 65 ms + 35 ms rig allowance (`LIVE_RIG_ALLOW_MS` in `bin/e2e_m8.rs`; the allowance is for the `DoAction` call plus the page's handler, and shrinks when the page can timestamp its own mutations). As everywhere in `bench/e2e`, a gate is the median over interleaved repeats of the per-run percentile; `fail` is the worst repeat.

### 5.2 Step statuses

| Status | Meaning | Gate effect |
|---|---|---|
| PASS | every wanted token present, none forbidden | none |
| GAP | the core tokens pass, the NVDA extras (field-diff counts, state words) do not | reported only |
| XFAIL | wrong and marked `xf("reason")`: the feature is not built | reported only |
| XPASS | marked expected-fail but passes | reported; the mark must be removed in the same commit as the feature |
| FAIL | wrong and not marked | **fails the run** (`<scn>.fail`) |

Rules for engineers: when a feature lands, remove the step's `xf(...)`; when a behaviour changes on purpose, change the step and the matrix row in the same commit; never loosen a token to make a step pass; a step that goes from PASS to FAIL is a regression even if the run is otherwise green. Timing-dependent steps carry a window (`win(ms)`); a step that is FLAKY across repeats (the steps report marks it) is a harness bug to fix, not a mark to add.

### 5.3 Adding a step

In `crates/gorca-bench/src/e2e/m8_steps.rs`, add `s("AREA.topic.n", P0|P1|P2, "what NVDA does", <act>, &["tokens"])` to the scenario's function, optionally `.not(...)`, `.extra(tokens, why)`, `.exact(...)`, `.max(n)`, `.passes_through()`, `.no_focus()`, `.win(ms)`, `.gate(Gate::Key|Mode|Live)`, `.xf(why)`. Acts: `Key`, `Repeat`, `Chord` (gorca modifier), `AppKeys` (evdev codes typed into Firefox, e.g. a real Tab), `Focus` and `Do` (AT-SPI `GrabFocus` and `DoAction` on a node found by role number and name fragment), `Top`, `Wait`, `Speak` (mock utterance length, for say-all). `Step::setup` steps position the cursor and are not judged. Unit tests in `m8.rs` cover the judging rules (`cargo test -p gorca-bench`).

### 5.4 Acceptance mapping (phase-2 M8)

| Acceptance line | Where |
|---|---|
| parity checklist, at least 60 scripted cases from `browseMode.py` | the 176 P0 steps of the 318 (`e2e-m8 list`). At M8 sign-off every P0 step is PASS: no GAP, no XFAIL, no line left in `m8_baseline.rs` for a P0 step. The baseline on `main` is 105 PASS, 40 GAP, 31 XFAIL (`docs/reports/m8-baseline.md`) |
| W1 at most 5 ms at 100k nodes | M8-PF1 |
| live-region last mutation to speech at most 65 ms | `live_ms` of M8-LV (with the rig allowance) |
| cursor restore at least 85 % on re-rendered corpus pages | M8-SPA `SP.restore` plus the B-6 re-render corpus; this harness checks the single case, the ratio needs the matcher's own counters |
| no stale speech after SPA route swaps | `SP.route`, `SP.route2` (`not` tokens) |

Host-load labelling: every report states the load average before and at the end of the run (printed by the runner). On a busy host (other agents running builds) the latency gates are not trustworthy; the status results are. Repeat on a quiet host (`bench/b1/quiet-host.sh`, reverted afterwards) before reading a latency regression as real.

## 6. Implementation order (suggested, for the M8 engineer)

1. Tables (3.3): the cell grid in `gorca-doc::web`, header lookup, the twelve `table.*` commands, "Edge of table" / "Not in a table cell", PF2. Largest P0 block, no dependency on M7b internals except the field-diff words.
2. Field diff (LP-2): list, table, block quote, landmark entry/exit; unblocks the `extra` tokens in QN, LM, NEWS.
3. Same-page anchors via caret correlation (3.9) and D-2 Tab override.
4. Live regions (3.6): classifier and token buckets are pure code (`gorca-doc::web::live`), testable without Firefox; delivery in the Reader.
5. Say-all in browse (3.10) on the ADR-0007 engine.
6. Modal scope (3.13), D-1 pass-through sets, state and description speech (required, invalid, read-only, expanded).
7. Elements list, find, selection and copy (D-3), remaining quick-nav keys (M, N, A, W, P, comma, unbound set).
8. Position restore (matcher) and SPA corpus work.

## 7. Manual checklist additions (user, real desktop; `docs/plan/checklists/M8.md` is written at sign-off)

Compare with NVDA behaviour on the same real pages: the page-load autofocus on a search site; visited links on a site you have visited; mouse click into a paragraph then Down; PDF in Firefox; a long Wikipedia article with say-all and Escape; a table-heavy site (a spreadsheet-like page) with Ctrl+Alt+arrows; GitHub issue list and diff; a site with a cookie dialog; a SPA route change on a real web mail; a live ticker (chat or sports scores) for the flood policy.

## 8. Open questions

1. Radio, tab, menu item and list item focus mode (D-1): adopt NVDA's set? (recommended)
2. Tab override (D-2): override in the Reader, or ask Firefox to move its sequential-focus start point (`Text.SetCaretOffset` on the cursor) and keep Tab unmodified? The override is deterministic and zero-IPC for the choice; the caret approach also fixes the "Tab after a mouse click" case. Recommended: override, plus caret sync for native selection mode.
3. Copy without native selection (D-3): clipboard through the Wayland data-control protocol, or only in native selection mode? The latter is less work but breaks NVDA parity for users who never toggled it.
4. Live regions in background tabs and while focus is in the address bar: ADR says muted for background tabs, spoken while focus is in the browser chrome of the active document's window; confirm.
5. NVDA announces "frame" on entry; most pages have untitled ad frames. Skip untitled frames in M (as NVDA does, it lists only frames with a name)?
