# ext-vte as a WASM component (M6 follow-up)

Status: done. The terminal policy runs as a `wasm32-wasip2` component by default; the native build of the
same source is the exclusive fallback (ADR-0008 amendment 1, `--terminal-policy=auto|native`). The ADR-0008
condition for a narrow hook-shape bake-off (ext-vte misses V1/V2) is **not met** (numbers below).
Raw data: `docs/reports/ext-vte-wasm-data/`, e2e results `bench/results/e2e/vte-*`.

## What changed

- **WIT 0.2.0.** The world exports a second interface, `terminal` (`configure`, `step`, `snapshot`,
  `counters`; the types were already in the file, M6 left them out of the world). Additions: `raw-key`
  and `term-input::raw` (the key as the core saw it), `term-counters` and `counters`. `Hooks::terminal()`
  (default `None`) is how an extension offers it; `export_extension!` exports both interfaces, so every
  extension answers `terminal` (no actions, no snapshot unless it is a terminal policy). ADR-0003/0008 had
  reserved 0.2 for the web hooks; those become 0.3 (additive either way).
- **The core imports no extension crate.** `gorca-core` -> `ext-vte` is gone (dev-dependency for tests
  only). Seams in `gorca-core::ext`: `AppExt::terminal(cfg) -> Option<Box<dyn TermSession>>` (extension
  host), `TermProvider` (the daemon's native build, installed with `Core::with_term_provider`). `Reader`
  tries the host first, then the provider. `unit_at` and the read-only screen rows (`Rows`) moved to
  `gorca_ext_api::terminal` (the snapshot contract, a parity test keeps `ext-vte`'s `Screen` in step); the
  review model uses `Rows`.
- **Readline bindings left the core** (m6-review finding 8): `classify_term_key` is now `ext-vte`'s
  `keys::classify` over `RawKey` (keysym, scalar, modifiers, physical letter). The core only builds the
  `RawKey` (`raw_key`) and asks `RawKey::types_a_character()` for the echo toggle.
- **`ext-vte` crate**: `cdylib` + `rlib`, `ext.toml` (hooks `["terminal"]`, no capabilities, `app_name`
  globs of the VTE terminals: gnome-terminal, ptyxis, kgx, tilix, xfce4-terminal, ...; the fake terminals
  of the e2e harness are now named `gnome-terminal-fake` / `-bg`). `cargo xtask build-ext` ships it.
- **Host**: `HookKind::Terminal` (16 ms CPU limit, 3x CPU backstop, 1 MiB result lift, 8 MiB growth per
  call: a screen of up to 256 KiB is copied, indexed and diffed), `HostTermSession` over the slot, slot
  `generation()`, manifest hook `terminal`. A fault does not lose the session: the next call configures the
  new instance and sends `focus` (the screen is read again) before the input.
- **Daemon**: `--terminal-policy=auto|native` (`GORCA_TERMINAL_POLICY`), native provider
  (`terminal_policy.rs`) with `catch_unwind`, three panics switch it off, restart after a panic re-baselines.
  Log line `terminal policy: ext-vte WASM component | native build`.
- **The "fetch/text not supported" gap did not need closing**: the `terminal` interface is a pure function;
  the extension asks for the screen as an action (`fetch-screen`) and the core reads it under its deadline
  and feeds it back as data.

## Passwords (VP) fail closed

Review request: a trap, busy slot, strike-disable or backstop must never lose the "sensitive" marking, and
no log path may see keystrokes.

- Core (`terminal.rs`, independent of the extension): `secret_hold` starts on the extension's own
  `sensitive` flag or on the core's own keyword check of the screen row under the caret (`is_secret_prompt`,
  broader than the extension's expression: it only over-marks) and ends on Enter, Ctrl+C, Ctrl+D or
  untracking. While held, every `Say` is marked sensitive, and a typed key the extension did not answer is
  claimed and spoken by the core (marked sensitive), never left to the generic echo.
- `TermSession::alive()`: false after a lost call (fault, busy, disabled): typed keys then count as secret
  until the policy answers again. The core reads the screen itself on focus if the policy did not ask.
- An application whose terminal policy is disabled keeps a `WatchOnly` session (reads the screen on focus
  and, rate limited, after changes, says nothing) so the core check still sees a prompt.
- Logs: `Debug` of `TermKey`, `TermEdit`, `TermInput`, `RawKey`, `TermAction` shows no text; the core's
  debug line names kinds and offsets only; fault messages carry no inputs.
- Tests: core `a_policy_that_dies_during_a_password_prompt_keeps_every_typed_key_sensitive`,
  `the_core_sees_a_password_prompt_even_when_the_policy_never_answers`,
  `a_terminal_whose_policy_was_disabled_is_still_watched_for_secret_prompts`,
  `the_secret_prompt_table_of_the_core`; host `after_a_fault_...` (alive false then true); daemon
  `raw_keys_and_a_password_prompt_behave_the_same_and_leak_nothing` (three runtimes, no secret character
  in any debug output). VP gate: 0 plaintext in all 6 fake arms; VV (real terminal) password: 0 plaintext,
  log non-empty, in all 6 arms.
- Residual: typed characters are spoken (user decision); the instance memory of the WASM guest is not
  zeroed on `blur` (the extension drops its state; the allocator keeps the bytes until reuse).

## Parity (native vs WASM over recorded sessions)

`crates/gorca-daemon/tests/term_parity.rs` drives the drivers of `extensions/ext-vte/tests/support/`
through three runtimes (the daemon's native provider, the native build behind the extension host, the
component): the recorded real gnome-terminal 3.56 / VTE 0.80 session (raw events and after the T-1
collapse) and 40 random typing sessions of 400 keys against a model of a real VTE. Every action with its
time, the snapshot after every input and the counters are equal in all three, 0 faults, 0 busy skips; also
raw keys + password prompt (identical, nothing leaks), and a 72 KB Cyrillic screen and one beyond the
256 KiB cap through the component. In a debug build the native-host leg is skipped in the fuzz (an
unoptimised native slot overruns the 16 ms check after the fact); `cargo test --release -p gorca-daemon
--test term_parity -p gorca-ext-host --test term` runs everything: 5 + 4 passed.
`crates/gorca-ext-host/tests/term.rs` (both backends, `ext-test` as a faulting policy): a panic, a loop and a
memory overrun each reset in bounded time and the next key re-baselines; three faults disable.

## Cost of the host path (X1 for the terminal, `x1_term`, release, load 21, ns)

`data/x1_term.tsv`, 20,000 iterations (screens 2,000 / 400 / 50):

| input | WASM p50 / p99 | native behind host p50 / p99 | native direct p50 / p99 |
|---|---|---|---|
| typed key / BackSpace | 478 / 513 | 94 / 104 | 25 / 30 |
| text-changed payload | 461 / 500 | 106 / 118 | 47 / 55 |
| caret | 305 / 338 | 84 / 92 | 25 / 34 |
| screen reply 24x80 (one line scrolled) | 13,138 / 13,955 | 4,219 / 4,540 | 4,147 / 4,553 |
| 60x240 | 63,965 / 86,420 | 28,605 / 37,986 | 28,394 / 32,383 |
| 300x120 Cyrillic (72 KB) | 444,775 / 458,957 | 254,937 / 266,346 | 254,528 / 264,481 |

WASM costs about 0.4 us more per key and 1.7 to 3 times the native screen step (worst 0.46 ms), 0 faults.
The 16 ms limit has 35x headroom over the largest real screen; the first limit (8 ms) faulted a screen at
the 256 KiB cap (fuel, not time), which is why it is 16.

## Gates (clean head, `git_dirty=false`; host load 8 to 25, governor performance, no quiet-host script)

`bench/results/e2e/vte-{fake,strict}-{native,wasm}-{1,2,3}`, 3 rounds, arms interleaved (round 1 native
first, round 2 WASM first, round 3 native first), `--impersonate-orca`, one repeat per run,
`GORCA_V2_SECS=10`, strict M1/M4 flags; load and memory pressure per arm in `data/arms.log` (PSI
memory `some avg10` 0.00 to 0.87). Summary table: `data/e2e-summary.md`. p99 ms, `F` = gate failed:

| gate | native 1 / 2 / 3 | WASM 1 / 2 / 3 |
|---|---|---|
| V1 (35) | 48.8F / 39.0F / 40.6F | 33.4 / 32.8 / 32.8 |
| VT key (3) | 3.61F / 0.54 / 0.53 | 3.28F / 0.77 / 0.63 |
| VT missing (0) | 0 / 0 / 1F | 0 / 0 / 0 |
| V2 reader CPU % (60) | 1.58 / 1.87 / 1.64 | 2.11 / 1.58 / 2.20 |
| V2 key in flood (4) | 5.80F / 0.53 / 0.61 | 0.48 / 3.39 / 0.69 |
| V3 stop (2) | 3.58F / 1.45 / 0.31 | 0.21 / 2.89F / 0.26 |
| VP plaintext (0) | 0 / 0 / 0 | 0 / 0 / 0 |
| VR (3) | 0.39 / 1.58 / 0.35 | 0.39 / 2.13 / 0.50 |
| VB key (4) | 0.46 / 0.45 / 0.50 | 0.53 / 7.00F / 0.55 |
| VW (35) | 32.7 / 32.2 / 32.3 | 32.4 / 35.0F / 32.4 |
| VF / VH failures (0) | 0 / 0 | 1F in one run, else 0 / 0 |
| strict K2 / K3 / R1 / R1s | 0.37 / 1.17 / 1.00 / 1.18 (round 1) | 0.30 / 1.14 / 0.51 / 0.48 (round 1) |
| K4 resent per 10k, lost | 0, 0 in all six | 0, 0 in all six |

Every miss is a load outlier of one run and appears in both arms (the native arm misses V1, VT, V2 key and
V3 in round 1, one VT key lost in round 3; the WASM arm misses VT key in round 1 and V3, VB, VW, one VF step
in round 2). Nothing correlates with the backend: at the quietest load (round 3, 8 to 10) the WASM arm
passes every row. The native V1 tail (p95 36 to 43 ms) looked systematic, so a dedicated A/B
(`vte-v1ab-*`, V1+VW, 4 rounds interleaved, one run edited by the merge and kept): V1 p99 native 32.8 /
32.6 / 32.9, WASM 33.1 / 32.7 / 34.6 / 38.2; the tail is host load, not the arm.

**V1/V2 threshold of ADR-0008 (bake-off only if ext-vte misses V1/V2): not hit.** V1 WASM p99 32.8 to 33.4
(gate 35, median overhead over the 30 ms settle 2.2 ms, the same as native); V2 reader CPU 1.6 to 2.2 %
(gate 60) with 0 lost keys.

Real terminal, `bench/results/e2e/vte-vv-{native,wasm}-{1,2,3}` (`tests/term/run_real_vte.sh`, real
gnome-terminal-server in a private headless session), p99 ms: key echo native 0.58 / 0.51 / 0.45, WASM
0.45 / 0.53 / 0.51; key during floods native 6.94F / 0.47 / 0.43, WASM 0.49 / 0.71 / 0.52; Ctrl+C stop 0.08
to 0.20 in all; reader CPU 1.6 to 2.6 %; `vv_failures`, password plaintext, type missing/double/extra,
server aborts, screen failures all 0 in all six. The one native miss is the first run at load 12 to 16.

## Repo gates

`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask deps`,
`cargo xtask build-ext`: clean. `cargo test --workspace --no-fail-fast` on the merged head: 1982 passed, 0 failed (ignored as before).
Merge of `main` (terminal flat-review staleness fix, M8) reconciled in `terminal.rs`: the tracking serial
and review adoption work over `TermSession` unchanged.

## Not done / open

- The extension serves terminals by application name: a VTE terminal that is not in the manifest list is
  not tracked (no screen diff). Adding a matcher on "has a Terminal-role node" needs the host to see roles
  at attach.
- Not run against a live GNOME session (live-session rule); real evidence is the private real VTE.
- A first-instantiation cost of the 200 KB component at daemon start (tens of ms, same blocking task as the
  Shell component) is not separately measured.
- `LIMIT_TERMINAL_MS` 16 ms runs inline on the Reader like `present`; a hostile 256 KiB screen can hold it
  that long once per fault (three strikes, then the policy is off and the core watches for secrets).
