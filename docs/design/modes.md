# gorca modes and keymaps

Status: normative for M1 (desktop modes, NVDA-style keymaps, Orca preset). Sections marked with a later
milestone are **reserved**: their keys are fixed now so that no M1 binding collides with them, and the
commands are inert (`Availability::Planned`, ADR-0005 KC-10) until their milestone lands.

Sources: ADR-0001 §7.2–7.3, ADR-0003 §11–12, ADR-0005; NVDA `refs/nvda/source/globalCommands.py`,
`browseMode.py`, `cursorManager.py`, `documentBase.py`, `user_docs/en/userGuide.md` §"Starting and
stopping NVDA"…; Orca `refs/orca/src/orca/*_command_definitions.py` (extracted 2026-10-13 from Orca main
`a968e8a`).

## 1. Modes

```rust
enum Mode {
    Focus,
    Browse      { doc: DocId, cursor: DocCursor, sticky: bool },   // ADR-0003 WebMode::Browse
    Review      { origin: NodeKey, cursor: ReviewCursor },
    PassThrough { scope: PassScope, resume: BaseMode },             // OneKey | UntilToggle
    InputHelp   { resume: BaseMode },
}
enum BaseMode { Focus, Browse { doc, cursor, sticky }, Review { origin, cursor } }   // Copy
```

| Mode | Purpose | Keys reaching gorca as commands | Keys reaching the app | Entered by | Left by | Earcon | Milestone |
|---|---|---|---|---|---|---|---|
| Focus | follow system focus | gorca chords; desktop layout from M4: numpad review keys | everything else | start; leaving any other mode | entering another mode | `mode.focus` when coming from Browse | M1 |
| Browse | virtual cursor over the document mirror | gorca chords, quick-nav letters, cursor keys, Enter/Space, Escape only in Focus{Auto} | modified keys not in the browse set (Ctrl+Enter, Alt+…, Tab…) | focus enters a web document; `browse.toggle` | focus leaves the document, editable focus (auto), `browse.toggle` | `mode.browse` | M7 |
| Review | review / object cursor detached from focus | gorca chords + review keys | everything else | first review or object command (NVDA), `review.toggle` (Orca preset) | `review.to_focus`, focus change when `review.focus_follows = true`, `review.toggle` | none | M4 |
| PassThrough{OneKey} | send the next key or chord to the app | none (grab set empty) | everything | `input.pass_next_key` | first non-modifier press, 10 s idle | none; spoken "pass next key" | M1 |
| PassThrough{UntilToggle} | per-app sleep mode | only `input.sleep_toggle` | everything else, gorca chords swallowed | `input.sleep_toggle`; focus returns to a sleeping app | `input.sleep_toggle`; focus leaves the sleeping app | none | M6 |
| InputHelp | learn mode: describe, do not execute | all keys (`GrabKeyboard`), only bypass commands execute | only the modifier double tap | `input.learn_mode_toggle` | toggle, Escape, 60 s idle, Reader stall (ADR-0005 LM-3) | none | M1 |

Mode-wide invariants:

- **MD-1** Every transition advances the epoch and emits `SetGrabSet { set, ctx }` (ADR-0005 §5.2).
- **MD-2** `PassThrough` and `InputHelp` never nest: entering one of them from the other replaces it
  and keeps the original `resume`.
- **MD-3** Focus events still update the Focus state machine in every mode; in `InputHelp` they are
  presented after learn mode ends only if still current (epoch rule).
- **MD-4** Sleeping apps are a Reader set (`sleeping: HashSet<AppId>`), not a mode field: the mode becomes
  `PassThrough{UntilToggle}` while focus is in such an app and returns to `resume` when it leaves.

## 2. Grab sets

`M` = gorca modifiers (`Insert`, `KP_Insert`, `Caps_Lock` unless dropped by the layout guard). Chord
entries are compiled by ADR-0005 K-8a (all layout groups, Caps Lock variants; keypad without Num Lock
variants, K-6a). Only implemented commands are grabbed (K-1).

