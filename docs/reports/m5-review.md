# M5 extension host: adversarial review (r1)

Date: 2026-10-16. Reviewer. Branch `worktree-agent-a5aefe82e077800c6`, head `8b6005b` (code at `00ac959`).
Review worktree: `.claude/worktrees/m5-review` (detached). Probes: `bench/results/ext-host/m5-review/`.

**Verdict: MERGE AFTER FIXES.** One high finding (keymap authorization bypass, reproduced) must be fixed;
the rest are medium/low.

## Gates rerun (load 9.5 to 15.5, governor performance, 16 CPUs: 6 P / 8 E / 2 LP-E)

| Gate | Result |
|---|---|
| `cargo fmt --all --check` | ok |
| `cargo clippy --workspace --all-targets -- -D warnings` | ok |
| `tools/check-deps.sh` | ok (34 crates, 300 packages; rule 9 present) |
| `cargo test --workspace` | 1545 passed, 0 failed (matches the implementer's count) |
| strict e2e K2/K3/K4/R1 with and without ext-test | **not rerun** in this round (time); the implementer's runs at `00ac959` are in `bench/results/e2e/m5-{noext,ext}/` |

## Findings

### H-1 (high): extension default chords bypass the "never shadow a core chord" rule through key aliases
`crates/gorca-core/src/commands/compile.rs` (the `mentioned` set in `compile_with`). The set is keyed by
`ChordSpec`, which keeps the key *name* as written (`normalise_key` only lower-cases). The resolver
(`gorca-input-api/src/keys.rs` `ALIASES`) maps `enter`/`return`, `page_up`/`prior`, `page_down`/`next`,
`esc`/`escape`, `kp_0..kp_9`/`kp_insert..kp_prior`, `plus`/`equal` to the same key. An extension that
spells a preset chord with the other alias passes the check, and `km.insert` replaces the core binding for
the same `(ChordKey, presses)`.
Probe `probes/probe_keymap_alias.rs` (a test inserted into `compile.rs`'s test module): **72 shadowings
over 20 distinct core chords** across the default, orca and laptop presets, e.g. `gorca+ctrl+return`
(review.activate) taken by `gorca+ctrl+enter`, `gorca+prior`/`gorca+next` (review paging) by
`gorca+page_up`/`gorca+page_down`, and every `gorca+kp_*` object-navigation chord by `gorca+kp_N`.
Output: `probe_keymap_alias.out`.
Fix: compare resolved `(ChordKey, presses)` (after `chord_key`) instead of `ChordSpec`, or canonicalise
aliases in `ChordSpec::parse`. Add the probe as a regression test.

### M-1 (medium): host-side work in host calls and in lifting results is not metered by fuel
`crates/gorca-ext-host/src/wasm.rs` (`wh::Host` impl) and `instance.rs` (`log`, `speak`, `set_setting`).
The per-call budgets and the 512-char/64-segment clips run *after* wasmtime has lifted the whole argument
into host memory; `present`/`on_event`/`command` results are likewise lifted in full before
`segments_from_iter` clamps them. Fuel counts only guest instructions, so a hook that spends its time in
host calls is bounded by the epoch backstop (10x the limit: 20 ms for `present`, on the Reader), not by the
2 ms CPU limit. The comment "never copy a hostile list whole" is true only for the native backend.
Measured (`probes/probe_review.rs`, hostile component `probes/ext-hostile`, release, load ~13):
a hostile `present` faulted and was reset in 6.5 to 6.8 ms on the calling thread, three times, then
disabled. This is well above the "0.35 ms worst" in the report, which covered only a bare loop.
Note: in these runs the guest faulted on its own large allocation, so the probe did not isolate the lift
cost itself; the unmetered lift is established by code reading, not by measurement.
Fix: make `log`/`set_setting`/`get_setting` take bounded inputs (check the length before lifting, e.g.
`WasmStr`/`WasmList` lazy types via `func_wrap` instead of typed bindgen arguments), lift results lazily
with caps, and lower the backstop for `present` (it runs on the Reader) to about 2 to 3x.

### M-2 (medium): the Reader-inline `present` costs up to the backstop three times per app, and the per-extension guard does not stop slots already attached
`slot.rs` `LoadedExt::strike`/`is_disabled` is checked only at `attach`. An extension disabled as a whole
keeps running in every app it is already attached to; each of those can still make three faulting calls.
With M-1 that is up to 3 x ~20 ms of Reader stall per attached app. "Never block" holds per call, but the
bound is not the 2 ms the design doc states.
Fix: check `ext.is_disabled()` in `Slot::call`; document the true worst-case bound.

### M-3 (medium): `try_lock` drops extension decisions silently
`slot.rs` `Slot::lock(false)`. `busy_skips` is counted (good), but nobody reads it: no log, no metric in
the daemon, and the counter's comment says "present calls" although `on_event` skips count too. While a
command runs (up to 20 ms CPU, 200 ms wall on a worker) every `on_event` for that slot is skipped, so
a stateful extension (the planned `ext-vte` prompt tracker, the Shell switcher port) loses events and a
`Suppress`/`Rewrite` becomes `Pass` (double speech). Fix: expose the counter (debug probe / periodic
log), and do not run commands under the same lock as the event hooks, or queue skipped events for the
next call.

### M-4 (medium): scope of the plan's M5 increment not delivered
Phase-2 plan section 3 M5: "`ext-gnome-shell` (and `ext-vte` skeleton) run as WASM components" and the
B-4 bake-off (WASM-only vs dual). Neither the port nor a bake-off ran; `docs/decisions.md` records the
dual build as decided. The orchestrator should either accept the deferral explicitly (decision line) or
keep M5 open. WIT 0.2 deferral to M7 is in line with the plan text ("TBD-WEB-7 hooks stay for M7").

### L-1: `FUEL_PER_MS` calibration on slower cores
Fuel is deterministic, so slow cores cause no false faults; they lengthen the wall time of a runaway
call. LP-E cores here run at 2.5 GHz vs 5.4 GHz, so the 2 ms budget is about 4 to 5 ms of wall time there,
still under the epoch backstop. Acceptable; state it in the design doc instead of "about 1 to 4 ms".

### L-2: `Core::with_ext` is a silent no-op if `shared` is already cloned
`runtime.rs` `with_ext`: `if let Some(sh) = Arc::get_mut(..)` with no else. Today `wire_ext` runs first
so it works; a reordering would load extensions that never run. Log or return an error.

### L-3: the "disabled" notice can be lost
`ext.rs` `BackgroundSink::speak` uses `try_send`; under a full background lane the one announcement the
acceptance requires is dropped. Low; consider a dedicated slot or retry on the next drain.

### L-4: documentation drift
`wit/gorca-ext.wit` header says hooks "never [run] on the Reader lane"; `present` does. `ExtStats::busy_skips`
comment (see M-3).

## Claims checked

| Claim | Status |
|---|---|
| Extension chords need the gorca modifier | holds (`ExtCommands::bind`) |
| Never shadow user/core chords | **fails** through aliases (H-1) |
| Cross-app scope | holds: commands go to the focused app's `AppExt`; `get_node` checks the app |
| Per-call host budgets, speech bucket, answer cap | hold for counts; the size caps apply after the lift (M-1) |
| Resets and drops on workers | holds (`Job::Rebuild`) |
| Strikes survive re-attach | holds while not detached; extension-wide guard only blocks new attaches (M-2) |
| Settings KV caps | hold (64 B key, 4 KiB value, 128 keys), checked after the lift (M-1) |
| Memory 32 MiB, pool 512 | configured as stated; the cap applies to guest memory only, not host lift buffers |
| No connector types in core | `check-deps` ok |
| Bulk memory ops metered | yes: wasmtime 49 charges `memory.copy`/`fill` per byte (`VariableOperatorCost`), so a bare loop is bounded as claimed |

## Required before merge
1. Fix H-1 and add the alias regression test.
2. M-1: bounded host-call inputs, or at least a lower `present` backstop and a measured worst case in the
   report. 3. M-2: honour extension-wide disable in `Slot::call`.
4. Orchestrator decision on M-4.
