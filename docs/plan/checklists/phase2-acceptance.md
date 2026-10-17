# Phase 2 user acceptance checklist

One keyboard-only walk-through for the person who tests gorca live. It consolidates the per-milestone
lists (`M1.md`, `M3.md`, `M4a.md`, `M4b.md`, `M6.md`, `M7a.md`, `M7b.md`, and `M8.md` and `M9.md` from
their branches) and adds the recent fixes you asked for. The per-milestone files stay the detailed
reference; this file is the order to work in.

**Time:** sections 0 to 8 take about 60 to 90 minutes. Section 9 ("Extended") is optional and longer.

## How to use this list

- Each item is one line: the keys, the expected speech, and a checkbox. Tick `[x]` when what you hear
  matches. When it does not, write what you heard after the item and press gorca+F1 (node info) at once.
- `gorca` means Insert (also KP_Insert with Num Lock off) or Caps Lock. Keys follow the NVDA desktop layout.
  The unmodified numpad review keys need Num Lock off.
- Speech is queued. New focus or a new key cancels stale speech; a notification waits for the sentence
  being spoken.
- Setup tags: **[LOCK]** lock screen, **[POLKIT]** polkit dialog, **[CHROMIUM]** needs Chrome or Chromium,
  **[M8]** needs the M8 build, **[M9]** needs the M9 build, **[TEXT]** needs gedit or GNOME Text Editor.
  Untagged items need nothing special.

## 0. Before you start

Build status: M8 (Firefox at NVDA parity) and M9 (Chromium) were not on `main` when this list was written
(branches `worktree-agent-adcbae30d4d7a4ad0` and `worktree-agent-a5bd784f7a1f0f564`). Ask which build you
are running, and skip the **[M8]** and **[M9]** items on a build without them.

- [ ] Orca is off. Press Super+Alt+G. Expected: "gorca on" (spoken by the toggle script, not by gorca).
- [ ] Tap Insert once. Expected: nothing is spoken and Caps Lock does not toggle.
- [ ] Insert+Tab. Expected: where am I: application, window, container with position, object.
- [ ] Insert+T. Expected: the window title.
- [ ] Insert+F12. Expected: the time; pressed twice, the date.
- [ ] Insert+X. Expected: the last spoken text again.
- [ ] Control. Expected: speech stops at once; Control still works in the application.
- [ ] Insert+S, then Insert+S again. Expected: "speech off", then "speech on".
- [ ] Insert+Ctrl+Up, then Insert+Ctrl+Down. Expected: rate goes up, then down; each confirmation is spoken at the new value.
- [ ] Insert+1, press a few keys, then Escape. Expected: learn mode describes each key without running it; Escape leaves it.
- [ ] Insert+F2, then Insert+X. Expected: the key reaches the application; commands work again afterwards.

## 1. Desktop and Shell (about 15 minutes)

