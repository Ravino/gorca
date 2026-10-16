# M4b live checklist: GNOME Shell and desktop polish

Run on the real GNOME 48 desktop with `tools/gorca-toggle.sh` (Super+Alt+G); gorca must be the only screen reader. These
checks are **user-only**: nothing here is ever automated against the live session (lock screen and polkit least of all).
Specification: `docs/plan/phase-2.md` section M4, `docs/reports/m4b-impl.md` for what the code does and why.

How to read the results: speech is queued, never cut. A notification, an announcement or an OSD **waits** for the
sentence being spoken; moving focus cancels what is stale, as always. "Title" means the window's accessible name.

| # | Step | Expected |
|---|---|---|
| 1 | Alt+Tab, hold Tab through several windows | each window name spoken as selection moves; no "tab" echo (K-regression check) |
| 2 | Alt+Tab, release | the chosen application's window title, then its focused widget; no "gnome-shell" window announced |
| 3 | Open Files, Settings, Text Editor in turn from the Overview | title then focus: "Settings, Search, edit" style; one utterance, no stutter |
| 4 | In Files select a file, press Delete, then empty the trash (Ctrl+Shift+Delete) | the confirm dialog: title, "dialog", its message text, then the focused button |
| 5 | Close an unsaved Text Editor document | "Save changes?" dialog read the same way, including the secondary text |
| 6 | Open a GTK About dialog (Text Editor, main menu, About) | title, "dialog", then the focus; the label text is not skipped |
| 7 | Super (Overview), type "term" | search field spoken on open; each result spoken as it is selected with Down/Tab |
| 8 | Overview, Super+A (app grid), arrow through icons | "App grid" group; each application icon named once per move |
| 9 | Open the quick settings menu (Super+S or the top-bar corner), Tab through the toggles | each toggle: name, "toggle button", on/off; Wi-Fi/Bluetooth toggles say their state |
| 10 | In quick settings focus the Volume and Brightness sliders, Left/Right | "Volume, slider, 40 percent" (a percentage, not 0.4); changes spoken |
| 11 | Press the keyboard volume keys (or Fn+volume) three times quickly | one announcement of the final level, "Volume, 70 percent" style, not one per step; it never cuts a sentence in progress |
| 12 | Press the brightness keys | "Brightness, N percent" the same way |
| 13 | `notify-send "Hello" "Body text"` from a terminal | "alert, Hello, Body text" after the terminal's own speech has finished; the terminal speech is not interrupted |
| 14 | Send five notifications in a second (`for i in 1 2 3 4 5; do notify-send "n$i"; done`) | no flood: at most about three spoken per second, the newest of the rest is spoken after |
| 15 | While gorca is reading a long paragraph (say-all), send a notification | the reading continues to its end; the notification follows, it does not replace it |
| 16 | Press a key (or move focus) right after a notification arrived | the new focus speaks at once; it is never held behind the notification |
| 17 | Open the notification list / calendar (Super+V), arrow over messages | each message read with its title and body |
| 18 | Alt+F2, type a wrong command, Enter | Run dialog read on open; the "Command not found" style error label is announced |
| 19 | Application menu in the top bar, Arrow through items; open a popover (Settings window menu) | each item name, role, state; submenus say expanded/collapsed |
| 20 | Alt+F4 an application with unsaved work, or log-out dialog (power menu, Log Out...) | dialog title, message text, then buttons |
| 21 | Screenshot UI (Print) | the toolbar buttons are named; Escape leaves silently |
| 22 | Two windows of one application: switch with Alt+` | the window title is spoken each time |
| 23 | Super+L (lock) then wake the screen | the lock screen announces itself (clock/user name read); the password field is spoken as a password edit; unlock with the password: your desktop window is announced. **Lock screen is checked by you only.** |
| 24 | Run `pkexec true` in a terminal | the polkit dialog is read on open: title, "dialog", message, then the password entry focused. **Polkit is checked by you only.** |
| 25 | Run `gorca` with another application flooding announcements (a script calling `notify-send` in a loop for 10 s) while you navigate a menu | your navigation speech is never delayed or dropped; the flood is rate-limited (no backlog is read afterwards) |

Record failures with the application, what was spoken, and `journalctl --user -t gorca` around the time. Settings are not in
`gorca.toml` yet: the budgets (three spoken notices per second per process, a global ceiling of 16 per second) are constants
documented in `crates/gorca-core/src/present/fair.rs`.
