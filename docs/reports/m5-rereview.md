# M5 extension host: adversarial review, round 2

Date: 2026-10-16. Reviewer. Branch `worktree-agent-a5aefe82e077800c6`, head `d7878c8` (code `6c3db08`),
compared with round 1 at `8b6005b` (`docs/reports/m5-review.md`). Review worktree:
`.claude/worktrees/m5-review` (detached at `d7878c8`, a trial merge of `main` at `b8521b1` was built and tested, then aborted).
Probes and outputs: `bench/results/ext-host/m5-rereview/`.

**Verdict: MERGE AFTER FIXES.** H-1 as reported is closed, but the same rule ("an extension default chord
never takes a core chord") is still broken through multi-press chords (new high, reproduced, one-line class of
fix). The M-1 numbers hold for the attacks the implementer tested, but the WASI shims are outside the host-work
meter: a `present` hook holds the Reader 5.1 ms on **every** call without ever faulting (new medium).

## Gates (load 10 to 12, governor performance)

| Gate | Result |
|---|---|
| trial merge with `main` (`b8521b1`) | one text conflict in `docs/decisions.md` (both sides append rows; keep both). Code merges clean |
| `cargo fmt --all --check` (merged) | ok |
| `cargo clippy --workspace --all-targets -- -D warnings` (merged) | ok |
| `cargo test --workspace --no-fail-fast` (merged) | 1629 passed, 0 failed (`merged-test.txt`) |
| strict e2e K2/K3/K4/R1 with ext-test loaded (merged, release) | see "E2E" below |

## Round-1 findings

| Finding | Status | Evidence |
|---|---|---|
| H-1 alias bypass | **closed for spellings** | `Mentioned` compares `(ChordKey, presses)` after the resolver. My r1 probe is now the regression test (0). New probe: 183 spelling variants (upper-case key, reversed modifier order, `Caps_Lock`/`Insert` for `gorca`, `Control`/`SUPER`): 0 shadowed. Keysym numbers are not accepted by the parser/resolver at all, so no vector. **But see N-1** |
| M-1 host work | **mostly closed; N-2 open** | Re-ran the implementer's fixture: worst 0.30 to 0.68 ms. My new vectors: `get_setting` loop of a 4 KiB value faults at 1.1 to 2.1 ms (meter works); oversize hook result (100 x 1000 B) refused by hostcall fuel in 50 to 70 us; 2000 x 1000 B in 0.36 to 0.49 ms; clock loop faults at 2.2 to 4.8 ms (fuel). WASI `get-random-bytes` loop: **6.1 to 6.4 ms, the backstop** |
| M-2 disable stops attached slots | closed | `call_outcome` and `health` check `ext.is_disabled()`; test `three_resets_in_a_minute...` asserts a kept slot passes with 0 faults |
| M-3 busy semantics | defined; see N-3 (low) | counters, rate-limited log, summary at stop |
| L-2 / L-3 / L-4 | closed | `with_ext` errors + `debug_assert`; `notice` retries on at most 4 threads with `blocking_send` (bounded leak only if the Reader never drains) |
| M-4 scope | deferred and recorded in `docs/decisions.md` | not re-checked |

## New findings

### N-1 (high): extension `*2`/`*3` chords take the core's multi-press behaviour
`crates/gorca-core/src/commands/compile.rs`, `Mentioned::insert` in `compile_with`. A core binding at
`(chord, 1)` with `max_press = m > 1` implicitly owns presses 2..m: the lane (`gorca-input-api/src/lane.rs`
`lookup_bound`) looks up an explicit `(chord, n)` first and only then falls back to `(chord, 1)` with a repeat
count. `Mentioned` records only `(key, 1)`, so an extension default chord `gorca+tab*2` is installed and wins
the second press.
Probe `probe_r2_keymap.rs` (inserted into the compile tests): **66 shadowings over 14 core chords** across
the default, orca and laptop presets, e.g. `gorca+tab` (where-am-I detailed, presses 2 and 3),
`gorca+t` (time/date and window title), `gorca+l`, `gorca+period`/`gorca+shift+period` (spell/phonetic),
`gorca+kp_begin`, `gorca+q*2` (quit confirmation). Output `probe_r2_keymap.txt`.
Failure scenario: an installed extension ships `chord = "gorca+tab*2"`; the user double-presses where-am-I
and hears the extension's command instead, with no warning (the `tracing::warn!` path is not taken).
Fix: when a core or user binding at `(k, 1)` is inserted, also insert `(k, 2..=bound.max_press)` into
`Mentioned` (the compiled `Bound` has `max_press`); add the probe as a test.

