# tests/term: a real gnome-terminal (VTE) in a private session

M6 needs facts about the events a real VTE sends, and a check that the reader works against one. Both run in
**private** sessions only (CLAUDE.md "Live-session safety"): nothing here touches the live compositor, the live
session or a11y bus, the KeyboardMonitor or any running terminal.

```
tests/term/run_real_vte.sh <cmd...>
```

starts two private worlds (as `tests/web/m7/run_m7.sh`): a dbus-daemon with a headless `gnome-shell` on a unique Wayland
socket (the compositor world), and the AT-SPI world of `tests/harness/run-isolated.sh` in which `<cmd>` runs with
`GORCA_VTE_WAYLAND`, `GORCA_VTE_CBUS` and `GORCA_VTE_TOOLS` set. The terminal gets keys from a Mutter RemoteDesktop
session (`tests/web/m7/rd.py`). Teardown signals only the PIDs recorded by the scripts.

| File | What it is |
|---|---|
| `vte_probe.py` | starts `gnome-terminal-server` and a bash window, types scripted sessions, writes every AT-SPI event of the terminal and `GetText(0, -1)` snapshots as JSON lines (`GORCA_VTE_OUT=<file>`); floods (`seq`, `yes`) are counted and their first 24 events recorded |
| `run_real_vte.sh` | the two worlds |

```sh
GORCA_VTE_OUT=/tmp/vte.jsonl tests/term/run_real_vte.sh /usr/bin/python3 tests/term/vte_probe.py
tests/term/run_real_vte.sh target/release/e2e run --label m6-real --only VV --quick \
  --reader daemon:$PWD/target/release/gorca,--impersonate-orca --reader-env GORCA_M6=1 --reader-env GORCA_M6_REAL=1
```

The second command is the VV scenario of `crates/gorca-bench/src/e2e/m6.rs`: the daemon against the real terminal
(typing, readline keys, history, Tab, `yes` and `seq` floods, Ctrl+C), keys going to the reader through the emulated Mutter
and to the terminal through the RemoteDesktop keyboard.

`extensions/ext-vte/tests/fixtures/gnome-terminal-3.56-vte-0.80-session.jsonl` is a trimmed recording of the first
part of a `vte_probe.py` session (gnome-terminal 3.56.2, libvte 0.80.1, Debian 13); `extensions/ext-vte/tests/replay.rs`
replays it through the terminal policy. The facts it records are in `docs/platform/vte-terminal-accessibility.md`
("Measured on a real VTE").

Limits of the rig: a headless seat has no keyboard until a virtual one is used (the script presses one key before the
window exists, otherwise `FOCUSED` arrives with the first key), the first key after a click can be lost, and the
compositor's virtual keyboard now and then drops a key.
