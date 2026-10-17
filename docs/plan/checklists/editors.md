# Editors live checklist: one editing model everywhere

Run on the real GNOME 48 desktop with `tools/gorca-toggle.sh` (Super+Alt+G); gorca must be the only screen reader.
Specification: `docs/reports/unified-editing.md` (the model, the quirk rows, the matrix results),
`docs/decisions.md` 2026-10-17, `docs/plan/phase-3.md` rows "Unified editing navigation" and "Text editors".
Keys are the NVDA desktop layout; defaults: `speech.punctuation = "some"` (a comma is "comma", a space "space").

The same keys are tried in every editor. **Every row of "The model" must sound the same in every editor**; the few
deliberate differences are in "Known differences". Everything below was run automatically in a private headless
session against the real applications (`tests/edit/run_edit.sh`, scenarios `ED-*`, 3 repeats); **not yet run on
your desktop**: the whole list.

Editors to try, in this order (the table says "in each"):

1. GNOME Terminal (bash, readline line);
2. gedit, GNOME Text Editor (new empty document, then a document with a few paragraphs);
3. any GTK3 entry (gedit's search entry, Files rename) and GTK4 entry (Settings search, Text Editor's search);
4. Firefox: an `<input>`, a `<textarea>` and a contenteditable (e.g. a mail composer, a GitHub comment box);
5. Chromium/Chrome: the same three kinds of field.

Type `hello, brave world` first (in a terminal after `echo `).

## The model (in each editor)

| # | Step | Expected |
|---|---|---|
| 1 | Type `hello, brave world` | each character once, from the key: `h e l l o comma space b r a v e space w o r l d` |
| 2 | Left | `d` (the character the caret moved onto) |
| 3 | Right | `blank` (behind the last character) |
| 4 | Home; End | `h`; `blank` |
| 5 | Left at the very start, Right at the very end | the character at the caret (`h`) and `blank`: a key that moves nothing is not silent |
| 6 | Ctrl+Left three times | `world`, `brave`, `hello` (the word the caret arrived at) |
| 7 | Ctrl+Left at the start | `hello` again |
| 8 | Ctrl+Right from the start, three times, then once more | `hello`, `brave`, `world`, then `blank`; the word you just passed, without its comma |
| 9 | BackSpace | the removed character (`d`) |
| 10 | Delete at the end | nothing |
| 11 | Home, Delete | the removed character (`h`) |
| 12 | Ctrl+BackSpace (at the end), Ctrl+Delete (at the start) | the removed word (`worl`, `ello`) |
| 13 | Shift+Right, Shift+Right, Shift+Left | `e selected`, `l selected`, `l unselected` |
| 14 | Ctrl+Shift+Right, Ctrl+Shift+Left | the word `selected`, then `unselected` |
| 15 | Shift+End, Shift+Home, Ctrl+A | `all selected, N characters` for a selection of everything; the text that goes out of the selection with `unselected` |
| 16 | Right after a selection | the selection collapses silently, then the character: `blank` at the end |

## Several lines (text views, gedit, Text Editor, textareas, contenteditable; not in a terminal or an entry)

Type `one two`, Return, a paragraph of about 100 characters that wraps in the window, Return, `last line`.

| # | Step | Expected |
|---|---|---|
| 17 | Return | `new line`, once (not also `blank`, also in a web contenteditable) |
| 18 | Up, Up, Up (through the wrapped paragraph) | one visual line each time, exactly as it is wrapped on the screen |
| 19 | Down | the next visual line |
| 20 | Ctrl+Up, Ctrl+Down | a whole paragraph |
| 21 | Ctrl+Home, Ctrl+End | `one two`, `last line` |
| 22 | PageUp, PageDown | the line the caret lands on |
| 23 | Shift+Down, Shift+Down, Shift+Up | `<line> selected`, `<line> selected`, `<line> unselected` |
| 24 | Ctrl+Shift+Down | the text added to the selection with `selected` |
| 25 | BackSpace over a selection | the removed text |
| 26 | Ctrl+Z | the restored text (not in a GTK3 text view: it has no undo) |

## gedit and GNOME Text Editor

| # | Step | Expected |
|---|---|---|
| 27 | Start with Ctrl+N or the launcher | the window title, then `edit` |
| 28 | Ctrl+F, type `bravo`, Enter, Escape | gedit: `Search`, the typed letters, the match, back in the text. Text Editor: see "Known differences" |
| 29 | Ctrl+H | gedit: the dialog (`Find and Replace, dialog`), then the field you are in; Text Editor: the field |
| 30 | Escape | back in the text |
| 31 | Ctrl+T (new tab), type, Ctrl+Alt+PageUp, Ctrl+Alt+PageDown | the focus lands in the text of each tab (`edit`); the tab's title is in the window title (Insert+T) |
| 32 | Ctrl+S | the save dialog with the name field (`Name`, the proposed name) |
| 33 | Escape | back in the text, window title spoken |
| 34 | Modified document: the window title | gedit speaks `star` before the title of a modified document (the `*` of its title) |
| 35 | Auto-indent on (Preferences), Return after an indented line | `new line`, then the indent as `N spaces` (or `1 tab`, `N tabs`) |

## Known differences (not bugs)

* **GtkSourceView (gedit, Text Editor) and Chromium** stop a Ctrl+arrow between a word and the comma behind it:
  after `hello` the next Ctrl+Right says `comma` and the one after that `brave`. The other editors skip the comma. The
  toolkit moves the caret, gorca says what it passed.
* **Firefox** takes the comma along with a word: Ctrl+Delete says `ello,` and Ctrl+Shift+Right `ello, selected`.
* **Wrapped lines** are wrapped where the window's width puts them: two editors never read the same lines.
* **A contenteditable** announces a selection inside one paragraph (`e selected`), not one that crosses paragraphs:
  Shift+Down speaks the line without `selected` (the browser sends no selection ranges for it). Pages differ.
* **GNOME Text Editor's find bar** (GTK4): the search entry sends no focus and no caret events at all. gorca cannot
  say `Search` when Ctrl+F opens it; the letters you type are still echoed and Escape returns to the text.
* **Tab switches** (gedit, Text Editor) speak `edit`, not the tab's name: the toolkit changes the window title on
  its own and gorca speaks it only with a focus or window change (follow-up in the report).
* **Terminal**: no selection (readline has none), no Up/Down lines (they are the history, M6 item 11); Ctrl+Left and
  Ctrl+Right are readline's word motions and say the word, not the path or option it is part of.

If anything speaks twice for one key, stays silent after a caret key, or reads the whole line where a character was
expected, note the editor, the keys and the toolkit version, and run `gorca+F1`.