- [ ] Alt+Tab, holding Tab through several windows. Expected: each window name as the selection moves; never "unknown"; no "tab" echo.
- [ ] Alt+Tab, release. Expected: the chosen window title, then its focused widget; no "gnome-shell" window.
- [ ] Alt+Shift+Tab, and quick Alt+Tab (press and release at once), five times each. Expected: never "unknown". A bare "button" on the very first item of a held Alt+Tab is a known open issue: note it and go on.
- [ ] Ctrl+Alt+Tab, Tab through the top bar, Ctrl+Alt+Tab again. Expected: items are named ("Top Bar, button", "Windows, button"); never "unknown".
- [ ] Super, then type "term". Expected: the search field on open; each result named as Down or Tab selects it.
- [ ] Super+A, arrow through the icons. Expected: "App grid"; each application named once per move.
- [ ] Open Settings, Files and Text Editor from the Overview. Expected: window title, then focus, as one utterance without stutter.
- [ ] Super+S (quick settings), Tab through the toggles. Expected: name, "toggle button", on or off.
- [ ] Focus the Volume slider, press Left or Right. Expected: "Volume, slider, N percent" (a percentage, not 0.4).
- [ ] Press the keyboard volume keys three times quickly. Expected: one announcement of the final level, not one per step; it never cuts a sentence in progress.
- [ ] In a terminal run `notify-send "Hello" "Body text"`. Expected: "alert, Hello, Body text" after the terminal speech finishes, without interrupting it.
- [ ] Run `for i in 1 2 3 4 5; do notify-send "n$i"; done`. Expected: no flood (about three per second at most); the newest is spoken last.
- [ ] Press a key right after a notification arrives. Expected: the new focus speaks at once, never held behind the notification.
- [ ] Alt+F2, type a wrong command, Enter. Expected: the Run dialog is read on open; the error label is announced.
- [ ] In Files select a file, press Delete, then Ctrl+Shift+Delete. Expected: the confirm dialog: title, "dialog", its message, then the focused button.
- [ ] In Text Editor type a character, then Ctrl+W. Expected: the "Save changes?" dialog is read with its secondary text.
- [ ] **[LOCK]** Super+L, wake the screen, type the password, Enter. Expected: the lock screen announces itself; the password field is a password edit and its characters are spoken; after unlock your window is announced. Only you can check this.
- [ ] **[POLKIT]** In a terminal run `pkexec true`. Expected: the polkit dialog is read on open (title, "dialog", message), then the password entry has focus. Only you can check this.

## 2. Terminal: GNOME Terminal (about 15 minutes)

- [ ] Alt+Tab to the terminal from another application, three times, pressing no key in it. Expected: window title and the prompt line each time. Then type `ls`, Enter. Expected: the output is read; the terminal is not silent after Alt+Tab.
- [ ] Open a new window or tab (Ctrl+Shift+T), type `date`, Enter. Expected: the output is spoken.
- [ ] Type `echo hello`. Expected: each character once; the space says "space".
- [ ] BackSpace twice. Expected: the removed character each time.
- [ ] Left, Left. Expected: the character under the cursor. Home: the first character. End: "blank".
- [ ] Ctrl+Left, Ctrl+Right. Expected: the word.
- [ ] Enter. Expected: `hello`, then the new prompt, in one utterance.
- [ ] Up, Up, Down, Down. Expected: each recalled command without the prompt; at the bottom "blank".
- [ ] Type `ec`, press Tab. Expected: "echo".
- [ ] Run `ls -la /usr/bin`. Expected: read once, in order, from the top.
- [ ] Run `yes`, wait ten seconds, press Ctrl+C. Expected: calm speech while it runs; Ctrl+C silences it at once, then "caret C" and the prompt.
- [ ] Run `seq 1 100000`. Expected: after it stops, the last lines and the prompt; speech never lags minutes behind.
- [ ] Run `sudo true`. Expected: the password prompt is spoken; the typed characters are spoken.
- [ ] Run `less /etc/services`, press Space, `b`, `q`. Expected: each page is read as new lines; `q` returns to the prompt.
- [ ] Flat review stays current: run `echo one`, then press KP8 (current line), KP7 (previous), KP9 (next). Run `echo two`, then KP8 again. Expected: review reads the new screen (the line with "two" or the new prompt), not the screen from before the command. Repeat after Alt+Tab away and back.
- [ ] gorca+Up. Expected: the current line; twice: spelled; three times: phonetic.
- [ ] Ctrl+Shift+T for a second tab, then Ctrl+PageUp and Ctrl+PageDown. Expected: the tab title and its cursor line each time.
- [ ] Run `yes` in tab 1 and work in tab 2. Expected: no speech from tab 1; tab 2 echoes at once.

## 3. Text editors: gedit and GNOME Text Editor (about 10 minutes) [TEXT]

Do these in both editors, with a document of several lines and one long wrapped paragraph. Type
`Привет, как дела твои?` once for the word tests.

