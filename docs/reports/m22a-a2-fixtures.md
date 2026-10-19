# M22a-A2: raw AT-SPI semantic fixture corpus (2026-10-19, linux-platform-engineer)

## What was produced
- `tests/fixtures/sem/pages/*.html`: 7 APG/ARIA/HTML fixtures (widgets, forms, composite, structure, tables, aria-props, names) plus 3 exotic-input pages (date, color, file).
- `tests/fixtures/sem/{firefox,chromium,gtk4}/<page>.jsonl`: one header line (versions, date, `unreadable` flag) plus one record per accessible in pre-order. Firefox ESR 153.4.0 and Chromium (Debian 13) were captured on the 7 A2 pages plus the 10 M8 pages (`m8-*`); the GTK4 gallery has 3 pages (controls, containers, accprops).
- `tools/sem-capture/`: `capture.py` (one page, one engine, plain `org.a11y.atspi.*` calls), `capture-all.sh` (one private world via `tests/shell/run_shell.sh`, every capture wrapped in `timeout -k 5 180`), `gtk4_gallery.py`, `expect_spec.py` (hand-authored expectations), `gen_expect.py` (offline: spec + corpus -> expect.toml).
- `tests/fixtures/sem/expect.toml`: 195 patterns (156 web, 39 GTK4). Per pattern: normalised role, tri-states, level, position, name and name source, required states/attributes, and per engine the raw role/ARIA token/states/interfaces/attributes/name and a status. `disagree` marks engines that differ; `missing_in_both` marks evidence neither engine exports.

## Why the previous attempt hung
Most likely a foreground wait: a monitor loop of the form `until ! pgrep -f "capture-all.sh __inner"` matches its own shell (the pattern is in its own command line) and never ends. Separately, Firefox ESR cannot expose a page containing `<input type=date>`: the document node is reachable but every call on it fails/times out, so `quiesce()` burns its 45 s deadline. Mitigations: per-capture `timeout`, an `unreadable` header flag plus a stderr warning, and the date input moved to `pages/exotic-date.html`. The full run (17 pages x 2 engines + GTK4) takes about 12 minutes; run it in the background with a log.

## Findings (input to the `inexpressible` rows and to B4)
- Neither engine exports the `aria-invalid` kind (true/spelling/grammar): only the INVALID_ENTRY state.
- Placeholder: when it is the accessible name (no label) neither engine exports it as an attribute (Blink `name-from: placeholder`); when the input has another name, Gecko exports `placeholder-text` and Blink exports `placeholder` (different keys, review fix); GTK4 exports `placeholder-text`.
- Firefox ESR 153 hangs the document tree on `<input type=date>` (reproducible, 2/2); Chromium exports it (13 nodes).
- Role differences Gecko vs Blink: alert (alert/notification), alertdialog (dialog/alert), log/timer/marquee (section,animation/log,timer,marquee), mark (mark/static), comment (comment/section), math (unknown/math), dl (list/description-list), summary (button/toggle-button), submenu trigger (menu/menu-item). The ARIA token comes through the `xml-roles` attribute where authored.
- Both prune `img alt=""`, layout tables and `aria-hidden` nodes (missing-node in both = correct).
- Firefox marks readonly textbox by omitting EDITABLE, Chromium by READ_ONLY; Firefox sets INVALID_ENTRY on an empty required input, Chromium does not.
- GTK4: switch is exported as check-box; `aria-valuetext`, `roledescription` and the mixed checked/expanded states set through the accessible API are missing on the wire in this gallery; navigation/main/feed are fillers, search is form, grid is table, level-bar for meter.
- GTK4 exports no per-widget id; fixtures carry `fx:<id>` in the description.

## Caveats
- `firefox/exotic-date.jsonl` was recaptured in review (private session, 3/3 hangs): `unreadable: true` now comes from the tool.
- `expect.toml` expectations are the ARIA-correct results drafted by the platform engineer; web-a11y-engineer review done 2026-10-19 (see below). The `st`/`at` evidence rows describe what a faithful export should carry; a failing status is an engine gap, not a test failure.
- Chromium and Firefox were run with load average ~20 on the host; counts are stable across two runs of composite/forms.
- Nothing touched the live session; each capture ran in its own private bus/Wayland world.

## Review 2026-10-19 (web-a11y-engineer)
expect.toml fixed (195 patterns, 32 disagree flags): `aria-current` invalid token is `true` (ARIA 1.2), not false; orphan `role=option` select is absent; GtkLabel with LEVEL stays `label`, not heading; menu posinset/setsize (Gecko counts per role, Blink per menu: 1/3 vs 1/9) now asserted and flagged; placeholder alias row; names/name sources asserted for figure, fieldset, labelledby, placeholder rows; implicit `aria-atomic` on status/alert (Gecko gap); ARIA 1.3 roles (mark, comment) annotated; two suspect GTK4 gallery rows (checked-mixed on GtkCheckButton, manual EXPANDED) annotated as fixture artifacts, not wire gaps. `gen_expect.py` gained alias keys (`a|b`) and pos/level checks.
