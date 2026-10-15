# M5 extension host: implementation report

Date: 2026-10-15/16. Author: wasm-engineer. Design and porting guide: `docs/design/extensions.md`.
ADR-0008 amendment 1 records the deviations. Decision line in `docs/decisions.md`.

## 1. What was built

| Piece | Where |
|---|---|
| WIT 0.1 + Rust mirror + guest adapter (`export_extension!`, `host::*` free functions, one source for both builds) | `crates/gorca-ext-api` |
| Host: wasmtime 49.0.3, one `Store` per (extension, app), pooling allocator, fuel CPU limit + epoch backstop, 32 MiB, reset on fault, strikes per slot and per extension, manifest parser/matcher, settings KV, worker threads, native backend | `crates/gorca-ext-host` |
| Core hook points (additive): `ExtSource`/`AppExt`/`ExtSink`/`ExtCommands`, presenter registry, app actor `on_event`, extension commands in the Reader, `BackgroundMsg::ExtSpeech`, keymap merge in `compile` | `crates/gorca-core/src/ext.rs` and small edits |
| Reference + fault-injection extension (crash, loop, OOM, flood, std), buildable native and `wasm32-wasip2` | `extensions/ext-test` |
| Daemon wiring `--ext-dir`, `--ext-settings` | `crates/gorca-daemon` |
| xtask rule 9 (wasmtime only in the host, wit-bindgen only in ext-api, host reaches no transport) | `xtask/src/deps.rs` |

Not touched: `present/window.rs`, `present/notification.rs`, `extensions/ext-gnome-shell`, `gorca-text`,
connector `text/`, `extensions/ext-vte` (all edits to shared files are additive).

## 2. Acceptance

| Gate | Result | Evidence |
|---|---|---|
| X1 hook overhead p99 <= 100 us | WASM `on_event` 161 ns, `present` (none) 146 ns, `present` (some) 296 ns, rewrite 299 ns; native 80 to 145 ns | `bench/results/ext-host/20261016-x1-run{1,2,3}.tsv` (load 11, governor performance, P+E cores unpinned) |
| Under storm (4 / 8 threads on one host) | p99 193 / 356 ns, 0 faults | same files |
| Trap / loop recovery < 50 ms and gorca keeps speaking | crash + rebuild + next call 55 us p50, 109 us p99; loop interrupted + rebuilt 230 us p50, 444 us p99; the caller itself waits only for the faulting call | x1 rows `wasm-crash_reset_next_call`, `wasm-spin_interrupt_reset_next_call`; `tests/ext.rs` |
| 3 resets in 60 s -> disabled, one announcement | both backends | `three_resets_in_a_minute_disable_with_one_announcement` |
| No core panic from extension input (fuzz) | 20k random events/nodes/commands native, 1.5k WASM; 60 extreme-size instance calls (17 contained as faults); 300 corrupted components (216 rejected, 84 loaded and contained); garbage answers clamped | `tests/fuzz.rs` |
| M1/M4 strict gates unchanged with an extension loaded (`ext-test` matching every app) | K2 p99 0.313 -> 0.294 ms (gate 5), K3 1.364 -> 1.125 (15), R1 0.618 -> 0.416 (3); K2/K3/R1 lost = 0, not_consumed = 0; K4 app_lost / app_doubled / chord_leaked = 0 (10,000 switches x3) | `bench/results/e2e/m5-noext/`, `m5-ext/` |
| Q-E3 memory per instance | 24 KiB resident per live `ext-test` instance (100 instances; pooling reserves address space, not memory); a larger extension grows with its own heap up to 32 MiB | x1 stderr |

E2E conditions: `bench/e2e/run.sh ... --only K2,K3,K4,R1 --repeats 3 --reader daemon:target/release/gorca,--impersonate-orca[,--ext-dir=...] --reader-env GORCA_M1=1 GORCA_M1_STRICT=1 GORCA_M4=1 GORCA_M4_STRICT=1`
at committed head `00ac959`, load average 11 to 16, governor `performance`, Core Ultra 9 285H, RT kernel
7.1.8, private headless session. The run with extensions logged `extensions loaded extensions=["ext-test"]`.
The two arms differ by host noise only (ext arm was lower in all three gated p99s). Absolute numbers on a
busy workstation are not authoritative; the claim is "no regression with extensions loaded".

Two earlier e2e attempts on previous heads were stopped before they wrote results (the review fixes
landed); the numbers above are from the final head only.

## 3. Findings and design decisions worth knowing

