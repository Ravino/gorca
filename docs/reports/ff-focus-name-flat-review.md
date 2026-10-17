# Firefox: Tab vs quick-nav naming, and flat review of web content (2026-10-17)

Both reports reproduced in a private session (`tests/web/m8/run_m8.sh <label> --only M8-TG|M8-FL`).
Pages: `tests/web/m8/pages/telegram.html` (13 variants of a voice-message play control), `news.html`.
Scenarios: `M8-TG` (Tab then B on every variant, same name expected), `M8-FL` (numpad review over a page).

## 1. Tab says "button", B says a label

Findings on current main (before the fix), per variant:

* The focus path speaks the AT-SPI name of the focused node only. Browse speaks the document mirror
  (name, subtree text, description). For labels present at load both agree in the name text.
* Root cause reproduced: a label the page sets after the focus event (React-style state update).
  The focus is spoken bare ("button"), the one late re-read at 120 ms only catches names that arrive
  within ~120 ms (V11 at 0 ms was caught by RV-1 refine; V12 at 150 ms and V13 at 600 ms were never
  spoken). The name-changed event of the focused node was dropped: `app_actor::route` forwarded
  only STATES/VALUE changes of the focused node to the Reader. Browse showed the label because the
  document mirror does follow name events. Order of focus: Tab focuses exactly one node (the
  `role=button` element or the inner `<button>`; a wrapper with an inner button gives two stops
  that browse merges into one B stop), no wrapper-vs-inner swap was seen.
* Icon fonts: a control named by a private-use glyph spoke the glyph (silent noise, also
  disarming the late re-read because the name was "non-empty").

Fixes (commit 2e4ca740 and the follow-up):

* Name-changed events of the focused node reach the Reader; a name-only change is read and spoken
  (name alone) within `NAME_FOLLOW_WINDOW` = 2 s of the focus, ignored later (timer-like labels).
* `name_is_blank`: whitespace, controls, U+FFFC and private-use glyphs count as no name, in the
  focus utterance, the late re-read and the follow-up; browse no longer speaks lone icon glyphs.
* `web_overlay` fills a nameless button-like web control from the document mirror's subtree text
  (the name browse reads), so focus is never poorer than browse.

Remaining difference (not changed, by design of browse mode): B announces container-style
("button, Play icon label, graphic") for a button holding a named child, while Tab says
"Play icon label, button". Same name, different order.

Compared with NVDA/Orca: both speak the name the engine gives and re-announce on name-changed of
the focused object (NVDA `event_nameChange` for the focus object, Orca `onNameChanged` on the
locus of focus); gorca now does the same with a time window.

## 2. Flat review shows the window but not the content

Verdict: a bug (not a missing feature). Flat (screen) review is geometry-based and does walk
into Firefox documents, but three defects emptied the page:

1. `objnav::class` treated a node without `SHOWING` as unavailable and pruned its whole subtree.
   Gecko sets `SHOWING` on links/controls only; containers, landmarks, paragraphs and text carry
   only `VISIBLE|ENABLED`, so almost all web content was pruned (528 prunes per build). Fix: inside
   a `DocumentWeb`, `VISIBLE` is enough (`class_in(.., web)`); off-screen content is clipped by extents.
2. The document node's own text (one U+FFFC per child) produced a line whose extents covered the
   whole page and swallowed the real zones. Lines made only of U+FFFC/whitespace are dropped.
3. (Observation) The first line then reads "M8 News article document Account menu button collapsed":
   the clustering mixes chrome and content on shared rows, as for any window.

Result: `M8-FL` passes: current line, next/previous line, bottom, object navigation. Planned work in
M21 (Document review kind, ADR-0006 LS-2: review over the browse buffer) is still open and separate:
this fix makes geometry flat review work, it does not add the document line source.

## Gates
fmt, clippy `--workspace --all-targets -D warnings` clean, `cargo test --workspace`: all pass except
`gorca-daemon/tests/shell_parity.rs` (2 tests; `wasm32-wasip2` target not installed: environmental).
`cargo xtask strings` ok. See also the M8 regression run `reg1` under `bench/results/e2e/`.

## 3. Custom ARIA buttons (2026-10-18), P2-ARIA-0

Fixtures: `tests/web/m8/pages/telegram2.html` (13 icon shapes, one per row, plus an inline run),
`telegram3.html` (mounted after load). Scenarios `M8-TG2` (Tab, Down, B per shape: label present, role
word exactly once: `Step::once`), `M8-TG3`, `M8-TG4` (exploratory char/word moves). Run on Firefox with
`run_m8.sh`, on Chromium with `tests/web/m9/run_m9.sh m8`.

Reproduced (Firefox): a `role=button` whose only child is a `<canvas>` (a block with no text) is skipped
by Down. Its subtree contributes a line break only, so `translate::markers` saw "descendants > 0" and
gave the button no U+FFFC anchor; the line is blank and arrows skip it. Fix (MI-1): descendants count
only if they hold non-white-space text, and candidates are processed deepest first with each change
applied at once (a button's anchor depends on whether its icon child already has one). Unit test
`a_button_holding_only_a_textless_block_keeps_an_anchor`.

Not reproduced: Chromium passes all 40 steps before and after the fix. The doubled role on Tab did not
reproduce in either browser for 13 shapes (also with labels set after the focus, nested role=button,
wrapper plus inner button, mounted after load). The only doubled "button" found is a label that itself
contains the word ("Send button" says "Send button, button"), which NVDA also does; left unchanged.
So PR-1 has a test (`once`) but no code change. Left/Right char moves over adjacent icon buttons pass
a silent white-space stop between buttons (cosmetic).

G1: AT-SPI `INDETERMINATE` (state 32) now maps to `MIXED` in the document source (`roles.rs`).
