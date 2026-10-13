# End-to-end latency harness (Phase 1)

Measures keypress / focus event to speech start on a private, headless AT-SPI session.
Code: `crates/gorca-bench/src/e2e/`, binaries `e2e` and `gorca-stub-reader`; stimulus scripts
`tests/harness/fakeapp.py` and `tests/harness/fake_mutter.py`.

```sh
bench/e2e/run.sh <label> [--quick] [--reader stub|daemon:/path/gorca-daemon[,arg...]] \
                 [--backend voicego|ssip] [--only L1,L2,...] [--repeats N] [--reader-env K=V]
bench/e2e/compare.py bench/results/e2e/<a>/<x>.json bench/results/e2e/<b>/<y>.json
```

Results land in `bench/results/e2e/<label>/<UTC>.{json,txt}` (same row format as
`bench/results/b2-ingress/`). Host metadata (load average at start and end, governor, CPU
count and model, kernel, git revision and dirty flag, reader, repeats, sample counts) is in the
`meta` object and in the `.txt` header.

## What runs where

```
run-isolated.sh: private dbus-daemon (session) + at-spi-bus-launcher (a11y bus) + registryd
  fakeapp.py x N      focus cmd -> t_emit_ns (CLOCK_MONOTONIC, taken just before the send)
  fake_mutter.py      org.freedesktop.a11y.Manager on the private session bus, KeyEvent unicast
  Rust storm app      b2 feeder over a raw bus connection, 10k / 50k ev/s
  reader (child)      gorca-stub-reader today, gorca-daemon later
  e2e (this process)  mock speech server (voicego binary / SSIP text), focus tap, scoring
```

`e2e` refuses to start unless both bus addresses live under a `/gorca-h.*` temp dir, so it
cannot touch the real session. It never signals anything it did not spawn.

## Scenarios and metrics (all milliseconds)

| id | stimulus | metrics |
|---|---|---|
| F0 | client writes SPEAK straight to the mock (no reader) | `f0_mock_ms`: harness measurement floor |
| L1 | 1200 focus changes of distinct nodes, 8-16 ms apart, idle | `l1_ms` emit to SPEAK; `l1g_ms` bus arrival to SPEAK (gorca side); `l0_ms` emit to bus arrival (floor) |
| L1s | as L1 against the speech-dispatcher text mock | same |
| L2 | focus A, wait for its SPEAK (3 s utterance), 15-45 ms later focus B | `l2_ms` emit B to STOP of A; `l2s_ms` emit B to SPEAK B; `stale_not_cancelled` |
| L3-10k, L3-50k | L1 while another app (terminal-style delete/insert/caret flood) emits 10k / 50k signals/s | as L1, plus achieved storm rate, reader CPU |
| L4 / L4c | L1 while a second app never answers any D-Bus call (L4) or answers normally (L4c control); reader runs with peer calls on (deadline 100 ms) | as L1, plus `peer_calls_hung` (proof the hang was exercised) |
| K1 | emulated Mutter `KeyEvent` 8-16 ms apart | `k1_ms` key emit to SPEAK |
| K2 | M1: gorca modifier + command key (zero-IPC command), idle | `k2_ms` command-key emit to SPEAK, `lost`, `not_consumed` |
| K3 | M1: the same chord while an L3-50k storm runs | `k3_ms`, `lost`, `not_consumed`, storm rate |
| K4 | M1: randomized mode / grab-set toggles (10k; 500 with `--quick`) interleaved with plain typing | `app_lost`, `app_doubled`, `chord_leaked` |

K2-K4 (`crates/gorca-bench/src/e2e/m1.rs`) are skeletons for the M1 command layer and are
**skipped** until it lands (`K<n>.skipped = 1`). `--reader-env GORCA_M1=1` runs them and enforces
the gates; `--reader-env GORCA_M1_STRICT=1` makes them fail instead of skip (proves the gate is
wired). They drive `tests/harness/fake_mutter.py`, which emulates Mutter's real grab semantics
(exact-mask `SetKeyGrabs` matching with the NumLock/CapsLock variants, consumed vs pass-through
keys, the a11y-modifier first-click rule, owner loss), and use its verdict (`consumed`, app
log) as ground truth. Chord keys are placeholders until ADR-0005: `GORCA_M1_MODIFIER` (default
Insert), `GORCA_M1_CMD_KEYSYM` (default `t`), `GORCA_M1_MODE_KEYSYM` (default `m`). Note: the
K3 here is "modifier chord under storm"; phase-2.md 2.1 lists K3 as "where am I", which stays
a later scenario under another id.

Every scenario also reports `lost` (probes that never produced a SPEAK) and reader CPU:
`cpu_us_per_event` is the reader's on-CPU time (all threads, `/proc/<pid>/task/*/schedstat`)
over the measured window minus its idle rate, divided by AT-SPI signals delivered (a focus
change is 3 signals). It is wake-up dominated when events are sparse (L1) and a true
per-event cost under a storm (L3).

## Statistics and gating

