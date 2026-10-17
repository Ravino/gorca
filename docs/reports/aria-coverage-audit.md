# WAI-ARIA coverage audit: gorca web presentation vs NVDA, Orca and the browsers (2026-10-18)

Author: web-a11y-engineer. Read-only audit; no production code changed. Uncommitted artifacts:
`tests/web/aria/pages/probe.html`, `tests/web/aria/pages/disabled.html`, raw exposure dumps
`tests/web/aria/exposure/{firefox,chromium}-probe.txt` (Firefox 153.4.0esr, Chromium 154.0.8037.92, Debian 13,
private session, `tests/web/m8/run_m8.sh --probe ... tests/web/m8b/dump_tree.py`).

Method: (1) read `aria.rs`, `browse/present.rs`, `present/utterance.rs`, `present/flow.rs`, `doc_actor/translate.rs`,
`doc-source-atspi/{roles,wire,extras}.rs`, i18n catalogues; (2) dumped the real AT-SPI tree for ~60 ARIA constructs in
both engines; (3) compared with `refs/nvda/source` (`aria.py`, `controlTypes/*`, `speech/speech.py`
`getPropertiesSpeech` / `getControlFieldSpeech`, `virtualBuffers/gecko_ia2.py`, `NVDAObjects/IAccessible/ia2Web.py`)
and `refs/orca/src/orca/{generator,speech_generator}.py`. Speech wording for gorca rows is from the code and unit
tests, not from a live run (no gorca speech run was done in this audit; see "Not verified").

Legend. FF = Firefox, CH = Chromium. "doc" = the browse mirror state (what browse lines and quick nav read);
"focus" = the focused-node presenter (`present_web`); "chg" = state-change announcement (`present_change`).
Priority = real-world frequency in Telegram Web, Gmail, GitHub, YouTube-type apps: P1 daily, P2 weekly, P3 rare.

## 0. What exists today (short)

- One word module: `crates/gorca-core/src/aria.rs` (NVDA labels and order). Used by browse lines, quick nav, and focus
  speech. Covers: `switch tab tablist tabpanel tree treeitem menu menubar menuitem menuitemcheckbox menuitemradio
  toolbar group/radiogroup` as `Kind`; states selected/busy/pressed/checked/half checked/read only/expanded/collapsed/
  haspopup kinds/required/invalid/on/off; `aria-current` values; tree `level`; description (after a pause); error text.
- Landmarks, list/table counts, block quote, dialog, frame, article, "out of list/table" in `browse/present.rs`.
- Live regions (M8: LV scenarios), focus-mode switching by role (D-1), table navigation.
- Everything the engines expose as object attributes reaches the mirror (dynamic interned `AttrKey`s, `wire.rs`
  `AttrInterner`), but only 10 keys are named in `web_attrs` and only 6 are read by speech (`xml-roles`, `level`,
  `current`, `haspopup`, `explicit-name`, `error-text`/description). `posinset`/`setsize` are named and unused.

## 1. Coverage matrix

Columns: AT-SPI as measured (both engines unless split) | gorca today | NVDA | gap, priority.

### 1.1 Roles

