# signal_cmp: Orca vs gorca on a Signal-shaped page

Private session only (headless gnome-shell on a unique Wayland socket + isolated AT-SPI session); nothing touches
the live session. Report: `docs/reports/signal-orca-vs-gorca.md`.

```text
tests/web/chr0/signal_cmp/run.sh READER APP SCENARIO OUTDIR     READER orca|gorca|none, APP chromium|chrome|electron
tests/web/chr0/signal_cmp/matrix.sh OUTDIR                     the 12-run matrix (about 10 minutes)
python3 -I tests/web/chr0/signal_cmp/show.py OUTDIR             compact step table of one run
```

* Scenarios (`driver.py` `SCENARIOS`): `list`, `chat`, `chat_focus`, `list_focus` (gorca+Space / Orca+A first),
  `doc` (control page), `probe` (what the focused node answers).
* Env: `SC_GORCA_BIN` (default `<repo>/target/release/gorca`), `SC_GORCA_ARGS` (e.g. `--config=FILE`),
  `SC_APPNAME` (Electron binary under `~/.cache/electron-probe/dist2`, hard link of `chromelectron`; default
  `signal-desktop`), `SC_SETTLE` seconds after each key (2.5 for Orca), `SC_BROWSER_FIRST=1`, `SC_PAGE`.
* Orca 48.1 needs `DISPLAY` set (any value) and a Wayland display; speech is read from its `--debug` log.
  gorca speech comes from a mock speech-dispatcher (`speech.log`) and `--bench-present-log` (`present.log`).
* Keys go through `tests/harness/fake_mutter.py` (grabs consume them) and, when not consumed, through the private
  RemoteDesktop keyboard (`tests/web/m7/rd.py`) into the page.
* Fixture: `tests/web/chr0/pages/signal-like.html` (`?focus=grid` or `?focus=timeline`).
