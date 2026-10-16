# Why `tabbing_through_a_form_is_heard` hung the test binary, and the fail-fast pattern

Date: 2026-10-16. Owner: test-engineer.

## Root cause of the hang: a real reader bug, not a test artefact

When `harness_focus` failed, the test body panicked and unwound. Unwinding dropped the `Rig`,
whose `Session` field tore down the private buses while the core and the AT-SPI connector were
still running (nothing had cancelled them). In `watch_shell`
(`crates/gorca-connector-atspi/src/connector.rs`) the `NameOwnerChanged` message stream then
ended; `s.next().await.is_some()` was `false` but the closed stream stayed in the `select!`, so
that branch completed instantly on every turn. Result: a busy loop sending D-Bus messages,
one tokio worker at 100 % CPU (perf: `zvariant` signature parsing and `memmove`), and a runtime
drop that never returned, because the loop never yields control back to the scheduler.

This is not only a test problem: **any loss of the session bus under a running gorca spins one
core forever.** Fixed in `watch_shell`: a closed or failed stream is dropped and the loop falls
back to the 5 s poll.

Regression test: `losing_the_buses_does_not_make_the_connector_spin` (harness_focus.rs). It
kills the buses under a running connector and requires process CPU < 50 % of wall time over 4 s.
Before the fix it fails with `4.00 s of CPU in 4.01 s`; after, it passes. It takes the write side
of an `RwLock` so the other tests in the binary cannot add CPU time.

## Why the original assertion failed under load

Not reproduced as a "speech lost" failure. 140 runs of the full binary at ambient load 25 to 57
(up to 30 parallel, 32 busy loops) passed. With the test niced to 19 on 8 CPUs shared with 24
busy loops, 10 of 30 runs failed, **all** with `isolated session: Timeout("READY")` (30 s
start-up deadline of the harness, hit under starvation); none hung after the fix. So the
original 5/20 failures are consistent with start-up/response starvation of the fake app and
python harness, and no focus speech loss was observed. The 5 s `wait_for` in the tests stays a
polled hang detector; the session READY deadline is now 120 s.

## Fail-fast and teardown pattern (applied)

* `gorca_testkit::harness::run_bounded(workers, deadline, body)`: runtime with a hard overall
  deadline, panic re-raised only after `Runtime::shutdown_timeout(15 s)`, which detaches stuck
  workers instead of waiting for them. Used by harness_focus, daemon e2e and desktop, and the
  three fetch strategy/review test modules.
* `Session::drop` no longer waits unboundedly: own process group, TERM to the group after 20 s,
  KILL after 30 s (PID recorded at spawn, never by name).
* Drop guards cancel the daemon/connector before the buses go: `Rig` (harness_focus, Drop impl),
  `Daemon` and desktop `Rig` (`DropGuard` fields), b1_ic start-up guard.
* `b1_ic` `DaemonSut::drop` joins its daemon thread for at most 40 s then detaches; the daemon
  runtime inside is shut down with `shutdown_timeout(10 s)`; start-up waits 20 s -> 120 s.
* b1_ic `the_policy_hears_of_a_new_app_before_it_does_anything`: the 10 s wait for the two
  announcements is now 60 s (load robustness; the assertions are unchanged).

## Verification

See the end of this file (filled in after the runs).

### Done

* `harness_focus` (4 tests, with the injected failure and with the real assertions): the
  injected failure now ends in 7 s instead of hanging; the CPU regression test fails (4.00 s of
  CPU in 4.01 s, ends in 22.6 s, no hang) on the old connector and passes on the fix.
* 140 runs of the full binary at load 25 to 57: all pass. 30 runs niced and starved: 10 fail on
  harness READY timeout (now 120 s), none hang.
* `cargo fmt --all`: clean. Everything above compiled (`cargo test --no-run` for gorca-daemon and
  gorca-connector-atspi).

### NOT done (blocked by host memory exhaustion, coordinator instruction 2026-10-16)

The host was at RAM 28/30 GB with swap full; a linker of mine stalled for 26 minutes and I
stopped my own build (PIDs recorded, nothing else touched). Not run: `cargo test --workspace`
three times under 1x-core load, `cargo clippy -- -D warnings`, `cargo xtask deps`, the merge
check against the newest main, and the daemon `e2e`/`desktop`/`b1_ic` suites after the edits.
Run those before merging. Not examined: the other storm-suite timing failures listed in
m6-focus-fix-review.md (ext-host hostile/fuzz, storm rate) were already addressed by
test-load-robustness.md; nothing new was applied to them here.

### Final gates (2026-10-16, after memory recovered; -j4, --test-threads=4)

* Merged main (CLAUDE.md, env.sh only). `cargo fmt --check`, `cargo clippy --workspace
  --all-targets -- -D warnings` (two lint fixes: `manual_assert` in `run_bounded`,
  `cast_precision_loss` in the CPU test) and `cargo xtask deps` are clean.
* gorca-daemon + gorca-connector-atspi + gorca-testkit: all pass, including `b1_ic` (3 tests,
  incl. `the_policy_hears_of_a_new_app...`, 258 s), `e2e` 9, `desktop` 6, `harness_focus` 4,
  once unloaded and once under 16 CPU-only busy loops. The only failures are the two
  `shell_parity` tests (see below). No hang in any run.
* `cargo test --workspace --no-fail-fast`, once, no hang: 42 failures, in two groups.
  1. All of gorca-ext-host (backstop, core_path, ext, fuzz, hostile) and the two `shell_parity`
     tests: they build a wasm component in a nested cargo and die with
     `error[E0463]: can't find crate for 'core' ... the wasm32-wasip2 target may not be
     installed`. `rustup target list --installed` in the shared `.toolchain` lists only
     x86_64 although `lib/rustlib/wasm32-wasip2` exists: a toolchain-install problem on this
     host, not the code under test. Unverified on main.
  2. `gorca-dbus drain::raw::tests::cache_signals_map_to_events_and_foreign_refs_are_counted_malformed`
     failed once (`left: 1 right: 2`, stats `pushed: 2, malformed: 1`) and passed 3 of 3 alone;
     the crate is untouched by this change, so it looks like a race in the test. Not
     investigated further.
* The second load run covered only the packages above (not the whole workspace) with 16 busy
  loops, which the coordinator then asked to be limited to 8; it was stopped (own process
  group) after it finished.