| ARIA | AT-SPI (measured) | gorca today | NVDA | Gap / pri |
|---|---|---|---|---|
| button, link, heading, img, checkbox, radio, textbox, searchbox, combobox, slider, spinbutton, progressbar, separator | native roles; `xml-roles` attr when ARIA | role word via `Role::spoken`; heading level; all done | same | none |
| switch | FF/CH toggle button + checked + `xml-roles=switch` | "switch", on/off (browse, focus, chg) | switch, on/off | none |
| tab / tablist / tabpanel | page tab (+`selected`), page tab list, scroll pane; `setsize`/`posinset` attrs; `controller-for` relation | "tab selected", "tab control", "property page" (named only); D-1 focus mode | same, plus "N of M" at focus | position missing (G2, P1) |
| tree / treeitem | tree, tree item; `level`, `posinset`, `setsize`, `expandable/expanded` | "tree view item expanded level 2"; "not selected" on focus | same, "level N" only when changed; "N of M" | level every time (P3); position (G2) |
| menu / menubar / menuitem(+checkbox/radio) | FF: menuitem with `aria-haspopup` arrives as role **menu** (33) plus `xml-roles=menuitem`; CH: menu item (35) | `Kind` from `xml-roles`: handled; "submenu" word | same | none (FF quirk already folded by `Kind`) |
| toolbar, group, radiogroup | tool bar; panel + `xml-roles` | named group/toolbar announced on entry | same | entry only on quick nav/arrows, not on Tab focus (G5, P1) |
| listbox / option | list box (98) / list item; options `selectable`, `selected`; FF name has "• " marker; options carry `posinset`/`setsize` | `Role::List` ("list") + "list with N items"; option = list item; `web_listbox_lead` on focus | "list" + "with N items"; item "N of M" | word "list" not "list box" (P2); multiselectable silent (P3); position (G2) |
| combobox | combo box + `expandable`, `has-popup`, attrs `haspopup=listbox`, FF `autocomplete=list`; CH `valuetext` = shown value | role, collapsed/expanded, value from selected option (`selected_value`) | same + "autocompletes" | CH div-combobox value comes from child text; OK. autocomplete silent (P3) |
| grid / gridcell / row / columnheader / rowheader | table, table row (`rowindex`, `setsize`=rowcount in FF), column header (`sort` attr), table cell | "table with C columns and R rows" (counts from `aria-rowcount`); cells one per line; table nav | table + headers spoken when entering a cell (row/col header text) | focus on gridcell: no col/row header, no coordinates (G6, P1); `sort` silent (G7, P2); the word is "table" for an interactive grid (P3) |
| treegrid | FF/CH tree table (66); rows `level`, `expandable/expanded` | `Role::Table`; row level/expanded not spoken | tree view | G7 P2 |
| dialog / alertdialog | dialog; `aria-modal` -> MODAL | `web_dialog_lead`, container entry "name dialog"; modal confinement | dialog name + description | none (FF/CH `desc` arrives) |
| alert | FF alert (2); **CH notification (101)** + `live=assertive` | FF: `Role::Alert`; **CH: `Role::Unknown`, empty role word** | alert | G3 P1 for CH focus/browse; live speech OK via `live` attr |
| status / log / timer / marquee | FF statusbar(54)/section+live; CH statusbar/**log(111)**/**timer(115)** | status bar word; log/timer **silent word** (log->Panel; timer->Unknown) | status, no word for log | G3 P2 |
| landmarks (banner, main, nav, search, complementary, contentinfo, region, form) | landmark (110) + `xml-roles`; FF/CH both | all announced with name; unnamed region/form skipped | same | none |
| article / feed | article (109); feed = panel + `xml-roles=feed`; `posinset`/`setsize` (CH only) | "article" on quick nav; feed silent | article; feed none | feed entry + "N of M" for feed articles (P2, Facebook/Twitter-type) |
| img / figure | image; figure = `xml-roles=figure` | graphic, unlabeled; figure quick-nav declared Planned | graphic/figure | P3 |
| mark / insertion / deletion / comment / suggestion | FF: mark(127), content deletion(125)/insertion(126), comment(97); CH: mark = static + `xml-roles=mark`, same del/ins | **Unknown: no word, no annotation** (quick nav `annotation` is declared; its role set is not audited here) | "highlighted", "deleted", "inserted", "comment", "suggestion" | G3 P2 (docs editors, GitHub diffs use ins/del, PR suggestion) |
| meter | FF/CH level bar (103) | **Unknown, no word** (value not spoken) | meter via level bar -> progress | G3 P2 (YouTube volume? disk meters) |
| term / definition / math / tooltip / directory / note | term(122), definition (123 CH / paragraph FF), math (CH 113 / FF unknown), tool tip(64), list, comment(97) | term, definition, math, note: silent role word | definition, math, tool tip spoken | P3 |
| application | FF embedded(78), CH? | `Role::Application`, container entry | application, forces focus mode | none |
| details/summary (native) | **FF button + expandable; CH toggle button + expandable** | FF: "button collapsed" OK. **CH: "toggle button not pressed collapsed"** (CheckStyle::Pressed always emits "not pressed") | NVDA: toggle button without pressed state says nothing for pressed | G11 P1 (GitHub menus, FAQs; CH only) |
| aria-roledescription | attr `roledescription` (FF+CH) | **dropped** (not a named key) | replaces the role word everywhere (`ia2Web.py _get_roleText`) | G4 P1 |
| generic/presentation/none/hidden | tag-only nodes; `aria-hidden` pruned | handled (M8 HD) | same | none |

### 1.2 States