- [ ] Right and Left along a line. Expected: the character under the caret; a space says "space"; past the last character "blank".
- [ ] Left at the very start of the document, Right at the very end. Expected: never silence: the character at the caret at the start, "blank" at the end.
- [ ] Home and End on a line. Expected: the character at the caret.
- [ ] Ctrl+Right repeatedly through the Russian sentence. Expected: each word you have just passed ("Привет", ",", "как", "дела", "твои", "?"), then "blank" at the end of the text; never the word that comes after the caret.
- [ ] Ctrl+Left back through it. Expected: the word at the caret each time.
- [ ] Down and Up; PageUp and PageDown. Expected: the line as wrapped on screen.
- [ ] Ctrl+Up and Ctrl+Down. Expected: the paragraph.
- [ ] Type letters, digits, punctuation, Space, Enter. Expected: each once; Enter says "new line".
- [ ] BackSpace, Delete, Ctrl+BackSpace. Expected: the deleted character (the word for Ctrl); never the key name.
- [ ] Shift+Right, Shift+Left, Ctrl+Shift+Right, Ctrl+A. Expected: "x selected" and "x unselected" for the change only; Ctrl+A says "all selected, N characters".
- [ ] Ctrl+C, move, Ctrl+V; then Ctrl+X. Expected: the pasted text; the cut text.
- [ ] gorca+Down (say all) from the top. Expected: reads in sentences; any key stops it at once.
- [ ] gorca+Up, then gorca+Shift+Up. Expected: the line at the caret; the selected text or "no selection".
- [ ] Ctrl+F, type a word, Enter. Expected: the find bar is announced and each match is spoken; Escape returns to the text.
- [ ] Open replace (gedit: Ctrl+H; Text Editor: Ctrl+R) and replace one match. Expected: fields are named; the result is spoken.
- [ ] Open a second document or tab (Ctrl+N or Ctrl+T) and switch. Expected: the document title each time.
- [ ] Hold Right for 3 seconds. Expected: no queue of old characters; speech ends on the last position.

## 4. Firefox (about 15 minutes)

Use a Wikipedia article and a login form. If available, add `tests/web/m8/pages/` and
`tests/web/m3b/pages/fields.html` through `file://`.

- [ ] Alt+Tab to Firefox. Expected: "browse mode", the page title, the line at the cursor.
- [ ] Down and Up. Expected: lines, with "link", "heading level 2", "button", "edit" in front of what they belong to.
- [ ] Left, Right, Ctrl+Left, Ctrl+Right, Home, End, Ctrl+Home, Ctrl+End. Expected: character, word, line edge, page edge; a space says "space".
- [ ] H, Shift+H, then 1 to 6. Expected: headings; "no next heading" at the end.
- [ ] K, U, V, F, E, B, X, R, C, L, I, G, S, Q, D, T. Expected: the next element of that kind, with its role word.
- [ ] Insert+Down at the top of an article. Expected: read in sentences while the page scrolls along; any key stops it.
- [ ] Ctrl+L, type a URL, Enter. Expected: "browse mode", the title, then the page reads itself from the top; any key stops it.
- [ ] Ctrl+PageDown, Ctrl+PageUp. Expected: the tab title and the line where the cursor was; the page does not start reading.
- [ ] Tab onto a text field. Expected: "focus mode"; typing is echoed.
- [ ] Escape. Expected: "browse mode". Insert+Space toggles focus mode by hand.
- [ ] Ctrl+L and type. Expected: every key arrives; browse keys never take letters in the address bar.
- [ ] Open a combo box list (Alt+Down), then Escape twice. Expected: the first Escape closes the list and stays in focus mode; the second says "browse mode".
- [ ] Add a Russian layout and press р, л, ш in browse mode. Expected: next heading, link, list item, like H, K, I.
- [ ] Insert+F2, then a letter. Expected: the letter reaches the page.
- [ ] Shift+Right, Ctrl+Shift+Right in text. Expected: "x selected"; the word selected.
- [ ] **[M8]** F, E, X, R, C on labelled fields. Expected: role, label and state ("Search term, edit, blank"; "Gift wrap, check box, not checked").
- [ ] **[M8]** L onto a list, T onto a table. Expected: "list with 3 items"; caption, then "table with 3 columns and 4 rows".
- [ ] **[M8]** Arrow out of a list or table. Expected: "out of list" before the next line.
- [ ] **[M8]** Comma in a list; Shift+comma; comma outside any container. Expected: first line after it with "out of list"; the start of the container; "Not in a container".
- [ ] **[M8]** N, then P. Expected: the next text after a block of links; the next paragraph.
- [ ] **[M8]** Ctrl+Alt+Right and Ctrl+Alt+Down in a table. Expected: next column or row, with the header when it changes; "Edge of table" at the border.
- [ ] **[M8]** Space on a check box in browse mode. Expected: only the new state, "checked".
- [ ] **[M8]** Arrow to mid-page, press Tab. Expected: focus goes to the first focusable element after the cursor and speaks it.
- [ ] **[M8]** A live region and a `role=alert`. Expected: polite text when you are idle; an alert at once; no repeat within 10 seconds.
- [ ] **[M8]** K, Down, Shift+Right, Ctrl+A in browse mode. Expected: reading never moves focus or opens hover menus.

