# M18a-0 shared harness

Own cargo workspace (outside the main workspace and its gates). Spec: `../SPEC.md`.
Build with `. ./env.sh`, `CARGO_TARGET_DIR=/var/tmp/gorca-shared-target CARGO_INCREMENTAL=0`.

| Path | What |
|---|---|
| `common/` (`m18a0-common`) | wasmtime 49.0.3 host, fuel/wall/strikes, slot machine, MQ-3 budget, MQ-2 token, stub writer + presenter + sink, `WLaneRunner` and the reference/self-test runners, timing, wake probes, corpora, gates |
| `arms/arm-a`, `arms/arm-bprime` | candidate crates. Placeholders: replace `lib.rs` only, keep `pub fn factory(Vec<PluginRuntime>) -> anyhow::Result<Box<dyn WLaneRunner>>` |
| `bin/` (`m18a0-run`) | `run --arm noop\|ref\|a\|bprime --scenario f.toml --condition quiet\|loaded\|dev`, `selftest`, `list` |
| `report/` (`m18a0-report`) | `summary.json` set -> gate verdicts (`gates.toml`) -> `report.md` |
| `load/` (`m18a0-load`) | `loaded` condition: 8 spin + 2 memwalk threads |
| `guests/` | one source, 22 components, `build.sh` writes `build/guests` + `SHA256SUMS` |
| `wit/`, `check-subset.sh` | subset of `wit/gorca-ext-0.10` and the proof it is a strict subset |
| `scenarios/` | one TOML per scenario |
| `host/` | `prepare.sh` (quiet-host on/off/check), `evict-cache.sh`, `strace-writer.sh` |

## Run one arm

```text
guests/build.sh                                             # once
cargo run --release -p m18a0-run -- run --arm bprime --scenario scenarios/benign.toml --condition quiet --out ../results
./run-all.sh --arms bprime --condition quiet --runs 5       # full matrix, randomized order
cargo run --release -p m18a0-report -- ../results --arm bprime --code arms
cargo run --release -p m18a0-run -- selftest                # harness must fail bad runners
```

Results: `../results/<stamp>-<arm>-<condition>-<scenario>/` (never overwritten): `meta.json`,
`summary.json`, `stall.u32.zst`, `latency.u32.zst`, `lateness.u32.zst`, `events.jsonl.zst`.
For `storm-*` the latency/lateness files hold ingress lag with plugins on / off.

## Deviations from the spec (recorded, not hidden)

1. Location: crates live under `harness/` (own workspace) instead of `prototypes/m18a-0/`.
2. `events.jsonl` is zstd-compressed (`events.jsonl.zst`), per-plugin arrays only for nonzero outcomes.
3. `TurnCtx` has an extra `wait: bool` (this writer owns the wait; false for the 2nd writer of a K1+K2
   path and inside the flapping window). `WLaneRunner` also has default methods `describe`,
   `thread_timer_slacks`, `plugin_queue_depths`.
4. Lazy parameters: bindgen lifts eagerly, but `Store::set_hostcall_fuel(4096)` bounds the bytes lifted
   per host call, so oversize arguments fail before any allocation or UTF-8 validation (`WasmStr` has no
   length accessor in 49.0.3). Verified by an allocation counter (`CountingAlloc`).
5. Import pre-charge (B-8) is a parallel counter capped at the grant (no store access from bindgen
   hosts), not a deduction from store fuel; combined bound is at most 2x the grant plus the 4x wall watchdog.
6. Guest limits: strings are 14 MiB (the 16 MiB memory cap cannot hold one of 16 MiB); A8 has no
   `requests` import in the subset, so a fact in group 900 stands for a refresh-like request.
7. Per-thread timer slack is read with `PR_GET_TIMERSLACK` (this kernel exposes `timerslack_ns` only for the
   process leader).
8. The writer busy-polls its mailbox while idle. M3 counts a pair as served only when the fact came from
   the reply to this turn's job.
9. kappa is calibrated at start with the spin guest (median of 9) unless `--kappa` is given; WQ-2 gives the table.
10. `adv-a6` adds nine spinners on top of the benign fixture (10 plugins at full grant is the spinners alone
    plus the fixture); `a3` runs three guests (log, assert text, attr key).
11. The K1+K2 pair runs sequentially on one thread (conservative for the combined stall).
12. `m8_isolation_pp` and `pf1_differences` are derived by `m18a0-report` across runs.

## Spec clarifications made after the first arm runs (harness fixes, gates and arms untouched)

- **C-1 flapping cap (SPEC section 2 "at most once per N ms", M3, M5).** The `N` ms cap now exists only in
  scenarios of `kind = "flap"` (default 20 ms there) or with an explicit `flap_ms`. Every other scenario
  (benign, adv-*, keypress, k1k2, storms) runs with the cap off (`flap_ms` 0 = no cap), so each focus change
  owns its bounded wait. Reason: the stock scenarios space focus changes 0.3-2 ms apart, so the 20 ms cap denied
  the wait on about 98 % of turns and the M3 gate (facts at first utterance) was impossible for any runner;
  the cap is a flapping-focus protection (M5), not a property of the benign mix. `flap_ms = 0` is valid
  (`flap_metrics` used to underflow). Arms that tune `N` are unaffected; no arm code changed.
- **C-2 run isolation.** `run-all.sh` writes into `results/<UTC stamp>-<pid>/<arm>/`, keeps its log and a private
  copy of the binaries there, and holds a PID lockfile (`/var/tmp/m18a0-run-all.lock`, `M18A0_LOCK` to
  override): a second invocation refuses while the PID is alive; a dead PID is a stale lock and is replaced.
- **C-3 watchdog per scenario.** `watchdog_s` in the scenario TOML, else `--watchdog-s`, else
  `default_watchdog_s()`: 600 s, except `keypress` = max(600, 1.5 x 2 x pairs x leg_s + 120) = 1020 s for
  keypress-onoff; adv-a3, adv-all, adv-each-added (per stage) = 2400 s (arm A needed it at 200k samples).
  `--watchdog-s` overrides everything.
- **C-4 quiet host.** `host/prepare.sh check|wait` requires loadavg1 < 1.5, PSI cpu some avg10 < 1.0,
  every governor `performance`, and no process named cargo, rustc or *e2e*. `run-all.sh --condition quiet`
  waits (`--quiet-wait`, default 900 s) before each run; on timeout the run still happens, labelled
  `quiet-precondition-timeout`. `meta.json` `quiet_violated` uses the same limits at run start and end
  (the busy-polling writer itself adds about 1.0 to loadavg1, so the old 1.0 limit flagged every run).
- **C-5 handle MAC.** `MacKey` = per-`PluginRuntime` random key; the MAC is keyed SipHash-1-3 (std
  `RandomState`, OS-seeded, never exposed), 32 bits kept in the handle. WQ-3 now measures that cost.

Known: `selftest` "every plugin saw stale replies dropped" is flaky on a loaded host (a cold first-call wall
overrun demotes a plugin; passed 2 of 3 here at loadavg 4+). `guests/build.sh` failed in this worktree
(E0463, wasm32-wasip2 std not found by `cargo`); the guests were copied from the B' worktree (same sources,
SHA256SUMS verified).