| ARIA | AT-SPI (measured) | gorca today | NVDA | Gap / pri |
|---|---|---|---|---|
| aria-expanded | `expandable` + `expanded` (FF never sets `collapsed`; source synthesises it) | words in browse, focus, chg; links/headings keep it | expanded/collapsed | none. Verify the flip after Enter in browse (EX1) |
| aria-checked true/false | `checked`, `checkable` | checked / not checked | same | none |
| aria-checked=mixed | FF/CH `indeterminate` (32) | focus: connector decode folds to MIXED, "half checked". **doc source `map_states` has no INDETERMINATE -> MIXED**: browse lines and quick nav say "not checked" | half checked | **G1 P1 bug** (Gmail "select all", GitHub select-all) |
| aria-pressed | toggle button + `pressed` (true), nothing when false; mixed = indeterminate | pressed / not pressed / half pressed | same | none for ARIA; see G11 for non-pressed toggles |
| aria-selected | `selected` + `selectable` | tab "selected"; list/tree/menu items quiet; "not selected" only if SELECTABLE and FOCUSABLE (options in both dumps are selectable but **not focusable**: activedescendant listboxes) | selected / not selected on focus | verify listbox with activedescendant (LB2); P2 |
| aria-disabled | **ENABLED and SENSITIVE absent** (FF+CH, probe `disabled.html`: buttons, menu items, links, tabs) | focus: "unavailable" for every role (DISABLED from missing SENSITIVE); browse line/quick nav only for `control` roles (button, toggle, checkbox, radio, combo, entry, spin). Menu item, tab, link, option, slider, tree item silent in browse | unavailable | G9 P2 |
| aria-readonly | CH `read-only`; **FF not exposed** on inputs (known) | "read only" for entries/checkbox only | read only | FF gap is engine side; grid cell / textbox readonly (P3) |
| aria-required, aria-invalid | `required`, `invalid-entry` (+ error-message relation 21, FF returns it on 2nd call) | required, invalid entry, error text one hop | same | none |
| aria-busy | `busy` | "busy" (browse, focus) | busy | none |
| aria-haspopup | `has-popup`, attr `haspopup` (CH maps `true` -> `menu`) | submenu / opens dialog/grid/list/tree | same | none |
| aria-multiselectable | `multiselectable` (18) | **silent** (StateSet has no bit) | "multi selectable" (setting reportMultiSelect) | G12 P3 |
| aria-modal | `modal` | confinement | dialog | none |
| aria-current | attr `current` | all 5 values + true | same | none; only for nodes that call `current_word` (links/tabs/items; not on `li`/rows in browse) P3 |
| aria-sort | attr `sort` (ascending/descending/other) on column headers | **silent** | sorted ascending/descending | G7 P2 (Gmail/GitHub tables, data grids) |
| aria-orientation | attr `orientation` | silent | silent (only vertical separators matter) | P3 |
| aria-autocomplete | FF attr `autocomplete`; CH not seen | silent | autocomplete state | P3 |
| aria-placeholder / placeholder | CH attr `placeholder`; FF name fallback | browse says "blank" | speaks placeholder when empty | P2 (search boxes) |
| aria-keyshortcuts / accesskey | `keyshortcuts` attr | silent | optional, off by default | P3 |

### 1.3 Properties and relations

| ARIA | AT-SPI | gorca today | NVDA | Gap / pri |
|---|---|---|---|---|
| aria-label / labelledby / title | `explicit-name=true` + name | M8b: authored name replaces content for link/button/tab/heading/menu items | same | none |
| aria-describedby / aria-description | `Description` property; relations 17/18 | description after pause, deduped against name | description on focus and quicknav | none |
| aria-errormessage | relation 21 (target static text) | error text appended for invalid controls | "error message" appended | none |
| aria-details | relation 19 `details` / 20 `details-for` | **silent**; `info.details` is Planned(M7) | "has details" on focus/quicknav/browse | G13 P2 |
| aria-controls | relation `controller-for`/`controlled-by` | unused | unused | none |
| aria-posinset / aria-setsize | attrs `posinset`, `setsize` on tab, radio, option, treeitem, menuitem, row (FF: rowcount), tablist/groups (CH); implicit (HTML lists) | **named, never spoken** | "N of M" on focus (`positionInfo`); Orca: only in verbose | **G2 P1** |
| aria-level | attr `level` (heading, treeitem, row, comment) | headings + tree items, always | tree/list level only when it changes | repetition (P3) |
| aria-rowcount/colcount/rowindex/colindex | attrs | counts used for "table with..." | same + coordinates optional | sparse-grid coordinates P3 |
| aria-valuetext / valuenow | attr `valuetext` (also CH combobox shown text), Value iface | slider value via Value (M8 FM.slider); valuetext attr unused | valuetext else percent | verify slider with valuetext (YouTube seek "1:23 of 4:56") SL1, P1 |
| aria-live / atomic / relevant / busy | `live`, `container-live`, `container-atomic`, `container-relevant`, `container-busy` | LV engine (M8), policy ADR-0003 13 | `liveRegionChange` | none (policy differs by design, D-5) |
| aria-hidden | pruned by engine | HD scenarios | same | none |
| aria-owns / flowto / activedescendant | reordered tree / `flows-to` / focus events on the option | owns by engine; activedescendant by engine focus events | same | none; test LB2 |
| aria-roledescription | attr `roledescription` | **dropped** | role text | G4 P1 |
| aria-colspan/rowspan | attrs on cells | span in table nav (partial) | spanned cells | P3 |