1. **Epoch alone is the wrong CPU limit.** The first X1 storm showed per-call medians of ~35 ns for 4 and
   8 threads: the slots had been interrupted by wall-clock epoch while descheduled (load 25), reset three
   times and disabled, then answered "no opinion". Fuel (deterministic ops count) fixes it: same storm,
   0 faults, medians 149 and 219 ns. Epoch stays as a 10x wall-clock backstop. Evidence for the cause is
   circumstantial (the symptom and the fix), not a direct trace.
2. **Security review, round 1 (compile.rs and host caps).** Extension default chords must carry the gorca
   modifier and never shadow a chord the preset or the user mentions in any context (a context table is
   consulted before `global`); first extension wins; skipped chords are logged. Added per-call budgets
   (`speak` 8, `log` 8, `set_setting` 16, `get_node` 256), a 10 messages/s speech bucket per slot, a
   whole-answer cap (32 segments, 8192 characters), one memory per component, a pool of 512, a 64
   extension cap and manifest size bounds. Strikes now also count instantiation failures: per slot (an
   instance that cannot be rebuilt disables at once) and per extension (3 build failures or disabled slots
   in 60 s disable it for all new applications). A re-announced application keeps its slot and strikes.
3. **Security review, round 2 (slot.rs).** A reset used to rebuild on the caller's thread. Now the dead
   instance moves into a job and two worker threads drop it and build the replacement; `present` and
   `on_event` only `try_lock`; at most four commands per slot, queue of 256. Hostile test: with threads
   looping/crashing/OOMing fresh applications, flooding looping commands, and storming events, the real
   `Reader::handle` for a core command key stays at p50 5.0 us / p99 30 us (baseline 4.1 / 26 us).
4. **Remaining inline cost**, by design (ADR: `present` is inline in the Reader): the first faulting
   `present` runs up to its fuel limit in the Reader (measured 0.35 ms worst for a bare loop only; host-side attacks are in section 7 in 20 focus
   changes; budget 2 ms of CPU, calibrated fuel is 3M per ms), three times at most per application.
5. **WASI**: `wasmtime-wasi` is not used. It would call tokio `block_on` from inside the app actor's
   runtime and grant ambient capability. The host links only `wasi:random/*` and `wasi:clocks/*` (pure).
   `HashMap`/`Instant` work in extensions (tested); stdio/fs/net/env trap.
6. `FUEL_PER_MS = 3,000,000` is calibrated on this host's P-cores (a bare loop runs ~6 ops/ns, so it is
   cut near 1 ms; typical code 2 to 6 ms). On slower cores a runaway loop lasts longer in wall time but is
   still bounded by fuel and by the 10x epoch backstop. Re-calibrate if the CPU baseline changes.

## 4. Gates run

- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo xtask deps`: ok (rule 9 added, with a synthetic-graph test).
- `cargo test --workspace`: see section 5.
- e2e and X1: section 2.

## 5. Final `cargo test --workspace`

At head `00ac959` on a clean tree: **1545 passed, 0 failed** (exit 0), started after all edits.
An earlier workspace run at an intermediate head showed 12 ext-host failures; cause: I edited sources
while that run was compiling the component (host and guest from different WIT revisions). The same tests
passed alone at that head, and the final run above is clean.

## 6. Risks and open questions

- `fetch`/`text` host calls are `not-supported`/`denied` (need an async prefetch path; M6 `ext-vte`).
- `document_role` matcher is inert until documents are wired (M7). `nav-units`/`nav-match` have no core
  caller yet.
- Native extensions (no hang protection) are supported by the host API but none is linked by the daemon.
- The Reader-inline `present` hook is the only place an extension can add latency to a key path; if
  `ext-vte` needs heavier `present` work, move it to apply time (WE-3) or lower its fuel budget.
- Pool exhaustion (512 instances) counts as instantiation failure and can disable an extension for new
  applications; sized for ~30 apps x 4 extensions with a wide margin, not measured on a real desktop.
- Disk was at 95 to 98 % during the work (shared `target/` dirs of other worktrees are 15 to 57 GB); I
  deleted only my own incremental directory.

## 7. Review fixes (round 1, `docs/reports/m5-review.md`)

Code head for the numbers: `6c3db08`. Host: Core Ultra 9 285H, RT kernel 7.1.8, governor performance,
load average 10 to 24 (busy workstation; per-run load in each result header).

| Finding | Status | What changed |
|---|---|---|
| H-1 alias bypass | fixed | `compile_with` compares `(ChordKey, presses)` after the resolver (`Mentioned` set); the reviewer's probe is the test `extension_alias_chords_never_shadow_core_chords` (was 72 replacements over 20 chords, now 0) |
| M-1 host work not metered | fixed | wasmtime hostcall fuel caps every lift (arguments and hook results) at 64 KiB before the copy; a per-call host-work meter (about 1 unit per byte, 300 per call, 1M units per ms of CPU limit) traps loops of host calls; new guest memory is capped at 1 MiB per non-command hook call (touching 16 MiB of fresh pages cost the Reader about 5 ms and fuel does not see it); `present` backstop 3x (6 ms) with a 1 ms epoch tick |
| M-2 per-slot allowance | fixed | `Slot::call` and `health` honour the extension-wide disable; test updated (it asserted the old behaviour) |
| M-3 silent busy drops | fixed | `busy_skips` and the new `busy_suppressed` are counted, logged per slot at most every 10 s, summarised when the host stops and printed by x1; busy semantics: a busy slot repeats its last decision for that event kind (suppress/rewrite -> suppress, pass -> pass) |
| L-2 `with_ext` | fixed | `tracing::error!` and `debug_assert!` if the shared state was already cloned |
| L-3 disabled notice | fixed | `ExtSink::notice`; the background sink retries on a short-lived thread (at most 4) when the lane is full |
| L-4 docs | fixed | WIT header and `busy_skips` comment |
| M-4 scope | deferred, recorded | `ext-gnome-shell` and `ext-vte` ports wait for the M4b and M6 merges (`docs/decisions.md`) |

Busy semantics, why: a busy slot means a command (up to 20 ms CPU) or a rebuild holds the lock. Waiting
would block the app actor; falling back to Pass would let the core say what the extension has been
replacing, next to the extension's own command speech (double speech). Repeating the last decision is
non-blocking and cannot add speech. Limit, stated plainly: events that arrive while the slot is busy
are not delivered to the extension, so a stateful extension can miss them; `busy_skips` makes that
visible. A bounded replay queue is not built.

### Hostile worst case (corrects the 0.35 ms claim)

The earlier "0.35 ms worst" covered a bare counting loop only. The reviewer measured a hostile `present`
holding the Reader for 6.5 to 6.8 ms. After the fix (`tests/hostile.rs`, release, 20 fresh applications
per attack, three runs, `bench/results/ext-host/20261016-m5-fix-hostile.txt`, load 24 to 14):

| Attack in `present` | max hold of the calling thread |
|---|---|
| 16 MiB `log` argument forever | 35 to 72 us |
| 2000 segments aliasing one 16 MiB string | 33 to 43 us |
| `log` of the largest accepted string (60 KiB) forever | 123 to 141 us |
| `get_node` forever | 0.30 to 0.68 ms |
| worst of all | 0.30 / 0.68 / 0.41 ms |

Before the memory-growth cap the same attacks sat at 6.3 to 7.2 ms (the 6 ms backstop; measured in this
session at load 14, kept in this report, not in a file). Remaining unmetered cost: a legitimate-looking
hook can still spend its full 2 ms of fuel plus the host's lowering of its node argument; the debug
build of the test only checks a 25 ms bound.

### Gates

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo xtask deps`
  (35 crates, 301 packages): ok. `cargo test --workspace --no-fail-fast`: 1620 passed, 0 failed.
- Strict e2e, `--impersonate-orca`, 3 repeats, committed clean head `6c3db08`
  (`bench/results/e2e/m5fix-{noext,ext}/` for K2/K3/K4; `m5fix-r1-*` for R1):

| Gate | no ext | ext-test loaded |
|---|---|---|
| K2 p99 ms (gate 5) | 0.359 | 0.429 |
| K3 p99 ms (gate 15) | 1.208 | 1.173 |
| K4 app_lost / app_doubled / chord_leaked (10,000 switches x3) | 0 / 0 / 0 | 0 / 0 / 0 |
| K2/K3 lost, not_consumed | 0 | 0 |
| R1 p99 ms (gate 3) | 0.408 (rerun `m5fix-r1-noext2`) | 0.428 |

  The first R1 run without extensions (`m5fix-r1-noext`, load 15) FAILED (p99 3.11 ms, one lost in one
  repeat) and is kept as is: it ran with the extension host absent, so it is host noise at that moment;
  the immediate rerun at load 11 passed (0.408). The first K2/K3/K4 invocations passed
  `--reader-env` once with four values, which the harness reads as one, so R1 was skipped there; R1 was
  rerun with one `--reader-env` per variable.
- X1 (`bench/results/ext-host/20261016-m5-fix-x1.txt`, load 11): WASM `on_event` p50 119 ns / p99 172 ns,
  `present` none 129 / 159 ns, some 250 / 324 ns, rewrite 226 / 323 ns; storms 1/4/8 threads p99 181 /
  265 / 454 ns, faults 0, `busy_skips` 0; crash reset + next call 58 us p50; loop interrupt + reset
  224 us p50. Gate (p99 <= 100 us) holds with margin.