### N-2 (medium): WASI shims bypass the host-work meter; `present` can hold the Reader ~5 ms on every call
`crates/gorca-ext-host/src/wasi_pure.rs` (`get-random-bytes` up to 64 KiB per call, `now`) are linked with
`func_wrap` and never charge `StoreData::work`. Fuel sees only the guest's few instructions per call.
Probe `ext-hostile2` + `probe_r2_present.rs` (release, one application, 300 consecutive `present` calls):

| `present` attack | hold of the calling thread | faults |
|---|---|---|
| `randtime:2000` (random bytes until 2 ms) | p50 2.05 ms, max 2.16 ms | 0 |
| `randtime:4000` | p50 4.05 ms, max 4.16 ms | 0 |
| `randtime:5000` | p50 5.08 ms, max 5.16 ms | **0 / 300, never disabled** |
| `randtime:5500` | max 5.62 ms | 3, disabled (backstop jitter) |
| `random` forever (fresh apps) | 6.1 to 6.4 ms | 20/20 (backstop) |

So the documented "worst Reader hold 0.30 to 0.68 ms" is not the worst case: a hostile or buggy extension
that self-times against the WASI clock adds ~5 ms to every focus presentation indefinitely, i.e. 2.5x the
2 ms `present` limit, and it never shows up as a fault. Two matching extensions double it.
Fix: charge every WASI shim to `work` (`get-random-bytes` by length, `now` by a fixed cost), or cap random
bytes per call; and record the per-call wall time of `present` with a strike when it exceeds the limit
(e.g. 3 consecutive over-limit calls) so a non-faulting slow hook is still disabled.

### N-3 (low): busy slot "repeat last decision" drops unrelated events of the same kind
`crates/gorca-ext-host/src/host.rs` `on_event` `Outcome::Busy`. The decision is keyed by event kind only.
The implementer's own test (`tests/ext.rs` `a_busy_slot_repeats_its_last_decision...`) shows it: after the
extension suppressed one announcement, an unrelated announcement ("hello", which the extension passes) is
suppressed while the slot is busy. Same for `FocusChanged`: an extension that suppresses focus on one
widget silences the next, different focus change if it arrives while a command or a `present` holds the lock
(present runs on the Reader and on_event on the app actor, so they do contend). Rewrite also maps to silence.
Silence on a focus change or live-region announcement is worse for a screen-reader user than a duplicate.
The window is small (busy_skips 0 in X1 storms; commands normally take microseconds), hence low.
Fix: on Busy, Pass for `FocusChanged`, `WindowActivated`, `Announcement`; keep "repeat suppress" only for
high-rate kinds (`TextChanged`, `CaretMoved`, `PropertiesChanged`, `ChildrenChanged`). Alternatively a
bounded try-lock spin (for example 200 us) before falling back.

### N-4 (low): the epoch ticker wakes 1000 times per second forever
`crates/gorca-ext-host/src/wasm.rs` `ensure_ticker`: `TICK` went from 5 ms to 1 ms; the thread runs while any
WASM extension is loaded, idle or not. Cost on a laptop: 1 kHz wakeups. Fix: park the ticker when no hook is
in flight (count in-flight calls, `unpark` on `begin_call`), or keep 1 ms only around `present`.

### N-5 (low, evidence): the "ext loaded" e2e result headers do not show `--ext-dir`
`bench/results/e2e/m5fix-ext/*.txt` and `m5fix-r1-ext/*.txt` print `reader=daemon:<path>/gorca` without the
daemon arguments, so the result files alone cannot prove ext-test was loaded. My rerun below records the
command line in this report.

