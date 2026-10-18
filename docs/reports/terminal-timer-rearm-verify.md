# Review: terminal protection watch (terminal-timer-rearm), head d05ef2e8

Date 2026-10-19. Reviewer. Code fix 367687d4. Verdict: **MERGE WITH FIXES** (F-1 must be fixed first).

## Gates (measured, load1 15-25)
- `.toolchain` is a symlink to `<repo>/.toolchain`.
- `cargo fmt --all -- --check`: clean.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace` x2 (4 threads): both rc=0, 126 `test result: ok` lines each, no FAILED.
  The "1 unidentified failure" of the first run did not reproduce in 2 runs, so I cannot name it.
  Under load the known candidate is `harness_focus` (see m6-focus-fix review F2).
- `cargo run -p xtask -- strings`: ok (333 literals, 113 allowed).
- `git merge-tree main HEAD`: clean. The merge base is 9aabf7f6, and main has one docs-only commit since.
- Untracked, not committed: `bench/results/e2e/term-rearm-{ff,chr}-fe2/` (reader logs).

## Root cause
It is plausible but not proven. The code paths exist as described: `on_revalidated` demotes when
the cache-served focus re-read lacks FOCUSED (flow.rs:961-966), `route_text` drops text events while
the app is unprotected (app_actor.rs:323), and before this change a reply without FOCUSED left
`Reconfirm::Asked` set (flow.rs:312). Whether the real VTE answered without FOCUSED is still an
inference: no trace shows the demote. I agree with medium confidence. The watch is a reasonable
fail-closed safety net whatever the precise trigger.

## Scope and side effects
- The watch covers terminals only: it is gated on `text.term.tracked` (ext-vte tracking).
  GTK entries and Firefox get no watch, and they can suffer the same silence through the same
  demote path. That is acceptable for this fix, but it should be noted as a follow-up.
- No focus stealing. The watch only reads state, and only a FOCUSED reply for the *presented* node
  publishes protection. The GTK3 VTE reports FOCUSED only when its toplevel is active.
- If the user switches away during the backoff, `begin_focus` untracks, so the watch ends.
  If the user switches to a window that emits no focus event, the terminal stays presented, the
  reads return "not FOCUSED", and the watch polls at 0.5 Hz until the next focus. That is bounded
  and fail closed.
- Nothing blocks. Each read is one deadline-bound fetch, with one read in flight.

## Findings (probes: `bench/results/e2e/term-rearm-review/probes.patch`, apply to terminal/tests.rs)

### F-1 (high, must fix): a key press during the watch read wedges the watch and the key path for the whole focus
terminal.rs:817 (`may_ask` needs `Idle|Failed`) together with flow.rs:723 (stale replies are dropped
by epoch) and flow.rs:298 (the key path refuses while `Asked`).
- The input lane advances the shared epoch on every non-modifier key (`interrupt_on_keys`, on by
  default).
- A key typed while a watch read is in flight therefore makes its reply stale. `on_reply` drops it,
  and `reconfirm` stays `Asked(node)` forever.
- The watch never asks again, and neither does `reconfirm_on_key`. The terminal is silent until the
  next focus change, which is exactly the symptom this change is meant to fix. The watch increases
  exposure because it issues more reads while the user is typing.
- Probe `probe_p1_stale_reply_wedges_watch`: **0 reads** over 60 s of timers plus a key, with
  `reconfirm=Asked(..)`.
- The old key path already had this bug; the watch inherits it.
- Fix: record the ask time in `Asked`, and treat an `Asked` older than the fetch deadline plus a
  margin as `Failed`. Alternatively, in `on_reply`, turn a stale `StateChange` reply for
  `reconfirm.node()` into `Failed(node, now)`.
- Add a test for it. Also give the statemodel a liveness check: while the world focus is the
  terminal, fetches are healthy and nothing is held, the app is published within about 3 s.

### F-2 (medium): a watch reply speaks a changed title-like name of an unfocused terminal
flow.rs:1128-1145 (`on_state_reply` speaks the `changed()` diff, and the name is part of it).
- `on_state_changed` deliberately does not echo a terminal's title-like name (flow.rs:1067-1069),
  but the watch's `StateChange` reply goes through `on_state_reply`, which does.
- Probe `probe_p3_watch_reply_speaks_name_change`: a reply without FOCUSED and with the name
  "me@box: ~/src" is spoken.
- Scenario: the user is in a window that emits no focus event, a background command changes the
  title, and within 2 s gorca says the title.
- Fix: for a reconfirm or watch read, compare states only, or skip the name when the reply lacks
  FOCUSED.

### F-3 (low): the timer re-arms every 150 ms forever while a timeout is charged
terminal.rs:817-833.
- When `may_ask` is false, `attempts` does not grow, so `wait` stays at `PROTECT_FIRST`.
- When `timeout_charged` is set, which is sticky for the focus, the watch never asks but wakes the
  Reader at 6.7 Hz until the focus changes.
- Probe `probe_p2_timeout_charged_spin`: 200 timer arms and 0 reads in 30 s.
- Fix: when asking is barred by `timeout_charged`, clear the watch (only a key can retry, per RR-1),
  or back off on skipped attempts too.

### F-4 (low): the `term-unwatched` invariant checks wiring, not liveness
- It only checks that `protect` is `Some`, and F-1 passes it.
- The world (`statemodel/world.rs:301`) always reports FOCUSED for the focused node, so the
  unprotected state arises only through `FailFetches` or cache paths.
- Make it engage: add an `Op::LateFocused(bool)` that withholds FOCUSED from the first N snapshots
  after a focus. Weight it next to `Focus` on TERM, and add the liveness check from F-1.

### F-5 (low): FE2 does not check that a stuck key is silent
webtext.rs, where the `None` expectation only waits 450 ms and asserts nothing. A regression that
speaks at the boundary in `cue` mode would pass. Assert that `f.heard(mark)` is empty for `None`.

## Not substantiated
The VV x30 result was not rerun here (it needs the live rig). 0/30 against a 1/25 base rate is weak
evidence, as the report itself says.