## 2. Concrete gaps with expected speech

Wording policy: English = NVDA's label where one exists; Russian = new catalogue entries in
`crates/gorca-i18n/catalog/ru/roles.msg` (`cargo xtask strings` keeps en/ru in step). Proposed keys in backticks.

### P1

**G1. Mixed check box says "not checked" in browse and quick nav (bug).**
Cause: `roles.rs::map_states` lacks `INDETERMINATE (32) -> StateSet::MIXED` (the focus path decodes it in
`connector-atspi/src/decode.rs:77`, the document source does not). Also `aria-pressed=mixed`.
Expected: browse line `check box Select all half checked` / ru `флажок Выбрать все частично отмечено`;
focus unchanged. One-line fix plus a `check_style` test. Applies to `menuitemcheckbox aria-checked=mixed`.

**G2. Position in set ("2 of 5").** `posinset`/`setsize` arrive for tab, radio, option, tree item, menu item, feed
article, row (both engines, also implicit for native lists and radios) and are never spoken.
Expected on Tab/arrow focus: `Two, tab, 2 of 2` / `Pear, list item, 2 of 3` / `docs, tree view item, collapsed, level 1, 2 of 2`
/ `Размер: M, радиокнопка, не отмечено, 2 из 2`. Keys `web-position = {n} of {total}` / `{n} из {total}`.
Rules (better than NVDA, section 3): speak at focus entry to a group and when the user asks; drop in browse lines;
ignore `setsize <= 0`, `posinset > setsize` and Chromium's group `setsize` on non-item roles (CH puts `setsize` on tablist,
listbox, tree, menubar, radiogroup: the container, not an item).

**G3. Roles with no word (Role::Unknown or Panel).** `roles.rs::map_role` sends these raw roles to `Unknown`/`Panel`:
NOTIFICATION 101 (**Chromium `role=alert`**), LOG 111, TIMER 115, LEVEL_BAR 103 (meter), MARK 127,
CONTENT_DELETION 125, CONTENT_INSERTION 126, COMMENT 97, DESCRIPTION_TERM 122, DESCRIPTION_VALUE 123, MATH 113,
TOOL_TIP 64, FEED (panel). Keep the coarse `Role` and add an `aria::Kind` entry (the `xml-roles`/`RAW_ROLE`
path is already used for Tab etc.), so no new `Role` variants are needed.
Expected words (en / ru): alert `alert / предупреждение`, mark `highlighted / выделено`, deletion `deleted / удалено`,
insertion `inserted / добавлено`, comment `comment / комментарий`, meter `meter / шкала` (+ value from Value iface),
tooltip `tool tip / подсказка`, math `math / формула`, term `term / термин`, definition `definition / определение`,
log `log / журнал`, timer `timer / таймер`, feed `feed / лента`. Inline ones (mark, ins, del) are spoken as a start/end
pair in a line like NVDA's `highlighted ... out of highlighted` only when the setting "report annotations" is on;
default: say the word once on entry.

**G4. `aria-roledescription` is dropped.** Both engines expose `roledescription`. NVDA replaces the role word with it
(browse, focus, quick nav, and braille separately). Expected: `Slide 1 of 3, carousel slide`. Rule: replace `role_word`
and the container word; do not add the states of the hidden role if the author used a custom word; if the description
already contains the name or equals the native word, ignore it (section 3). Needs a named key
(`web_attrs::ROLEDESCRIPTION`) so no dynamic-key lookup is needed in `aria.rs`.

**G5. Focus ancestry (container context on Tab).** `web_dialog_lead` and `web_listbox_lead` exist; every other container is
silent when Tab or a click lands inside it. NVDA announces new ancestors of the focus (name + role for grouping, tab
control, toolbar, tree, list, menu, dialog, region/landmark, table).
Expected (first time the focus enters the group only):
- radio group: `Size, grouping, S, radio button, checked, 1 of 2` / `Размер, группа, S, радиокнопка, отмечено, 1 из 2`
- tab list: `Views, tab control, One, tab, selected, 1 of 2`
- toolbar: `Fmt, tool bar, B, button`
- tree: `Files, tree view, src, tree view item, expanded, level 1, 1 of 2`
- menu: `File menu, menu bar` then `New, menu item`
- list box: existing "Fruit, list box with 3 items, Apple, selected".
Implementation hint: generalise `web_listbox_lead` to the `Kind::is_container()` chain computed once from the
mirror at focus time; compare with the previous focus's chain so moving between siblings is quiet.

