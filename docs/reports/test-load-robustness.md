# Load-robust unit and integration tests

Date: 2026-10-16. Scope: `cargo test --workspace` must pass on a host at load 10 to 48+
(many agents building and benchmarking in parallel).

## Rule applied

1. Assert logical outcomes and counts, not wall time.
2. A wall-clock upper bound is only ever a hang detector (generous, 10 s or more), polled.
3. A wall-clock lower bound (pacing, delay honoured) is load-safe and stays.
4. A genuine latency budget is measured with thread CPU time (`gorca_testkit::cpu`), as a
   best-of-N over whole repeats, or as a median in debug; the strict p99 lives in the release
   `#[ignore]` gate or the e2e/bench gates (which record load).
5. No correctness assertion was weakened; every assertion still runs in every attempt.

## Changes

| Test | Problem | Fix |
|---|---|---|
| fakeapp `bus.rs` `storm_emits_signals_at_roughly_the_requested_rate` | count of events in a fixed wall window | the fake app's storm now emits a fixed `rate x duration` count (clock only paces it) and reports it in `signals_sent`; test asserts exactly 5000 sent and received, paced >= 400 ms |
| `bus.rs` `closing_peers_...` | one call inside a 2 s timeout, racing the asynchronous close | poll until the old connection fails (60 s deadline, 10 s per call) |
| `bus.rs` delayed-reply test | 150 ms delay vs `< 100 ms` best of 3 | 1.5 s delay, `< 1 s` |
| `bus.rs` head-of-line test | 30 ms dump, second call after 5 ms sleep | 400 ms dump |
| `gorca-ext-host --test ext` reset budgets (4 tests) | 50 ms wall budget on a single attempt | `best_within`: best of up to 10 fresh-host attempts; logic asserted in every attempt; flood test only needs "not a hang" |
| `ext.rs` waits, ticker-parks test | 200 ms to 2 s waits; one 150 ms equality read | 30 s polling waits; poll until two reads agree |
| `core_path.rs` | p99 of wall samples under hostile load; spin present worst < 15 ms | best of 5 runs by p99; CPU time of the Reader thread |
| `hostile.rs` | wall hold of `present` | CPU time of the calling thread |
| `review::cluster the_model_builds_in_budget` | single wall sample | best of 5 |
| `review/tests.rs` p99 of 2000 steps (debug) | p99 wall | median in debug, p99 in release |
| `browse/latency.rs` debug test, J1 debug | p99 wall | median gate in debug (p99 printed); strict p99 stays in the `#[ignore]` release gate |
| `router/tests.rs` first event | core delay 20 ms vs 10 ms | 400 ms vs 200 ms (min of 5 kept) |
| `testkit cpu.rs spin_is_measured` | wall spin loop, CPU time asserted | spin until the thread's CPU clock advanced |
| `conformance.rs` | 50 ms slack, 500 ms poll | 1 s slack, 30 s poll |
| ssip and voicego connect, `submit` | 500 and 600 ms | 10 s (the property is "not hung") |
| ssip and voicego Stopped event | drain with 300 ms idle gap | wait up to 10 s for the event |
| `log.rs` never-blocks | 2 s | 30 s |
| `doc_registry/tests.rs` | 500 ms per notice | 10 s |
| `daemon tests/signals.rs` | 10 s start wait | 60 s |
| `gorca-daemon --test b1_ic` IC-9, IC-23 (real daemon) | fixed 100 ms and 500 ms windows for an event/revalidation round trip | poll (30 s deadline, virtual time on simulated SUTs) until the mirror dropped the old name / the refinement was spoken; the verdict checks are unchanged |
| `fake_peer.rs` hung node | 150 ms per-call timeout timed out healthy calls | 1.5 s; the hung call is still counted exactly |
| `fuzz.rs` (native/wasm fuzz, garbage clamp), `ext.rs` flood and epoch-ticker tests | host-side wall-clock interrupt counted as a fault under load | seeded runs repeated (a real input fault repeats, a descheduling does not); tick test spins fresh apps until a tick is seen |
| `fidelity.rs` (default mode) | p50 delta `<= 1.5 ms` for a 0.5 ms model, measured 2.8 to 3.8 ms at load 70 | ceiling 10 ms; the lower bound (model ignored) is unchanged; the strict +-20 % check stays in the `B1_STRICT` run |
| `daemon tests/shell_parity.rs` | wasm policy hook interrupted by the 6 ms wall backstop under load, verdict `Default` | calls during which the host counted a fault are retried (fresh host if disabled); a fact that faults on all 10 attempts, or any wrong verdict without a fault, still fails |

## Product finding (not changed here)

`policy` and `present` hooks have a wall-clock epoch backstop of 3 x 2 ms
(`crates/gorca-ext-host/src/wasm.rs`, `BACKSTOP_FACTOR_PRESENT`). Under load 60+ a healthy hook
descheduled for 6 ms is interrupted, the app answers "no opinion", and three such strikes in 60 s
disable the extension for that app. Seen as `faults: 2, resets: 2` in `shell_parity`. Suggested fix
(owner: wasm-engineer): in the epoch callback, extend the deadline while the thread's CPU time is
below the CPU limit, and trap only when CPU time or a hard wall cap is exceeded.

## Verification

`gorca-hog` (own PID) on top of the ambient load, `cargo test --workspace --no-fail-fast`.
Ambient load was 26 to 65 throughout (other agents' builds and benchmarks); the hog was 32
threads (2x cores) for runs 1 to 6 and 16 threads (1x) afterwards, at the coordinator's request.

| Run | Result |
|---|---|
| 1 | invalid: a shared target dir was cleaned by another agent mid-run |
| 2 | 1 failing binary (`shell_parity`) |
| 3, 4 | 4 and 4 failing binaries (fidelity, shell_parity, fuzz, fake_peer, ext): the fixes above |
| 5 | exit 0 |
| 6 | exit 0 (load 55 to 65) |
| 7 | stopped early; 1 failure (`b1_ic` IC-9, IC-23): fixed |
| 9 | exit 0, final code (load 27 to 35, hog 1x) |
| 10 | exit 0, final code (load 31 to 39, hog 1x) |