## R1 failure without extensions (`m5fix-r1-noext`, p99 3.11 ms, 1 lost)
Load, not M5. The extension host is absent in that run (no `--ext-dir`: no host, no ticker). All three
repeats were 3.06 to 3.63 ms with p50 0.32 ms, i.e. a sustained tail during that minute at load 15. History
in `bench/results/e2e/*`: R1 single repeats over 3 ms and `lost` up to 2 occur at load 9.6 to 14 on
other branches (`adopt-k3` repeat max 3.66 ms at load 9.6; `m4a-final` repeat max 3.03 ms and lost 2, a FAIL, at load 14.4). The rerun at load
11 passed (0.408 ms), as did the ext run (0.428 ms) and my run below. R1's 3 ms gate is sensitive to host load.

## E2E (merged tree, ext-test loaded)
Not gate-valid: during every run another agent's load generator (`gorca-hog`, ~1400 % CPU) and builds kept
load at 18 to 40. Kept as is (labels carry `-hog`); the git tree is the uncommitted trial merge (`git_dirty=true`).
Command: `tests/harness/run-isolated.sh target/release/e2e run --label <l> --only ... --repeats 3 --reader
daemon:<abs>/target/release/gorca,--impersonate-orca[,--ext-dir=<scratch>/extdir] --reader-env GORCA_M1=1
--reader-env GORCA_M1_STRICT=1 --reader-env GORCA_M4=1 --reader-env GORCA_M4_STRICT=1`; `extdir/ext-test` =
`extensions/ext-test/ext.toml` + the `ext_test.wasm` built from the merged tree.

| Run (`bench/results/e2e/<label>/`) | load | K2 p99 med/max ms | K2 lost max | R1 p99 med/max ms | R1 lost max | other |
|---|---|---|---|---|---|---|
| `m5rr-merged-ext` (K2,K3,K4,R1) | ~38 | 5.97 / 9.34 FAIL | 12 | 5.73 / 8.05 FAIL | 1 | K3 lost 2; K4 chord_resent 10/10k FAIL |
| `m5rr-merged-noext-hog` (K2,R1) | 37 | 5.32 / 5.55 FAIL | 0 | 4.79 / 5.06 FAIL | 1 | |
| `m5rr-pair-k2-ext-hog` | 34 | 6.89 / 7.67 FAIL | 5 | - | - | |
| `m5rr-pair-k2-noext-hog` | 32 to 20 | 0.39 / 2.63 | 0 | - | - | load fell during run |
| `m5rr-pair1-k2r1-ext` | 19 | 1.98 / 3.18 | 1 | 2.86 / 5.63 | 0 | |
| `m5rr-pair1-k2r1-noext` | 18 | 5.53 / 6.98 FAIL | 3 | 5.66 / 9.18 FAIL | 2 | |
| `m5rr-pair2-k2r1-ext` | 32 | 4.82 / 5.69 | 1 | 4.95 / 5.59 FAIL | 0 | |
| `m5rr-pair2-k2r1-noext` | 35 | 5.42 / 7.64 FAIL | 5 | 7.04 / 7.71 FAIL | 8 | |

Reading: alternating pairs fail with and without ext-test alike (no-ext was worse in both A/B pairs), so I see
no extension-attributable regression, but this round cannot confirm the strict gates pass on the merged tree.
**Open:** rerun K2/K3/K4/R1 with and without ext-test on the merged tree at load below ~12 before merging.

## Reproduce
- Keymap probe: append `probe_r2_keymap.rs` inside `mod tests` of `crates/gorca-core/src/commands/compile.rs`,
  `cargo test -p gorca-core --lib review_probe_r2 -- --nocapture`.
- Present probe: `cargo build --manifest-path bench/results/ext-host/m5-rereview/ext-hostile2/Cargo.toml
  --target wasm32-wasip2 --release --target-dir target/ext-wasm`, copy `probe_r2_present.rs` to
  `crates/gorca-ext-host/tests/`, `cargo test --release -p gorca-ext-host --test probe_r2_present --
  --nocapture --test-threads=1`.