**G6. Cells of a grid: no headers on focus.** Focus on `Ann | 3` (gridcell) says `3, table cell, selected`; NVDA adds
the changed row/column header text. Expected `Qty 3, table cell, selected` when the column changes, `Ann` when
the row changes; ru `Количество 3, ячейка, выделено`. Use the table-nav header lookup (`tablenav.rs`) from the
mirror; speak coordinates only on request. Gmail thread list, GitHub tables, spreadsheets-lite: daily.

**G11. Chromium `<summary>` says "not pressed".** CH exposes `summary` as toggle button + expandable with no
`checkable`; `CheckStyle::Pressed` always emits "not pressed". Expected `More, button, collapsed` / `Ещё, кнопка, свёрнуто`
(both engines). Rule: for `Role::ToggleButton`, speak pressed/not pressed only when the element is ARIA-pressed, i.e.
`xml-roles` is `button` with a `pressed` attr, the state PRESSED, or the CH attr `checkable=true`; otherwise treat it as a button.
`aria-pressed=false` buttons arrive as a toggle button with no state in both engines (probe `ib4`), so "not pressed" must stay for them: the discriminator is the `summary` tag (or a toggle button that is `expandable` and has no `xml-roles`).

**SL1. Slider/seek valuetext** (YouTube): `valuetext` attr arrives; confirm focus speech uses it before the numeric
value ("seek slider, 1 minute 23 seconds of 4 minutes 56 seconds"). Verification task only.

**G14. State-change announcements: complete the set and unify.** `present_change` (utterance.rs) is its own copy of
the logic with `STATE_WORDS`, not `aria.rs`. Works today for: checked / not checked, half checked, on / off (switch),
pressed / not pressed, expanded / collapsed, selected, unavailable / available, required, invalid / valid, name and value.
Missing or wrong:
- role change words use `Role::spoken`, not the ARIA word (a node that becomes `role=switch`): small;
- `aria-busy` flip, `aria-current` flip (tab strip moves current), `aria-sort` flip on the focused header
  (`Name, sorted ascending`), `aria-invalid` with the new error text, `aria-haspopup` change: silent;
- activation of a disclosure button in browse mode relies on a focus event and a state event arriving in the right
  order (click focuses; `self.focus == node` gate in `app_actor.rs:955`). Expected `Actions, expanded` right after Enter.
  Test EX1 (below) must prove it for FF and CH, including a `menu button` whose menu opens and takes focus
  (expected: `menu` entry speech wins; no stale "collapsed").
Expected speech (en / ru): `expanded / развёрнуто`, `collapsed / свёрнуто`, `selected / выделено`,
`sorted ascending / отсортировано по возрастанию`, `off / выключено`, `Like, pressed / Нравится, нажато`.

### P2

**G7. Sort and treegrid.** `sort` -> after the header name: `Name, column header, sorted ascending` (`state-sorted-asc`,
`state-sorted-desc`, `state-sorted`; ru `отсортировано по возрастанию / по убыванию / отсортировано`). Treegrid: map
raw 66 to `Table` plus `Kind::TreeGrid` (word `tree grid`, ru `древовидная таблица`); rows: `a, row, expanded, level 1`.

**G9. Disabled non-controls in browse.** `push_states` only treats `Button, ToggleButton, CheckBox, RadioButton, ComboBox,
TextEntry, PasswordEntry, SpinButton` as can-be-disabled. Add Link, Tab, MenuItem (ARIA kind), ListItem (option),
TreeItem, Slider, Cell when the node has FOCUSABLE/SELECTABLE/an ARIA widget `xml-roles`. Expected `Tab off, tab, unavailable`.
Danger: Gecko clears ENABLED on static content, so keep the "widget role" gate.

**G13. aria-details.** `has details / есть подробности` after the name on focus and on quick nav; target text via
`info.details` (Planned M7). NVDA says "has details"; Orca "has details" plus the target.

**G15. Placeholder.** Empty field with placeholder: `Search, edit, Search mail` instead of `Search, edit, blank`
(NVDA speaks the placeholder when the value is empty). CH exposes attr `placeholder`; FF exposes it as the name fallback.

**G16. List box word and feed.** `Role::List` for LIST_BOX: use `list box` (the word already exists as `web-list-box`) when
`xml-roles=listbox`; feed entry: `Feed, feed` then articles `article 3 of 20`. NVDA says "list" for listbox: deliberate improvement.

### P3

