# M7b live checklist: reading, fields and layouts in Firefox (with M3b)

Run on the real GNOME 48 desktop with `tools/gorca-toggle.sh` (Super+Alt+G), your normal Firefox, Orca off. The gorca key is
Insert (and Caps Lock unless it switches your layout). This list extends `docs/plan/checklists/M7a.md` (everything there still
holds) and `docs/plan/checklists/M3.md` (the text rows now apply to Firefox fields).

Compare every row with what NVDA says for the same keys on the same page. Note where gorca is slower, louder, wrong or silent.

## Pages

A Wikipedia article, an MDN page, a GNOME Help page, a login form, a web mail compose window (contenteditable), a chat box.
Test pages: `tests/web/m3b/pages/fields.html` (every kind of field), `long.html` (a page to read), `frames.html` (articles and an
iframe). Open them with `file://`.

## Say-all on a page (browse mode)

| Step | Expected |
|---|---|
| Insert+Down at the top of an article | the page is read from the cursor in sentences; headings say "heading level N", links say "link"; the speech starts at once |
| While it reads, look at the page | it scrolls along (a short pause after each chunk, not on every one) |
| Press Down, Ctrl+Right or any browse key while it reads | speech stops at once; the key acts from the chunk you were hearing, not from where you started |
| Press Escape or Ctrl while it reads | it stops |
| Let it reach the bottom | it stops quietly; the cursor is on the last chunk |
| Insert+Down in the middle of a page | reads from the cursor |

## The page reads itself when it loads

| Step | Expected |
|---|---|
| Open a page (type a URL, press Enter) | when it has loaded: "browse mode", the title, then the page is read from the top |
| Follow a link (Enter on a link in browse mode) | the same for the new page |
| Press any key while it reads | it stops; nothing starts again |
| Switch tabs and back (Ctrl+PageDown, Ctrl+PageUp) | the title and the line where the cursor was; the page does **not** start reading |
| Alt+Tab to Firefox | the same: no reading |
| `[browse] say_all_on_load = false` in `gorca.toml` | the page speaks its title and first line only, as in M7a |

## Keys on the Russian layout (add Russian to Settings > Keyboard > Input Sources)

| Step | Expected |
|---|---|
| In browse mode on the Russian layout: р (the H key), л (K), ш (I), с (C), а (F) | next heading, link, list item, combo box, form field, as on the US layout |
| Shift+р | previous heading |
| 1 .. 6, and Shift+1 .. Shift+6, on either layout | next / previous heading of that level |
| Switch layouts while on the page and press H, then р | both work, with no pause or "again" |
| Type Russian text in a field | nothing is swallowed (the browse keys are off in focus mode) |
| Add or remove a layout in Settings while gorca runs | within about 10 seconds the quick-nav keys of the new layout work, the removed layout's extra grabs are gone |

## Frames and articles

| Step | Expected |
|---|---|
| M / Shift+M | next / previous frame: "frame", its title, then its first line; "no next frame" at the end |
| Quick navigation to an article (bind a key: `[keymap.browse]` `"w" = "quicknav.next.article"`) | "article", then its first line |
| Read a page with an iframe with the arrows | the frame's content follows in order |

## Escape and an open popup

| Step | Expected |
|---|---|
| Tab onto a drop-down or a combo box with a list (a site's search box with suggestions), open the list with Alt+Down | the list opens |
| Escape | the **list closes** and you stay in focus mode (no "browse mode") |
| Escape again | "browse mode" |

## Typing and the caret in Firefox fields (M3b)

Do the rows of `M3.md` in: a login field, the search box, a `<textarea>` (a comment box), a contenteditable (a mail compose
window, a rich editor), a password field.

| Step | Expected |
|---|---|
| Left / Right over Cyrillic text | the character, once |
| Left / Right over an emoji (type one with Ctrl+Shift+E in GTK apps or paste it) | the emoji is one step and is spoken as itself, never twice, never "blank" |
| Up / Down in a textarea | the line |
| Up / Down between the paragraphs of a contenteditable | the line of the new paragraph |
| Typing letters, digits, space, Enter | each once; Enter in a text area or an editor says "new line" |
| BackSpace / Delete over a letter, over an emoji | the deleted character |
| Shift+Right / Shift+Left | "x selected", "x unselected" for the change only |
| Ctrl+A | "all selected, N characters" (in a plain field) |
| Insert+Down in a long textarea or comment | reads from the caret; any key stops it; the caret is where the reading was |
| Insert+F (formatting) in a field | the font, size, bold/italic/underline, colours, language of the character under the caret |
| Tab into a field, Escape, Enter on the field, type | echo works every time after each switch |
| Password field | the characters (or bullets) are spoken; nothing is logged |

Known gaps (not done in M7b): say-all in a contenteditable reads the current paragraph only; Ctrl+A in a contenteditable says
the marks of its blocks (stripped, so "blank"); live regions, modal dialogs, container announcements ("list with 5 items"),
paragraph and sentence moves, pre-warming of background tabs (all M8 or later).

## Gates (private Firefox, not your session)

`tests/web/m7/run_m7.sh <label>` runs the M7a scenarios and the M3b and M7b scenarios below in private sessions; results land in
`bench/results/e2e/<label>/`. Scenario ids and gates are in `crates/gorca-bench/src/e2e/webtext.rs`.

| Gate | Limit |
|---|---|
| FC1h Right/Left over emoji, Firefox caret event to speech; expected text from the page's own caret events | p99 3 ms, 0 lost, 0 doubled, 0 extra, 0 swallowed |
| FC1 Down through 300 distinct lines | p99 15 ms, 0 lost |
| FE1 typed letters in an input, key to speech | p99 3 ms, 0 lost, 0 doubled, 0 extra |
| FCE2 typed letters in a contenteditable, key to speech | p99 3 ms, 0 lost, 0 doubled, 0 extra |
| FE2 randomised typing session (letters, space, BackSpace, emoji and Cyrillic commits, arrows) | 0 missing, 0 doubled, 0 key names, 0 wrong |
| FS2 Shift+arrows | p99 15 ms, 0 lost, 0 extra |
| FSA1 / FSA3 say-all in a field, stop, caret placement | first chunk p99 15 ms; stop p99 2 ms; caret at a chunk start |
| FCE1 contenteditable caret, typing, BackSpace | 0 lost, 0 doubled, 0 extra |
| FT1 browse/focus switches with a field | 0 failures |
| FP1 Escape with a popup open | 0 failures |
| BSA1 browse say-all, cursor follows, key stops | first chunk p99 5 ms, 0 failures |
| BAR1 page reads itself after load, key stops | 0 failures |
| BL1 Russian layout and Shift+digit | 0 failures |
| FR1 frame and article quick navigation | 0 failures |

How keys are scored (FC1h, FC1, FE1, FCE2, FS2, FCE1): every speech from the first key on is aligned with the keys in
order (longest common subsequence; a speech may stand for a key only if it has the key's text and comes after the key,
within 1.5 s). A key with no such speech is `lost`; a leftover speech is `fc_double` when it repeats the one before it,
else `fc_extra`. Nothing is skipped silently. Firefox under load now and then delivers a key's caret event after the
next key, which a window "until the next key" would have blamed on gorca.