* Every run yields exact p50/p95/p99/p999/max over its samples.
* Repeats are interleaved: round-robin over the scenarios, rotated every round, each scenario
  run gets a fresh bus clients, mock, reader and apps.
* The reported value of a percentile is the **median over repeats**; min and max over repeats
  are the spread. Gates apply to that median (B-2 lesson: this host sits at load ~10, a
  single repeat is noise). A p999 from 1200 samples is the 2nd largest sample: treat as
  informational.
* Gates (`op`/`threshold` columns): see "Proposed gates" below. `lost` must be 0.

## Proposed gates

| gate | threshold | why |
|---|---|---|
| L1 p99 (emit to SPEAK received) | <= 15 ms | CLAUDE.md product goal for gorca-side processing; the measured path additionally contains the app's send and the bus hop (L0, < 1 ms p99), so the gate is conservative |
| L1 under storm 10k and 50k, L4, K1 | <= 15 ms | the goal must hold under the stress the product promises to sustain; same bound as idle |
| L2 p99 (newer focus to STOP of stale speech) | <= 5 ms | cancelling must be cheaper than starting: one write on an already open socket; leaves 3x margin over the stub baseline |
| `lost` | == 0 (max over repeats) | a probe is lost when the reader never spoke it. L1/L1s: every unspoken probe. L3/L4: only an unspoken probe with no spoken successor, because the ingress slot keeps only the newest `Focus` (ADR-0001 R-1), so a probe overtaken by the next one is `superseded`, not lost |
| `superseded_max_run` (L3, L4) | <= 10 in every run | bounds how stale the reader may fall under stress: 10 probes at the 8-16 ms gap is about 120 ms; `superseded` (count) is informational |
| K2 p99 / K3 p99 | <= 5 ms / <= 15 ms | phase-2.md 2.1 ceilings (targets 2 / 5 ms) |
| `not_consumed`, `app_lost`, `app_doubled`, `chord_leaked` (K2-K4) | == 0 in every run | a command key that reaches the app, or a typed key lost or doubled, is a correctness failure |
| storm rate | >= 90% of target | otherwise the stress is not what its name says |
| `peer_calls_hung` (L4) | >= 1 | otherwise L4 is vacuous |

Tighten after the daemon lands: the stub is a floor (no mirror, FSM or verbalizer), so a
reasonable end state is L1 gorca-side p99 <= 5 ms and L2 p99 <= 2 ms, with 15 ms as the
non-negotiable ceiling.

## The reader seam (what gorca-daemon must honour)

The reader is a child process, configured by environment only:

| variable | meaning |
|---|---|
| `GORCA_A11Y_BUS_ADDRESS` | a11y bus (also `AT_SPI_BUS_ADDRESS`); the daemon must not call `org.a11y.Bus.GetAddress` on the real session when this is set |
| `DBUS_SESSION_BUS_ADDRESS` | private session bus: `org.gnome.Orca.KeyboardMonitor` ownership + `org.freedesktop.a11y.Manager` |
| `GORCA_SPEECH_BACKEND` | `voicego` (binary framing) or `ssip` |
| `GORCA_SPEECH_SOCKET` | unix socket path of the speech server; the daemon must connect to it instead of its default |
| `GORCA_STUB_CALLS`, `GORCA_STUB_DELAY_US` | stub only |

Readiness is detected by the mock seeing a connection plus a warm-up focus producing a SPEAK;
the daemon needs no ready line. Correlation: the harness understands `... node-<N> ...` in the
spoken text (fakeapp names its nodes `node-<N>`), so a daemon that speaks the accessible name
needs nothing special. Keys are correlated by order when the text has no `k:<keycode>` tag.

The daemon must also: (1) honour SIGTERM (the harness sends it after closing stdin), (2) not
require a display, (3) run with the keyboard monitor authorisation of the emulated Manager
(`RequestName org.gnome.Orca.KeyboardMonitor`, then `WatchKeyboard`).

## Validating the harness itself

* `F0`: the mock server plus timestamping costs p99 ~0.2 ms.
* Injected delay: `--reader-env GORCA_STUB_DELAY_US=5000` shifts L1 p50 by +5.1 ms
  (measured 0.33 -> 5.46 ms), so the harness sees what it should.
* `--reader daemon:target/release/gorca-stub-reader` exercises the daemon launch path with the
  stub as an arbitrary external binary.
* Unit tests: `cargo test -p gorca-bench --lib e2e` (mock server framing, correlation).

## Known limits

* One dbus-daemon, not dbus-broker (F-3 broker backpressure is covered by the B-2 harness).
* fakeapp.py is Python: its `t_emit_ns` is taken a few tens of microseconds before the bytes
  leave the process, which is included in L1 (conservative).
* The Rust storm app is a raw-socket writer; the real GTK/Firefox storms are burstier.
* The mock timestamps arrival, not audio; real voicego adds `701 BEGIN` ~2 ms (null sink) and
  audio-device buffering, both outside gorca's budget.