## 5. Chromium and Chrome (about 10 minutes) [CHROMIUM] [M9]

Use `tests/web/m7/pages/article.html` and `tests/web/m8/pages/iframes.html` through `file://`, plus a real
site. If a page is silent or wrong, restart the browser with `--force-renderer-accessibility` and compare.

- [ ] Alt+Tab to Chrome. Expected: the page title once ("Gorca article, document", not "... - Google Chrome, Gorca article, document"), "browse mode", the first line; Down reads the second line.
- [ ] Down through headings, links and paragraphs with links. Expected: every word once, never doubled.
- [ ] Insert+Down on an article. Expected: one smooth reading, no repeated chunks.
- [ ] Switch tabs, then follow a link. Expected: the title and the cursor line; the new page within about a second.
- [ ] Enter on a "Go to section" link. Expected: the cursor lands on the target heading and it is read.
- [ ] `iframes.html`: H, then Down. Expected: frame content read in order, including frames below the fold.
- [ ] Type in a text field and a contenteditable. Expected: each key once; BackSpace says the character; no doubled echo when typing fast.
- [ ] Open a dialog on a web app. Expected: "New message, dialog, To, edit" style.
- [ ] Ctrl+L, then Alt+F. Expected: Chrome's own address bar and menu are known gaps; note what is silent or wrong.

## 6. Settings and configuration (about 10 minutes)

The file is `~/.config/gorca/gorca.toml`; a missing file is fine.

- [ ] In a terminal run `gorca config print-default`. Expected: every setting with its default and a one-line comment, including `[speech]` `language`, `output_module`, `voice` and `backend`.
- [ ] Run `gorca config check`. Expected: no problems, or a readable list.
- [ ] Add `[speech]` with `language = "ru"` and save while gorca runs. Expected: "settings reloaded" within a second; speech continues in Russian (a Russian voice must exist on the chosen server). Change it to `"en"` and save; expected: English returns. Empty means your locale.
- [ ] Set `output_module = "espeak-ng"` (speech-dispatcher backend) and save. Expected: "settings reloaded"; that module speaks. Empty restores the server default.
- [ ] Set `voice` to a name the server lists and save. Expected: that voice speaks.
- [ ] Set `backend = "ssip"`, restart gorca with Super+Alt+G twice, then `"voicego"`, then `"auto"` the same way. Expected: it speaks after each restart. `auto` uses voicego and falls back to speech-dispatcher when voicego is unreachable.
- [ ] Introduce a typo (`[speech]` `rat = 70`) and save. Expected: it says how many problems; Insert+Ctrl+Shift+R reads them. Fix it; "settings reloaded".
- [ ] Insert+Ctrl+Up to change the rate, then Insert+Ctrl+R. Expected: the runtime change is dropped and the file reloaded. Insert+Ctrl+C writes runtime changes into the file and keeps your comments.
- [ ] Insert+Shift+V, Insert+2, Insert+3, Insert+4. Expected: verbosity cycles brief, normal, verbose; typed characters, words and command-key echo toggle.

## 7. Robustness (about 10 minutes)

