# tests/web/chr0: Chromium-engine mirror freshness (CHR-0)

Spec: `docs/reports/chromium-base-design-review.md`. Implementation notes and results:
`docs/reports/chr0-implementation.md`.

The failure it reproduces: Chromium suppresses `children-changed:add` while a page is loading and
sends no structural event when the load ends, so a mirror built early (Signal: 4 nodes) stayed small
forever. The pages hold a load open with one image that a loopback server answers late.

| File | What |
|---|---|
| `slow_server.py PORT` | loopback server; `/slow?ms=N` answers after N ms (the page asks for 25 s) |
| `with_slow_server.sh CMD...` | starts the server (PID recorded), runs `CMD`, stops it by PID |
| `probe_growth.py` | step 0: timeline of tree size plus timestamps of `load-complete`, `busy` and `add` events |
| `run_probe.sh LABEL PAGE chrome\|chromium\|electron` | one probe run in a private session; results in `docs/reports/chr0-step0/` |
| `step0_all.sh` | the step-0 matrix (3 engines x slow page and control) |
| `run_e2e.sh ENGINE LABEL IDS [READER_BIN]` | the e2e scenarios with the server up; `E2E_EXTRA="--repeats 1"` adds options |
| `matrix.sh` | new reader vs main on Chrome and Firefox, new reader on Chromium and Electron |

Pages (in `tests/web/m8/pages/`): `slowload-auto.html` (grows 300 ms after DOMContentLoaded; step 0),
`slowload.html` (grows when the test presses "Render app", after gorca mirrored the small page),
`slowload-ctl.html` / `slowload-auto-ctl.html` (no slow image), `messenger.html` (dialog list, feed of
articles with roving tabindex, list of list items, listbox with `aria-activedescendant`).

Scenarios (`crates/gorca-bench/src/e2e/m8_steps.rs`): `M8-SLOW`, `M8-SLOW0` (control), `M8-MSG`.

```text
tests/web/chr0/run_e2e.sh chrome my-label M8-SLOW,M8-SLOW0,M8-MSG
CHR0_BASE_BIN=/path/to/main/gorca tests/web/chr0/matrix.sh
```

Everything runs in private sessions (the M7 pattern). The slow server binds 127.0.0.1 and is
killed by its recorded PID.