| Mode | M1 | M4 adds | M7 adds |
|---|---|---|---|
| Focus | `M` | desktop layout: unmodified and Shift+ keypad review keys (§4.3) | — |
| Review | `M` | same as Focus | — |
| Browse | — | — | `M` + browse set (§4.4): quick-nav letters (± Shift), digits 1–9 (± Shift), `comma` (± Shift), arrows, Ctrl/Shift/Alt+arrows per §4.4, Home/End (± Ctrl, ± Shift), Prior/Next (± Shift), Return, KP_Enter, space, Ctrl+A, Ctrl+C, Ctrl+Alt+table keys; Escape only in Focus{Auto} |
| PassThrough{OneKey} | ∅ | ∅ | ∅ |
| PassThrough{UntilToggle} | — | — | `M` (M6) |
| InputHelp | `GrabKeyboard` + `M` | same | same |

When `browse.single_letter_toggle` turns single-letter navigation off, the letters, digits and comma
leave the Browse grab set.

## 3. Transition table

`r` = the current base mode (`resume`). Messages are `ModeMsg` variants (`gorca-core/src/mode.rs`);
new ones are marked *new*.

| From | Message | To | Notes |
|---|---|---|---|
| Focus | `FocusEnteredDocument{doc,cursor}` | Browse{sticky: false} | or Focus if ADR-0003 `P(n, Focus)` is true (M7) |
| Browse{sticky: false} | `FocusEnteredEditable` | Focus | ADR-0003 auto focus mode |
| Browse{sticky: true} | `FocusEnteredEditable` | unchanged | |
| Browse | `FocusLeftDocument` / `ExitBrowse` | Focus | bookmark saved (ADR-0003 RS-4) |
| Focus (in a document) | `EnterBrowse` (`browse.toggle`, Escape in Focus{Auto}) | Browse | |
| Focus, Browse | `EnterReview{origin,cursor}` (review/object command, `review.toggle`) | Review | origin = focus node or browse cursor |
| Review | `ExitReview` (`review.to_focus`, `review.toggle`) | Focus, or Browse if the review started in Browse | *new*: Review carries `from: ReviewFrom` (`Focus` or `Browse{..}`, never Review) |
| Review | `FocusChanged` with `review.focus_follows = true` *new* | Focus | NVDA "focus moves navigator object" default on |
| Focus, Browse, Review | `StartPassThrough(OneKey)` | PassThrough{OneKey, resume: r} | grab set ∅ |
| PassThrough{OneKey} | `PassKeyConsumed` *new* (input lane: first non-modifier press) | `resume` | |
| PassThrough{OneKey} | `PassTimeout` *new* (10 s) | `resume` | spoken "pass next key cancelled" |
| Focus, Browse, Review | `AppSleep(app)` *new*, focus in a sleeping app | PassThrough{UntilToggle, resume: r} | M6 |
| PassThrough{UntilToggle} | `AppWake(app)` or focus leaves the app *new* | `resume` | M6 |
| Focus, Browse, Review | `ToggleInputHelp` | InputHelp{resume: r} | `GrabKeyboard` |
| PassThrough{s, resume} | `ToggleInputHelp` | InputHelp{resume} | MD-2 |
| InputHelp | `ToggleInputHelp`, `InputHelpEscape` *new*, `InputHelpTimeout` *new*, `InputHelpStall` *new* | `resume` | stall and timeout originate on the input lane (LM-3) |
| InputHelp | any other message | unchanged | focus updates still tracked (MD-3) |
| any | `MonitorLost` (keyboard monitor `Lost`) | unchanged | grab set re-sent on `Watching` |

## 4. NVDA-style keymaps (default preset)

### 4.1 Notation

- `gorca` = any gorca modifier held. Key names are XKB keysym names (`Up`, `Prior` = Page Up, `Next` =
  Page Down, `KP_Home` = numpad 7 with Num Lock off). Letters are physical keys (ADR-0005 K-8).
