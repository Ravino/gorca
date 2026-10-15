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