- [ ] Orca toggle. With gorca running press Super+Alt+S. Expected: Orca starts and speaks; gorca says once that commands are unavailable and still reads focus. Press Super+Alt+S again to stop Orca, then Super+Alt+G twice. Expected: "gorca off", then "gorca on". gorca never stops or replaces Orca by itself.
- [ ] gorca toggle. Press Super+Alt+G twice quickly. Expected: the second press while the first runs is ignored; you end in a clear state (on or off), not a flap.
- [ ] Stuck Insert hint. Hold Insert for about one second, then release. Expected: if keys stop working, gorca says once "Insert stuck, tap Insert" (also "Keypad Insert" or "Caps Lock"); tap Insert once and commands work again. This is a Mutter 48.7 bug (`docs/reports/live-keys-swallowed.md`).
- [ ] Hung application. Start a test application yourself from a terminal (for example `gedit &`), note its PID, then run `kill -STOP <that PID>`. Alt+Tab to it and press arrows. Expected: nothing or "not responding" within about a second; the reader stays usable and other applications are not delayed. Resume with `kill -CONT <that PID>`. Never stop a system process.
- [ ] Event flood. Run `yes` in one terminal and navigate a menu in another application. Expected: your navigation speech is never delayed or dropped.
- [ ] Restart. With the terminal focused run `systemctl --user restart gorca.service`. Expected: gorca returns within two seconds and reads the focus.

## 8. Wrap-up

- [ ] Insert+Ctrl+C to keep runtime changes, or Insert+Ctrl+R to drop them.
- [ ] For every failed item note the application, the keys, what was spoken, and the time. Save `journalctl --user -t gorca` for that period.

## 9. Extended (optional, longer)

Drawn from the per-milestone lists; do them when time allows.

- Terminal (`M6.md` items 14 to 16, 21, 23, 25, 26, and the open platform questions): completion candidates, Ctrl+U, Ctrl+W, `make -j`, `htop`, `mc`, `nano`, zsh and fish, wide and combining characters, a flooding background tab delaying another tab.
- Native text (`M3.md`): Russian layout and AltGr characters, input-method and compose text, mouse selection, `gorca+P` punctuation levels, typed words (`gorca+3`), GTK3 entries (`gtk3-demo`), St entries (Alt+F2, overview search), `speech.caps = "say"`, password fields leaving nothing in `journalctl --user -t gorca` or `RUST_LOG=trace` output.
- Review (`M4a.md`): object navigation (`gorca+KP8`, `KP2`, `KP6`, `KP4`), review kinds, `gorca+KP_Minus`, `gorca+Shift+KP_Minus`, `gorca+KP_Enter`, `gorca+End` (status bar), the Orca preset (`keyboard.preset = "orca"`) and laptop layout (`keyboard.layout = "laptop"`).
- Shell (`M4b.md` items 17 to 25): the notification list, menus and popovers, the log-out dialog, the screenshot UI, switching windows of one application, a notification flood while navigating a menu.
- Firefox forms (`M7b.md`): layout changes while gorca runs, emoji over Left and Right, contenteditable editors, `Insert+F` (formatting), frames with M, `say_all_on_load = false`.
- Firefox M8 (`M8.md`): sentences (Alt+Down), selection variants, live-region flood summaries, single-page-app route changes, infinite scroll, modal dialogs, tri-state controls, `gorca+Shift+Space` (single-letter navigation off), `gorca+Shift+F10` (native selection).
- Chromium (`M9.md`): very large pages, background tabs built on first view, nested frames, Chromium (Debian package) next to Chrome stable.
- Start order: Orca first, then gorca (one announcement). Do not run `gnome-shell --replace` in the live session.

## Known gaps (do not report)

- The elements list (`gorca+F7`) and find (`gorca+Ctrl+F`) in browse mode are not implemented.
- `aria-describedby` help text may follow a moment later or not at all.
- Firefox spelling errors inside text are not found by W.
- Chrome's own interface (address bar, tab strip, menus) is not tuned yet.
- A bare "button" on the first item of a held Alt+Tab (an unnamed switcher button).
