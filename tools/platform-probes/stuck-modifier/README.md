# Mutter 48.7 stuck grabbed modifier (2026-10-14)

PRIVATE headless gnome-shell only. Scripts hard-code `L=` (a scratch dir holding `sh/` = the shell's
work dir); edit `L` first. `fresh.sh start` starts `contrib/gnome-shell-extension/headless-shell.sh`
and prints its bus address, `fresh.sh stop` kills only the recorded process group.

* `bare.py ADDR HOLD_S KEYSYM EVDEV_KC` : a bare client grabs one modifier (no gorca), the key is held
  HOLD_S seconds, then a GTK4 window is probed with Tab and a. `bare.py ADDR 0.9 0xff63 110` -> all keys
  swallowed. Caps_Lock (0xffe5, 58) is not affected (it does not autorepeat).
* `replay2.py ADDR GORCA JSON_SEQ TAG` : same with gorca, replays key events, restarts gorca.
* `model48.py` : Python model of Mutter 48.7 `is_a11y_modifier_first_click` + `notify_clients`; predicts
  which hold times leave `grabbed_keypresses[modifier]` stale (matches the live results).
Report: docs/reports/live-keys-swallowed.md

Added 2026-10-14 (all PRIVATE headless shell only; each script hard-codes `L=` scratch dir, edit first):
* `heal.py ADDR` : which no-injection actions clear a stale `grabbed_keypresses` entry (none do).
* `status.sh ADDR` : `Set org.a11y.Status ScreenReaderEnabled|IsEnabled` and the private GSettings keyfile.
* `gorca_enable.sh ADDR GORCA_BIN plain|orca` : what gorca writes at start and stop.
* `apps_a11y.sh ADDR GORCA_BIN gorca|off firefox|gtk4|gtk3` (+ `a11y_pids.py`, `gtk3app.py`) : does the app
  connect to the a11y bus when only `IsEnabled` is set.