- `×2` / `×3` = second or third press within `keyboard.multi_press_ms`.
- Status: **M1** implemented in M1; `Mn` reserved for that milestone; `—` not planned in Phase 2 (kept
  free so NVDA users' muscle memory does not hit a different command).
- Watched gestures (not grabbed, the app also receives the key): Control, Shift tap.

### 4.2 Global commands (Focus, Browse and Review contexts)

| Command | Desktop | Laptop | Status |
|---|---|---|---|
| `speech.stop` | Control (watched) | Control | M1 |
| `speech.pause_toggle` | Shift tap (watched) | Shift tap | M1 |
| `speech.mode_cycle` | gorca+S | gorca+S | M1 |
| `speech.repeat_last` | gorca+X | gorca+X | M1 |
| `speech.copy_last` | gorca+Ctrl+X | gorca+Ctrl+X | — (clipboard) |
| `speech.ring_inc` | gorca+Ctrl+Up | gorca+Ctrl+Shift+Up | M1 |
| `speech.ring_dec` | gorca+Ctrl+Down | gorca+Ctrl+Shift+Down | M1 |
| `speech.ring_next` | gorca+Ctrl+Right | gorca+Ctrl+Shift+Right | M1 |
| `speech.ring_prev` | gorca+Ctrl+Left | gorca+Ctrl+Shift+Left | M1 |
| `speech.ring_inc_large` | gorca+Ctrl+Prior | gorca+Ctrl+Shift+Prior | M1 |
| `speech.ring_dec_large` | gorca+Ctrl+Next | gorca+Ctrl+Shift+Next | M1 |
| `speech.rate_up/down`, `pitch_up/down`, `volume_up/down` | unbound | unbound | M1 |
| `speech.punctuation_cycle` | gorca+P | gorca+P | M3 |
| `info.where_am_i` (×2 detailed, ×3 spell) | gorca+Tab | gorca+Tab | M1 |
| `info.window_title` (×2 spell, ×3 copy) | gorca+T | gorca+T | M1 (×3: —) |
| `info.time_date` (×2 date) | gorca+F12 | gorca+F12 | M1 |
| `info.current_line` (×2 spell, ×3 phonetic) | gorca+Up | gorca+L | M3 |
| `info.selection` | gorca+Shift+Up | gorca+Shift+S | M3 |
| `info.formatting` | gorca+F | gorca+F | M3 |
| `info.read_window` | gorca+B | gorca+B | M4 |
| `info.status_bar` | gorca+End | gorca+Shift+End | M4 |
| `info.battery` | gorca+Shift+B | gorca+Shift+B | M4 |
| `info.focus_shortcut` | Shift+KP_Down | gorca+Ctrl+Shift+period | M4 |
| `info.clipboard` | gorca+C | gorca+C | — (clipboard) |
| `info.link_destination` | gorca+K | gorca+K | M7 |
| `info.details` | gorca+D | gorca+D | M7 |
| `text.say_all` | gorca+Down | gorca+A | M3 |
| `echo.typed_chars_toggle` | gorca+2 | gorca+2 | M1 |
| `echo.typed_words_toggle` | gorca+3 | gorca+3 | M1 |
| `echo.command_keys_toggle` | gorca+4 | gorca+4 | M1 |
| `browse.live_regions_toggle` | gorca+5 | gorca+5 | M7 |
| `review.caret_follows_toggle` | gorca+6 | gorca+6 | M4 |
| `review.focus_follows_toggle` | gorca+7 | gorca+7 | M4 |
| `verbosity.cycle` | gorca+Shift+V | gorca+Shift+V | M1 (gorca-specific) |
| `input.learn_mode_toggle` | gorca+1 | gorca+1 | M1 |
| `input.pass_next_key` | gorca+F2 | gorca+F2 | M1 |
| `input.sleep_toggle` | gorca+Shift+S | gorca+Shift+Z | M6 |
| `settings.reload` (revert to saved) | gorca+Ctrl+R | gorca+Ctrl+R | M1 |
| `settings.save` | gorca+Ctrl+C | gorca+Ctrl+C | M1 |
| `settings.report_problems` | gorca+Ctrl+Shift+R | gorca+Ctrl+Shift+R | M1 (gorca-specific) |
| `debug.node_info` | gorca+F1 | gorca+F1 | M1 |
| `debug.app_info` | gorca+Ctrl+F1 | gorca+Ctrl+F1 | M1 |
| `gorca.quit` (×2 to confirm) | gorca+Q | gorca+Q | later |
| NVDA settings dialogs (gorca+N, gorca+Ctrl+G/S/V/K/…) | kept free | kept free | — (no GUI, U-4) |
| mouse (KP_Divide, KP_Multiply, gorca+KP_Divide/KP_Multiply, laptop gorca+[ ] and Shift variants) | kept free | kept free | — (no pointer synthesis on Wayland in Phase 2) |
| magnifier, screen curtain, OCR, remote, sound split | kept free | kept free | — |

### 4.3 Review and object navigation (M4, reserved)

Desktop review keys are **unmodified keypad keys with Num Lock off**; they join the Focus grab set only in
M4 (K-1). Laptop review uses gorca+letter/arrow chords.

| Command | Desktop | Laptop |
|---|---|---|
| `review.prev_line` | KP_Home (7) | gorca+Up |
| `review.cur_line` (×2 spell, ×3 phonetic) | KP_Up (8) | gorca+Shift+period |
| `review.next_line` | KP_Prior (9) | gorca+Down |
| `review.prev_word` | KP_Left (4) | gorca+Ctrl+Left |
| `review.cur_word` (×2 spell, ×3 phonetic) | KP_Begin (5) | gorca+Ctrl+period |
| `review.next_word` | KP_Right (6) | gorca+Ctrl+Right |
| `review.prev_char` | KP_End (1) | gorca+Left |
| `review.cur_char` (×2 phonetic, ×3 code point) | KP_Down (2) | gorca+period |
| `review.next_char` | KP_Next (3) | gorca+Right |
| `review.top` | Shift+KP_Home | gorca+Ctrl+Home |
| `review.bottom` | Shift+KP_Prior | gorca+Ctrl+End |
| `review.start_of_line` | Shift+KP_End | gorca+Home |
| `review.end_of_line` | Shift+KP_Next | gorca+End |
| `review.prev_page` | gorca+Prior | gorca+Shift+Prior |
| `review.next_page` | gorca+Next | gorca+Shift+Next |
| `review.say_all` | KP_Add | gorca+Shift+A |
| `review.mode_next` (object / document / screen review) | gorca+KP_Home | gorca+Prior |
| `review.mode_prev` | gorca+KP_End | gorca+Next |
| `review.to_focus` | gorca+KP_Subtract | gorca+BackSpace |
| `review.move_focus` | gorca+Shift+KP_Subtract | gorca+Shift+BackSpace |
| `review.activate` | gorca+KP_Enter | gorca+Return |
| `review.start_of_selection` / `end_of_selection` | gorca+Alt+Home / gorca+Alt+End | same |
| `object.current` (×2 spell) | gorca+KP_Begin | gorca+Shift+O |
| `object.parent` | gorca+KP_Up | gorca+Shift+Up |
| `object.first_child` | gorca+KP_Down | gorca+Shift+Down |
| `object.next` | gorca+KP_Right | gorca+Shift+Right |
| `object.prev` | gorca+KP_Left | gorca+Shift+Left |
| `object.next_in_flow` | gorca+KP_Next | gorca+Shift+bracketright |
| `object.prev_in_flow` | gorca+KP_Prior | gorca+Shift+bracketleft |
| `review.copy_mark` / `review.copy` | gorca+F9 / gorca+F10 | same (— clipboard) |

Laptop note: gorca+Up, gorca+Down, gorca+Shift+Up and gorca+Shift+S mean different things in the two
layouts (NVDA does the same). The layout is chosen once (`keyboard.layout`).

### 4.4 Browse mode (M7/M8, reserved; identical in both layouts)

These keys are grabbed only in `Browse` (and the letters only while single-letter navigation is on).
Shift reverses direction. They replace the provisional letters in ADR-0003 §12.

| Key | Next / previous (Shift) | Key | Next / previous (Shift) |
|---|---|---|---|
| H | heading | 1–9 | heading at level 1–9 |
| K | link | U / V | unvisited / visited link |
| F | form field | E | edit field |
| B | button | X | checkbox |
| C | combobox | R | radio button |
| L | list | I | list item |
| T | table | G | graphic |
| Q | block quote | S | separator |
| M | frame | O | embedded object |
| D | landmark | N | text after a block of links |
| A | annotation | W | spelling or grammar error |
| P | text paragraph | comma / Shift+comma | end / start of container |

Unbound by default (assignable): slider, clickable, article, grouping, tab, figure, menu item, toggle
button, progress bar, math, live region, same/different style, reference.

| Command | Key |
|---|---|
| `browse.toggle` | gorca+space |
| `browse.single_letter_toggle` | gorca+Shift+space |
| `browse.elements_list` | gorca+F7 |
| `browse.find` / `find_next` / `find_prev` | gorca+Ctrl+F / gorca+F3 / gorca+Shift+F3 |
| `browse.refresh` | gorca+F5 |
| `browse.activate` | Return, KP_Enter, space |
| back to browse from Focus{Auto} | Escape (first Escape goes to an expanded popup, ADR-0003 §11) |
| `browse.screen_layout_toggle`, native selection mode | gorca+V, gorca+Shift+F10 (—) |
| line / character | Up, Down / Left, Right |
| word | Ctrl+Left, Ctrl+Right |
| paragraph | Ctrl+Up, Ctrl+Down |
| sentence | Alt+Up, Alt+Down |
| line start / end; top / bottom | Home, End; Ctrl+Home, Ctrl+End |
| page | Prior, Next |
| select (char, word, line, page, paragraph, to line/document edges) | Shift+ / Ctrl+Shift+ the keys above |
| select all / copy | Ctrl+A / Ctrl+C |
| `table.next/prev_row` | Ctrl+Alt+Down / Ctrl+Alt+Up |
| `table.next/prev_column` | Ctrl+Alt+Right / Ctrl+Alt+Left |
| `table.first/last_row` | Ctrl+Alt+Prior / Ctrl+Alt+Next |
| `table.first/last_column` | Ctrl+Alt+Home / Ctrl+Alt+End |
| `table.say_all_row` / `say_all_column` | gorca+Ctrl+Alt+Right / gorca+Ctrl+Alt+Down |
| `table.say_row` / `say_column` | gorca+Ctrl+Alt+Left / gorca+Ctrl+Alt+Up |
| Tab, Shift+Tab, Ctrl+Return, Shift+Return, Alt+Return, Menu, Shift+F10 | not grabbed: reach the app (NVDA's explicit `passThrough` list, `browseMode.py:899-911`, is implicit here) |

Conflict check: gorca+Ctrl+Alt+arrows (tables) do not collide with any global chord; Ctrl+Alt+arrows
collide with GNOME's default workspace switching only on custom setups (KC-7 warns).

## 5. Orca preset

`keyboard.preset = "orca"`. Same commands, Orca's chords. Rows marked *gorca extra* keep the NVDA chord
because Orca has no binding for that command and the chord is free in Orca. Rows marked *not mapped*
are Orca commands with no gorca equivalent yet.

| gorca command | Orca desktop | Orca laptop | Status |
|---|---|---|---|
| `info.where_am_i` (×2 detailed) | KP_Enter | Orca+Return | M1 |
| `info.window_title` | Orca+KP_Enter | Orca+slash | M1 |
| `info.status_bar` | Orca+KP_Enter ×2 | Orca+slash ×2 | M4 (explicit `×2` binding, CR-4) |
| `info.time_date` (×2 date) | Orca+T | Orca+T | M1 |
| `info.selection` | Orca+Shift+Up | Orca+Shift+Up | M3 |
| `info.formatting` | Orca+F | Orca+F | M3 |
| `input.learn_mode_toggle` | Orca+H | Orca+H | M1 |
| `speech.mode_cycle` | Orca+S | Orca+S | M1 |
| `verbosity.cycle` | Orca+V | Orca+V | M1 |
| `speech.stop` / `speech.pause_toggle` | Control / Shift tap | same | M1 (watched gestures; Orca stops on any key) |
| `speech.repeat_last`, `input.pass_next_key`, `echo.*_toggle`, `settings.*`, `debug.*` | gorca+X, gorca+F2, gorca+2/3/4, gorca+Ctrl+R/C, gorca+Ctrl+Shift+R, gorca+F1, gorca+Ctrl+F1 | same | M1, *gorca extra* |
| `speech.ring_*`, `speech.rate_*`, `pitch_*`, `volume_*` | unbound (Orca binds none; Orca+Ctrl+arrows is object navigation) | unbound | M1 |
| `input.sleep_toggle` | Ctrl+Alt+Shift+Q | Ctrl+Alt+Shift+Q | M6 |
| Orca bypass mode (all Orca keys off) | Alt+BackSpace | Alt+BackSpace | *not mapped* (global `PassThrough{UntilToggle}`, with M6) |
| `text.say_all` | KP_Add | Orca+semicolon | M3 |
| `review.toggle` | KP_Subtract | Orca+P | M4 |
| `review.prev_line` / `cur_line` / `next_line` | KP_Home / KP_Up / KP_Prior | Orca+U / Orca+I / Orca+O | M4 |
| `review.prev_word` / `cur_word` / `next_word` | KP_Left / KP_Begin / KP_Right | Orca+J / Orca+K / Orca+L | M4 |
| `review.prev_char` / `cur_char` / `next_char` | KP_End / KP_Down / KP_Next | Orca+M / Orca+comma / Orca+period | M4 |
| ×2 spell, ×3 phonetic / code point on the current line, word, char | ×2, ×3 of the same keys | same | M4 (repeat count, CR-4) |
| `review.top` / `review.bottom` | Orca+KP_Home / Orca+KP_Prior | Orca+Ctrl+U / Orca+Ctrl+O | M4 |
| `review.end_of_line` | Orca+KP_End | Orca+Ctrl+M | M4 |
| `review.say_all` | KP_Add ×2 | Orca+semicolon ×2 | M4 (explicit `×2`) |
| `object.current` | Orca+KP_Begin | Orca+Ctrl+K | M4 |
| review above / below | Orca+KP_Left / Orca+KP_Right | Orca+Ctrl+J / Orca+Ctrl+L | *not mapped* (flat-review geometry, ADR-0006) |
| `object.parent` / `first_child` / `next` / `prev` | Orca+Ctrl+Up / Down / Right / Left | same | M4 |
| `review.activate` | Orca+Ctrl+Return | Orca+Ctrl+Return | M4 |
| flat review find / next / previous | KP_Delete / Orca+KP_Delete / Orca+Shift+KP_Delete | Orca+[ / Orca+] / Orca+Ctrl+] | *not mapped* |
| mouse left/right click, route pointer | KP_Divide / KP_Multiply / Orca+KP_Divide | Orca+7 / Orca+8 / Orca+9 | — |
| `browse.toggle` (×2 sticky focus, ×3 sticky browse) | Orca+A | Orca+A | M7 |
| `browse.single_letter_toggle` (approx. Orca structural-nav mode cycle) | Orca+Z | Orca+Z | M7 |
| `browse.live_regions_toggle` | Orca+backslash | Orca+backslash | M7 |
| quick nav (Shift = previous) | Q blockquote, B button, X checkbox, C combobox, E entry, F form field, H heading, G image, M landmark, L list, I list item, P paragraph, R radio, T table, K link, U unvisited, V visited, O large object, A clickable, S separator, D live region, Y last live region, comma container end, Shift+comma start, 1–6 heading level | same | M7 |
| elements lists | Shift+Alt+letter or digit | same | M7 (`browse.elements_list` filtered) |
| table cell navigation | Shift+Alt+arrows, Shift+Alt+Home/End; Orca+Alt+Shift+arrows for row/column edges | same | M8 |
| Orca+E default button, Orca+Shift+A actions list, Orca+F11 table cell mode, Orca+Shift+T table navigator, Orca+Shift+R/C dynamic headers, Orca+Shift+D speech monitor, Orca+F12 caret navigation, Orca+Alt+M math, Orca+space preferences | — | — | *not mapped* |

Differences an Orca user will notice: gorca adds pass-next-key, repeat-last, settings and debug chords;
learn mode also accepts gorca+1; Control stops speech (Orca stops on any key, which gorca also does
through K-3).

## 6. Context lookup and conflicts (summary of ADR-0005 §5.2, §7.5)

- Lookup order: `(mode, app)` → `(mode, *)` → `(global, app)` → `(global, *)`.
- `global` applies in Focus, Browse and Review. PassThrough and InputHelp are fixed (§1).
- Plain keys without the gorca modifier are allowed in `global`/`focus` only for keypad keys, chords with
  two of Ctrl/Alt/Super, and F13–F24 (KC-5). Letters and digits alone are allowed in `browse` and `review`.
- Reserved: Escape in InputHelp, the gorca modifiers, Control and Shift alone (KC-6).
- Extension bindings (M5) live in `app:<matcher>` contexts, never in `global`, and never shadow
  `speech.stop`, `input.learn_mode_toggle`, `input.pass_next_key`, `settings.reload`.

User override example:

```toml
[keymap.global]
"gorca+w" = "info.window_title"   # add a second chord
"gorca+F12" = "none"              # unbind the clock

[keymap.browse]
"j" = "quicknav.next.live_region"
"shift+j" = "quicknav.prev.live_region"
```