G12 multiselectable: speak `multi selectable` on entering a listbox/tree/grid only (NVDA does at every focus, we do it once);
`aria-autocomplete`; `aria-orientation`; `aria-keyshortcuts`; level repeated on every tree item (speak only on change, like
NVDA's `oldTreeLevel`); `colspan`/`rowspan` words; sparse `colindex` coordinates.

### Known gap from the owner: icon-only `role=button` with `aria-label` is skipped by browse arrows, role spoken twice on Tab

Status: **not reproduced at the speech level in this audit**; exposure verified, causes narrowed. Read this before fixing.

Exposure (probe.html `ib1..ib4`, both engines): `div[role=button][aria-label]` is a push button with the authored name and
`explicit-name=true`. Children differ: `ib1` (svg) -> unnamed image child (FF and CH); `ib2` (empty span) -> FF gives an
empty `section` child, CH gives none; `ib3` (`<button>` + `<img alt="">`) -> no child at all; `ib4` (`aria-pressed=false`)
-> toggle button, **no pressed state in either engine**. Telegram-style icon fonts add a static child holding a
private-use glyph (the unmerged `tests/web/m8/pages/telegram.html`, commit `2e4ca740` on branch
`worktree-agent-a006f03fcecc9742b`, V1..V13, already drops glyph names; main does not contain it).

What the code does for these shapes (reading `translate.rs::markers`, `present.rs::describe_from`, `cursor.rs::is_blank`):
- A button with no text and no descendants gets one U+FFFC marker (`marker_role`); a button whose only descendant is
  an unnamed image gets **no marker of its own**: the image child holds the U+FFFC and the button is announced through
  the ancestor chain (`enter_items` + `authored_name_items`, which skips the child's marker with `skip_to`). That path is
  correct on paper (`button Close dialog`), so a plain Down onto one such button should speak. Candidate causes, in the
  order I would test:
  1. **Inline run.** Icon buttons are inline: a toolbar of `[Close][Menu][Search]` is one flat-text line `\u{FFFC}\u{FFFC}\u{FFFC}`.
     Down moves line by line (so the 2nd and 3rd are "skipped"); Ctrl+Right is blind because the verbalizer strips
     U+FFFC (`verbalize.rs:129`: `unit("a\u{FFFC}b", Word) == "ab"`) and word movement treats the run as one word; Right
     (char) lands on each marker but the char path speaks "object". NVDA puts the *name text* in the buffer, so word and
     character moves stop at each control. Fix direction: in `describe`/cursor, make word and char moves over a marker
     speak the control (role + authored name), and make a marker-run word boundary per marker.
  2. **Glyph text child** (icon fonts, `::before`): the line is non-blank (`is_blank` only drops whitespace) but the speech
     is the glyph, which `Verbalizer` names or drops; `takes_authored_name` replaces the text only on the entering path,
     not when the cursor lands mid-node (Left/Right/Home). Pairs with commit `2e4ca740`.
  3. **Cursor lands on the child.** `describe_from` announces ancestors only when `eff_start(ancestor) == a`;
     a cursor placed (`web_place`, Tab-follow, quick nav) on the image node inside the button gets no button
     announcement and an "unlabeled graphic" marker (`marker_items` inserts `Item::State(unlabeled)` for a nameless image
     even though its parent button carries the name).
- Role twice on Tab: the presenter says `Name, button` (`present_web`). A second "button" must come from one of: the
  cold-tab/first-useful re-speak (`describe_node` word order, "role first": documented in `ff-tabs-urlbar.md` section 2 as
  a known difference), a repeated focus event (FF fires focus on the button and CH on the document, then the button), or
  `web_container_tail` / `web_speak_entry` describing the node after the presenter already did. Run the new test
  BT1 below with `GORCA_TAP_TRACE=1` to see the utterances and the focus events.

Recommended fix scope: (a) one rule "a marker inside an authored-name control is the control": resolve the cursor/quick-nav
node to the nearest `announced_node` ancestor whose name is authored, and speak that; (b) word/char moves over markers;
(c) dedupe: one utterance per focus; a re-read that repeats the same words within 800 ms is dropped.

## 3. Where gorca can beat NVDA

1. **One wording path for browse, focus, quick nav and change announcements.** NVDA has three code paths (buffer
   `getControlFieldSpeech`, object `getPropertiesSpeech`, `event_stateChange`) and their wording drifts (e.g. quick nav
   speaks position and level differently from focus). gorca already shares `aria.rs` for 3 of the 4; move
   `present_change` onto it (G14) and add a table test "every `(role, state)` cell speaks the same words in all four".
2. **Role/state repetition dedupe.** Skip the role word when the authored name already ends with it ("Search button"
   -> "Search button"; not "Search button, button"); skip `roledescription` equal to name; merge `menu button` +
   `haspopup=menu` + `collapsed` to `Actions, menu button, collapsed`; drop "submenu" when the role word already says it.
   The same rule removes the double-"button" family of bugs structurally (one utterance builder, items deduped by
   normalised text within an utterance).
3. **Position information with context.** NVDA repeats "2 of 5" on every arrow press and "level 2" for every tree item
   unless changed. gorca: speak `N of M` on focus entry into a group and on request (info key), not on arrowing within it;
   speak tree `level` only when it changes; give the group summary once (`Views, tab control, 3 tabs`).
4. **Smarter container entry.** One utterance for the whole chain (`Views, tab control, One, tab, selected, 1 of 3`)
   instead of NVDA's separate ancestor then control speech, so the cancel-stale rule never drops half of it.
5. **roledescription handling.** NVDA trusts any author string. gorca: trust it, but ignore when it duplicates the name or
   the native word, cap length (limits.rs), and keep the native role in the info/spell key so a hostile page cannot hide
   that a button is a button.
6. **State-change announcements with causes.** Coalesce flip bursts (expanded + live content + focus move) into one short
   utterance and drop the state word when focus moved into the opened popup (NVDA says "expanded" then the first item).
7. **Interactive grid is not "table".** `grid with 3 columns and 10 rows` / `сетка из 3 столбцов и 10 строк`; `tree grid`.
8. **List box vs list, feed, annotations** (G16, G3): words NVDA lacks or flattens.
9. **Russian wording per word class**, one catalogue, consistent gender/short form ("отмечено / не отмечено / частично
   отмечено" already): NVDA's Russian build mixes registers per control.
10. **Reliable tri-state.** Fixing G1 makes mixed check boxes consistent across all four paths; NVDA's browse mode
    mixes "half checked" and "indeterminate".

Do not copy: NVDA's "clickable" and "linked" state words, position info in browse lines, `multi selectable` on every item.

## 4. Test-corpus plan and NVDA-parity regression

### 4.1 Pages (all under `tests/web/aria/pages/`, APG patterns; each ~40-80 lines, no scripts except what the pattern needs)

| Page | Pattern (APG) | Exercises |
|---|---|---|
| `disclosure.html` | disclosure, accordion, details/summary | expanded/collapsed on Enter/Space, CH summary (G11), aria-controls |
| `buttons.html` | button, toggle, menu button, icon-only (probe `ib1..ib4`, Telegram V1..V13 shapes), inline toolbar | BT1, pressed, haspopup, role-once, inline-run navigation |
| `checkbox-radio.html` | checkbox (mixed select-all), radio group (roving tabindex, activedescendant), switch | G1, G2, G5, on/off, group name |
| `tabs.html` | tabs automatic and manual activation, `aria-selected`, `aria-current`, vertical tabs | tab control entry, selected flips, position, disabled tab |
| `listbox.html` | single, multi, scrollable, `aria-activedescendant`, grouped | LB1/LB2, list box word, multiselectable, selected/not selected on focus |
| `combobox.html` | select-only, editable with list autocomplete, grid popup | haspopup kinds, expanded flips, autocomplete, activedescendant speech |
| `menu.html` | menubar with submenus, menu button, menuitemcheckbox/radio | FF menu role quirk, submenu word, checked states, Escape |
| `tree.html` | file tree (single/multi), treegrid | level, expanded, position, G7 |
| `grid.html` | data grid with sort, row selection, headers, sparse indices | G6, sort, cell headers, coordinates |
| `table.html` | static table with caption, spans, aria-rowcount | existing TB scenarios plus counts |
| `slider-meter.html` | slider (valuetext, orientation), multi-thumb, spinbutton, progressbar, meter | SL1, meter word, value change announcements |
| `dialog.html` | modal dialog, alertdialog, non-modal cookie dialog | existing APP scenarios plus description |
| `feed-live.html` | feed, alert, status, log, timer, aria-busy | live policy, G3 words, feed position |
| `forms-aria.html` | required, invalid + errormessage, readonly, disabled, describedby, details, placeholder | G9, G13, G15, FF read-only gap |
| `annotations.html` | mark, ins, del, comment, suggestion, tooltip, math, definition | G3 words |
| `carousel.html` | roledescription, rotation control, slide groups | G4 |
| `landmarks.html` | exists in `m8` corpus | reuse |

Stability: no animation, no remote assets; every state-flip page exposes a deterministic JS handler (key -> attribute),
so e2e does not depend on timing beyond the existing 65 ms live gate.

### 4.2 E2E steps per pattern (extend `crates/gorca-bench/src/e2e/m8_steps.rs` with scenario ids `M8-ARIA-*`)

Same step format as M8 (token multiset, any order, plus an order-checked variant for the core `name role state` triple):
1. **Tab walk** (focus mode independent): Tab to each interactive node; expected tokens from the matrix (role word, state
   words, position, group lead the first time).
2. **Browse line:** Down/Up over the section; expect role-first lines.
3. **Quick nav:** the declared key (B, F, E, X, R, C, L, I, T, D, G ...) lands and speaks the same tokens as Tab.
4. **Operate:** Space/Enter/arrow; expect the single change utterance (token set) within 100 ms; no stale re-speak.
5. **Negative:** the forbidden word list per step (e.g. `BT1` forbids two "button" tokens in one utterance; `G11` forbids
   "not pressed" for summary; `G1` forbids "not checked" for mixed).
6. **Engine switch:** every scenario runs on FF and CH (`GORCA_M7_CHROMIUM`); engine-specific expectations are rows in the
   existing quirk table, not forks of the step.

Mandatory first scenarios (small, high value): `BT1` icon-only button (Tab, Down, Right, Ctrl+Right, B, Space; speech
tokens, utterance count, role count), `MX1` mixed checkbox (browse + Tab), `PI1` position (Tab through tab list, radio
group, option list), `RD1` roledescription, `EX1` Enter on disclosure/menu button in browse mode, `DS1` disabled
non-controls, `CH1` Chromium summary.

### 4.3 NVDA parity regression

1. **Reference extraction, static (no NVDA needed):** `tools/nvda-ref-words.py` parses `refs/nvda/source/controlTypes/role.py`,
   `state.py`, `aria.py` (`ariaRolesToNVDARoles`, `landmarkRoles`, sort/haspopup/current maps) with `ast` into
   `tests/web/aria/nvda-ref.json` (English labels; refreshed with `tools/fetch-refs.sh`). A Rust test in `aria.rs` loads it
   and asserts that every `Kind` word and state word equals the reference except entries listed in
   `tests/web/aria/deviations.toml` (each with a reason: "list box", "grid", "highlighted" etc.). A refs update that
   changes a label breaks the test and forces a conscious decision.
2. **Order table:** same script emits NVDA's `State` bit order and the `getControlFieldSpeech` sequence
   (`name, role, states, containerContains, current, details, value, description, level`). A test builds the cross
   product `role x state subset` and compares with `aria::state_words` ordering (the existing M8b order tests are
   hand-written; this makes them exhaustive).
3. **Exposure snapshots:** keep `tests/web/aria/exposure/<engine>-probe.txt` from `dump_tree.py` on `probe.html` and
   `disabled.html`, with the browser version in the header; a weekly job diffs a fresh dump. A mapping change in Firefox
   or Chromium (the FF `menu` quirk, CH `summary`, the absent ENABLED on aria-disabled) shows up as a diff before users
   hear it.
4. **Golden transcripts from real NVDA (optional, once):** owner records NVDA speech-viewer text on the APG pages
   (Windows, same Firefox ESR) into `tests/web/aria/golden/<page>.nvda.txt` with NVDA version and date. The gate compares
   token sets after normalisation (case, punctuation, the deviation list) and reports `missing`, `extra`, `order`.
   Intentional improvements are kept in `deviations.toml`, so a deviation is a decision recorded in git and the catalogue
   (also in `docs/decisions.md` per project rules), never a silent drift.
5. **Matrix as gate input:** the rows of section 1 get ids (`ARIA.state.mixed`, `ARIA.role.mark`, ...) in the same
   `m8-spec` style, so `e2e-m8 list` shows coverage and an `xfail` marker moves to `pass` when a gap closes.

## 5. Implementation order (smallest risk first)

1. G1 (state map), G11 (summary), BT1 repro and the double-role dedupe: bugs, no new words.
2. G3 + G4 + G16 words in `aria.rs` and catalogue (en/ru), `WebQuirks` rows for engine differences.
3. G2 position + G5 focus ancestry (needs the group chain from the mirror; settings `web.position = off|focus|always`).
4. G6/G7 grid headers and sort (reuses `tablenav.rs` header lookup).
5. G14 unify change announcements onto `aria.rs`; EX1 verification; G9, G13, G15.

## Not verified / risks

- No live gorca speech run in this audit: all "today" wording comes from code and unit tests. BT1/EX1/LB2/SL1 are the
  first things to run. The icon-button cause is a ranked hypothesis list, not a finding.
- `2e4ca740` (name follow, glyph names, nameless web control filled from the mirror) is not on `main`; behaviour on `main`
  may differ from what the owner heard if they run that branch, and its V1..V13 page should be merged into the corpus.
- FF `aria-readonly` on inputs and `aria-multiselectable` details were taken from the earlier M8b facts and one probe;
  Chromium `aria-roledescription` on a plain `div role=button` verified, on landmarks not tried.
- NVDA behaviours are from source (refs/nvda at the pinned checkout), not from running NVDA; Russian strings are proposals
  and need the owner's review.
- Position info and group lead add words: verbosity settings and the 15 ms gorca-side budget must be re-measured (all
  data is in the mirror, no new IPC, so the cost should be string building only).
